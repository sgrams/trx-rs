// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;
use trx_core::rig::state::RdsData;
use trx_rds::RdsDecoder;

use super::{math::demod_fm_with_prev, DcBlocker};

const RDS_SUBCARRIER_HZ: f32 = 57_000.0;
const RDS_BPF_Q: f32 = 10.0;
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
    enabled: bool,
}

impl StereoDenoise {
    fn new(audio_rate: f32) -> Self {
        let bands = std::array::from_fn(|idx| {
            DenoiseSubband::new(audio_rate, DENOISE_CENTERS[idx], DENOISE_Q[idx])
        });
        Self {
            bands,
            enabled: true,
        }
    }

    #[inline]
    fn process(&mut self, sum: f32, diff_i: f32, diff_q: f32) -> f32 {
        if !self.enabled {
            return diff_i;
        }

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
        diff_i * broadband_gain
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
    rds_bpf: BiquadBandPass,
    rds_dc: DcBlocker,
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
}

impl WfmStereoDecoder {
    pub fn new(
        composite_rate: u32,
        audio_rate: u32,
        output_channels: usize,
        stereo_enabled: bool,
        deemphasis_us: u32,
    ) -> Self {
        let composite_rate_f = composite_rate.max(1) as f32;
        let output_phase_inc = audio_rate.max(1) as f64 / composite_rate.max(1) as f64;
        let deemphasis_us = deemphasis_us as f32;
        Self {
            output_channels: output_channels.max(1),
            stereo_enabled,
            rds_decoder: RdsDecoder::new(composite_rate),
            rds_bpf: BiquadBandPass::new(composite_rate_f, RDS_SUBCARRIER_HZ, RDS_BPF_Q),
            rds_dc: DcBlocker::new(0.995),
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
            denoise: StereoDenoise::new(audio_rate.max(1) as f32),
            prev_blend: 0.0,
            output_phase_inc,
            output_phase: 0.0,
        }
    }

    pub fn process_iq(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }

        let disc = demod_fm_with_prev(samples, &mut self.prev_iq);
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
                let pilot_lock = ((pilot_coherence - 0.4) / 0.2).clamp(0.0, 1.0);
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
                self.detect_counter = 0;
                self.detect_pilot_mag_acc = 0.0;
                self.detect_pilot_abs_acc = 0.0;
            }
            let stereo_blend_target = if self.stereo_detected { 1.0 } else { 0.0 };

            let rds_quality = (0.35 + pilot_mag * 20.0).clamp(0.35, 1.0);
            let rds_clean = self.rds_dc.process(self.rds_bpf.process(x));
            let _ = self.rds_decoder.process_sample(rds_clean, rds_quality);

            let sum = self.sum_lpf2.process(self.sum_lpf1.process(x));

            let sin_est = sin_p * err_cos + cos_p * err_sin;
            let cos_est = cos_p * err_cos - sin_p * err_sin;
            let sin_2p = 2.0 * sin_est * cos_est;
            let cos_2p = 2.0 * cos_est * cos_est - 1.0;
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
            let diff_i =
                (diff_i_raw * trim_cos + diff_q * trim_sin) * self.stereo_separation_gain;
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
    }

    pub fn set_denoise_enabled(&mut self, enabled: bool) {
        self.denoise.enabled = enabled;
    }

    pub fn stereo_detected(&self) -> bool {
        self.stereo_detected
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

        let mut decoder = WfmStereoDecoder::new(composite_rate, audio_rate, 2, true, 50);
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

            let mut decoder = WfmStereoDecoder::new(composite_rate, audio_rate, 2, true, 50);
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

        let mut decoder = WfmStereoDecoder::new(composite_rate, audio_rate, 2, true, 50);
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
    fn test_denoise_bypass_when_disabled() {
        let mut denoise = StereoDenoise::new(48_000.0);
        denoise.enabled = false;

        for &value in &[0.0_f32, 0.5, -0.3, 1.0, -1.0, 0.001] {
            assert_eq!(denoise.process(0.1, value, 0.2), value);
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

        let mut decoder = WfmStereoDecoder::new(composite_rate, audio_rate, 2, true, 50);
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
}
