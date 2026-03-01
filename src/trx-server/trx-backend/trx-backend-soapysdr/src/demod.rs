// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;
use trx_core::rig::state::{RdsData, RigMode};
use trx_rds::RdsDecoder;

const RDS_SUBCARRIER_HZ: f32 = 57_000.0;
const RDS_BPF_Q: f32 = 10.0;
/// Pilot tone frequency (Hz).
const PILOT_HZ: f32 = 19_000.0;
/// Audio bandwidth for WFM (Hz).
/// 15.8 kHz leaves guard band below the 19 kHz pilot and reduces top-end
/// artifacts on strong signals while preserving the useful broadcast range.
const AUDIO_BW_HZ: f32 = 18_000.0;
/// Stereo L-R subchannel bandwidth for WFM (Hz).
/// Must match AUDIO_BW_HZ so the sum and diff filter paths have identical
/// group delay, which is critical for stereo separation across all frequencies.
const STEREO_DIFF_BW_HZ: f32 = AUDIO_BW_HZ;
/// Q values for a 4th-order Butterworth cascade (two 2nd-order stages).
/// Stage 1: Q = 1 / (2 cos(π/8))
const BW4_Q1: f32 = 0.5412;
/// Stage 2: Q = 1 / (2 cos(3π/8))
const BW4_Q2: f32 = 1.3066;
/// Q for the 19 kHz pilot notch (~3.8 kHz 3 dB bandwidth).
const PILOT_NOTCH_Q: f32 = 5.0;
/// Narrow 19 kHz band-pass used to derive zero-crossings for switching stereo demod.
const PILOT_BPF_Q: f32 = 20.0;
/// Fixed phase trim on the recovered L-R channel to compensate pilot-path delay.
const STEREO_SEPARATION_PHASE_TRIM: f32 = 0.0;
/// Fixed gain trim on the recovered L-R channel.
const STEREO_SEPARATION_GAIN: f32 = 1.000;
/// Extra headroom in the stereo matrix to reduce stereo-only clipping/IMD on
/// strong program material. This keeps bass excursions from flattening treble.
const STEREO_MATRIX_GAIN: f32 = 0.80;
/// Stereo detection runs every N composite samples to reduce CPU.
const STEREO_DETECT_DECIMATION: u32 = 16;
/// Gentle high-pass memory for the stereo L-R path.
/// This trims only very low-frequency difference energy that can eat headroom
/// and modulate higher-frequency stereo detail.
const STEREO_DIFF_DC_R: f32 = 0.9995;
/// Fractional-resampler FIR taps for WFM audio reconstruction.
const WFM_RESAMP_TAPS: usize = 32;
/// Polyphase slots for the WFM fractional FIR resampler.
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
                0.35875 - 0.48829 * tw.cos()
                    + 0.14128 * (2.0 * tw).cos()
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

/// Polyphase FIR resample from a circular buffer.
/// `pos` points to the oldest sample (next write position).
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
    let mask = WFM_RESAMP_TAPS - 1; // power-of-2 bitmask
    for tap in 0..WFM_RESAMP_TAPS {
        acc += hist[(pos + tap) & mask] * coeffs[tap];
    }
    acc
}

#[inline]
fn fast_atan2(y: f32, x: f32) -> f32 {
    if x == 0.0 {
        if y > 0.0 {
            return std::f32::consts::FRAC_PI_2;
        }
        if y < 0.0 {
            return -std::f32::consts::FRAC_PI_2;
        }
        return 0.0;
    }

    #[inline]
    fn fast_atan(z: f32) -> f32 {
        let abs_z = z.abs();
        if abs_z <= 1.0 {
            z * (std::f32::consts::FRAC_PI_4 + 0.273 * (1.0 - abs_z))
        } else {
            let inv = 1.0 / z;
            let base = inv * (std::f32::consts::FRAC_PI_4 + 0.273 * (1.0 - inv.abs()));
            if z > 0.0 {
                std::f32::consts::FRAC_PI_2 - base
            } else {
                -std::f32::consts::FRAC_PI_2 - base
            }
        }
    }

    let angle = if x > 0.0 {
        fast_atan(y / x)
    } else if x < 0.0 {
        if y >= 0.0 {
            fast_atan(y / x) + std::f32::consts::PI
        } else {
            fast_atan(y / x) - std::f32::consts::PI
        }
    } else {
        0.0
    };
    angle
}

/// 7th-order minimax atan approximation for |z| ≤ 1.
/// Max error ≈ 2.4e-7 rad (~0.000014°).
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn atan_poly_avx2(z: std::arch::x86_64::__m256) -> std::arch::x86_64::__m256 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    // Coefficients for atan(z) ≈ z·(c0 + z²·(c1 + z²·(c2 + z²·c3)))
    let c0 = _mm256_set1_ps(0.999_999_5_f32);
    let c1 = _mm256_set1_ps(-0.333_326_1_f32);
    let c2 = _mm256_set1_ps(0.199_777_1_f32);
    let c3 = _mm256_set1_ps(-0.138_776_8_f32);

    let z2 = _mm256_mul_ps(z, z);
    // Horner: c2 + z²·c3
    let p = _mm256_add_ps(c2, _mm256_mul_ps(z2, c3));
    // c1 + z²·p
    let p = _mm256_add_ps(c1, _mm256_mul_ps(z2, p));
    // c0 + z²·p
    let p = _mm256_add_ps(c0, _mm256_mul_ps(z2, p));
    // z · p
    _mm256_mul_ps(z, p)
}

