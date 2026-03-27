// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;
use trx_core::rig::state::{RdsData, WfmDenoiseLevel};
use trx_rds::RdsDecoder;

use super::{math::demod_fm_with_prev, DcBlocker};

const RDS_SUBCARRIER_HZ: f32 = 57_000.0;
/// Tech 2: pilot lock level above which the ×3 pilot reference is used.
/// Effective pilot coherence threshold ≈ ONSET + THRESHOLD × 0.2 = 0.36.
const PILOT_LOCK_THRESHOLD: f32 = 0.20;
/// Coherence below which pilot_lock contribution is zero (linear ramp 0→1
/// over the range [ONSET, ONSET+0.2]).  Lower value → pilot ref used on
/// weaker stations; risk: noisier reference.  0.30 vs original 0.40 means
/// we engage at coherence ≥ 0.36 instead of ≥ 0.45.
const PILOT_LOCK_ONSET: f32 = 0.30;
/// Tech 9: number of complex CMA equalizer taps.
const CMA_N_TAPS: usize = 8;
/// Tech 9: CMA LMS step size.
const CMA_STEP_SIZE: f32 = 1e-5;
/// Tech 9: slow adaptation rate for the CMA radius estimate.
const CMA_RADIUS_ALPHA: f32 = 1e-3;
/// Pilot tone frequency (Hz).
const PILOT_HZ: f32 = 19_000.0;
/// Audio bandwidth for WFM (Hz).
const AUDIO_BW_HZ: f32 = 18_000.0;
/// Stereo L-R subchannel bandwidth for WFM (Hz).
const STEREO_DIFF_BW_HZ: f32 = AUDIO_BW_HZ;
/// Stage 1 Butterworth Q.
const BW4_Q1: f32 = 0.5412;
/// Stage 2 Butterworth Q.
const BW4_Q2: f32 = 1.3066;
/// Q for the 19 kHz pilot notch.
const PILOT_NOTCH_Q: f32 = 5.0;
/// Narrow 19 kHz band-pass used to derive zero-crossings for switching stereo demod.
const PILOT_BPF_Q: f32 = 20.0;
/// Fixed phase trim on the recovered L-R channel.
const STEREO_SEPARATION_PHASE_TRIM: f32 = 0.434;
/// Lower bound for dynamic gain trim on the recovered L-R channel.
const STEREO_SEPARATION_GAIN_MIN: f32 = 0.92;
/// Upper bound for dynamic gain trim on the recovered L-R channel.
const STEREO_SEPARATION_GAIN_MAX: f32 = 1.08;
/// Extra headroom in the stereo matrix.
const STEREO_MATRIX_GAIN: f32 = 1.20;
/// Stereo detection runs every N composite samples.
const STEREO_DETECT_DECIMATION: u32 = 16;

const DENOISE_BANDS: usize = 6;
const DENOISE_CENTERS: [f32; DENOISE_BANDS] = [250.0, 800.0, 2500.0, 5500.0, 10000.0, 16000.0];
const DENOISE_Q: [f32; DENOISE_BANDS] = [0.3, 0.35, 0.4, 0.5, 0.6, 0.7];
const DENOISE_NOISE_SMOOTH_HZ: f32 = 10.0;
const DENOISE_SIGNAL_SMOOTH_HZ: f32 = 30.0;
const DENOISE_BETA: f32 = 1.0;
const DENOISE_ALPHA: f32 = 0.5;
const DENOISE_FLOOR: f32 = 1e-10;
const DENOISE_KNEE: f32 = 4.0;
const DENOISE_STEREO_PRESERVE_MIN: f32 = 0.18;
const DENOISE_STEREO_PRESERVE_MAX: f32 = 0.42;
const STEREO_DIFF_DC_R: f32 = 0.9995;
const WFM_RESAMP_TAPS: usize = 32;
const WFM_RESAMP_PHASES: usize = 64;

fn build_wfm_resample_bank(cutoff: f32) -> [[f32; WFM_RESAMP_TAPS]; WFM_RESAMP_PHASES] {
    let mut bank = [[0.0; WFM_RESAMP_TAPS]; WFM_RESAMP_PHASES];
    let anchor = (WFM_RESAMP_TAPS / 2 - 1) as f32;
    for (phase_idx, phase) in bank.iter_mut().enumerate() {
        let frac = phase_idx as f32 / WFM_RESAMP_PHASES as f32;
        let center = anchor + frac;
        let mut sum = 0.0_f32;
        for (tap_idx, coeff) in phase.iter_mut().enumerate() {
            let x = tap_idx as f32 - center;
            let sinc = if x.abs() < 1e-6 {
                cutoff
            } else {
                let arg = std::f32::consts::PI * x * cutoff;
                arg.sin() / (std::f32::consts::PI * x)
            };
            let window = if WFM_RESAMP_TAPS == 1 {
                1.0
            } else {
                let pos = tap_idx as f32 / (WFM_RESAMP_TAPS - 1) as f32;
                let tw = 2.0 * std::f32::consts::PI * pos;
                0.35875 - 0.48829 * tw.cos() + 0.14128 * (2.0 * tw).cos()
                    - 0.01168 * (3.0 * tw).cos()
            };
            *coeff = sinc * window;
            sum += *coeff;
        }
        if sum.abs() > 1e-9 {
            let inv = 1.0 / sum;
            for coeff in phase.iter_mut() {
                *coeff *= inv;
            }
        }
    }
    bank
}

#[inline]
fn polyphase_resample_ring(
    hist: &[f32; WFM_RESAMP_TAPS],
    pos: usize,
    bank: &[[f32; WFM_RESAMP_TAPS]; WFM_RESAMP_PHASES],
    frac: f32,
) -> f32 {
    let phase = (frac.clamp(0.0, 0.999_999) * WFM_RESAMP_PHASES as f32).round() as usize;
    let phase = phase.min(WFM_RESAMP_PHASES - 1);
    let coeffs = &bank[phase];
    let mut acc = 0.0_f32;
    let mask = WFM_RESAMP_TAPS - 1;
    for tap in 0..WFM_RESAMP_TAPS {
        acc += hist[(pos + tap) & mask] * coeffs[tap];
    }
    acc
}

#[derive(Debug, Clone)]
struct OnePoleLowPass {
    alpha: f32,
    y: f32,
}

