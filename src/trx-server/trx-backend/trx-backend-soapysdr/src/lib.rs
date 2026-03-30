// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

pub mod demod;
pub mod dsp;
pub mod real_iq_source;
pub mod vchan_impl;

use dsp::IqSource as _;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use trx_core::radio::freq::{Band, Freq};
use trx_core::rig::response::RigError;
use trx_core::rig::state::{RigFilterState, SpectrumData, VchanRdsEntry, WfmDenoiseLevel};
use trx_core::rig::{
    AudioSource, Rig, RigAccessMethod, RigCapabilities, RigCat, RigInfo, RigSdr, RigStatusFuture,
};
use trx_core::{DynResult, RigMode};

const AIS_CHANNEL_SPACING_HZ: i64 = 50_000;

pub use vchan_impl::SdrVirtualChannelManager;

/// Configuration struct for constructing a [`SoapySdrRig`].
///
/// Replaces the 20+ parameter `new_with_config()` constructor with a more
/// readable and maintainable builder.  All fields have sensible defaults via
/// the `Default` implementation.
#[derive(Debug, Clone)]
pub struct SoapySdrConfig {
    /// SoapySDR device args string (e.g. `"driver=rtlsdr"`).
    pub args: String,
    /// Per-channel tuples of `(channel_if_hz, initial_mode, audio_bandwidth_hz)`.
    pub channels: Vec<(f64, RigMode, u32)>,
    /// `"auto"` or `"manual"`.
    pub gain_mode: String,
    /// Gain in dB; used when `gain_mode == "manual"`.
    pub gain_db: f64,
    /// Optional hard ceiling for the applied hardware gain in dB.
    pub max_gain_db: Option<f64>,
    /// Output PCM rate (Hz).
    pub audio_sample_rate: u32,
    /// Number of audio channels.
    pub audio_channels: usize,
    /// Output frame length (ms).
    pub frame_duration_ms: u16,
    /// WFM deemphasis time constant in microseconds.
    pub wfm_deemphasis_us: u32,
    /// Initial dial frequency.
    pub initial_freq: Freq,
    /// Initial demodulation mode.
    pub initial_mode: RigMode,
    /// IQ capture rate (Hz).
    pub sdr_sample_rate: u32,
    /// Hardware IF filter bandwidth to apply to the device.
    pub bandwidth_hz: u32,
    /// The hardware is tuned this many Hz *below* the dial frequency so the
    /// desired signal lands off-DC.  The DSP mixer shifts it back.
    pub center_offset_hz: i64,
    /// Enable software squelch for all modes except WFM.
    pub squelch_enabled: bool,
    /// Squelch open threshold in dBFS.
    pub squelch_threshold_db: f32,
    /// Close hysteresis in dB.
    pub squelch_hysteresis_db: f32,
    /// Tail hold time in milliseconds.
    pub squelch_tail_ms: u32,
    /// Maximum number of dynamic virtual channels.
    pub max_virtual_channels: usize,
    /// Whether the noise blanker is enabled on the primary channel.
    pub nb_enabled: bool,
    /// Noise blanker impulse threshold multiplier.
    pub nb_threshold: f64,
}

impl Default for SoapySdrConfig {
    fn default() -> Self {
        Self {
            args: String::new(),
            channels: Vec::new(),
            gain_mode: "auto".to_string(),
            gain_db: 30.0,
            max_gain_db: None,
            audio_sample_rate: 48_000,
            audio_channels: 1,
            frame_duration_ms: 20,
            wfm_deemphasis_us: 50,
            initial_freq: Freq { hz: 144_300_000 },
            initial_mode: RigMode::USB,
            sdr_sample_rate: 1_920_000,
            bandwidth_hz: 1_500_000,
            center_offset_hz: 0,
            squelch_enabled: false,
            squelch_threshold_db: -65.0,
            squelch_hysteresis_db: 3.0,
            squelch_tail_ms: 180,
            max_virtual_channels: 4,
            nb_enabled: false,
            nb_threshold: 10.0,
        }
    }
}