/// Branchless AVX2 atan2 using argument reduction and 7th-order polynomial.
///
/// Uses the |y| > |x| swap technique to keep the atan input in [-1, 1],
/// then corrects with π/2 and sign adjustments.  No divisions by zero,
/// no NaN branches — pure SIMD throughput.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn fast_atan2_8_avx2(
    y: std::arch::x86_64::__m256,
    x: std::arch::x86_64::__m256,
) -> std::arch::x86_64::__m256 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let abs_mask = _mm256_castsi256_ps(_mm256_set1_epi32(0x7FFF_FFFF_u32 as i32));
    let sign_mask = _mm256_castsi256_ps(_mm256_set1_epi32(0x8000_0000_u32 as i32));
    let pi = _mm256_set1_ps(std::f32::consts::PI);
    let pi_2 = _mm256_set1_ps(std::f32::consts::FRAC_PI_2);

    let abs_y = _mm256_and_ps(y, abs_mask);
    let abs_x = _mm256_and_ps(x, abs_mask);

    // If |y| > |x|, swap so the atan input is in [-1, 1].
    let swap_mask = _mm256_cmp_ps(abs_y, abs_x, _CMP_GT_OS);
    let num = _mm256_blendv_ps(y, x, swap_mask);
    let den = _mm256_blendv_ps(x, y, swap_mask);

    // Add tiny epsilon to denominator to avoid 0/0 NaN when both x and y are zero.
    let eps = _mm256_set1_ps(1.0e-30);
    let safe_den = _mm256_or_ps(den, _mm256_and_ps(
        _mm256_cmp_ps(den, _mm256_setzero_ps(), _CMP_EQ_OQ),
        eps,
    ));
    let atan_input = _mm256_div_ps(num, safe_den);
    let mut result = atan_poly_avx2(atan_input);

    // If swapped, result = copysign(π/2, atan_input) - result
    let adj = _mm256_sub_ps(
        _mm256_or_ps(pi_2, _mm256_and_ps(atan_input, sign_mask)),
        result,
    );
    result = _mm256_blendv_ps(result, adj, swap_mask);

    // Quadrant correction: if x < 0, add ±π (sign from y).
    let x_sign_mask = _mm256_castsi256_ps(_mm256_srai_epi32(
        _mm256_castps_si256(x),
        31,
    ));
    let correction = _mm256_and_ps(
        _mm256_xor_ps(pi, _mm256_and_ps(sign_mask, y)),
        x_sign_mask,
    );
    _mm256_add_ps(result, correction)
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

#[derive(Debug, Clone)]
pub(crate) struct DcBlocker {
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlocker {
    pub(crate) fn new(r: f32) -> Self {
        Self {
            r: r.clamp(0.9, 0.9999),
            x1: 0.0,
            y1: 0.0,
        }
    }

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }

    pub(crate) fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

/// Soft AGC with a fast-attack / slow-release envelope follower.
///
/// Tracks the signal envelope and adjusts gain so the output level converges
/// toward `target`.  Gain decreases quickly when the signal is louder than
/// the target (prevents clipping) and recovers slowly during quieter periods
/// (avoids pumping noise).  A `max_gain` cap prevents excessive amplification
/// of noise during silence.
#[derive(Debug, Clone)]
pub(crate) struct SoftAgc {
    gain: f32,
    envelope: f32,
    attack_coeff: f32,
    release_coeff: f32,
    target: f32,
    max_gain: f32,
}

impl SoftAgc {
    /// Create a new `SoftAgc`.
    ///
    /// - `sample_rate`: audio sample rate in Hz
    /// - `attack_ms`:   envelope follower attack time (fast, e.g. 1–10 ms)
    /// - `release_ms`:  envelope follower release time (slow, e.g. 50–1000 ms)
    /// - `target`:      desired peak output level (e.g. `0.5`)
    /// - `max_gain_db`: maximum gain cap in dB (e.g. `30.0`)
    pub(crate) fn new(
        sample_rate: f32,
        attack_ms: f32,
        release_ms: f32,
        target: f32,
        max_gain_db: f32,
    ) -> Self {
        let sr = sample_rate.max(1.0);
        let attack_coeff = 1.0 - (-1.0 / (attack_ms * 1e-3 * sr)).exp();
        let release_coeff = 1.0 - (-1.0 / (release_ms * 1e-3 * sr)).exp();
        Self {
            gain: 1.0,
            envelope: 0.0,
            attack_coeff,
            release_coeff,
            target: target.max(0.01),
            max_gain: 10.0_f32.powf(max_gain_db / 20.0),
        }
    }

    fn update_gain(&mut self, level: f32) -> f32 {
        // Update envelope tracker (peak-hold with attack/release).
        let env_coeff = if level > self.envelope {
            self.attack_coeff
        } else {
            self.release_coeff
        };
        self.envelope += env_coeff * (level - self.envelope);

        // Compute desired gain; fast response when reducing, slow when recovering.
        if self.envelope > 1e-6 {
            let desired = (self.target / self.envelope).min(self.max_gain);
            let gain_coeff = if desired < self.gain {
                self.attack_coeff
            } else {
                self.release_coeff
            };
            self.gain += gain_coeff * (desired - self.gain);
        }

        self.gain
    }

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        let gain = self.update_gain(x.abs());
        (x * gain).clamp(-1.0, 1.0)
    }

    #[allow(dead_code)]
    pub(crate) fn process_pair(&mut self, left: f32, right: f32) -> (f32, f32) {
        let gain = self.update_gain(left.abs().max(right.abs()));
        (
            (left * gain).clamp(-1.0, 1.0),
            (right * gain).clamp(-1.0, 1.0),
        )
    }

    pub(crate) fn process_complex(&mut self, x: Complex<f32>) -> Complex<f32> {
        let gain = self.update_gain((x.re * x.re + x.im * x.im).sqrt());
        let mut y = x * gain;
        let mag = (y.re * y.re + y.im * y.im).sqrt();
        if mag > 1.0 {
            y /= mag;
        }
        y
    }
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

