// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;
use tokio::sync::broadcast;
use trx_core::rig::state::{RdsData, RigMode, WfmDenoiseLevel};

use crate::demod::{DcBlocker, Demodulator, SamDemod, SoftAgc, WfmStereoDecoder};

use super::{BlockFirFilterPair, IQ_BLOCK_SIZE};

// ---------------------------------------------------------------------------
// Noise blanker
// ---------------------------------------------------------------------------

/// IQ-domain impulse noise blanker.
///
/// Maintains a running RMS estimate of the IQ magnitude.  When a sample's
/// magnitude exceeds `threshold × rms`, it is replaced by linear interpolation
/// between the last clean sample and the next clean sample (lookahead of 1).
///
/// The RMS tracker uses exponential smoothing with a time constant of ~128
/// samples at the IQ sample rate, fast enough to track band-noise changes
/// but slow enough not to follow individual impulses.
#[derive(Debug, Clone)]
pub struct NoiseBlanker {
    enabled: bool,
    threshold: f32,
    /// Exponentially-smoothed mean-square estimate.
    mean_sq: f32,
    /// Last clean sample (used for interpolation fill).
    last_clean: Complex<f32>,
}

const NB_ALPHA: f32 = 1.0 / 128.0;

impl NoiseBlanker {
    pub fn new(enabled: bool, threshold: f32) -> Self {
        Self {
            enabled,
            threshold: threshold.max(1.0),
            mean_sq: 1e-10,
            last_clean: Complex::new(0.0, 0.0),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold.max(1.0);
    }

    /// Process a block of IQ samples in-place, blanking impulse spikes.
    pub fn process(&mut self, block: &mut [Complex<f32>]) {
        if !self.enabled || block.is_empty() {
            return;
        }

        let thresh_sq = self.threshold * self.threshold;

        for sample in block.iter_mut() {
            let s = *sample;
            let mag_sq = s.re * s.re + s.im * s.im;

            if mag_sq > thresh_sq * self.mean_sq {
                // Impulse detected — replace with last clean sample.
                *sample = self.last_clean;
            } else {
                // Clean sample — update RMS tracker.
                self.mean_sq += NB_ALPHA * (mag_sq - self.mean_sq);
                self.last_clean = s;
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NoiseBlankerConfig {
    pub enabled: bool,
    pub threshold: f32,
}

impl Default for NoiseBlankerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 10.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VirtualSquelchConfig {
    pub enabled: bool,
    pub threshold_db: f32,
    pub hysteresis_db: f32,
    pub tail_blocks: u32,
}

impl Default for VirtualSquelchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -65.0,
            hysteresis_db: 3.0,
            tail_blocks: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct VirtualSquelch {
    cfg: VirtualSquelchConfig,
    open: bool,
    tail_countdown: u32,
}

impl VirtualSquelch {
    fn new(cfg: VirtualSquelchConfig) -> Self {
        Self {
            cfg,
            open: !cfg.enabled,
            tail_countdown: 0,
        }
    }

    fn reset(&mut self) {
        self.open = !self.cfg.enabled;
        self.tail_countdown = 0;
    }

    fn set_enabled(&mut self, enabled: bool) {
        if self.cfg.enabled == enabled {
            return;
        }
        self.cfg.enabled = enabled;
        self.reset();
    }

    fn set_threshold_db(&mut self, threshold_db: f32) {
        self.cfg.threshold_db = threshold_db;
        self.reset();
    }

    fn supports_mode(mode: &RigMode) -> bool {
        !matches!(mode, RigMode::WFM)
    }

    fn update(&mut self, mode: &RigMode, level_db: f32) -> bool {
        if !self.cfg.enabled || !Self::supports_mode(mode) {
            self.open = true;
            self.tail_countdown = 0;
            return true;
        }

        let close_threshold_db = self.cfg.threshold_db - self.cfg.hysteresis_db.max(0.0);
        if self.open {
            if level_db >= close_threshold_db {
                self.tail_countdown = self.cfg.tail_blocks;
            } else if self.tail_countdown > 0 {
                self.tail_countdown -= 1;
            } else {
                self.open = false;
            }
        } else if level_db >= self.cfg.threshold_db {
            self.open = true;
            self.tail_countdown = self.cfg.tail_blocks;
        }

        self.open
    }
}

/// Frequency shift for the IQ bandpass filter, expressed as a fraction of Fs.
///
/// For SSB modes the symmetric LPF (cutoff ±BW/2) is modulated by ±cutoff_norm
/// to produce a one-sided passband:
///   USB / CW   → [0,  BW] Hz  (shift up   by +cutoff_norm)
///   LSB / CWR  → [-BW, 0] Hz  (shift down by -cutoff_norm)
///   Everything else → symmetric LPF (shift_norm = 0)
///
/// After filtering, `demod_usb` / `demod_lsb` take `.re`, which correctly
/// reconstructs the audio from the one-sided complex signal.
fn ssb_shift_norm(mode: &RigMode, cutoff_norm: f32) -> f32 {
    match mode {
        RigMode::USB | RigMode::DIG | RigMode::CW | RigMode::Other(_) => cutoff_norm,
        RigMode::LSB | RigMode::CWR => -cutoff_norm,
        _ => 0.0,
    }
}

fn agc_for_mode(mode: &RigMode, audio_sample_rate: u32) -> SoftAgc {
    let sr = audio_sample_rate.max(1) as f32;
    match mode {
        RigMode::CW | RigMode::CWR => SoftAgc::new(sr, 1.0, 50.0, 0.5, 30.0),
        RigMode::AM | RigMode::SAM => SoftAgc::new(sr, 5.0, 200.0, 0.5, 36.0),
        _ => SoftAgc::new(sr, 5.0, 500.0, 0.5, 30.0),
    }
}

fn iq_agc_for_mode(mode: &RigMode, sample_rate: u32) -> Option<SoftAgc> {
    let sr = sample_rate.max(1) as f32;
    match mode {
        RigMode::FM | RigMode::PKT => Some(SoftAgc::new(sr, 0.5, 150.0, 0.8, 12.0)),
        RigMode::AIS => Some(SoftAgc::new(sr, 0.5, 150.0, 0.8, 12.0)),
        // AM: normalize carrier amplitude before envelope detection so the
        // DC blocker always sees the same steady-state bias (~0.7) regardless
        // of RF signal strength.  Fast attack (0.5 ms) catches sudden carrier
        // appearance; 50 ms release tracks slow fading without distorting audio.
        RigMode::AM | RigMode::SAM => Some(SoftAgc::new(sr, 0.5, 50.0, 0.7, 30.0)),
        RigMode::WFM => None,
        _ => None,
    }
}

fn dc_for_mode(mode: &RigMode) -> Option<DcBlocker> {
    match mode {
        RigMode::WFM => None,
        // SAM: DC is handled inside SamDemod per channel (L and R separately).
        RigMode::SAM => None,
        // AM: the envelope detector output has a large carrier-amplitude DC
        // bias (A_c).  r=0.999 gives τ≈125 ms at 8 kHz, tracking carrier
        // level ~10× faster than r=0.9999 while still passing all audio
        // (highpass cutoff <2 Hz, well below 100 Hz speech floor).
        RigMode::AM => Some(DcBlocker::new(0.999)),
        _ => Some(DcBlocker::new(0.9999)),
    }
}

fn default_bandwidth_for_mode(mode: &RigMode) -> u32 {
    match mode {
        RigMode::LSB | RigMode::USB | RigMode::DIG => 3_000,
        RigMode::PKT => 25_000,
        RigMode::CW | RigMode::CWR => 500,
        RigMode::AM | RigMode::SAM => 9_000,
        RigMode::FM => 12_500,
        RigMode::WFM => 180_000,
        RigMode::AIS => 25_000,
        RigMode::VDES => 100_000,
        RigMode::Other(_) => 3_000,
    }
}

/// Calculate the FIR tap count automatically from the normalised cutoff frequency.
///
/// Uses the Hann-windowed sinc rule-of-thumb: taps = ceil(3.32 / cutoff_norm),
/// clamped to [63, 16383].  This gives enough taps so the filter transition band
/// equals one passband width (image rejection starts at audio_bandwidth_hz).
fn auto_taps(cutoff_norm: f32) -> usize {
    if cutoff_norm <= 0.0 {
        return 63;
    }
    ((3.32 / cutoff_norm).ceil() as usize).clamp(63, 16383)
}

/// Per-channel DSP state: mixer, FFT-FIR, decimator, demodulator, frame accumulator.
pub struct ChannelDsp {
    pub channel_if_hz: f64,
    pub demodulator: Demodulator,
    mode: RigMode,
    lpf_iq: BlockFirFilterPair,
    sdr_sample_rate: u32,
    audio_sample_rate: u32,
    audio_bandwidth_hz: u32,
    wfm_deemphasis_us: u32,
    wfm_stereo: bool,
    wfm_denoise: WfmDenoiseLevel,
    pub decim_factor: usize,
    output_channels: usize,
    pub frame_buf: Vec<f32>,
    frame_buf_offset: usize,
    pub frame_size: usize,
    pub pcm_tx: broadcast::Sender<Vec<f32>>,
    pub iq_tx: broadcast::Sender<Vec<Complex<f32>>>,
    scratch_mixed_i: Vec<f32>,
    scratch_mixed_q: Vec<f32>,
    scratch_filtered_i: Vec<f32>,
    scratch_filtered_q: Vec<f32>,
    scratch_decimated: Vec<Complex<f32>>,
    scratch_iq_tap: Vec<Complex<f32>>,
    pub mixer_phase: f64,
    pub mixer_phase_inc: f64,
    decim_counter: usize,
    iq_tap_counter: usize,
    resample_phase: f64,
    resample_phase_inc: f64,
    wfm_decoder: Option<WfmStereoDecoder>,
    sam_decoder: Option<SamDemod>,
    iq_agc: Option<SoftAgc>,
    audio_agc: SoftAgc,
    audio_dc: Option<DcBlocker>,
    processing_enabled: bool,
    force_mono_pcm: bool,
    squelch: VirtualSquelch,
    noise_blanker: NoiseBlanker,
}

impl ChannelDsp {
    fn clamp_bandwidth_for_mode(mode: &RigMode, bandwidth_hz: u32) -> u32 {
        match mode {
            // SAM stereo requires ≥ 9 kHz to capture both sum (L+R) and difference
            // (L−R) sidebands; narrower bandwidths would discard stereo content.
            RigMode::SAM => bandwidth_hz.max(9_000),
            _ => bandwidth_hz,
        }
    }

    pub fn set_channel_if_hz(&mut self, channel_if_hz: f64) {
        self.channel_if_hz = channel_if_hz;
        self.mixer_phase_inc = if self.sdr_sample_rate == 0 {
            0.0
        } else {
            2.0 * std::f64::consts::PI * channel_if_hz / self.sdr_sample_rate as f64
        };
    }

    fn pipeline_rates(
        mode: &RigMode,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        audio_bandwidth_hz: u32,
    ) -> (usize, u32) {
        if sdr_sample_rate == 0 {
            return (1, audio_sample_rate.max(1));
        }

        let target_rate = match mode {
            // Ensure composite rate is at least 120 kHz so the IQ filter can
            // pass the 57 kHz RDS subcarrier regardless of the user's audio BW.
            RigMode::WFM => audio_bandwidth_hz
                .max(audio_sample_rate.saturating_mul(4))
                .max(120_000),
            RigMode::VDES => audio_sample_rate.max(96_000),
            _ => audio_sample_rate.max(1),
        };
        let decim_factor = (sdr_sample_rate / target_rate.max(1)).max(1) as usize;
        let channel_sample_rate = (sdr_sample_rate / decim_factor as u32).max(1);
        (decim_factor, channel_sample_rate)
    }

    fn rebuild_filters(&mut self, reset_wfm_decoder: bool) {
        self.audio_bandwidth_hz =
            Self::clamp_bandwidth_for_mode(&self.mode, self.audio_bandwidth_hz);
        let (next_decim_factor, channel_sample_rate) = Self::pipeline_rates(
            &self.mode,
            self.sdr_sample_rate,
            self.audio_sample_rate,
            self.audio_bandwidth_hz,
        );
        let cutoff_hz = {
            let raw = self
                .audio_bandwidth_hz
                .min(channel_sample_rate.saturating_sub(1)) as f32
                / 2.0;
            // For WFM, always pass at least the 57 kHz RDS subcarrier.
            // Audio bandwidth is handled inside WfmStereoDecoder, so widening
            // the IQ prefilter here does not affect output audio quality.
            if self.mode == RigMode::WFM {
                raw.max(60_000.0)
            } else {
                raw
            }
        };
        let cutoff_norm = if self.sdr_sample_rate == 0 {
            0.1
        } else {
            (cutoff_hz / self.sdr_sample_rate as f32).min(0.499)
        };
        self.lpf_iq = BlockFirFilterPair::new(
            cutoff_norm,
            ssb_shift_norm(&self.mode, cutoff_norm),
            auto_taps(cutoff_norm),
            IQ_BLOCK_SIZE,
        );
        let rate_changed = self.decim_factor != next_decim_factor;
        self.decim_factor = next_decim_factor;
        self.decim_counter = 0;
        self.iq_tap_counter = 0;
        self.resample_phase = 0.0;
        self.resample_phase_inc = if self.sdr_sample_rate == 0 {
            1.0
        } else {
            self.audio_sample_rate as f64 / self.sdr_sample_rate as f64
        };
        if self.mode == RigMode::WFM {
            if reset_wfm_decoder || rate_changed || self.wfm_decoder.is_none() {
                self.wfm_decoder = Some(WfmStereoDecoder::new(
                    channel_sample_rate,
                    self.audio_sample_rate,
                    self.output_channels,
                    self.wfm_stereo,
                    self.wfm_deemphasis_us,
                    self.wfm_denoise,
                ));
            }
        } else {
            self.wfm_decoder = None;
        }
        if self.mode == RigMode::SAM {
            self.sam_decoder = Some(SamDemod::new(self.audio_sample_rate));
        } else {
            self.sam_decoder = None;
        }
        self.iq_agc = iq_agc_for_mode(&self.mode, channel_sample_rate);
        self.audio_agc = agc_for_mode(&self.mode, self.audio_sample_rate);
        self.audio_dc = dc_for_mode(&self.mode);
        self.frame_buf.clear();
        self.frame_buf_offset = 0;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel_if_hz: f64,
        mode: &RigMode,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        output_channels: usize,
        frame_duration_ms: u16,
        audio_bandwidth_hz: u32,
        wfm_deemphasis_us: u32,
        wfm_stereo: bool,
        force_mono_pcm: bool,
        squelch_cfg: VirtualSquelchConfig,
        nb_cfg: NoiseBlankerConfig,
        pcm_tx: broadcast::Sender<Vec<f32>>,
        iq_tx: broadcast::Sender<Vec<Complex<f32>>>,
    ) -> Self {
        let output_channels = output_channels.max(1);
        let audio_bandwidth_hz = Self::clamp_bandwidth_for_mode(mode, audio_bandwidth_hz);
        let frame_size = if audio_sample_rate == 0 || frame_duration_ms == 0 {
            960 * output_channels
        } else {
            (audio_sample_rate as usize * frame_duration_ms as usize * output_channels) / 1000
        };

        let (decim_factor, channel_sample_rate) =
            Self::pipeline_rates(mode, sdr_sample_rate, audio_sample_rate, audio_bandwidth_hz);
        let cutoff_hz = {
            let raw = audio_bandwidth_hz.min(channel_sample_rate.saturating_sub(1)) as f32 / 2.0;
            if *mode == RigMode::WFM {
                raw.max(60_000.0)
            } else {
                raw
            }
        };
        let cutoff_norm = if sdr_sample_rate == 0 {
            0.1
        } else {
            (cutoff_hz / sdr_sample_rate as f32).min(0.499)
        };

        let mixer_phase_inc = if sdr_sample_rate == 0 {
            0.0
        } else {
            2.0 * std::f64::consts::PI * channel_if_hz / sdr_sample_rate as f64
        };

        Self {
            channel_if_hz,
            demodulator: Demodulator::for_mode(mode),
            mode: mode.clone(),
            lpf_iq: BlockFirFilterPair::new(
                cutoff_norm,
                ssb_shift_norm(mode, cutoff_norm),
                auto_taps(cutoff_norm),
                IQ_BLOCK_SIZE,
            ),
            sdr_sample_rate,
            audio_sample_rate,
            audio_bandwidth_hz,
            wfm_deemphasis_us,
            wfm_stereo,
            wfm_denoise: WfmDenoiseLevel::Auto,
            decim_factor,
            output_channels,
            frame_buf: Vec::with_capacity(frame_size + output_channels),
            frame_buf_offset: 0,
            frame_size,
            pcm_tx,
            iq_tx,
            scratch_mixed_i: Vec::with_capacity(IQ_BLOCK_SIZE),
            scratch_mixed_q: Vec::with_capacity(IQ_BLOCK_SIZE),
            scratch_filtered_i: Vec::with_capacity(IQ_BLOCK_SIZE),
            scratch_filtered_q: Vec::with_capacity(IQ_BLOCK_SIZE),
            scratch_decimated: Vec::with_capacity(IQ_BLOCK_SIZE / decim_factor.max(1) + 1),
            scratch_iq_tap: Vec::with_capacity(IQ_BLOCK_SIZE / decim_factor.max(1) + 1),
            mixer_phase: 0.0,
            mixer_phase_inc,
            decim_counter: 0,
            iq_tap_counter: 0,
            resample_phase: 0.0,
            resample_phase_inc: if sdr_sample_rate == 0 {
                1.0
            } else {
                audio_sample_rate as f64 / sdr_sample_rate as f64
            },
            wfm_decoder: if *mode == RigMode::WFM {
                Some(WfmStereoDecoder::new(
                    channel_sample_rate,
                    audio_sample_rate,
                    output_channels,
                    wfm_stereo,
                    wfm_deemphasis_us,
                    WfmDenoiseLevel::Auto,
                ))
            } else {
                None
            },
            sam_decoder: if *mode == RigMode::SAM {
                Some(SamDemod::new(audio_sample_rate))
            } else {
                None
            },
            iq_agc: iq_agc_for_mode(mode, channel_sample_rate),
            audio_agc: agc_for_mode(mode, audio_sample_rate),
            audio_dc: dc_for_mode(mode),
            processing_enabled: true,
            force_mono_pcm,
            squelch: VirtualSquelch::new(squelch_cfg),
            noise_blanker: NoiseBlanker::new(nb_cfg.enabled, nb_cfg.threshold),
        }
    }

    pub fn set_processing_enabled(&mut self, enabled: bool) {
        self.processing_enabled = enabled;
    }

    pub fn set_force_mono_pcm(&mut self, enabled: bool) {
        self.force_mono_pcm = enabled;
    }

    pub fn set_squelch(&mut self, enabled: bool, threshold_db: f32) {
        self.squelch.set_enabled(enabled);
        self.squelch.set_threshold_db(threshold_db);
    }

    pub fn set_noise_blanker(&mut self, enabled: bool, threshold: f32) {
        self.noise_blanker.set_enabled(enabled);
        self.noise_blanker.set_threshold(threshold);
    }

    pub fn set_mode(&mut self, mode: &RigMode) {
        self.mode = mode.clone();
        if *mode != RigMode::WFM {
            self.audio_bandwidth_hz = default_bandwidth_for_mode(mode);
        }
        self.demodulator = Demodulator::for_mode(mode);
        self.squelch.reset();
        self.rebuild_filters(true);
    }

    pub fn set_filter(&mut self, bandwidth_hz: u32) {
        self.audio_bandwidth_hz = Self::clamp_bandwidth_for_mode(&self.mode, bandwidth_hz);
        self.rebuild_filters(false);
    }

    pub fn set_wfm_deemphasis(&mut self, deemphasis_us: u32) {
        self.wfm_deemphasis_us = deemphasis_us;
        self.rebuild_filters(true);
    }

    pub fn set_wfm_stereo(&mut self, enabled: bool) {
        self.wfm_stereo = enabled;
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.set_stereo_enabled(enabled);
        }
    }

    pub fn set_sam_stereo_width(&mut self, width: f32) {
        if let Some(decoder) = &mut self.sam_decoder {
            decoder.set_stereo_width(width);
        }
    }

    pub fn set_sam_carrier_sync(&mut self, enabled: bool) {
        if let Some(decoder) = &mut self.sam_decoder {
            decoder.set_carrier_sync(enabled);
        }
    }

    pub fn set_wfm_denoise(&mut self, level: WfmDenoiseLevel) {
        self.wfm_denoise = level;
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.set_denoise_level(level);
        }
    }

    pub fn rds_data(&self) -> Option<RdsData> {
        self.wfm_decoder
            .as_ref()
            .and_then(WfmStereoDecoder::rds_data)
    }

    pub fn wfm_stereo_detected(&self) -> bool {
        self.wfm_decoder
            .as_ref()
            .map(WfmStereoDecoder::stereo_detected)
            .unwrap_or(false)
    }

    pub fn wfm_cci(&self) -> u8 {
        self.wfm_decoder
            .as_ref()
            .map(WfmStereoDecoder::cci_level)
            .unwrap_or(0)
    }

    pub fn wfm_aci(&self) -> u8 {
        self.wfm_decoder
            .as_ref()
            .map(WfmStereoDecoder::aci_level)
            .unwrap_or(0)
    }

    pub fn reset_rds(&mut self) {
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.reset_rds();
        }
    }

    pub fn reset_wfm_state(&mut self) {
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.reset_state();
        }
    }

    pub fn process_block(&mut self, block: &[Complex<f32>]) {
        if !self.processing_enabled {
            return;
        }
        let n = block.len();
        if n == 0 {
            return;
        }

        // Apply noise blanker on a mutable copy when enabled.
        let block = if self.noise_blanker.enabled {
            let mut nb_buf = block.to_vec();
            self.noise_blanker.process(&mut nb_buf);
            nb_buf
        } else {
            block.to_vec()
        };
        let block = &block[..];

        self.scratch_mixed_i.resize(n, 0.0);
        self.scratch_mixed_q.resize(n, 0.0);
        let mixed_i = &mut self.scratch_mixed_i;
        let mixed_q = &mut self.scratch_mixed_q;

        let phase_start = self.mixer_phase;
        let phase_inc = self.mixer_phase_inc;
        let (mut sin_phase, mut cos_phase) = phase_start.sin_cos();
        let (sin_inc, cos_inc) = phase_inc.sin_cos();
        for (idx, &sample) in block.iter().enumerate() {
            let lo_re = cos_phase as f32;
            let lo_im = -(sin_phase as f32);
            mixed_i[idx] = sample.re * lo_re - sample.im * lo_im;
            mixed_q[idx] = sample.re * lo_im + sample.im * lo_re;
            let next_sin = sin_phase * cos_inc + cos_phase * sin_inc;
            let next_cos = cos_phase * cos_inc - sin_phase * sin_inc;
            sin_phase = next_sin;
            cos_phase = next_cos;
        }
        self.mixer_phase = (phase_start + n as f64 * phase_inc).rem_euclid(std::f64::consts::TAU);

        self.lpf_iq.filter_block_into(
            mixed_i,
            mixed_q,
            &mut self.scratch_filtered_i,
            &mut self.scratch_filtered_q,
        );
        let filtered_i = &self.scratch_filtered_i;
        let filtered_q = &self.scratch_filtered_q;

        let capacity = n / self.decim_factor + 1;
        self.scratch_decimated.clear();
        if self.scratch_decimated.capacity() < capacity {
            self.scratch_decimated
                .reserve(capacity - self.scratch_decimated.capacity());
        }
        if matches!(self.mode, RigMode::VDES) && self.iq_tx.receiver_count() > 0 {
            self.scratch_iq_tap.clear();
            if self.scratch_iq_tap.capacity() < capacity {
                self.scratch_iq_tap
                    .reserve(capacity - self.scratch_iq_tap.capacity());
            }
            for idx in 0..n {
                self.iq_tap_counter += 1;
                if self.iq_tap_counter >= self.decim_factor {
                    self.iq_tap_counter = 0;
                    let fi = filtered_i.get(idx).copied().unwrap_or(0.0);
                    let fq = filtered_q.get(idx).copied().unwrap_or(0.0);
                    self.scratch_iq_tap.push(Complex::new(fi, fq));
                }
            }
            if !self.scratch_iq_tap.is_empty() {
                let _ = self.iq_tx.send(self.scratch_iq_tap.clone());
            }
        }
        let decimated = &mut self.scratch_decimated;
        if self.wfm_decoder.is_some() {
            for idx in 0..n {
                self.decim_counter += 1;
                if self.decim_counter >= self.decim_factor {
                    self.decim_counter = 0;
                    let fi = filtered_i.get(idx).copied().unwrap_or(0.0);
                    let fq = filtered_q.get(idx).copied().unwrap_or(0.0);
                    decimated.push(Complex::new(fi, fq));
                }
            }
        } else {
            for idx in 0..n {
                self.resample_phase += self.resample_phase_inc;
                if self.resample_phase >= 1.0 {
                    self.resample_phase -= 1.0;
                    let fi = filtered_i.get(idx).copied().unwrap_or(0.0);
                    let fq = filtered_q.get(idx).copied().unwrap_or(0.0);
                    decimated.push(Complex::new(fi, fq));
                }
            }
        }

        if decimated.is_empty() {
            return;
        }

        if let Some(iq_agc) = &mut self.iq_agc {
            for sample in decimated.iter_mut() {
                *sample = iq_agc.process_complex(*sample);
            }
        }

        let signal_power = decimated
            .iter()
            .map(|s| s.re * s.re + s.im * s.im)
            .sum::<f32>()
            / decimated.len() as f32;
        let signal_db = 10.0 * signal_power.max(1e-12).log10();
        const WFM_OUTPUT_GAIN: f32 = 0.50;
        let mut audio = if let Some(decoder) = self.wfm_decoder.as_mut() {
            let mut out = decoder.process_iq(decimated);
            for sample in &mut out {
                *sample = (*sample * WFM_OUTPUT_GAIN).clamp(-1.0, 1.0);
            }
            out
        } else if let Some(decoder) = self.sam_decoder.as_mut() {
            let stereo = decoder.demodulate_stereo(decimated);
            // Apply stereo-aware AGC (shared gain preserves L/R balance).
            let mut out = Vec::with_capacity(stereo.len());
            let mut it = stereo.chunks_exact(2);
            for chunk in it.by_ref() {
                let (l, r) = self.audio_agc.process_pair(chunk[0], chunk[1]);
                if self.output_channels >= 2 && !self.force_mono_pcm {
                    out.push(l);
                    out.push(r);
                } else {
                    out.push((l + r) * 0.5);
                }
            }
            out
        } else {
            let mut raw = self.demodulator.demodulate(decimated);
            for sample in &mut raw {
                if let Some(dc) = &mut self.audio_dc {
                    *sample = dc.process(*sample);
                }
                *sample = self.audio_agc.process(*sample);
            }
            if self.output_channels >= 2 && !self.force_mono_pcm {
                let mut stereo = Vec::with_capacity(raw.len() * self.output_channels);
                for sample in raw {
                    stereo.push(sample);
                    stereo.push(sample);
                }
                stereo
            } else {
                raw
            }
        };
        if !self.squelch.update(&self.mode, signal_db) {
            audio.fill(0.0);
        }

        self.frame_buf.extend_from_slice(&audio);
        while self.frame_buf.len().saturating_sub(self.frame_buf_offset) >= self.frame_size {
            let start = self.frame_buf_offset;
            let end = start + self.frame_size;
            let frame = self.frame_buf[start..end].to_vec();
            self.frame_buf_offset = end;
            let _ = self.pcm_tx.send(frame);
        }
        if self.frame_buf_offset > 0 && self.frame_buf_offset * 2 >= self.frame_buf.len() {
            self.frame_buf.copy_within(self.frame_buf_offset.., 0);
            self.frame_buf
                .truncate(self.frame_buf.len() - self.frame_buf_offset);
            self.frame_buf_offset = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_dsp_processes_silence() {
        let (pcm_tx, _pcm_rx) = broadcast::channel::<Vec<f32>>(8);
        let (iq_tx, _iq_rx) = broadcast::channel::<Vec<Complex<f32>>>(8);
        let mut dsp = ChannelDsp::new(
            0.0,
            &RigMode::USB,
            48_000,
            8_000,
            1,
            20,
            3000,
            75,
            true,
            false,
            VirtualSquelchConfig::default(),
            NoiseBlankerConfig::default(),
            pcm_tx,
            iq_tx,
        );
        let block = vec![Complex::new(0.0_f32, 0.0_f32); 4096];
        dsp.process_block(&block);
    }

    #[test]
    fn channel_dsp_set_mode() {
        let (pcm_tx, _) = broadcast::channel::<Vec<f32>>(8);
        let (iq_tx, _) = broadcast::channel::<Vec<Complex<f32>>>(8);
        let mut dsp = ChannelDsp::new(
            0.0,
            &RigMode::USB,
            48_000,
            8_000,
            1,
            20,
            3000,
            75,
            true,
            false,
            VirtualSquelchConfig::default(),
            NoiseBlankerConfig::default(),
            pcm_tx,
            iq_tx,
        );
        assert_eq!(dsp.demodulator, Demodulator::Usb);
        dsp.set_mode(&RigMode::FM);
        assert_eq!(dsp.demodulator, Demodulator::Fm);
    }

    #[test]
    fn noise_blanker_suppresses_impulse() {
        let mut nb = NoiseBlanker::new(true, 5.0);
        // Feed a steady signal to establish the RMS baseline.
        let mut block: Vec<Complex<f32>> = (0..256).map(|_| Complex::new(0.01, 0.01)).collect();
        nb.process(&mut block);
        // Now inject a single massive spike at index 0.
        let mut block2: Vec<Complex<f32>> = (0..256).map(|_| Complex::new(0.01, 0.01)).collect();
        block2[0] = Complex::new(10.0, 10.0);
        nb.process(&mut block2);
        // The spike should have been blanked (replaced by last clean sample).
        let mag = (block2[0].re * block2[0].re + block2[0].im * block2[0].im).sqrt();
        assert!(
            mag < 1.0,
            "expected impulse to be blanked, got magnitude {}",
            mag
        );
    }

    #[test]
    fn noise_blanker_disabled_passes_through() {
        let mut nb = NoiseBlanker::new(false, 5.0);
        let mut block = vec![Complex::new(10.0, 10.0); 4];
        nb.process(&mut block);
        assert_eq!(block[0], Complex::new(10.0, 10.0));
    }
}