/// RX-only backend for any SoapySDR-compatible device.
pub struct SoapySdrRig {
    info: RigInfo,
    freq: Freq,
    mode: RigMode,
    pipeline: Arc<dsp::SdrPipeline>,
    /// Index of the primary channel in `pipeline.channel_dsps`.
    primary_channel_idx: usize,
    /// Current filter state of the primary channel (for filter_controls support).
    bandwidth_hz: u32,
    /// Shared spectrum magnitude buffer populated by the IQ read loop.
    spectrum_buf: Arc<Mutex<Option<Vec<f32>>>>,
    /// How many Hz below the dial frequency the SDR hardware is actually tuned.
    /// The DSP mixer compensates for this offset to demodulate the dial frequency.
    center_offset_hz: i64,
    /// Actual hardware center frequency currently tuned on the SDR.
    center_hz: i64,
    /// Used to send hardware retune commands to the IQ read loop.
    retune_cmd: Arc<std::sync::Mutex<Option<f64>>>,
    /// Current WFM deemphasis setting in microseconds.
    wfm_deemphasis_us: u32,
    /// Whether WFM stereo decode is enabled.
    wfm_stereo: bool,
    /// Whether WFM stereo denoise is enabled.
    wfm_denoise: WfmDenoiseLevel,
    /// SAM stereo width (0.0 = mono, 1.0 = full stereo).
    sam_stereo_width: f32,
    /// SAM carrier synchronization enabled.
    sam_carrier_sync: bool,
    /// Requested hardware gain setting in dB.
    gain_db: f64,
    /// Optional hard ceiling for the applied hardware gain in dB.
    max_gain_db: Option<f64>,
    /// Requested LNA gain element setting in dB (None if not set by user).
    lna_gain_db: Option<f64>,
    /// Whether hardware AGC is currently enabled.
    agc_enabled: bool,
    /// Whether software squelch is enabled on primary channel (except WFM mode).
    squelch_enabled: bool,
    /// Software squelch threshold (dBFS) on primary channel.
    squelch_threshold_db: f32,
    /// Whether the noise blanker is enabled on the primary channel.
    nb_enabled: bool,
    /// Noise blanker impulse threshold multiplier.
    nb_threshold: f64,
    /// Hidden AIS decoder channels (A and B) when available.
    ais_channel_indices: Option<(usize, usize)>,
    /// Virtual channel manager shared with external consumers (e.g. RigHandle).
    channel_manager: Arc<vchan_impl::SdrVirtualChannelManager>,
}

impl SoapySdrRig {
    fn default_bandwidth_for_mode(mode: &RigMode) -> u32 {
        match mode {
            RigMode::LSB | RigMode::USB | RigMode::DIG => 3_000,
            RigMode::PKT | RigMode::AIS => 25_000,
            RigMode::VDES => 100_000,
            RigMode::CW | RigMode::CWR => 500,
            RigMode::AM | RigMode::SAM => 9_000,
            RigMode::FM => 12_500,
            RigMode::WFM => 180_000,
            RigMode::Other(_) => 3_000,
        }
    }