        // RBJ band-pass, constant skirt gain.
        let b0 = alpha * inv_a0;
        let b1 = 0.0;
        let b2 = -alpha * inv_a0;
        let a1 = (-2.0 * cos_w0) * inv_a0;
        let a2 = (1.0 - alpha) * inv_a0;

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
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
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

/// 2nd-order IIR low-pass filter (RBJ cookbook).  Use [`BW4_Q1`]/[`BW4_Q2`] in
/// cascade for a proper 4th-order Butterworth response.
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
        Self { b0, b1, b2, a1, a2, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1 - self.a2 * self.y2;
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

/// 2nd-order IIR notch (band-reject) filter (RBJ cookbook).
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
        let b0 = a0_inv;
        let b1 = -2.0 * cos_w0 * a0_inv;
        let b2 = a0_inv;
        let a1 = b1;
        let a2 = (1.0 - alpha) * a0_inv;
        Self { b0, b1, b2, a1, a2, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1 - self.a2 * self.y2;
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
        let tau = (tau_us.max(1.0)) * 1e-6;
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
pub struct WfmStereoDecoder {
    output_channels: usize,
    stereo_enabled: bool,
    rds_decoder: RdsDecoder,
    rds_bpf: BiquadBandPass,
    rds_dc: DcBlocker,
    prev_iq: Option<Complex<f32>>,
    /// Quadrature NCO state: (cos, sin) of pilot phase.
    nco_cos: f32,
    nco_sin: f32,
    /// Precomputed rotation increment: (cos(Δ), sin(Δ)) where Δ = 2π·19000/fs.
    nco_inc_cos: f32,
    nco_inc_sin: f32,
    /// Sample counter for periodic NCO renormalization.
    nco_counter: u32,
    pilot_i_lp: OnePoleLowPass,
    pilot_q_lp: OnePoleLowPass,
    pilot_abs_lp: OnePoleLowPass,
    pilot_bpf: BiquadBandPass,
    /// 4th-order Butterworth cascade for L+R (two 2nd-order stages, Q = BW4_Q1/BW4_Q2).
    sum_lpf1: BiquadLowPass,
    sum_lpf2: BiquadLowPass,
    /// Notch at 19 kHz for the mono output path — keeps pilot tone out of mono
    /// audio without introducing phase mismatch with the diff channel.
    sum_notch: BiquadNotch,
    /// Notch at 19 kHz on composite before diff demod — removes pilot that would
    /// create intermod products when multiplied by the 38 kHz carrier.
    diff_pilot_notch: BiquadNotch,
    /// 4th-order Butterworth cascade for L-R (matched to sum path for stereo phase accuracy).
    diff_lpf1: BiquadLowPass,
    diff_lpf2: BiquadLowPass,
    /// Quadrature companion of the L-R path used for phase trim / crosstalk adjustment.
    diff_q_lpf1: BiquadLowPass,
    diff_q_lpf2: BiquadLowPass,
    /// Gentle high-pass on the stereo-difference path to reduce bass-driven IMD.
    diff_dc: DcBlocker,
    diff_q_dc: DcBlocker,
    /// DC blockers on audio outputs — remove carrier-offset DC from the FM discriminator.
    dc_m: DcBlocker,
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    deemph_m: Deemphasis,
    deemph_l: Deemphasis,
    deemph_r: Deemphasis,
    /// Smoothed pilot-derived stereo detection strength in [0, 1].
    stereo_detect_level: f32,
    /// Hysteretic pilot-lock result used by the UI.
    stereo_detected: bool,
    /// Decimation counter for stereo detection (runs every STEREO_DETECT_DECIMATION samples).
    detect_counter: u32,
    /// Accumulated pilot magnitude for decimated detection.
    detect_pilot_mag_acc: f32,
    /// Accumulated pilot abs for decimated detection.
    detect_pilot_abs_acc: f32,
    /// FM discriminator gain normalization.
    ///
    /// `demod_fm` outputs `atan2(…)/π ≈ 2·Δf/fs` for small deviations.
    /// For standard WFM ±75 kHz deviation we want ±1.0 at full deviation,
    /// so `fm_gain = fs / (2 · 75_000)`.
    fm_gain: f32,
    /// Shared coefficient bank for the polyphase fractional audio resampler.
    resample_bank: [[f32; WFM_RESAMP_TAPS]; WFM_RESAMP_PHASES],
    /// Circular buffer for polyphase FIR resampling of the sum channel.
    sum_hist: [f32; WFM_RESAMP_TAPS],
    /// Circular buffer for polyphase FIR resampling of the diff channel.
    diff_hist: [f32; WFM_RESAMP_TAPS],
    /// Circular buffer for polyphase FIR resampling of the quadrature diff channel.
    diff_q_hist: [f32; WFM_RESAMP_TAPS],
    /// Write position for the circular history buffers.
    hist_pos: usize,
    /// Previous pilot blend sample for simple linear interpolation.
    prev_blend: f32,
    /// Fractional phase increment per composite sample = audio_rate / composite_rate.
    /// Avoids integer-division rate error when composite_rate is not an exact
    /// multiple of audio_rate (e.g. 250 kHz composite → 48 kHz audio).
    output_phase_inc: f64,
    /// Fractional phase accumulator (0 .. 1).  Emits an output sample whenever
    /// it crosses 1.0, ensuring the long-term rate is exactly audio_rate.
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
            // 4th-order Butterworth: two cascaded biquads with BW4_Q1/BW4_Q2.
            // At 19 kHz (pilot): ≈ −12 dB; at 38 kHz (DSB carrier): ≈ −32 dB.
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
            detect_counter: 0,
            detect_pilot_mag_acc: 0.0,
            detect_pilot_abs_acc: 0.0,
            fm_gain: composite_rate_f / (2.0 * 75_000.0),
            resample_bank: build_wfm_resample_bank(audio_rate as f32 / composite_rate_f),
            sum_hist: [0.0; WFM_RESAMP_TAPS],
            diff_hist: [0.0; WFM_RESAMP_TAPS],
            diff_q_hist: [0.0; WFM_RESAMP_TAPS],
            hist_pos: 0,
            prev_blend: 0.0,
            output_phase_inc,
            output_phase: 0.0,
        }
    }

    pub fn process_iq(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }

        // Batch FM discriminator using AVX2 atan2 when available.
        let disc = demod_fm_with_prev(samples, &mut self.prev_iq);

        let mut output = Vec::with_capacity(
            ((samples.len() as f64 * self.output_phase_inc).ceil() as usize + 1)
                * self.output_channels.max(1),
        );
        let (trim_sin, trim_cos) = STEREO_SEPARATION_PHASE_TRIM.sin_cos();

        for &disc_sample in &disc {
            // Normalize discriminator output so ±75 kHz deviation maps to ±1.0.
            let x = disc_sample * self.fm_gain;

            let pilot_tone = self.pilot_bpf.process(x);

            // --- Pilot phase estimator (quadrature NCO) ---
            let sin_p = self.nco_sin;
            let cos_p = self.nco_cos;
            let i = self.pilot_i_lp.process(pilot_tone * cos_p);
            let q = self.pilot_q_lp.process(pilot_tone * -sin_p);
            let pilot_mag = (i * i + q * q).sqrt();
            // Derive sin/cos of the PLL phase error from the I/Q arms directly.
            let inv_mag = 1.0 / (pilot_mag + 1e-12);
            let err_sin = q * inv_mag;
            let err_cos = i * inv_mag;
            // Advance NCO via complex rotation: (cos,sin) *= (cos_inc,sin_inc).
            let new_cos = self.nco_cos * self.nco_inc_cos - self.nco_sin * self.nco_inc_sin;
            let new_sin = self.nco_cos * self.nco_inc_sin + self.nco_sin * self.nco_inc_cos;
            self.nco_cos = new_cos;
            self.nco_sin = new_sin;
            // Renormalize NCO every 1024 samples to prevent drift.
            self.nco_counter += 1;
            if self.nco_counter >= 1024 {
                self.nco_counter = 0;
                let mag = (self.nco_cos * self.nco_cos + self.nco_sin * self.nco_sin).sqrt();
                let inv = 1.0 / mag;
                self.nco_cos *= inv;
                self.nco_sin *= inv;
            }

            // --- Decimated stereo detection ---
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
            let stereo_blend_target = if self.stereo_detected {
                1.0
            } else {
                0.0
            };

            // --- RDS ---
            let rds_quality = (0.35 + pilot_mag * 20.0).clamp(0.35, 1.0);
            let rds_band = self.rds_bpf.process(x);
            let rds_clean = self.rds_dc.process(rds_band);
            let _ = self.rds_decoder.process_sample(rds_clean, rds_quality);

            // --- L+R (sum): 4th-order Butterworth ---
            // The pilot notch is NOT applied here so the sum and diff paths have
            // identical phase responses, which is required for good stereo separation.
            // The notch is applied only on the mono output path where phase matching
            // with the diff channel is irrelevant.
            let sum = self.sum_lpf2.process(self.sum_lpf1.process(x));

            // --- L-R (diff): 38 kHz demod + 6th-order Butterworth (unblended) ---
            // Notch the 19 kHz pilot from the composite before multiplying by the
            // 38 kHz carrier to prevent pilot×carrier intermod products.
            // Blend is applied per-band at audio rate in the emit step below.
            // Reconstruct sin/cos of the estimated pilot phase from the NCO
            // sin_p/cos_p rotated by the PLL error (whose sin/cos we derived
            // from the I/Q arms above, avoiding a second sin_cos call).
            let sin_est = sin_p * err_cos + cos_p * err_sin;
            let cos_est = cos_p * err_cos - sin_p * err_sin;
            // Double-angle identity for 38 kHz carrier: sin(2θ) = 2·sin·cos,
            // cos(2θ) = 2·cos²-1. Eliminates the second sin_cos call entirely.
            let sin_2p = 2.0 * sin_est * cos_est;
            let cos_2p = 2.0 * cos_est * cos_est - 1.0;
            let x_notched = self.diff_pilot_notch.process(x);
            let diff_i = self
                .diff_dc
                .process(self.diff_lpf2.process(self.diff_lpf1.process(x_notched * (cos_2p * 2.0))));
            let diff_q = self
                .diff_q_dc
                .process(self.diff_q_lpf2.process(self.diff_q_lpf1.process(x_notched * (-sin_2p * 2.0))));

            // --- Polyphase FIR fractional resampling ---
            // This uses a short windowed-sinc bank instead of cubic interpolation
            // to reduce top-end overshoot/ringing near the audio cutoff.
            // Circular buffer: O(1) write instead of O(N) shift.
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

            // `frac` positions the output sample within the current fractional
            // interval. The FIR bank reconstructs a band-limited sample using
            // a fixed two-sample lookahead in the decoder.
            let frac = ((1.0 - prev_phase) / self.output_phase_inc) as f32;
            let ring_pos = self.hist_pos; // oldest sample = next write position
            let sum_i = polyphase_resample_ring(&self.sum_hist, ring_pos, &self.resample_bank, frac);
            let diff_i = polyphase_resample_ring(&self.diff_hist, ring_pos, &self.resample_bank, frac);
            let diff_q = polyphase_resample_ring(&self.diff_q_hist, ring_pos, &self.resample_bank, frac);
            let blend_i =
                (self.prev_blend + frac * (stereo_blend_target - self.prev_blend)).clamp(0.0, 1.0);
            self.prev_blend = stereo_blend_target;
            let diff_i = (diff_i * trim_cos + diff_q * trim_sin) * STEREO_SEPARATION_GAIN;

            // --- Deemphasis + DC block + output ---
            if self.output_channels >= 2 && self.stereo_enabled {
                let diff = diff_i * blend_i;
                let left_corr = (sum_i + diff) * STEREO_MATRIX_GAIN;
                let right_corr = (sum_i - diff) * STEREO_MATRIX_GAIN;
                let left = self
                    .dc_l
                    .process(self.deemph_l.process(left_corr))
                    .clamp(-1.0, 1.0);
                let right = self
                    .dc_r
                    .process(self.deemph_r.process(right_corr))
                    .clamp(-1.0, 1.0);
                output.push(left);
                output.push(right);
            } else {
                // Mono path: apply the pilot notch here so the 19 kHz pilot tone
                // does not leak into mono audio.  Phase matching with diff is not
                // a concern for mono, so the notch can sit anywhere in the chain.
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
        self.detect_counter = 0;
        self.detect_pilot_mag_acc = 0.0;
        self.detect_pilot_abs_acc = 0.0;
        self.sum_hist = [0.0; WFM_RESAMP_TAPS];
        self.diff_hist = [0.0; WFM_RESAMP_TAPS];
        self.diff_q_hist = [0.0; WFM_RESAMP_TAPS];
        self.hist_pos = 0;
        self.prev_blend = 0.0;
        self.output_phase = 0.0;
    }

    pub fn stereo_detected(&self) -> bool {
        self.stereo_detected
    }
}

/// Selects the demodulation algorithm for a channel.
#[derive(Debug, Clone, PartialEq)]
pub enum Demodulator {
    /// Upper sideband SSB: take the real part of baseband IQ.
    Usb,
    /// Lower sideband SSB: negate imaginary part before taking real part.
    Lsb,
    /// AM envelope detector: magnitude of IQ, DC-removed.
    Am,
    /// Narrow-band FM: instantaneous frequency via quadrature (arg of s[n]*conj(s[n-1])).
    Fm,
    /// Wide-band FM: same algorithm as FM, wider pre-filter (handled upstream in DSP).
    Wfm,
    /// CW: magnitude of IQ after narrow BPF (BPF applied upstream), envelope.
    Cw,
    /// Pass-through (DIG, PKT): same as USB.
    Passthrough,
}

impl Demodulator {
    /// Construct the appropriate demodulator for a [`RigMode`].
    pub fn for_mode(mode: &RigMode) -> Self {
        match mode {
            RigMode::USB => Self::Usb,
            RigMode::LSB => Self::Lsb,
            RigMode::AM => Self::Am,
            RigMode::FM => Self::Fm,
            RigMode::WFM => Self::Wfm,
            RigMode::CW | RigMode::CWR => Self::Cw,
            RigMode::DIG => Self::Passthrough,
            // VHF/UHF packet radio (APRS, AX.25) is FM-encoded AFSK.
            // FM-demodulate the signal before passing audio to the APRS decoder.
            RigMode::PKT => Self::Fm,
            RigMode::Other(_) => Self::Usb,
        }
    }

    /// Demodulate a block of baseband IQ samples.
    ///
    /// `samples`: complex f32 IQ already centred at 0 Hz.
    /// Returns real f32 audio samples, same length as input.
    pub fn demodulate(&self, samples: &[Complex<f32>]) -> Vec<f32> {
        match self {
            Self::Usb | Self::Passthrough => demod_usb(samples),
            Self::Lsb => demod_lsb(samples),
            Self::Am => demod_am(samples),
            Self::Fm | Self::Wfm => demod_fm(samples),
            Self::Cw => demod_cw(samples),
        }
    }
}

// ---------------------------------------------------------------------------
// USB / Passthrough
// ---------------------------------------------------------------------------

/// USB demodulator: take the real part of each IQ sample.
fn demod_usb(samples: &[Complex<f32>]) -> Vec<f32> {
    samples.iter().map(|s| s.re).collect()
}

// ---------------------------------------------------------------------------
// LSB
// ---------------------------------------------------------------------------

/// LSB demodulator: LSB mixing is handled upstream by negating `channel_if_hz`;
/// the demodulator itself is identical to USB — just take `.re`.
fn demod_lsb(samples: &[Complex<f32>]) -> Vec<f32> {
    samples.iter().map(|s| s.re).collect()
}

// ---------------------------------------------------------------------------
// AM
// ---------------------------------------------------------------------------

/// AM envelope detector: magnitude of IQ.
///
/// Returns the raw envelope amplitude.  DC removal (carrier offset) and level
/// normalisation are handled downstream by the per-channel DC blocker and AGC.
fn demod_am(samples: &[Complex<f32>]) -> Vec<f32> {
    samples
        .iter()
        .map(|s| (s.re * s.re + s.im * s.im).sqrt())
        .collect()
}

// ---------------------------------------------------------------------------
// FM / WFM
// ---------------------------------------------------------------------------

/// FM quadrature discriminator: instantaneous frequency via arg(s[n] * conj(s[n-1])).
/// Output is in radians/sample, scaled by 1/π to normalise to [-1, 1].
fn demod_fm_with_prev(
    samples: &[Complex<f32>],
    prev: &mut Option<Complex<f32>>,
) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    let inv_pi = std::f32::consts::FRAC_1_PI;
    let mut output = Vec::with_capacity(samples.len());

    if let Some(prev_sample) = prev.as_ref().copied() {
        let product = samples[0] * prev_sample.conj();
        let angle = fast_atan2(product.im, product.re);
        output.push(angle * inv_pi);
    } else {
        output.push(0.0_f32);
    }

    let mut i = 1usize;

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if std::arch::is_x86_feature_detected!("avx2") {
        i = demod_fm_body_avx2(samples, i, inv_pi, &mut output);
    }

    for idx in i..samples.len() {
        let product = samples[idx] * samples[idx - 1].conj();
        let angle = fast_atan2(product.im, product.re);
        output.push(angle * inv_pi);
    }

    *prev = samples.last().copied();
    output
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn demod_fm_body_avx2(
    samples: &[Complex<f32>],
    start: usize,
    inv_pi: f32,
    output: &mut Vec<f32>,
) -> usize {
    unsafe { demod_fm_body_avx2_impl(samples, start, inv_pi, output) }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn demod_fm_body_avx2_impl(
    samples: &[Complex<f32>],
    start: usize,
    inv_pi: f32,
    output: &mut Vec<f32>,
) -> usize {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let len = samples.len();
    let mut i = start;
    let mut cur_re = [0.0_f32; 8];
    let mut cur_im = [0.0_f32; 8];
    let mut prev_re = [0.0_f32; 8];
    let mut prev_im = [0.0_f32; 8];
    let mut angles = [0.0_f32; 8];
    let inv_pi_v = _mm256_set1_ps(inv_pi);

    while i + 8 <= len {
        for lane in 0..8 {
            let cur = samples[i + lane];
            let prev = samples[i + lane - 1];
            cur_re[lane] = cur.re;
            cur_im[lane] = cur.im;
            prev_re[lane] = prev.re;
            prev_im[lane] = prev.im;
        }

        let cur_re_v = _mm256_loadu_ps(cur_re.as_ptr());
        let cur_im_v = _mm256_loadu_ps(cur_im.as_ptr());
        let prev_re_v = _mm256_loadu_ps(prev_re.as_ptr());
        let prev_im_v = _mm256_loadu_ps(prev_im.as_ptr());

        let re_v = _mm256_add_ps(
            _mm256_mul_ps(cur_re_v, prev_re_v),
            _mm256_mul_ps(cur_im_v, prev_im_v),
        );
        let im_v = _mm256_sub_ps(
            _mm256_mul_ps(cur_im_v, prev_re_v),
            _mm256_mul_ps(cur_re_v, prev_im_v),
        );

        let angle_v = _mm256_mul_ps(fast_atan2_8_avx2(im_v, re_v), inv_pi_v);
        _mm256_storeu_ps(angles.as_mut_ptr(), angle_v);
        output.extend_from_slice(&angles);

        i += 8;
    }

    i
}

/// FM quadrature discriminator: instantaneous frequency via arg(s[n] * conj(s[n-1])).
/// Output is in radians/sample, scaled by 1/π to normalise to [-1, 1].
fn demod_fm(samples: &[Complex<f32>]) -> Vec<f32> {
    let mut prev = None;
    demod_fm_with_prev(samples, &mut prev)
}

// ---------------------------------------------------------------------------
// CW
// ---------------------------------------------------------------------------

/// CW demodulator: take the real part of each baseband IQ sample.
///
/// The upstream FIR filter centres the CW carrier at the configured audio
/// offset (e.g. 700 Hz), so demodulating identically to USB produces the
/// characteristic CW side-tone.  Level normalisation is handled downstream
/// by the per-channel AGC.
fn demod_cw(samples: &[Complex<f32>]) -> Vec<f32> {
    samples.iter().map(|s| s.re).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use num_complex::Complex;

    fn complex_tone(freq_norm: f32, len: usize) -> Vec<Complex<f32>> {
        use std::f32::consts::TAU;
        (0..len)
            .map(|n| Complex::from_polar(1.0, TAU * freq_norm * n as f32))
            .collect()
    }

    fn assert_approx_eq(a: f32, b: f32, tol: f32, label: &str) {
        assert!(
            (a - b).abs() <= tol,
            "{}: expected {} ≈ {} (tol {})",
            label,
            a,
            b,
            tol
        );
    }

    // Test 1: USB and Passthrough return the real part of each sample.
    #[test]
    fn test_usb_passthrough_takes_real_part() {
        let input = vec![
            Complex::new(1.0_f32, 2.0),
            Complex::new(3.0, 4.0),
            Complex::new(-1.0, 0.0),
            Complex::new(0.0, -1.0),
        ];
        let expected = vec![1.0_f32, 3.0, -1.0, 0.0];

        let usb_out = Demodulator::Usb.demodulate(&input);
        assert_eq!(usb_out, expected, "USB should return real parts");

        let pass_out = Demodulator::Passthrough.demodulate(&input);
        assert_eq!(pass_out, expected, "Passthrough should return real parts");
    }

    // Test 2: LSB returns the real part (mixing handled upstream).
    #[test]
    fn test_lsb_takes_real_part() {
        let input = vec![
            Complex::new(1.0_f32, 2.0),
            Complex::new(3.0, 4.0),
            Complex::new(-1.0, 0.0),
            Complex::new(0.0, -1.0),
        ];
        let expected = vec![1.0_f32, 3.0, -1.0, 0.0];

        let lsb_out = Demodulator::Lsb.demodulate(&input);
        assert_eq!(lsb_out, expected, "LSB should return real parts");
    }

    // Test 3: AM on a constant-magnitude signal returns the raw envelope (1.0).
    // DC removal and normalization are handled downstream by the DC blocker and AGC.
    #[test]
    fn test_am_raw_magnitude_constant() {
        let input: Vec<Complex<f32>> = (0..8).map(|_| Complex::new(1.0, 0.0)).collect();
        let out = Demodulator::Am.demodulate(&input);
        assert_eq!(out.len(), 8);
        for (i, &v) in out.iter().enumerate() {
            assert_approx_eq(v, 1.0, 1e-6, &format!("AM raw magnitude sample {}", i));
        }
    }

    // Test 4: AM on alternating-magnitude signal returns the raw envelope.
    #[test]
    fn test_am_raw_magnitude_varying() {
        let input = vec![
            Complex::new(0.0_f32, 0.0),
            Complex::new(1.0, 0.0),
            Complex::new(0.0, 0.0),
            Complex::new(1.0, 0.0),
        ];
        let expected = [0.0_f32, 1.0, 0.0, 1.0];
        let out = Demodulator::Am.demodulate(&input);
        assert_eq!(out.len(), 4);
        for (i, (&got, &exp)) in out.iter().zip(expected.iter()).enumerate() {
            assert_approx_eq(got, exp, 1e-6, &format!("AM raw magnitude sample {}", i));
        }
    }

    // Test 5: FM discriminator on a pure tone at 0.25 cycles/sample.
    // arg(s[n]*conj(s[n-1])) = 2π*0.25 = π/2; scaled by 1/π → 0.5.
    #[test]
    fn test_fm_tone_frequency() {
        let input = complex_tone(0.25, 16);
        let out = Demodulator::Fm.demodulate(&input);
        assert_eq!(out.len(), 16);
        // First sample is 0.0 by convention.
        assert_approx_eq(out[0], 0.0, 1e-6, "FM tone sample 0 (zero by convention)");
        // Remaining samples should be approximately 0.5.
        for (i, &sample) in out.iter().enumerate().skip(1) {
            assert_approx_eq(sample, 0.5, 0.01, &format!("FM tone sample {}", i));
        }
    }

    // Test 6: FM discriminator on a DC (constant-phase) signal outputs all zeros.
    #[test]
    fn test_fm_silence_is_zero() {
        let input: Vec<Complex<f32>> = (0..8).map(|_| Complex::new(1.0, 0.0)).collect();
        let out = Demodulator::Fm.demodulate(&input);
        assert_eq!(out.len(), 8);
        for (i, &v) in out.iter().enumerate() {
            assert_approx_eq(v, 0.0, 1e-6, &format!("FM silence sample {}", i));
        }
    }

    // Test 7: CW demodulator returns the real part (same as USB).
    #[test]
    fn test_cw_takes_real_part() {
        let input = vec![
            Complex::new(3.0_f32, 4.0),
            Complex::new(0.0, 0.0),
            Complex::new(1.0, 0.0),
        ];
        let out = Demodulator::Cw.demodulate(&input);
        assert_eq!(out.len(), 3);
        assert_approx_eq(out[0], 3.0, 1e-6, "CW sample 0");
        assert_approx_eq(out[1], 0.0, 1e-6, "CW sample 1");
        assert_approx_eq(out[2], 1.0, 1e-6, "CW sample 2");
    }

    // Test 8: Demodulator::for_mode maps each RigMode to the correct variant.
    #[test]
    fn test_demodulator_for_mode_mapping() {
        assert_eq!(Demodulator::for_mode(&RigMode::USB), Demodulator::Usb);
        assert_eq!(Demodulator::for_mode(&RigMode::LSB), Demodulator::Lsb);
        assert_eq!(Demodulator::for_mode(&RigMode::AM), Demodulator::Am);
        assert_eq!(Demodulator::for_mode(&RigMode::FM), Demodulator::Fm);
        assert_eq!(Demodulator::for_mode(&RigMode::WFM), Demodulator::Wfm);
        assert_eq!(Demodulator::for_mode(&RigMode::CW), Demodulator::Cw);
        assert_eq!(Demodulator::for_mode(&RigMode::CWR), Demodulator::Cw);
        assert_eq!(
            Demodulator::for_mode(&RigMode::DIG),
            Demodulator::Passthrough
        );
        assert_eq!(Demodulator::for_mode(&RigMode::PKT), Demodulator::Fm);
    }

    // Test 9: Synthetic FM stereo — verify L/R separation.
    //
    // Generate a composite FM stereo signal with a 1 kHz tone on L only:
    //   L = sin(2π·1000·t),  R = 0
    //   sum  = L + R = sin(2π·1000·t)
    //   diff = L - R = sin(2π·1000·t)
    //   composite = sum + 0.1·cos(2π·19000·t) + diff·cos(2π·38000·t)
    //
    // FM-modulate at 240 kHz composite rate, run through WfmStereoDecoder,
    // and verify L has significant energy while R is near zero.
    #[test]
    fn test_wfm_stereo_separation() {
        use std::f32::consts::TAU;

        let composite_rate: u32 = 240_000;
        let audio_rate: u32 = 48_000;
        let fs = composite_rate as f32;
        let duration_secs = 0.5_f32; // 500 ms — enough for PLL lock + measurement
        let num_samples = (fs * duration_secs) as usize;

        // --- Build composite baseband ---
        let audio_freq = 1000.0_f32;
        let pilot_freq = 19_000.0_f32;
        let carrier_freq = 38_000.0_f32;
        let mut composite = vec![0.0_f32; num_samples];
        for n in 0..num_samples {
            let t = n as f32 / fs;
            let audio = (TAU * audio_freq * t).sin(); // L = audio, R = 0
            let sum = audio;       // L + R
            let diff = audio;      // L - R
            let pilot = 0.1 * (TAU * pilot_freq * t).cos();
            let carrier = (TAU * carrier_freq * t).cos();
            composite[n] = sum + pilot + diff * carrier;
        }

        // --- FM-modulate composite → IQ samples ---
        // deviation: peak composite ≈ ±2.1, map to ±75 kHz
        // phase per sample = 2π · (75000 / peak_composite) · composite[n] / fs
        let peak_composite = 2.1_f32; // sum(1) + pilot(0.1) + diff(1)
        let deviation_hz = 75_000.0_f32;
        let mod_index = TAU * deviation_hz / (peak_composite * fs);
        let mut phase: f32 = 0.0;
        let mut iq = Vec::with_capacity(num_samples);
        for &c in &composite {
            phase += mod_index * c;
            iq.push(Complex::from_polar(1.0, phase));
        }

        // --- Decode ---
        let mut decoder = WfmStereoDecoder::new(
            composite_rate,
            audio_rate,
            2,    // stereo output
            true, // stereo enabled
            50,   // 50 µs deemphasis
        );
        let output = decoder.process_iq(&iq);

        // Output is interleaved L, R.  Skip the first 200 ms for PLL lock.
        let skip_samples = (0.2 * audio_rate as f32) as usize;
        let stereo_pairs = output.len() / 2;
        assert!(
            stereo_pairs > skip_samples + 100,
            "not enough output samples: {} pairs, need > {}",
            stereo_pairs,
            skip_samples + 100
        );

        // Measure RMS of L and R channels in the measurement window.
        let mut l_energy = 0.0_f64;
        let mut r_energy = 0.0_f64;
        let mut count = 0_u64;
        for i in skip_samples..stereo_pairs {
            let l = output[2 * i] as f64;
            let r = output[2 * i + 1] as f64;
            l_energy += l * l;
            r_energy += r * r;
            count += 1;
        }
        let l_rms = (l_energy / count as f64).sqrt();
        let r_rms = (r_energy / count as f64).sqrt();

        eprintln!("L RMS = {l_rms:.6}, R RMS = {r_rms:.6}");

        // L should have significant energy (tone is present).
        assert!(
            l_rms > 0.01,
            "L channel has no energy: L_rms = {l_rms:.6}"
        );

        // R should be much smaller than L (>20 dB separation).
        let separation_db = if r_rms > 1e-10 {
            20.0 * (l_rms / r_rms).log10()
        } else {
            f64::INFINITY
        };
        eprintln!("stereo separation = {separation_db:.1} dB");

        assert!(
            separation_db > 20.0,
            "stereo separation too low: {separation_db:.1} dB (L_rms={l_rms:.6}, R_rms={r_rms:.6})"
        );
    }

    /// Multi-tone stereo separation test.
    ///
    /// Generates a composite FM stereo signal with tones at 400, 2000, 8000
    /// and 14000 Hz on a single channel (L-only then R-only), demodulates,
    /// and verifies that the silent channel stays quiet across the full
    /// audio band.  This catches group-delay and phase-trim problems that
    /// a single 1 kHz tone would miss.
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

        // Test both L-only (diff = +audio) and R-only (diff = -audio).
        for (label, diff_sign) in [("L-only", 1.0_f32), ("R-only", -1.0_f32)] {
            let mut composite = vec![0.0_f32; num_samples];
            for n in 0..num_samples {
                let t = n as f32 / fs;
                let audio: f32 = freqs.iter().map(|&f| (TAU * f * t).sin()).sum::<f32>()
                    / freqs.len() as f32;
                let sum = audio;                     // L + R (same for both cases)
                let diff = audio * diff_sign;        // L - R
                let pilot = 0.1 * (TAU * pilot_freq * t).cos();
                let carrier = (TAU * carrier_freq * t).cos();
                composite[n] = sum + pilot + diff * carrier;
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
            );
            let output = decoder.process_iq(&iq);

            let skip_samples = (0.3 * audio_rate as f32) as usize;
            let stereo_pairs = output.len() / 2;
            assert!(stereo_pairs > skip_samples + 100,
                "{label}: not enough output samples");

            let mut active_energy = 0.0_f64;
            let mut silent_energy = 0.0_f64;
            let mut count = 0_u64;
            for i in skip_samples..stereo_pairs {
                let l = output[2 * i] as f64;
                let r = output[2 * i + 1] as f64;
                if diff_sign > 0.0 {
                    // L-only: L is active, R is silent
                    active_energy += l * l;
                    silent_energy += r * r;
                } else {
                    // R-only: R is active, L is silent
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

            eprintln!("{label}: active RMS = {active_rms:.6}, silent RMS = {silent_rms:.6}, separation = {separation_db:.1} dB");

            assert!(active_rms > 0.01,
                "{label}: active channel has no energy: {active_rms:.6}");
            assert!(separation_db > 15.0,
                "{label}: multitone stereo separation too low: {separation_db:.1} dB");
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
        );
        let output = decoder.process_iq(&iq);

        assert!(
            !decoder.stereo_detected(),
            "decoder should not detect stereo without a 19 kHz pilot"
        );

        let skip_samples = (0.2 * audio_rate as f32) as usize;
        let stereo_pairs = output.len() / 2;
        assert!(stereo_pairs > skip_samples + 100);

        let mut diff_energy = 0.0_f64;
        let mut count = 0_u64;
        for i in skip_samples..stereo_pairs {
            let l = output[2 * i] as f64;
            let r = output[2 * i + 1] as f64;
            let d = l - r;
            diff_energy += d * d;
            count += 1;
        }
        let diff_rms = (diff_energy / count as f64).sqrt();
        assert!(
            diff_rms < 0.01,
            "mono signal without pilot should not develop audible stereo difference: diff_rms={diff_rms:.6}"
        );
    }

    // Test 10: All demodulators return an empty Vec for empty input without panicking.
    #[test]
    fn test_empty_input() {
        let empty: Vec<Complex<f32>> = Vec::new();
        let demodulators = [
            Demodulator::Usb,
            Demodulator::Lsb,
            Demodulator::Am,
            Demodulator::Fm,
            Demodulator::Wfm,
            Demodulator::Cw,
            Demodulator::Passthrough,
        ];
        for demod in &demodulators {
            let out = demod.demodulate(&empty);
            assert!(
                out.is_empty(),
                "{:?} should return empty Vec for empty input",
                demod
            );
        }
    }
}
