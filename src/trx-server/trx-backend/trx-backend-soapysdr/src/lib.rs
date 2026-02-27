// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

pub mod demod;
pub mod dsp;
pub mod real_iq_source;

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use trx_core::radio::freq::{Band, Freq};
use trx_core::rig::response::RigError;
use trx_core::rig::state::{RigFilterState, SpectrumData};
use trx_core::rig::{
    AudioSource, Rig, RigAccessMethod, RigCapabilities, RigCat, RigInfo, RigStatusFuture,
};
use trx_core::{DynResult, RigMode};

/// RX-only backend for any SoapySDR-compatible device.
pub struct SoapySdrRig {
    info: RigInfo,
    freq: Freq,
    mode: RigMode,
    pipeline: dsp::SdrPipeline,
    /// Index of the primary channel in `pipeline.channel_dsps`.
    primary_channel_idx: usize,
    /// Current filter state of the primary channel (for filter_controls support).
    bandwidth_hz: u32,
    fir_taps: u32,
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
}

impl SoapySdrRig {
    /// Full constructor.  All channel configuration is passed as plain
    /// parameters so this crate does not need to depend on `trx-server`
    /// (which is a binary, not a library crate).
    ///
    /// # Parameters
    /// - `args`: SoapySDR device args string (e.g. `"driver=rtlsdr"`).
    ///   Opens a real hardware device via SoapySDR.
    /// - `channels`: per-channel tuples of
    ///   `(channel_if_hz, initial_mode, audio_bandwidth_hz, fir_taps)`.
    /// - `gain_mode`: `"auto"` or `"manual"`.
    /// - `gain_db`: gain in dB; used when `gain_mode == "manual"`.
    ///   When `gain_mode == "auto"` hardware AGC is not yet wired, so this
    ///   value acts as the fallback.
    /// - `audio_sample_rate`: output PCM rate (Hz).
    /// - `frame_duration_ms`: output frame length (ms).
    /// - `initial_freq`: initial dial frequency reported by `get_status`.
    /// - `initial_mode`: initial demodulation mode.
    /// - `sdr_sample_rate`: IQ capture rate (Hz).
    /// - `bandwidth_hz`: hardware IF filter bandwidth to apply to the device.
    /// - `center_offset_hz`: the hardware is tuned this many Hz *below* the
    ///   dial frequency so the desired signal lands off-DC.  The DSP mixer
    ///   shifts it back.  Pass 0 to tune exactly to the dial frequency.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_config(
        args: &str,
        channels: &[(f64, RigMode, u32, usize)],
        gain_mode: &str,
        gain_db: f64,
        audio_sample_rate: u32,
        audio_channels: usize,
        frame_duration_ms: u16,
        initial_freq: Freq,
        initial_mode: RigMode,
        sdr_sample_rate: u32,
        bandwidth_hz: u32,
        center_offset_hz: i64,
    ) -> DynResult<Self> {
        tracing::info!(
            "initialising SoapySDR backend (args={:?}, gain_mode={:?}, gain_db={})",
            args,
            gain_mode,
            gain_db,
        );

        if gain_mode == "auto" {
            tracing::warn!(
                "SoapySDR hardware AGC is not yet implemented; falling back to configured \
                 gain of {} dB",
                gain_db,
            );
        }

        // The hardware tunes `center_offset_hz` below the dial frequency so
        // the desired signal avoids the DC spike.  The DSP mixer compensates.
        let hardware_center_hz = initial_freq.hz as i64 - center_offset_hz;

        // Create real IQ source from hardware device.
        let iq_source: Box<dyn dsp::IqSource> = Box::new(real_iq_source::RealIqSource::new(
            args,
            hardware_center_hz as f64,
            sdr_sample_rate as f64,
            bandwidth_hz as f64,
            gain_db,
        )?);

        let pipeline = dsp::SdrPipeline::start(
            iq_source,
            sdr_sample_rate,
            audio_sample_rate,
            audio_channels,
            frame_duration_ms,
            75,
            channels,
        );

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
                    RigMode::WFM,
                    RigMode::FM,
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
        let (bandwidth_hz, fir_taps) = channels
            .first()
            .map(|&(_, _, bw, taps)| (bw, taps as u32))
            .unwrap_or((3000, 64));

        let spectrum_buf = pipeline.spectrum_buf.clone();
        let retune_cmd = pipeline.retune_cmd.clone();

        Ok(Self {
            info,
            freq: initial_freq,
            mode: initial_mode,
            pipeline,
            primary_channel_idx: 0,
            bandwidth_hz,
            fir_taps,
            spectrum_buf,
            center_offset_hz,
            center_hz: hardware_center_hz,
            retune_cmd,
            wfm_deemphasis_us: 75,
        })
    }

    /// Simple constructor for backward compatibility with the factory function.
    /// Creates a pipeline with no channels — the DSP loop runs but produces no
    /// PCM frames.
    pub fn new(args: &str) -> DynResult<Self> {
        Self::new_with_config(
            args,
            &[], // no channels — pipeline does nothing; filter defaults applied in new_with_config
            "auto",
            30.0,
            48_000,
            1,
            20,
            Freq { hz: 144_300_000 },
            RigMode::USB,
            1_920_000,
            1_500_000, // bandwidth_hz
            0,         // center_offset_hz
        )
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
        if let Some(sender) = self.pipeline.pcm_senders.get(self.primary_channel_idx) {
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
            self.freq = freq;
            let half_span_hz = i128::from(self.pipeline.sdr_sample_rate) / 2;
            let current_center_hz = i128::from(self.center_hz);
            let target_hz = i128::from(freq.hz);
            let within_current_span = target_hz >= current_center_hz - half_span_hz
                && target_hz <= current_center_hz + half_span_hz;

            if !within_current_span {
                // Only retune when the requested dial frequency leaves the
                // currently captured SDR bandwidth.
                let hardware_hz = freq.hz as i64 - self.center_offset_hz;
                self.center_hz = hardware_hz;
                if let Ok(mut cmd) = self.retune_cmd.lock() {
                    *cmd = Some(hardware_hz as f64);
                }
            }
            Ok(())
        })
    }

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
            // Update the primary channel's demodulator in the live pipeline.
            if let Some(dsp_arc) = self.pipeline.channel_dsps.get(self.primary_channel_idx) {
                dsp_arc.lock().unwrap().set_mode(&mode);
            }
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
            if let Some(dsp_arc) = self.pipeline.channel_dsps.get(self.primary_channel_idx) {
                dsp_arc.lock().unwrap().set_wfm_deemphasis(deemphasis_us);
            }
            Ok(())
        })
    }

    fn get_signal_strength<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        // RSSI from real device pending SDR hardware wiring; return 0 for now.
        Box::pin(async move { Ok(0u8) })
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

    fn set_bandwidth<'a>(
        &'a mut self,
        bandwidth_hz: u32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!("SoapySdrRig: set_bandwidth -> {} Hz", bandwidth_hz);
            self.bandwidth_hz = bandwidth_hz;
            if let Some(dsp_arc) = self.pipeline.channel_dsps.get(self.primary_channel_idx) {
                dsp_arc
                    .lock()
                    .unwrap()
                    .set_filter(bandwidth_hz, self.fir_taps as usize);
            }
            Ok(())
        })
    }

    fn set_fir_taps<'a>(
        &'a mut self,
        taps: u32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!("SoapySdrRig: set_fir_taps -> {}", taps);
            self.fir_taps = taps;
            if let Some(dsp_arc) = self.pipeline.channel_dsps.get(self.primary_channel_idx) {
                dsp_arc
                    .lock()
                    .unwrap()
                    .set_filter(self.bandwidth_hz, taps as usize);
            }
            Ok(())
        })
    }

    fn filter_state(&self) -> Option<RigFilterState> {
        Some(RigFilterState {
            bandwidth_hz: self.bandwidth_hz,
            fir_taps: self.fir_taps,
            cw_center_hz: 700,
            wfm_deemphasis_us: self.wfm_deemphasis_us,
        })
    }

    fn get_spectrum(&self) -> Option<SpectrumData> {
        let bins = self.spectrum_buf.lock().ok()?.clone()?;
        let rds = self
            .pipeline
            .channel_dsps
            .get(self.primary_channel_idx)
            .and_then(|dsp| dsp.lock().ok().and_then(|d| d.rds_data()));
        Some(SpectrumData {
            bins,
            center_hz: self.center_hz.max(0) as u64,
            sample_rate: self.pipeline.sdr_sample_rate,
            rds,
        })
    }

    /// Override: this backend provides demodulated PCM audio.
    fn as_audio_source(&self) -> Option<&dyn AudioSource> {
        Some(self)
    }
}