    /// Construct from a [`SoapySdrConfig`] struct.
    ///
    /// This is the preferred constructor.  See [`SoapySdrConfig`] for field
    /// documentation and defaults.
    pub fn new_from_config(config: SoapySdrConfig) -> DynResult<Self> {
        let args = &config.args;
        let channels = &config.channels;
        let gain_mode = &config.gain_mode;
        let gain_db = config.gain_db;
        let max_gain_db = config.max_gain_db;
        let audio_sample_rate = config.audio_sample_rate;
        let audio_channels = config.audio_channels;
        let frame_duration_ms = config.frame_duration_ms;
        let wfm_deemphasis_us = config.wfm_deemphasis_us;
        let initial_freq = config.initial_freq;
        let initial_mode = config.initial_mode;
        let sdr_sample_rate = config.sdr_sample_rate;
        let bandwidth_hz = config.bandwidth_hz;
        let center_offset_hz = config.center_offset_hz;
        let squelch_enabled = config.squelch_enabled;
        let squelch_threshold_db = config.squelch_threshold_db;
        let squelch_hysteresis_db = config.squelch_hysteresis_db;
        let squelch_tail_ms = config.squelch_tail_ms;
        let max_virtual_channels = config.max_virtual_channels;
        let nb_enabled = config.nb_enabled;
        let nb_threshold = config.nb_threshold;
        tracing::info!(
            "initialising SoapySDR backend (args={:?}, gain_mode={:?}, gain_db={}, max_gain_db={:?})",
            args,
            gain_mode,
            gain_db,
            max_gain_db,
        );

        let effective_gain_db = max_gain_db
            .map(|max_gain| gain_db.min(max_gain))
            .unwrap_or(gain_db);
        if (effective_gain_db - gain_db).abs() > f64::EPSILON {
            tracing::info!(
                "Clamping SoapySDR gain from {} dB to {} dB due to configured max_value",
                gain_db,
                effective_gain_db,
            );
        }

        // The hardware tunes `center_offset_hz` below the dial frequency so
        // the desired signal avoids the DC spike.  The DSP mixer compensates.
        let hardware_center_hz = initial_freq.hz as i64 - center_offset_hz;

        // Create real IQ source from hardware device.
        let mut iq_source = real_iq_source::RealIqSource::new(
            args,
            hardware_center_hz as f64,
            sdr_sample_rate as f64,
            bandwidth_hz as f64,
            effective_gain_db,
        )?;
        // Read the initial LNA gain from the hardware before the source is
        // moved into the pipeline thread.  Returns None on devices that do
        // not expose an "LNA" gain element (e.g. RTL-SDR exposes "TUNER").
        let initial_lna_gain_db = iq_source.read_named_gain("LNA");
        if let Some(lna) = initial_lna_gain_db {
            tracing::info!("SDR LNA gain element present, initial value: {:.1} dB", lna);
        }

        // Enable hardware AGC by default if the device supports it.
        let agc_enabled = if iq_source.has_gain_mode() {
            match iq_source.set_gain_mode(true) {
                Ok(()) => {
                    tracing::info!("Hardware AGC enabled by default");
                    true
                }
                Err(e) => {
                    tracing::warn!("Failed to enable hardware AGC: {}", e);
                    false
                }
            }
        } else {
            tracing::debug!("Hardware AGC not supported by this device");
            false
        };
        let iq_source: Box<dyn dsp::IqSource> = Box::new(iq_source);

        let primary_channel_count = channels.len();
        let mut all_channels = channels.to_vec();
        all_channels.push((
            (initial_freq.hz as i64 - hardware_center_hz) as f64,
            RigMode::FM,
            25_000,
        ));
        all_channels.push((
            (initial_freq.hz as i64 + AIS_CHANNEL_SPACING_HZ - hardware_center_hz) as f64,
            RigMode::FM,
            25_000,
        ));
        let block_ms = if sdr_sample_rate == 0 {
            0.0
        } else {
            dsp::IQ_BLOCK_SIZE as f64 * 1000.0 / sdr_sample_rate as f64
        };
        let squelch_tail_blocks = if block_ms <= 0.0 {
            0
        } else {
            (squelch_tail_ms as f64 / block_ms).ceil().max(0.0) as u32
        };

        let pipeline = Arc::new(dsp::SdrPipeline::start(
            iq_source,
            sdr_sample_rate,
            audio_sample_rate,
            audio_channels,
            frame_duration_ms,
            wfm_deemphasis_us,
            true, // wfm_stereo: enabled by default
            dsp::VirtualSquelchConfig {
                enabled: squelch_enabled,
                threshold_db: squelch_threshold_db,
                hysteresis_db: squelch_hysteresis_db,
                tail_blocks: squelch_tail_blocks,
            },
            dsp::NoiseBlankerConfig {
                enabled: nb_enabled,
                threshold: nb_threshold as f32,
            },
            &all_channels,
        ));

        let info = RigInfo {
            manufacturer: "SoapySDR".to_string(),
            model: args.to_string(),
            revision: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: RigCapabilities {
                min_freq_step_hz: 1,
                supported_bands: vec![Band {
                    low_hz: 0,
                    high_hz: 6_000_000_000,
                    tx_allowed: false,
                }],
                supported_modes: vec![
                    RigMode::LSB,
                    RigMode::USB,
                    RigMode::CW,
                    RigMode::CWR,
                    RigMode::AM,
                    RigMode::SAM,
                    RigMode::WFM,
                    RigMode::FM,
                    RigMode::AIS,
                    RigMode::VDES,
                    RigMode::DIG,
                    RigMode::PKT,
                ],
                num_vfos: 1,
                lock: false,
                lockable: false,
                attenuator: false,
                preamp: false,
                rit: false,
                rpt: false,
                split: false,
                tx: false,
                tx_limit: false,
                vfo_switch: false,
                filter_controls: true,
                signal_meter: true,
            },
            // No serial/TCP access for SDR devices; carry args in addr field.
            access: RigAccessMethod::Tcp {
                addr: format!("soapysdr:{}", args),
            },
        };

        // Initialise filter state from primary channel config (index 0), or defaults.
        let bandwidth_hz = channels.first().map(|&(_, _, bw)| bw).unwrap_or(3000);

        let spectrum_buf = pipeline.spectrum_buf.clone();
        let retune_cmd = pipeline.retune_cmd.clone();
        // Initial center_hz stored in the pipeline's shared atomic so the
        // virtual channel manager can read it without a reference to SoapySdrRig.
        pipeline
            .shared_center_hz
            .store(hardware_center_hz, Ordering::Relaxed);
        // Fixed slots: user-configured channels + 2 AIS channels.
        let fixed_slot_count = all_channels.len();
        let channel_manager = Arc::new(vchan_impl::SdrVirtualChannelManager::new(
            pipeline.clone(),
            fixed_slot_count,
            max_virtual_channels,
        ));

        let rig = Self {
            info,
            freq: initial_freq,
            mode: initial_mode,
            pipeline,
            primary_channel_idx: 0,
            bandwidth_hz,
            spectrum_buf,
            center_offset_hz,
            center_hz: hardware_center_hz,
            retune_cmd,
            wfm_deemphasis_us,
            wfm_stereo: true,
            wfm_denoise: WfmDenoiseLevel::Auto,
            sam_stereo_width: 1.0,
            sam_carrier_sync: true,
            gain_db,
            max_gain_db,
            lna_gain_db: initial_lna_gain_db,
            agc_enabled,
            squelch_enabled,
            squelch_threshold_db,
            nb_enabled,
            nb_threshold,
            ais_channel_indices: Some((primary_channel_count, primary_channel_count + 1)),
            channel_manager,
        };
        rig.apply_ais_channel_activity();
        Ok(rig)
    }