#[derive(Debug, Clone)]
struct BiquadBandPass {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BiquadBandPass {
    fn new(sample_rate: f32, center_hz: f32, q: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let center = center_hz.clamp(100.0, sr * 0.45);
        let q = q.max(0.2);
        let w0 = 2.0 * std::f32::consts::PI * center / sr;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();

        let a0 = 1.0 + alpha;
        let inv_a0 = 1.0 / a0;

        Self {
            b0: alpha * inv_a0,
            b1: 0.0,
            b2: -alpha * inv_a0,
            a1: (-2.0 * cos_w0) * inv_a0,
            a2: (1.0 - alpha) * inv_a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

#[derive(Debug, Clone)]
struct BiquadLowPass {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BiquadLowPass {
    fn new(sample_rate: f32, cutoff_hz: f32, q: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let fc = cutoff_hz.clamp(1.0, sr * 0.45);
        let q = q.max(0.1);
        let w0 = 2.0 * std::f32::consts::PI * fc / sr;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();
        let a0_inv = 1.0 / (1.0 + alpha);
        let b0 = (1.0 - cos_w0) * 0.5 * a0_inv;
        let b1 = (1.0 - cos_w0) * a0_inv;
        let b2 = b0;
        let a1 = -2.0 * cos_w0 * a0_inv;
        let a2 = (1.0 - alpha) * a0_inv;
        Self {
            b0,
            b1,
            b2,
            a1,
            a2,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

#[derive(Debug, Clone)]
struct BiquadNotch {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BiquadNotch {
    fn new(sample_rate: f32, center_hz: f32, q: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let fc = center_hz.clamp(1.0, sr * 0.45);
        let q = q.max(0.1);
        let w0 = 2.0 * std::f32::consts::PI * fc / sr;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();
        let a0_inv = 1.0 / (1.0 + alpha);
        Self {
            b0: a0_inv,
            b1: -2.0 * cos_w0 * a0_inv,
            b2: a0_inv,
            a1: -2.0 * cos_w0 * a0_inv,
            a2: (1.0 - alpha) * a0_inv,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Tech 4: 8th-order 57 kHz bandpass filter (4 cascaded biquads)
// ---------------------------------------------------------------------------

/// Four cascaded biquad bandpass sections forming an effective 8th-order BPF.
/// Q=3.5 per section gives ≈ ±3560 Hz composite passband at 57 kHz.
///
/// At α=0.30 the RDS signal extends ±1544 Hz from center.  The previous
/// Q=5 gave only ±2480 Hz composite bandwidth, which tapered the RDS band
/// edges by −1.2 dB — enough to distort the RRC matched filter's expected
/// pulse shape, cause ISI, and degrade soft-decision confidence values.
/// Q=3.5 reduces edge attenuation to −0.59 dB while still providing
/// ≈−4 dB rejection at the stereo difference signal edge (53 kHz) and
/// steep 8th-order roll-off beyond.
#[derive(Debug, Clone)]
struct Iir8BandPass {
    stages: [BiquadBandPass; 4],
}

impl Iir8BandPass {
    fn new(sample_rate: f32, center_hz: f32) -> Self {
        const Q: f32 = 3.5;
        Self {
            stages: std::array::from_fn(|_| BiquadBandPass::new(sample_rate, center_hz, Q)),
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let mut y = x;
        for stage in &mut self.stages {
            y = stage.process(y);
        }
        y
    }

    fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.reset();
        }
    }
}

// ---------------------------------------------------------------------------
// Tech 9: CMA blind equalizer (pre-FM-demodulation, constant-modulus)
// ---------------------------------------------------------------------------

/// Fractionally-spaced complex LMS equalizer driven by the constant-modulus
/// cost function.  FM is constant-envelope, so E[|y|²] = R² drives tap
/// adaptation without requiring a training sequence.  Applied to the IQ
/// stream before FM discrimination to suppress adjacent-channel interference.
#[derive(Debug, Clone)]
struct CmaEqualizer {
    taps: [Complex<f32>; CMA_N_TAPS],
    buf: [Complex<f32>; CMA_N_TAPS],
    pos: usize,
    /// Adaptive radius estimate (tracks long-term input power).
    radius_sq: f32,
}

impl CmaEqualizer {
    fn new() -> Self {
        let mut taps = [Complex::new(0.0_f32, 0.0_f32); CMA_N_TAPS];
        // Initialise as identity: tap at the centre = 1+0j.
        taps[CMA_N_TAPS / 2] = Complex::new(1.0, 0.0);
        Self {
            taps,
            buf: [Complex::new(0.0_f32, 0.0_f32); CMA_N_TAPS],
            pos: 0,
            radius_sq: 1.0,
        }
    }

    #[inline]
    fn process(&mut self, x: Complex<f32>) -> Complex<f32> {
        // Update power estimate (very slow, tracks long-term signal level).
        self.radius_sq =
            self.radius_sq * (1.0 - CMA_RADIUS_ALPHA) + x.norm_sqr() * CMA_RADIUS_ALPHA;

        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) % CMA_N_TAPS;

        // Compute filter output y = Σ w[k] * x[n-k].
        let mut y = Complex::new(0.0_f32, 0.0_f32);
        for k in 0..CMA_N_TAPS {
            y += self.taps[k] * self.buf[(self.pos + k) % CMA_N_TAPS];
        }

        // CMA gradient: e = |y|² − R²; update w[k] -= μ·e·y·conj(x[n-k]).
        let err = y.norm_sqr() - self.radius_sq;
        let scale = CMA_STEP_SIZE * err;
        for k in 0..CMA_N_TAPS {
            let x_k = self.buf[(self.pos + k) % CMA_N_TAPS];
            self.taps[k] -= Complex::new(scale, 0.0) * y * x_k.conj();
        }

        y
    }

    fn reset(&mut self) {
        let mut taps = [Complex::new(0.0_f32, 0.0_f32); CMA_N_TAPS];
        taps[CMA_N_TAPS / 2] = Complex::new(1.0, 0.0);
        self.taps = taps;
        self.buf = [Complex::new(0.0_f32, 0.0_f32); CMA_N_TAPS];
        self.pos = 0;
        self.radius_sq = 1.0;
    }
}

impl OnePoleLowPass {
    fn new(sample_rate: f32, cutoff_hz: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let cutoff = cutoff_hz.clamp(1.0, sr * 0.49);
        let dt = 1.0 / sr;
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
        let alpha = dt / (rc + dt);
        Self { alpha, y: 0.0 }
    }

    fn process(&mut self, x: f32) -> f32 {
        self.y += self.alpha * (x - self.y);
        self.y
    }

    fn reset(&mut self) {
        self.y = 0.0;
    }
}

#[derive(Debug, Clone)]
struct Deemphasis {
    alpha: f32,
    y: f32,
}

impl Deemphasis {
    fn new(sample_rate: f32, tau_us: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let tau = tau_us.max(1.0) * 1e-6;
        let alpha = 1.0 - (-1.0 / (sr * tau)).exp();
        Self { alpha, y: 0.0 }
    }

    fn process(&mut self, x: f32) -> f32 {
        self.y += self.alpha * (x - self.y);
        self.y
    }

    fn reset(&mut self) {
        self.y = 0.0;
    }
}

#[derive(Debug, Clone)]
struct DenoiseSubband {
    sum_bp: BiquadBandPass,
    diff_i_bp: BiquadBandPass,
    diff_q_bp: BiquadBandPass,
    noise_lp: OnePoleLowPass,
    diff_lp: OnePoleLowPass,
    sum_lp: OnePoleLowPass,
}

impl DenoiseSubband {
    fn new(audio_rate: f32, center_hz: f32, q: f32) -> Self {
        Self {
            sum_bp: BiquadBandPass::new(audio_rate, center_hz, q),
            diff_i_bp: BiquadBandPass::new(audio_rate, center_hz, q),
            diff_q_bp: BiquadBandPass::new(audio_rate, center_hz, q),
            noise_lp: OnePoleLowPass::new(audio_rate, DENOISE_NOISE_SMOOTH_HZ),
            diff_lp: OnePoleLowPass::new(audio_rate, DENOISE_SIGNAL_SMOOTH_HZ),
            sum_lp: OnePoleLowPass::new(audio_rate, DENOISE_SIGNAL_SMOOTH_HZ),
        }
    }

    #[inline]
    fn process(&mut self, sum: f32, diff_i: f32, diff_q: f32) -> (f32, f32) {
        let sum_band = self.sum_bp.process(sum);
        let diff_i_band = self.diff_i_bp.process(diff_i);
        let diff_q_band = self.diff_q_bp.process(diff_q);

        let noise_power = self
            .noise_lp
            .process(diff_q_band * diff_q_band)
            .max(DENOISE_FLOOR);
        let diff_power =
            (self.diff_lp.process(diff_i_band * diff_i_band) - DENOISE_BETA * noise_power).max(0.0);
        let sum_power =
            (self.sum_lp.process(sum_band * sum_band) - DENOISE_ALPHA * noise_power).max(0.0);

        let hden = (diff_power / noise_power).sqrt().min(1.0);
        let diff_snr = diff_power / noise_power;
        let weight_a = diff_snr / (diff_snr + DENOISE_KNEE);

        let noise_indicator = (noise_power / (diff_power + DENOISE_FLOOR)).min(1.0);
        let weight_b_raw = diff_power / (sum_power + diff_power + DENOISE_FLOOR);
        let weight_b = 1.0 - noise_indicator * (1.0 - weight_b_raw);

        let gain = hden * weight_a * weight_b;
        let band_energy = self.diff_lp.y.max(DENOISE_FLOOR);
        (gain, band_energy)
    }

    fn reset(&mut self) {
        self.sum_bp.reset();
        self.diff_i_bp.reset();
        self.diff_q_bp.reset();
        self.noise_lp.reset();
        self.diff_lp.reset();
        self.sum_lp.reset();
    }
}

#[derive(Debug, Clone)]
struct StereoDenoise {
    bands: [DenoiseSubband; DENOISE_BANDS],
    level: WfmDenoiseLevel,
}

impl StereoDenoise {
    fn new(audio_rate: f32) -> Self {
        let bands = std::array::from_fn(|idx| {
            DenoiseSubband::new(audio_rate, DENOISE_CENTERS[idx], DENOISE_Q[idx])
        });
        Self {
            bands,
            level: WfmDenoiseLevel::Auto,
        }
    }

    #[inline]
    fn process(&mut self, sum: f32, diff_i: f32, diff_q: f32) -> f32 {
        let mut gain_sum = 0.0_f32;
        let mut weight_sum = 0.0_f32;
        for band in &mut self.bands {
            let (gain, weight) = band.process(sum, diff_i, diff_q);
            gain_sum += gain * weight;
            weight_sum += weight;
        }

        let broadband_gain = if weight_sum > DENOISE_FLOOR {
            (gain_sum / weight_sum).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let effective_gain = match self.level {
            WfmDenoiseLevel::Off => 1.0,
            WfmDenoiseLevel::Auto => {
                let strength = (0.45 + (1.0 - broadband_gain) * 0.55).clamp(0.45, 1.0);
                1.0 - (1.0 - broadband_gain) * strength
            }
            WfmDenoiseLevel::Low => 1.0 - (1.0 - broadband_gain) * 0.35,
            WfmDenoiseLevel::Medium => 1.0 - (1.0 - broadband_gain) * 0.65,
            // Extra attenuation profile for noisy stereo difference channels.
            WfmDenoiseLevel::High => broadband_gain.powf(1.45),
        };
        diff_i * effective_gain.clamp(0.0, 1.0)
    }

    fn reset(&mut self) {
        for band in &mut self.bands {
            band.reset();
        }
    }
}

#[derive(Debug, Clone)]
pub struct WfmStereoDecoder {
    output_channels: usize,
    stereo_enabled: bool,
    rds_decoder: RdsDecoder,
    /// Tech 4: 8th-order 57 kHz bandpass filter (4 cascaded biquads).
    rds_bpf: Iir8BandPass,
    rds_dc: DcBlocker,
    /// Tech 9: CMA blind equalizer applied before FM demodulation.
    cma: CmaEqualizer,
    prev_iq: Option<Complex<f32>>,
    nco_cos: f32,
    nco_sin: f32,
    nco_inc_cos: f32,
    nco_inc_sin: f32,
    nco_counter: u32,
    pilot_i_lp: OnePoleLowPass,
    pilot_q_lp: OnePoleLowPass,
    pilot_abs_lp: OnePoleLowPass,
    pilot_bpf: BiquadBandPass,
    sum_lpf1: BiquadLowPass,
    sum_lpf2: BiquadLowPass,
    sum_notch: BiquadNotch,
    diff_pilot_notch: BiquadNotch,
    diff_lpf1: BiquadLowPass,
    diff_lpf2: BiquadLowPass,
    diff_q_lpf1: BiquadLowPass,
    diff_q_lpf2: BiquadLowPass,
    diff_dc: DcBlocker,
    diff_q_dc: DcBlocker,
    dc_m: DcBlocker,
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    deemph_m: Deemphasis,
    deemph_l: Deemphasis,
    deemph_r: Deemphasis,
    stereo_detect_level: f32,
    stereo_detected: bool,
    pilot_lock_level: f32,
    stereo_separation_gain: f32,
    detect_counter: u32,
    detect_pilot_mag_acc: f32,
    detect_pilot_abs_acc: f32,
    fm_gain: f32,
    resample_bank: [[f32; WFM_RESAMP_TAPS]; WFM_RESAMP_PHASES],
    sum_hist: [f32; WFM_RESAMP_TAPS],
    diff_hist: [f32; WFM_RESAMP_TAPS],
    diff_q_hist: [f32; WFM_RESAMP_TAPS],
    hist_pos: usize,
    denoise: StereoDenoise,
    prev_blend: f32,
    output_phase_inc: f64,
    output_phase: f64,
    /// Smoothed CCI (Co-Channel Interference) estimate, 0–100 scale.
    cci_level: f32,
    /// Smoothed ACI (Adjacent Channel Interference) estimate, 0–100 scale.
    aci_level: f32,
}

impl WfmStereoDecoder {
    pub fn new(
        composite_rate: u32,
        audio_rate: u32,
        output_channels: usize,
        stereo_enabled: bool,
        deemphasis_us: u32,
        denoise_level: WfmDenoiseLevel,
    ) -> Self {
        let composite_rate_f = composite_rate.max(1) as f32;
        let output_phase_inc = audio_rate.max(1) as f64 / composite_rate.max(1) as f64;
        let deemphasis_us = deemphasis_us as f32;
        Self {
            output_channels: output_channels.max(1),
            stereo_enabled,
            rds_decoder: RdsDecoder::new(composite_rate),
            rds_bpf: Iir8BandPass::new(composite_rate_f, RDS_SUBCARRIER_HZ),
            rds_dc: DcBlocker::new(0.995),
            cma: CmaEqualizer::new(),
            prev_iq: None,
            nco_cos: 1.0,
            nco_sin: 0.0,
            nco_inc_cos: (2.0 * std::f32::consts::PI * PILOT_HZ / composite_rate_f).cos(),
            nco_inc_sin: (2.0 * std::f32::consts::PI * PILOT_HZ / composite_rate_f).sin(),
            nco_counter: 0,
            pilot_i_lp: OnePoleLowPass::new(composite_rate_f, 400.0),
            pilot_q_lp: OnePoleLowPass::new(composite_rate_f, 400.0),
            pilot_abs_lp: OnePoleLowPass::new(composite_rate_f, 400.0),
            pilot_bpf: BiquadBandPass::new(composite_rate_f, PILOT_HZ, PILOT_BPF_Q),
            sum_lpf1: BiquadLowPass::new(composite_rate_f, AUDIO_BW_HZ, BW4_Q1),
            sum_lpf2: BiquadLowPass::new(composite_rate_f, AUDIO_BW_HZ, BW4_Q2),
            sum_notch: BiquadNotch::new(composite_rate_f, PILOT_HZ, PILOT_NOTCH_Q),
            diff_pilot_notch: BiquadNotch::new(composite_rate_f, PILOT_HZ, PILOT_NOTCH_Q),
            diff_lpf1: BiquadLowPass::new(composite_rate_f, STEREO_DIFF_BW_HZ, BW4_Q1),
            diff_lpf2: BiquadLowPass::new(composite_rate_f, STEREO_DIFF_BW_HZ, BW4_Q2),
            diff_q_lpf1: BiquadLowPass::new(composite_rate_f, STEREO_DIFF_BW_HZ, BW4_Q1),
            diff_q_lpf2: BiquadLowPass::new(composite_rate_f, STEREO_DIFF_BW_HZ, BW4_Q2),
            diff_dc: DcBlocker::new(STEREO_DIFF_DC_R),
            diff_q_dc: DcBlocker::new(STEREO_DIFF_DC_R),
            dc_m: DcBlocker::new(0.9999),
            dc_l: DcBlocker::new(0.9999),
            dc_r: DcBlocker::new(0.9999),
            deemph_m: Deemphasis::new(audio_rate.max(1) as f32, deemphasis_us),
            deemph_l: Deemphasis::new(audio_rate.max(1) as f32, deemphasis_us),
            deemph_r: Deemphasis::new(audio_rate.max(1) as f32, deemphasis_us),
            stereo_detect_level: 0.0,
            stereo_detected: false,
            pilot_lock_level: 0.0,
            stereo_separation_gain: 1.0,
            detect_counter: 0,
            detect_pilot_mag_acc: 0.0,
            detect_pilot_abs_acc: 0.0,
            fm_gain: composite_rate_f / (2.0 * 75_000.0),
            resample_bank: build_wfm_resample_bank(audio_rate as f32 / composite_rate_f),
            sum_hist: [0.0; WFM_RESAMP_TAPS],
            diff_hist: [0.0; WFM_RESAMP_TAPS],
            diff_q_hist: [0.0; WFM_RESAMP_TAPS],
            hist_pos: 0,
            denoise: {
                let mut denoise = StereoDenoise::new(audio_rate.max(1) as f32);
                denoise.level = denoise_level;
                denoise
            },
            prev_blend: 0.0,
            output_phase_inc,
            output_phase: 0.0,
            cci_level: 0.0,
            aci_level: 0.0,
        }
    }

    pub fn process_iq(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }

        // ACI estimation: measure IQ envelope variance before hard-limiting.
        // A clean FM signal has constant envelope (zero variance); ACI causes
        // amplitude modulation that raises the coefficient of variation.
        {
            let n = samples.len() as f32;
            let mut sum_mag = 0.0_f32;
            let mut sum_mag_sq = 0.0_f32;
            for s in samples.iter() {
                let mag = s.norm();
                sum_mag += mag;
                sum_mag_sq += mag * mag;
            }
            let mean_mag = sum_mag / n;
            let var = (sum_mag_sq / n - mean_mag * mean_mag).max(0.0);
            let cv = if mean_mag > 1e-8 { var.sqrt() / mean_mag } else { 0.0 };
            // Map CV to 0–100. Empirically, CV > 0.35 is heavy ACI.
            let raw_aci = (cv * 100.0 / 0.35).clamp(0.0, 100.0);
            let alpha = 0.08_f32;
            self.aci_level += alpha * (raw_aci - self.aci_level);
        }

        // Tech 9: apply CMA blind equalizer to IQ samples before FM demodulation.
        // The constant-modulus property of FM drives tap adaptation without a
        // training sequence, suppressing adjacent-channel interference.
        let mut equalized: Vec<Complex<f32>> = samples.iter().map(|&s| self.cma.process(s)).collect();

        // Hard-limit to unit magnitude after CMA (preserves phase for FM demod
        // while preventing clipping artefacts).
        for s in equalized.iter_mut() {
            let mag = s.norm();
            if mag > 1.0 {
                *s /= mag;
            }
        }

        let disc = demod_fm_with_prev(&equalized, &mut self.prev_iq);
        let mut output = Vec::with_capacity(
            ((samples.len() as f64 * self.output_phase_inc).ceil() as usize + 1)
                * self.output_channels.max(1),
        );
        let (trim_sin, trim_cos) = STEREO_SEPARATION_PHASE_TRIM.sin_cos();

        for &disc_sample in &disc {
            let x = disc_sample * self.fm_gain;
            let pilot_tone = self.pilot_bpf.process(x);

            let sin_p = self.nco_sin;
            let cos_p = self.nco_cos;
            let i = self.pilot_i_lp.process(pilot_tone * cos_p);
            let q = self.pilot_q_lp.process(pilot_tone * -sin_p);
            let pilot_mag = (i * i + q * q).sqrt();
            let inv_mag = 1.0 / (pilot_mag + 1e-12);
            let err_sin = q * inv_mag;
            let err_cos = i * inv_mag;

            let new_cos = self.nco_cos * self.nco_inc_cos - self.nco_sin * self.nco_inc_sin;
            let new_sin = self.nco_cos * self.nco_inc_sin + self.nco_sin * self.nco_inc_cos;
            self.nco_cos = new_cos;
            self.nco_sin = new_sin;
            self.nco_counter += 1;
            if self.nco_counter >= 1024 {
                self.nco_counter = 0;
                let mag = (self.nco_cos * self.nco_cos + self.nco_sin * self.nco_sin).sqrt();
                let inv = 1.0 / mag;
                self.nco_cos *= inv;
                self.nco_sin *= inv;
            }

            let pilot_abs = self.pilot_abs_lp.process(pilot_tone.abs());
            self.detect_pilot_mag_acc += pilot_mag;
            self.detect_pilot_abs_acc += pilot_abs;
            self.detect_counter += 1;
            if self.detect_counter >= STEREO_DETECT_DECIMATION {
                let inv_n = 1.0 / STEREO_DETECT_DECIMATION as f32;
                let avg_mag = self.detect_pilot_mag_acc * inv_n;
                let avg_abs = self.detect_pilot_abs_acc * inv_n;
                let pilot_coherence = (avg_mag / (avg_abs + 1e-4)).clamp(0.0, 1.0);
                let pilot_lock = ((pilot_coherence - PILOT_LOCK_ONSET) / 0.2).clamp(0.0, 1.0);
                self.pilot_lock_level += 0.12 * (pilot_lock - self.pilot_lock_level);
                let stereo_drive = (avg_mag * pilot_lock * 120.0).clamp(0.0, 1.0);
                let detect_coeff = if stereo_drive > self.stereo_detect_level {
                    0.0008 * STEREO_DETECT_DECIMATION as f32
                } else {
                    0.00005 * STEREO_DETECT_DECIMATION as f32
                };
                self.stereo_detect_level +=
                    detect_coeff * (stereo_drive - self.stereo_detect_level);
                if self.stereo_detected {
                    if self.stereo_detect_level < 0.22 {
                        self.stereo_detected = false;
                    }
                } else if self.stereo_detect_level > 0.6 {
                    self.stereo_detected = true;
                }
                // CCI estimation: a clean 19 kHz pilot has coherence ≈ π/4
                // (ratio of coherent magnitude to rectified absolute value).
                // Co-channel interference degrades this coherence by adding
                // incoherent energy at 19 kHz.  Normalise by the theoretical
                // maximum so a clean pilot reads 0 %.  Only report CCI when
                // the pilot is actually detected; mono signals have no pilot
                // and CCI is not meaningful.
                let raw_cci = if self.pilot_lock_level > 0.1 {
                    const MAX_COHERENCE: f32 = std::f32::consts::FRAC_PI_4;
                    let norm = (pilot_coherence / MAX_COHERENCE).clamp(0.0, 1.0);
                    ((1.0 - norm) * 100.0).clamp(0.0, 100.0)
                } else {
                    0.0
                };
                let cci_alpha = 0.08_f32;
                self.cci_level += cci_alpha * (raw_cci - self.cci_level);

                self.detect_counter = 0;
                self.detect_pilot_mag_acc = 0.0;
                self.detect_pilot_abs_acc = 0.0;
            }
            let stereo_blend_target = if self.stereo_detected { 1.0 } else { 0.0 };

            // Phase-corrected pilot estimates (exact real pilot phase).
            let sin_est = sin_p * err_cos + cos_p * err_sin;
            let cos_est = cos_p * err_cos - sin_p * err_sin;
            // Double-angle (38 kHz stereo carrier).
            let sin_2p = 2.0 * sin_est * cos_est;
            let cos_2p = 2.0 * cos_est * cos_est - 1.0;

            // Tech 2: derive the 57 kHz RDS carrier reference from the 19 kHz
            // pilot via the triple-angle formula: cos(3θ) = cos(2θ+θ), etc.
            // This gives a phase-coherent reference that is far cleaner than
            // the RDS decoder's autonomous free-running NCO.
            let cos_3p = cos_2p * cos_est - sin_2p * sin_est;
            let sin_3p = sin_2p * cos_est + cos_2p * sin_est;
            if self.pilot_lock_level > PILOT_LOCK_THRESHOLD {
                self.rds_decoder.set_pilot_ref(cos_3p, sin_3p);
            } else {
                self.rds_decoder.clear_pilot_ref();
            }

            // Adaptive RDS quality: base metric from pilot strength, then
            // penalise for CCI and ACI so the decoder weights bits lower when
            // interference is present (reduces block-error rate).
            let rds_base = (0.35 + pilot_mag * 20.0).clamp(0.35, 1.0);
            let cci_penalty = 1.0 - (self.cci_level * 0.006).clamp(0.0, 0.45);
            let aci_penalty = 1.0 - (self.aci_level * 0.004).clamp(0.0, 0.30);
            let rds_quality = (rds_base * cci_penalty * aci_penalty).clamp(0.10, 1.0);
            let rds_clean = self.rds_dc.process(self.rds_bpf.process(x));
            let _ = self.rds_decoder.process_sample(rds_clean, rds_quality);

            let sum = self.sum_lpf2.process(self.sum_lpf1.process(x));
            let x_notched = self.diff_pilot_notch.process(x);
            let diff_i = self.diff_dc.process(
                self.diff_lpf2
                    .process(self.diff_lpf1.process(x_notched * (cos_2p * 2.0))),
            );
            let diff_q = self.diff_q_dc.process(
                self.diff_q_lpf2
                    .process(self.diff_q_lpf1.process(x_notched * (-sin_2p * 2.0))),
            );

            let pos = self.hist_pos;
            self.sum_hist[pos] = sum;
            self.diff_hist[pos] = diff_i;
            self.diff_q_hist[pos] = diff_q;
            self.hist_pos = (pos + 1) & (WFM_RESAMP_TAPS - 1);

            let prev_phase = self.output_phase;
            self.output_phase += self.output_phase_inc;
            if self.output_phase < 1.0 {
                self.prev_blend = stereo_blend_target;
                continue;
            }
            self.output_phase -= 1.0;

            let frac = ((1.0 - prev_phase) / self.output_phase_inc) as f32;
            let ring_pos = self.hist_pos;
            let sum_i =
                polyphase_resample_ring(&self.sum_hist, ring_pos, &self.resample_bank, frac);
            let diff_i_raw =
                polyphase_resample_ring(&self.diff_hist, ring_pos, &self.resample_bank, frac);
            let diff_q =
                polyphase_resample_ring(&self.diff_q_hist, ring_pos, &self.resample_bank, frac);
            let blend_i =
                (self.prev_blend + frac * (stereo_blend_target - self.prev_blend)).clamp(0.0, 1.0);
            self.prev_blend = stereo_blend_target;
            let separation_drive =
                (self.pilot_lock_level * 0.65 + self.stereo_detect_level * 0.35).clamp(0.0, 1.0);
            let separation_target = STEREO_SEPARATION_GAIN_MIN
                + (STEREO_SEPARATION_GAIN_MAX - STEREO_SEPARATION_GAIN_MIN) * separation_drive;
            self.stereo_separation_gain +=
                0.015 * (separation_target - self.stereo_separation_gain);
            let diff_i = (diff_i_raw * trim_cos + diff_q * trim_sin) * self.stereo_separation_gain;
            let denoised_diff_i = self.denoise.process(sum_i, diff_i, diff_q);
            let preserve = DENOISE_STEREO_PRESERVE_MIN
                + (DENOISE_STEREO_PRESERVE_MAX - DENOISE_STEREO_PRESERVE_MIN) * separation_drive;
            let diff_i = denoised_diff_i + (diff_i - denoised_diff_i) * preserve;

            if self.output_channels >= 2 && self.stereo_enabled {
                let diff = diff_i * blend_i;
                let left = self
                    .dc_l
                    .process(self.deemph_l.process((sum_i + diff) * STEREO_MATRIX_GAIN))
                    .clamp(-1.0, 1.0);
                let right = self
                    .dc_r
                    .process(self.deemph_r.process((sum_i - diff) * STEREO_MATRIX_GAIN))
                    .clamp(-1.0, 1.0);
                output.push(left);
                output.push(right);
            } else {
                let mono = self
                    .dc_m
                    .process(self.deemph_m.process(self.sum_notch.process(sum_i)))
                    .clamp(-1.0, 1.0);
                output.push(mono);
                if self.output_channels >= 2 {
                    output.push(mono);
                }
            }
        }

        output
    }

    pub fn set_stereo_enabled(&mut self, enabled: bool) {
        self.stereo_enabled = enabled;
    }

    pub fn rds_data(&self) -> Option<RdsData> {
        self.rds_decoder.snapshot()
    }

    pub fn reset_rds(&mut self) {
        self.rds_decoder.reset();
    }

    pub fn reset_stereo_detect(&mut self) {
        self.stereo_detect_level = 0.0;
        self.stereo_detected = false;
        self.detect_counter = 0;
        self.detect_pilot_mag_acc = 0.0;
        self.detect_pilot_abs_acc = 0.0;
    }

    pub fn reset_demod_state(&mut self) {
        self.prev_iq = None;
    }

    pub fn reset_state(&mut self) {
        self.rds_decoder.reset();
        self.rds_bpf.reset();
        self.rds_dc.reset();
        self.cma.reset();
        self.prev_iq = None;
        self.nco_cos = 1.0;
        self.nco_sin = 0.0;
        self.nco_counter = 0;
        self.pilot_i_lp.reset();
        self.pilot_q_lp.reset();
        self.pilot_abs_lp.reset();
        self.pilot_bpf.reset();
        self.sum_lpf1.reset();
        self.sum_lpf2.reset();
        self.sum_notch.reset();
        self.diff_pilot_notch.reset();
        self.diff_lpf1.reset();
        self.diff_lpf2.reset();
        self.diff_q_lpf1.reset();
        self.diff_q_lpf2.reset();
        self.diff_dc.reset();
        self.diff_q_dc.reset();
        self.dc_m.reset();
        self.dc_l.reset();
        self.dc_r.reset();
        self.deemph_m.reset();
        self.deemph_l.reset();
        self.deemph_r.reset();
        self.stereo_detect_level = 0.0;
        self.stereo_detected = false;
        self.pilot_lock_level = 0.0;
        self.stereo_separation_gain = 1.0;
        self.detect_counter = 0;
        self.detect_pilot_mag_acc = 0.0;
        self.detect_pilot_abs_acc = 0.0;
        self.sum_hist = [0.0; WFM_RESAMP_TAPS];
        self.diff_hist = [0.0; WFM_RESAMP_TAPS];
        self.diff_q_hist = [0.0; WFM_RESAMP_TAPS];
        self.hist_pos = 0;
        self.denoise.reset();
        self.prev_blend = 0.0;
        self.output_phase = 0.0;
        self.cci_level = 0.0;
        self.aci_level = 0.0;
    }

    pub fn set_denoise_level(&mut self, level: WfmDenoiseLevel) {
        self.denoise.level = level;
    }

    pub fn stereo_detected(&self) -> bool {
        self.stereo_detected
    }

    /// Current CCI (Co-Channel Interference) level, 0–100 scale.
    pub fn cci_level(&self) -> u8 {
        self.cci_level.round().clamp(0.0, 100.0) as u8
    }

    /// Current ACI (Adjacent Channel Interference) level, 0–100 scale.
    pub fn aci_level(&self) -> u8 {
        self.aci_level.round().clamp(0.0, 100.0) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wfm_stereo_separation() {
        use std::f32::consts::TAU;

        let composite_rate: u32 = 240_000;
        let audio_rate: u32 = 48_000;
        let fs = composite_rate as f32;
        let duration_secs = 0.5_f32;
        let num_samples = (fs * duration_secs) as usize;

        let audio_freq = 1000.0_f32;
        let pilot_freq = 19_000.0_f32;
        let carrier_freq = 38_000.0_f32;
        let mut composite = vec![0.0_f32; num_samples];
        for (n, sample) in composite.iter_mut().enumerate() {
            let t = n as f32 / fs;
            let audio = (TAU * audio_freq * t).sin();
            let sum = audio;
            let diff = audio;
            let pilot = 0.1 * (TAU * pilot_freq * t).cos();
            let carrier = (TAU * carrier_freq * t).cos();
            *sample = sum + pilot + diff * carrier;
        }

        let peak_composite = 2.1_f32;
        let deviation_hz = 75_000.0_f32;
        let mod_index = TAU * deviation_hz / (peak_composite * fs);
        let mut phase: f32 = 0.0;
        let mut iq = Vec::with_capacity(num_samples);
        for &c in &composite {
            phase += mod_index * c;
            iq.push(Complex::from_polar(1.0, phase));
        }

        let mut decoder = WfmStereoDecoder::new(
            composite_rate,
            audio_rate,
            2,
            true,
            50,
            WfmDenoiseLevel::Auto,
        );
        let output = decoder.process_iq(&iq);

        let skip_samples = (0.2 * audio_rate as f32) as usize;
        let stereo_pairs = output.len() / 2;
        assert!(stereo_pairs > skip_samples + 100);

        let mut l_energy = 0.0_f64;
        let mut r_energy = 0.0_f64;
        let mut count = 0_u64;
        for idx in skip_samples..stereo_pairs {
            let l = output[2 * idx] as f64;
            let r = output[2 * idx + 1] as f64;
            l_energy += l * l;
            r_energy += r * r;
            count += 1;
        }
        let l_rms = (l_energy / count as f64).sqrt();
        let r_rms = (r_energy / count as f64).sqrt();
        let separation_db = if r_rms > 1e-10 {
            20.0 * (l_rms / r_rms).log10()
        } else {
            f64::INFINITY
        };

        assert!(l_rms > 0.01);
        assert!(separation_db > 20.0);
    }

    #[test]
    fn test_wfm_stereo_separation_multitone() {
        use std::f32::consts::TAU;

        let composite_rate: u32 = 240_000;
        let audio_rate: u32 = 48_000;
        let fs = composite_rate as f32;
        let duration_secs = 0.8_f32;
        let num_samples = (fs * duration_secs) as usize;
        let freqs = [400.0_f32, 2_000.0, 8_000.0, 14_000.0];
        let pilot_freq = 19_000.0_f32;
        let carrier_freq = 38_000.0_f32;

        for (label, diff_sign) in [("L-only", 1.0_f32), ("R-only", -1.0_f32)] {
            let mut composite = vec![0.0_f32; num_samples];
            for (n, sample) in composite.iter_mut().enumerate() {
                let t = n as f32 / fs;
                let audio: f32 = freqs
                    .iter()
                    .map(|&freq| (TAU * freq * t).sin())
                    .sum::<f32>()
                    / freqs.len() as f32;
                let sum = audio;
                let diff = audio * diff_sign;
                let pilot = 0.1 * (TAU * pilot_freq * t).cos();
                let carrier = (TAU * carrier_freq * t).cos();
                *sample = sum + pilot + diff * carrier;
            }

            let peak_composite = 2.1_f32;
            let deviation_hz = 75_000.0_f32;
            let mod_index = TAU * deviation_hz / (peak_composite * fs);
            let mut phase: f32 = 0.0;
            let mut iq = Vec::with_capacity(num_samples);
            for &c in &composite {
                phase += mod_index * c;
                iq.push(Complex::from_polar(1.0, phase));
            }

            let mut decoder = WfmStereoDecoder::new(
                composite_rate,
                audio_rate,
                2,
                true,
                50,
                WfmDenoiseLevel::Auto,
            );
            let output = decoder.process_iq(&iq);

            let skip_samples = (0.3 * audio_rate as f32) as usize;
            let stereo_pairs = output.len() / 2;
            assert!(
                stereo_pairs > skip_samples + 100,
                "{label}: not enough output"
            );

            let mut active_energy = 0.0_f64;
            let mut silent_energy = 0.0_f64;
            let mut count = 0_u64;
            for idx in skip_samples..stereo_pairs {
                let l = output[2 * idx] as f64;
                let r = output[2 * idx + 1] as f64;
                if diff_sign > 0.0 {
                    active_energy += l * l;
                    silent_energy += r * r;
                } else {
                    active_energy += r * r;
                    silent_energy += l * l;
                }
                count += 1;
            }
            let active_rms = (active_energy / count as f64).sqrt();
            let silent_rms = (silent_energy / count as f64).sqrt();
            let separation_db = if silent_rms > 1e-10 {
                20.0 * (active_rms / silent_rms).log10()
            } else {
                f64::INFINITY
            };

            assert!(active_rms > 0.01, "{label}: no active energy");
            assert!(separation_db > 15.0, "{label}: separation too low");
        }
    }

    #[test]
    fn test_wfm_no_pilot_stays_mono_detect() {
        use std::f32::consts::TAU;

        let composite_rate: u32 = 240_000;
        let audio_rate: u32 = 48_000;
        let fs = composite_rate as f32;
        let duration_secs = 0.5_f32;
        let num_samples = (fs * duration_secs) as usize;

        let audio_freq = 1000.0_f32;
        let mut composite = vec![0.0_f32; num_samples];
        for (n, sample) in composite.iter_mut().enumerate() {
            let t = n as f32 / fs;
            *sample = (TAU * audio_freq * t).sin();
        }

        let deviation_hz = 75_000.0_f32;
        let mod_index = TAU * deviation_hz / fs;
        let mut phase: f32 = 0.0;
        let mut iq = Vec::with_capacity(num_samples);
        for &c in &composite {
            phase += mod_index * c;
            iq.push(Complex::from_polar(1.0, phase));
        }

        let mut decoder = WfmStereoDecoder::new(
            composite_rate,
            audio_rate,
            2,
            true,
            50,
            WfmDenoiseLevel::Auto,
        );
        let output = decoder.process_iq(&iq);

        assert!(!decoder.stereo_detected());

        let skip_samples = (0.2 * audio_rate as f32) as usize;
        let stereo_pairs = output.len() / 2;
        assert!(stereo_pairs > skip_samples + 100);

        let mut diff_energy = 0.0_f64;
        let mut count = 0_u64;
        for idx in skip_samples..stereo_pairs {
            let l = output[2 * idx] as f64;
            let r = output[2 * idx + 1] as f64;
            let d = l - r;
            diff_energy += d * d;
            count += 1;
        }
        let diff_rms = (diff_energy / count as f64).sqrt();
        assert!(diff_rms < 0.01);
    }

    #[test]
    fn test_denoise_noisy_diff_attenuation() {
        let audio_rate = 48_000.0;
        let num_samples = (audio_rate * 0.5) as usize;
        let mut denoise = StereoDenoise::new(audio_rate);

        let mut rng_state = 12345_u64;
        let mut next_noise = || -> f32 {
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((rng_state >> 33) as f32 / (1u64 << 31) as f32) - 1.0
        };

        let mut input_energy = 0.0_f64;
        let mut output_energy = 0.0_f64;
        let skip = (audio_rate * 0.1) as usize;

        for idx in 0..num_samples {
            let noise_i = next_noise() * 0.5;
            let noise_q = next_noise() * 0.5;
            let out = denoise.process(0.0, noise_i, noise_q);
            if idx >= skip {
                input_energy += (noise_i as f64) * (noise_i as f64);
                output_energy += (out as f64) * (out as f64);
            }
        }

        let input_rms = (input_energy / (num_samples - skip) as f64).sqrt();
        let output_rms = (output_energy / (num_samples - skip) as f64).sqrt();
        let attenuation_db = 20.0 * (input_rms / (output_rms + 1e-20)).log10();
        assert!(attenuation_db > 6.0);
    }

    #[test]
    fn test_denoise_clean_stereo_preservation() {
        use std::f32::consts::TAU;

        let audio_rate = 48_000.0;
        let num_samples = (audio_rate * 0.5) as usize;
        let mut denoise = StereoDenoise::new(audio_rate);
        let tone_freq = 1000.0;

        let mut input_energy = 0.0_f64;
        let mut output_energy = 0.0_f64;
        let skip = (audio_rate * 0.15) as usize;

        for idx in 0..num_samples {
            let t = idx as f32 / audio_rate;
            let tone = (TAU * tone_freq * t).sin() * 0.5;
            let out = denoise.process(tone, tone, 0.0);
            if idx >= skip {
                input_energy += (tone as f64) * (tone as f64);
                output_energy += (out as f64) * (out as f64);
            }
        }

        let input_rms = (input_energy / (num_samples - skip) as f64).sqrt();
        let output_rms = (output_energy / (num_samples - skip) as f64).sqrt();
        let loss_db = 20.0 * (input_rms / (output_rms + 1e-20)).log10();
        assert!(loss_db < 4.0);
    }

    #[test]
    fn test_denoise_low_preserves_more_than_high() {
        let mut low = StereoDenoise::new(48_000.0);
        low.level = WfmDenoiseLevel::Low;
        let mut high = StereoDenoise::new(48_000.0);
        high.level = WfmDenoiseLevel::High;

        for &value in &[0.0_f32, 0.5, -0.3, 1.0, -1.0, 0.001] {
            let low_out = low.process(0.1, value, 0.2).abs();
            let high_out = high.process(0.1, value, 0.2).abs();
            assert!(low_out + 0.000_001 >= high_out);
        }
    }

    #[test]
    fn test_denoise_off_is_bypass() {
        let mut off = StereoDenoise::new(48_000.0);
        off.level = WfmDenoiseLevel::Off;

        for &(sum, diff_i, diff_q) in &[
            (0.1_f32, 0.5_f32, 0.2_f32),
            (0.0_f32, -0.3_f32, 0.8_f32),
            (1.0_f32, 1.0_f32, -0.5_f32),
            (-0.2_f32, 0.001_f32, 0.0_f32),
        ] {
            let out = off.process(sum, diff_i, diff_q);
            assert!((out - diff_i).abs() < 0.000_001);
        }
    }

    #[test]
    fn test_denoise_per_band_selectivity() {
        use std::f32::consts::TAU;

        let audio_rate = 48_000.0;
        let num_samples = (audio_rate * 0.5) as usize;
        let mut denoise = StereoDenoise::new(audio_rate);
        let low_freq = 400.0;

        let mut rng_state = 67890_u64;
        let mut next_noise = || -> f32 {
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((rng_state >> 33) as f32 / (1u64 << 31) as f32) - 1.0
        };

        let skip = (audio_rate * 0.15) as usize;
        let mut output_samples = Vec::with_capacity(num_samples - skip);

        for idx in 0..num_samples {
            let t = idx as f32 / audio_rate;
            let low_tone = (TAU * low_freq * t).sin() * 0.3;
            let high_noise = next_noise() * 0.3;
            let out = denoise.process(low_tone, low_tone + high_noise, high_noise);
            if idx >= skip {
                output_samples.push(out);
            }
        }

        let mut low_bp = BiquadBandPass::new(audio_rate, low_freq, 2.0);
        let mut low_energy = 0.0_f64;
        let mut total_energy = 0.0_f64;
        for &sample in &output_samples {
            let low_part = low_bp.process(sample);
            low_energy += (low_part as f64) * (low_part as f64);
            total_energy += (sample as f64) * (sample as f64);
        }

        let low_ratio = low_energy / (total_energy + 1e-20);
        assert!(low_ratio > 0.3);
    }

    #[test]
    fn test_wfm_stereo_separation_with_denoise() {
        use std::f32::consts::TAU;

        let composite_rate: u32 = 240_000;
        let audio_rate: u32 = 48_000;
        let fs = composite_rate as f32;
        let duration_secs = 0.5_f32;
        let num_samples = (fs * duration_secs) as usize;

        let audio_freq = 1000.0_f32;
        let pilot_freq = 19_000.0_f32;
        let carrier_freq = 38_000.0_f32;
        let mut composite = vec![0.0_f32; num_samples];
        for (n, sample) in composite.iter_mut().enumerate() {
            let t = n as f32 / fs;
            let audio = (TAU * audio_freq * t).sin();
            let sum = audio;
            let diff = audio;
            let pilot = 0.1 * (TAU * pilot_freq * t).cos();
            let carrier = (TAU * carrier_freq * t).cos();
            *sample = sum + pilot + diff * carrier;
        }

        let peak_composite = 2.1_f32;
        let deviation_hz = 75_000.0_f32;
        let mod_index = TAU * deviation_hz / (peak_composite * fs);
        let mut phase: f32 = 0.0;
        let mut iq = Vec::with_capacity(num_samples);
        for &c in &composite {
            phase += mod_index * c;
            iq.push(Complex::from_polar(1.0, phase));
        }

        let mut decoder = WfmStereoDecoder::new(
            composite_rate,
            audio_rate,
            2,
            true,
            50,
            WfmDenoiseLevel::Auto,
        );
        let output = decoder.process_iq(&iq);

        let skip_samples = (0.2 * audio_rate as f32) as usize;
        let stereo_pairs = output.len() / 2;
        assert!(stereo_pairs > skip_samples + 100);

        let mut l_energy = 0.0_f64;
        let mut r_energy = 0.0_f64;
        let mut count = 0_u64;
        for idx in skip_samples..stereo_pairs {
            let l = output[2 * idx] as f64;
            let r = output[2 * idx + 1] as f64;
            l_energy += l * l;
            r_energy += r * r;
            count += 1;
        }
        let l_rms = (l_energy / count as f64).sqrt();
        let r_rms = (r_energy / count as f64).sqrt();
        let separation_db = if r_rms > 1e-10 {
            20.0 * (l_rms / r_rms).log10()
        } else {
            f64::INFINITY
        };

        assert!(l_rms > 0.01);
        assert!(separation_db > 15.0);
    }

    /// Helper: generate stereo FM IQ samples from a composite signal.
    fn fm_modulate(composite: &[f32], peak: f32, deviation_hz: f32, fs: f32) -> Vec<Complex<f32>> {
        use std::f32::consts::TAU;
        let mod_index = TAU * deviation_hz / (peak * fs);
        let mut phase = 0.0_f32;
        composite
            .iter()
            .map(|&c| {
                phase += mod_index * c;
                Complex::from_polar(1.0, phase)
            })
            .collect()
    }

    /// Helper: build a stereo FM composite signal (1 kHz audio in L only).
    fn stereo_composite(fs: f32, n: usize) -> Vec<f32> {
        use std::f32::consts::TAU;
        let audio_freq = 1000.0_f32;
        let pilot_freq = 19_000.0_f32;
        let carrier_freq = 38_000.0_f32;
        let mut composite = vec![0.0_f32; n];
        for (i, sample) in composite.iter_mut().enumerate() {
            let t = i as f32 / fs;
            let audio = (TAU * audio_freq * t).sin();
            let pilot = 0.1 * (TAU * pilot_freq * t).cos();
            let carrier = (TAU * carrier_freq * t).cos();
            *sample = audio + pilot + audio * carrier;
        }
        composite
    }

    #[test]
    fn test_clean_signal_aci_near_zero() {
        let composite_rate: u32 = 240_000;
        let audio_rate: u32 = 48_000;
        let fs = composite_rate as f32;
        let n = (fs * 0.5) as usize;

        let composite = stereo_composite(fs, n);
        let iq = fm_modulate(&composite, 2.1, 75_000.0, fs);

        let mut decoder = WfmStereoDecoder::new(
            composite_rate, audio_rate, 2, true, 50, WfmDenoiseLevel::Auto,
        );
        let _ = decoder.process_iq(&iq);

        // Clean constant-envelope FM should show near-zero ACI.
        assert!(
            decoder.aci_level() < 5,
            "clean signal ACI should be near 0, got {}",
            decoder.aci_level()
        );
    }

    #[test]
    fn test_aci_nonzero_with_adjacent_channel() {
        use std::f32::consts::TAU;

        let composite_rate: u32 = 240_000;
        let audio_rate: u32 = 48_000;
        let fs = composite_rate as f32;
        let n = (fs * 1.0) as usize;

        // Main stereo FM signal
        let composite = stereo_composite(fs, n);
        let mut iq = fm_modulate(&composite, 2.1, 75_000.0, fs);

        // Add a strong adjacent-channel signal offset by 150 kHz.
        // This creates amplitude modulation on the combined IQ envelope.
        let adj_freq_offset = 150_000.0_f32;
        let adj_composite: Vec<f32> = (0..n)
            .map(|i| (TAU * 3_000.0 * i as f32 / fs).sin())
            .collect();
        let adj_mod_index = TAU * 75_000.0 / (1.1 * fs);
        let mut adj_phase = 0.0_f32;
        for (i, s) in iq.iter_mut().enumerate() {
            let t = i as f32 / fs;
            adj_phase += adj_mod_index * adj_composite[i];
            let adj = Complex::from_polar(0.5, adj_phase + TAU * adj_freq_offset * t);
            *s = *s + adj;
        }

        let mut decoder = WfmStereoDecoder::new(
            composite_rate, audio_rate, 2, true, 50, WfmDenoiseLevel::Auto,
        );
        let _ = decoder.process_iq(&iq);

        assert!(
            decoder.aci_level() > 5,
            "adjacent-channel signal should raise ACI above 5 %, got {}",
            decoder.aci_level()
        );
    }

    #[test]
    fn test_clean_stereo_cci_near_zero() {
        let composite_rate: u32 = 240_000;
        let audio_rate: u32 = 48_000;
        let fs = composite_rate as f32;
        let n = (fs * 0.5) as usize;

        let composite = stereo_composite(fs, n);
        let iq = fm_modulate(&composite, 2.1, 75_000.0, fs);

        let mut decoder = WfmStereoDecoder::new(
            composite_rate, audio_rate, 2, true, 50, WfmDenoiseLevel::Auto,
        );
        let _ = decoder.process_iq(&iq);

        // A clean stereo signal should show CCI near zero.
        assert!(
            decoder.cci_level() < 10,
            "clean stereo CCI should be near 0, got {}",
            decoder.cci_level()
        );
    }

    #[test]
    fn test_cci_nonzero_with_cochannel() {
        use std::f32::consts::TAU;

        let composite_rate: u32 = 240_000;
        let audio_rate: u32 = 48_000;
        let fs = composite_rate as f32;
        let n = (fs * 1.0) as usize;

        // Main stereo FM signal.
        let composite = stereo_composite(fs, n);
        let mut iq = fm_modulate(&composite, 2.1, 75_000.0, fs);

        // Add a co-channel interferer: another FM station at the SAME
        // frequency but with a different pilot phase and different audio.
        let mut intf_composite = vec![0.0_f32; n];
        for (i, sample) in intf_composite.iter_mut().enumerate() {
            let t = i as f32 / fs;
            let audio = (TAU * 5_000.0 * t).sin();
            // Pilot with a different starting phase.
            let pilot = 0.1 * (TAU * 19_000.0 * t + 1.5).cos();
            let carrier = (TAU * 38_000.0 * t + 3.0).cos();
            *sample = audio + pilot + audio * carrier;
        }
        let intf_iq = fm_modulate(&intf_composite, 2.1, 75_000.0, fs);

        // Mix at 70 % of the main signal's amplitude — strong enough to
        // overcome the FM capture effect and visibly degrade pilot coherence.
        for (s, intf) in iq.iter_mut().zip(intf_iq.iter()) {
            *s = *s + intf * 0.7;
        }

        let mut decoder = WfmStereoDecoder::new(
            composite_rate, audio_rate, 2, true, 50, WfmDenoiseLevel::Auto,
        );
        let _ = decoder.process_iq(&iq);

        assert!(
            decoder.cci_level() > 2,
            "co-channel interference should raise CCI above 2 %, got {}",
            decoder.cci_level()
        );
    }
}