    /// Legacy constructor kept for backward compatibility.
    ///
    /// Prefer [`Self::new_from_config`] with a [`SoapySdrConfig`] struct for
    /// better readability.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_config(
        args: &str,
        channels: &[(f64, RigMode, u32)],
        gain_mode: &str,
        gain_db: f64,
        max_gain_db: Option<f64>,
        audio_sample_rate: u32,
        audio_channels: usize,
        frame_duration_ms: u16,
        wfm_deemphasis_us: u32,
        initial_freq: Freq,
        initial_mode: RigMode,
        sdr_sample_rate: u32,
        bandwidth_hz: u32,
        center_offset_hz: i64,
        squelch_enabled: bool,
        squelch_threshold_db: f32,
        squelch_hysteresis_db: f32,
        squelch_tail_ms: u32,
        max_virtual_channels: usize,
        nb_enabled: bool,
        nb_threshold: f64,
    ) -> DynResult<Self> {
        Self::new_from_config(SoapySdrConfig {
            args: args.to_string(),
            channels: channels.to_vec(),
            gain_mode: gain_mode.to_string(),
            gain_db,
            max_gain_db,
            audio_sample_rate,
            audio_channels,
            frame_duration_ms,
            wfm_deemphasis_us,
            initial_freq,
            initial_mode,
            sdr_sample_rate,
            bandwidth_hz,
            center_offset_hz,
            squelch_enabled,
            squelch_threshold_db,
            squelch_hysteresis_db,
            squelch_tail_ms,
            max_virtual_channels,
            nb_enabled,
            nb_threshold,
        })
    }

    /// Simple constructor for backward compatibility with the factory function.
    /// Creates a pipeline with no channels — the DSP loop runs but produces no
    /// PCM frames.
    pub fn new(args: &str) -> DynResult<Self> {
        Self::new_from_config(SoapySdrConfig {
            args: args.to_string(),
            ..SoapySdrConfig::default()
        })
    }

    /// Return the virtual channel manager for this SDR rig.
    /// Used by `RigHandle` to expose the channel API.
    pub fn channel_manager(&self) -> trx_core::vchan::SharedVChanManager {
        self.channel_manager.clone()
    }

    fn update_ais_channel_offsets(&self) {
        let Some((ais_a_idx, ais_b_idx)) = self.ais_channel_indices else {
            return;
        };
        let dsps = self.pipeline.channel_dsps.read().unwrap();
        if let Some(dsp_arc) = dsps.get(ais_a_idx) {
            let if_hz = (self.freq.hz as i64 - self.center_hz) as f64;
            dsp_arc.lock().unwrap().set_channel_if_hz(if_hz);
        }
        if let Some(dsp_arc) = dsps.get(ais_b_idx) {
            let if_hz = (self.freq.hz as i64 + AIS_CHANNEL_SPACING_HZ - self.center_hz) as f64;
            dsp_arc.lock().unwrap().set_channel_if_hz(if_hz);
        }
    }

    fn apply_ais_channel_filters(&self) {
        let Some((ais_a_idx, ais_b_idx)) = self.ais_channel_indices else {
            return;
        };
        let dsps = self.pipeline.channel_dsps.read().unwrap();
        for idx in [ais_a_idx, ais_b_idx] {
            if let Some(dsp_arc) = dsps.get(idx) {
                dsp_arc.lock().unwrap().set_filter(self.bandwidth_hz);
            }
        }
    }

    fn apply_ais_channel_activity(&self) {
        let Some((ais_a_idx, ais_b_idx)) = self.ais_channel_indices else {
            return;
        };
        let enabled = matches!(self.mode, RigMode::AIS);
        let dsps = self.pipeline.channel_dsps.read().unwrap();
        for idx in [ais_a_idx, ais_b_idx] {
            if let Some(dsp_arc) = dsps.get(idx) {
                dsp_arc.lock().unwrap().set_processing_enabled(enabled);
            }
        }
    }

    /// Current hardware center frequency (Hz).
    pub fn center_hz(&self) -> i64 {
        self.center_hz
    }

    /// Half of the SDR capture bandwidth (Hz).
    pub fn half_span_hz(&self) -> i64 {
        i64::from(self.pipeline.sdr_sample_rate) / 2
    }

    pub fn subscribe_iq_channel(
        &self,
        channel_idx: usize,
    ) -> tokio::sync::broadcast::Receiver<Vec<num_complex::Complex<f32>>> {
        // iq_senders covers fixed channels only (primary + AIS).
        if let Some(sender) = self.pipeline.iq_senders.get(channel_idx) {
            sender.subscribe()
        } else {
            let (tx, rx) = tokio::sync::broadcast::channel(1);
            drop(tx);
            rx
        }
    }
}

// ---------------------------------------------------------------------------
// Rig
// ---------------------------------------------------------------------------

impl Rig for SoapySdrRig {
    fn info(&self) -> &RigInfo {
        &self.info
    }
}

// ---------------------------------------------------------------------------
// AudioSource
// ---------------------------------------------------------------------------

impl AudioSource for SoapySdrRig {
    fn subscribe_pcm(&self) -> tokio::sync::broadcast::Receiver<Vec<f32>> {
        self.subscribe_pcm_channel(self.primary_channel_idx)
    }

    fn subscribe_pcm_channel(
        &self,
        channel_idx: usize,
    ) -> tokio::sync::broadcast::Receiver<Vec<f32>> {
        if let Some(sender) = self.pipeline.pcm_senders.get(channel_idx) {
            sender.subscribe()
        } else {
            // No channels configured — return a receiver that will never
            // produce frames (drop the sender immediately).
            let (tx, rx) = tokio::sync::broadcast::channel(1);
            drop(tx);
            rx
        }
    }
}

// ---------------------------------------------------------------------------
// RigCat
// ---------------------------------------------------------------------------

impl RigCat for SoapySdrRig {
    // -- Supported RX methods -----------------------------------------------

    fn get_status<'a>(&'a mut self) -> RigStatusFuture<'a> {
        Box::pin(async move { Ok((self.freq, self.mode.clone(), None)) })
    }

    fn set_freq<'a>(
        &'a mut self,
        freq: Freq,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!("SoapySdrRig: set_freq -> {} Hz", freq.hz);
            let freq_changed = self.freq.hz != freq.hz;
            self.freq = freq;
            let half_span_hz = i128::from(self.pipeline.sdr_sample_rate) / 2;
            let current_center_hz = i128::from(self.center_hz);
            let target_lo_hz = i128::from(freq.hz);
            let target_hi_hz = if self.mode == RigMode::AIS {
                i128::from(freq.hz) + i128::from(AIS_CHANNEL_SPACING_HZ)
            } else {
                i128::from(freq.hz)
            };
            let within_current_span = target_lo_hz >= current_center_hz - half_span_hz
                && target_hi_hz <= current_center_hz + half_span_hz;

            if !within_current_span {
                // Only retune when the requested dial frequency leaves the
                // currently captured SDR bandwidth.
                let hardware_hz = freq.hz as i64 - self.center_offset_hz;
                self.center_hz = hardware_hz;
                if let Ok(mut cmd) = self.retune_cmd.lock() {
                    *cmd = Some(hardware_hz as f64);
                }
                self.channel_manager.update_center_hz(hardware_hz);
            }

            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    let channel_if_hz = (self.freq.hz as i64 - self.center_hz) as f64;
                    let mut dsp = dsp_arc.lock().unwrap();
                    dsp.set_channel_if_hz(channel_if_hz);
                    if freq_changed {
                        dsp.reset_wfm_state();
                    }
                }
            }
            self.update_ais_channel_offsets();
            Ok(())
        })
    }

    fn set_mode<'a>(
        &'a mut self,
        mode: RigMode,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!("SoapySdrRig: set_mode -> {:?}", mode);
            self.mode = mode.clone();
            self.bandwidth_hz = Self::default_bandwidth_for_mode(&mode);
            // Update the primary channel's demodulator in the live pipeline.
            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    let mut dsp = dsp_arc.lock().unwrap();
                    dsp.set_mode(&mode);
                    dsp.set_filter(self.bandwidth_hz);
                }
            }
            self.apply_ais_channel_activity();
            self.apply_ais_channel_filters();
            Ok(())
        })
    }

    fn get_signal_strength<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move {
            let signal_db = self
                .pipeline
                .channel_dsps
                .read()
                .unwrap()
                .get(self.primary_channel_idx)
                .and_then(|dsp| dsp.lock().ok().map(|d| d.signal_db()))
                .unwrap_or(-120.0);
            // Map DSP signal power (roughly -120 .. 0 dBFS) to 0..15 range
            // to match the FT-817 meter scale used by map_signal_strength.
            let clamped = signal_db.clamp(-120.0, 0.0);
            let raw = ((clamped + 120.0) / 120.0 * 15.0).round() as u8;
            Ok(raw.min(15))
        })
    }

    fn get_signal_strength_db<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = Option<f64>> + Send + 'a>> {
        Box::pin(async move {
            self.pipeline
                .channel_dsps
                .read()
                .unwrap()
                .get(self.primary_channel_idx)
                .and_then(|dsp| dsp.lock().ok().map(|d| d.signal_db() as f64))
        })
    }

    // -- TX / unsupported methods -------------------------------------------

    fn set_ptt<'a>(
        &'a mut self,
        _ptt: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("set_ptt"))
                as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn power_on<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("power_on"))
                as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn power_off<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("power_off"))
                as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn get_tx_power<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("get_tx_power"))
                as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn get_tx_limit<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("get_tx_limit"))
                as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn set_tx_limit<'a>(
        &'a mut self,
        _limit: u8,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("set_tx_limit"))
                as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn toggle_vfo<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("toggle_vfo"))
                as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn lock<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("lock"))
                as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn unlock<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("unlock"))
                as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    /// Override: this backend provides demodulated PCM audio.
    fn as_audio_source(&self) -> Option<&dyn AudioSource> {
        Some(self)
    }

    fn as_sdr(&mut self) -> Option<&mut dyn RigSdr> {
        Some(self)
    }

    fn as_sdr_ref(&self) -> Option<&dyn RigSdr> {
        Some(self)
    }
}

// ---------------------------------------------------------------------------
// RigSdr — SDR-specific extension
// ---------------------------------------------------------------------------

impl RigSdr for SoapySdrRig {
    fn set_center_freq<'a>(
        &'a mut self,
        freq: Freq,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!("SoapySdrRig: set_center_freq -> {} Hz", freq.hz);
            self.center_hz = freq.hz as i64;
            if let Ok(mut cmd) = self.retune_cmd.lock() {
                *cmd = Some(self.center_hz as f64);
            }
            self.channel_manager.update_center_hz(self.center_hz);
            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    let channel_if_hz = (self.freq.hz as i64 - self.center_hz) as f64;
                    dsp_arc.lock().unwrap().set_channel_if_hz(channel_if_hz);
                }
            }
            self.update_ais_channel_offsets();
            Ok(())
        })
    }

    fn set_bandwidth<'a>(
        &'a mut self,
        bandwidth_hz: u32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!("SoapySdrRig: set_bandwidth -> {} Hz", bandwidth_hz);
            self.bandwidth_hz = bandwidth_hz;
            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    dsp_arc.lock().unwrap().set_filter(bandwidth_hz);
                }
            }
            self.apply_ais_channel_filters();
            Ok(())
        })
    }

    fn set_wfm_deemphasis<'a>(
        &'a mut self,
        deemphasis_us: u32,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            let deemphasis_us = match deemphasis_us {
                50 | 75 => deemphasis_us,
                other => {
                    return Err(format!("unsupported WFM deemphasis {}", other).into());
                }
            };
            self.wfm_deemphasis_us = deemphasis_us;
            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    dsp_arc.lock().unwrap().set_wfm_deemphasis(deemphasis_us);
                }
            }
            Ok(())
        })
    }

    fn set_sdr_gain<'a>(
        &'a mut self,
        gain_db: f64,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            if !gain_db.is_finite() {
                return Err("gain must be finite".into());
            }
            if gain_db < 0.0 {
                return Err("gain must be >= 0".into());
            }
            self.gain_db = gain_db;
            let effective_gain_db = self
                .max_gain_db
                .map(|max_gain| gain_db.min(max_gain))
                .unwrap_or(gain_db);
            if let Ok(mut cmd) = self.pipeline.gain_cmd.lock() {
                *cmd = Some(effective_gain_db);
            }
            Ok(())
        })
    }

    fn set_sdr_lna_gain<'a>(
        &'a mut self,
        gain_db: f64,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            if !gain_db.is_finite() {
                return Err("LNA gain must be finite".into());
            }
            if gain_db < 0.0 {
                return Err("LNA gain must be >= 0".into());
            }
            self.lna_gain_db = Some(gain_db);
            if let Ok(mut cmd) = self.pipeline.lna_gain_cmd.lock() {
                *cmd = Some(gain_db);
            }
            Ok(())
        })
    }

    fn set_sdr_agc<'a>(
        &'a mut self,
        enabled: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            self.agc_enabled = enabled;
            if let Ok(mut cmd) = self.pipeline.agc_cmd.lock() {
                *cmd = Some(enabled);
            }
            Ok(())
        })
    }

    fn set_sdr_squelch<'a>(
        &'a mut self,
        enabled: bool,
        threshold_db: f64,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            if !threshold_db.is_finite() {
                return Err("squelch threshold must be finite".into());
            }
            if !(-140.0..=0.0).contains(&threshold_db) {
                return Err("squelch threshold must be in range -140..=0 dBFS".into());
            }
            self.squelch_enabled = enabled;
            self.squelch_threshold_db = threshold_db as f32;
            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    dsp_arc
                        .lock()
                        .unwrap()
                        .set_squelch(enabled, self.squelch_threshold_db);
                }
            }
            Ok(())
        })
    }

    fn set_sdr_noise_blanker<'a>(
        &'a mut self,
        enabled: bool,
        threshold: f64,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            if !threshold.is_finite() {
                return Err("noise blanker threshold must be finite".into());
            }
            if !(1.0..=100.0).contains(&threshold) {
                return Err("noise blanker threshold must be in range 1..=100".into());
            }
            self.nb_enabled = enabled;
            self.nb_threshold = threshold;
            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    dsp_arc
                        .lock()
                        .unwrap()
                        .set_noise_blanker(enabled, threshold as f32);
                }
            }
            Ok(())
        })
    }

    fn set_wfm_stereo<'a>(
        &'a mut self,
        enabled: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            self.wfm_stereo = enabled;
            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    dsp_arc.lock().unwrap().set_wfm_stereo(enabled);
                }
            }
            Ok(())
        })
    }

    fn set_wfm_denoise<'a>(
        &'a mut self,
        level: WfmDenoiseLevel,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            self.wfm_denoise = level;
            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    dsp_arc.lock().unwrap().set_wfm_denoise(level);
                }
            }
            Ok(())
        })
    }

    fn set_sam_stereo_width<'a>(
        &'a mut self,
        width: f32,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            self.sam_stereo_width = width.clamp(0.0, 1.0);
            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    dsp_arc.lock().unwrap().set_sam_stereo_width(width);
                }
            }
            Ok(())
        })
    }

    fn set_sam_carrier_sync<'a>(
        &'a mut self,
        enabled: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            self.sam_carrier_sync = enabled;
            {
                let dsps = self.pipeline.channel_dsps.read().unwrap();
                if let Some(dsp_arc) = dsps.get(self.primary_channel_idx) {
                    dsp_arc.lock().unwrap().set_sam_carrier_sync(enabled);
                }
            }
            Ok(())
        })
    }

    fn filter_state(&self) -> Option<RigFilterState> {
        let (wfm_stereo_detected, wfm_cci, wfm_aci) = {
            let dsps = self.pipeline.channel_dsps.read().unwrap();
            let dsp = dsps.get(self.primary_channel_idx);
            let stereo = dsp
                .and_then(|d| d.lock().ok().map(|d| d.wfm_stereo_detected()))
                .unwrap_or(false);
            let cci = dsp
                .and_then(|d| d.lock().ok().map(|d| d.wfm_cci()))
                .unwrap_or(0);
            let aci = dsp
                .and_then(|d| d.lock().ok().map(|d| d.wfm_aci()))
                .unwrap_or(0);
            (stereo, cci, aci)
        };
        Some(RigFilterState {
            bandwidth_hz: self.bandwidth_hz,
            cw_center_hz: 700,
            sdr_gain_db: Some(
                self.max_gain_db
                    .map(|max_gain| self.gain_db.min(max_gain))
                    .unwrap_or(self.gain_db),
            ),
            sdr_lna_gain_db: self.lna_gain_db,
            sdr_agc_enabled: Some(self.agc_enabled),
            sdr_squelch_enabled: Some(self.squelch_enabled),
            sdr_squelch_threshold_db: Some(self.squelch_threshold_db as f64),
            sdr_nb_enabled: Some(self.nb_enabled),
            sdr_nb_threshold: Some(self.nb_threshold),
            wfm_deemphasis_us: self.wfm_deemphasis_us,
            wfm_stereo: self.wfm_stereo,
            wfm_stereo_detected,
            wfm_denoise: self.wfm_denoise,
            wfm_cci,
            wfm_aci,
            sam_stereo_width: self.sam_stereo_width,
            sam_carrier_sync: self.sam_carrier_sync,
        })
    }

    fn get_spectrum(&self) -> Option<SpectrumData> {
        let bins = self.spectrum_buf.lock().ok()?.clone()?;
        let rds = self
            .pipeline
            .channel_dsps
            .read()
            .unwrap()
            .get(self.primary_channel_idx)
            .and_then(|dsp| dsp.lock().ok().and_then(|d| d.rds_data()));
        Some(SpectrumData {
            bins,
            center_hz: self.center_hz.max(0) as u64,
            sample_rate: self.pipeline.sdr_sample_rate,
            rds,
        })
    }

    fn get_vchan_rds(&self) -> Option<Vec<VchanRdsEntry>> {
        Some(self.channel_manager.rds_snapshots())
    }
}
