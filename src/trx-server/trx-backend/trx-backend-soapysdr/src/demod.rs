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
const AUDIO_BW_HZ: f32 = 15_000.0;
/// Q values for a proper 4th-order Butterworth cascade (two 2nd-order stages).
/// Stage 1: Q = 1 / (2 cos(π/8))
const BW4_Q1: f32 = 0.5412;
/// Stage 2: Q = 1 / (2 cos(3π/8))
const BW4_Q2: f32 = 1.3066;
/// Q for the 19 kHz pilot notch (~3.8 kHz 3 dB bandwidth).
const PILOT_NOTCH_Q: f32 = 5.0;

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

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        // Update envelope tracker (peak-hold with attack/release).
        let abs_x = x.abs();
        let env_coeff = if abs_x > self.envelope {
            self.attack_coeff
        } else {
            self.release_coeff
        };
        self.envelope += env_coeff * (abs_x - self.envelope);

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

        (x * self.gain).clamp(-1.0, 1.0)
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
}

#[derive(Debug, Clone)]
pub struct WfmStereoDecoder {
    output_channels: usize,
    rds_decoder: RdsDecoder,
    rds_bpf: BiquadBandPass,
    rds_dc: DcBlocker,
    pilot_phase: f32,
    pilot_freq: f32,
    pilot_freq_err: f32,
    pilot_i_lp: OnePoleLowPass,
    pilot_q_lp: OnePoleLowPass,
    /// 4th-order Butterworth cascade for L+R (two 2nd-order stages, Q = BW4_Q1/BW4_Q2).
    sum_lpf1: BiquadLowPass,
    sum_lpf2: BiquadLowPass,
    /// Notch at 19 kHz to suppress pilot tone leakage in the L+R channel.
    sum_notch: BiquadNotch,
    /// 4th-order Butterworth cascade for L-R (matched to sum path for stereo phase accuracy).
    diff_lpf1: BiquadLowPass,
    diff_lpf2: BiquadLowPass,
    /// DC blockers on audio outputs — remove carrier-offset DC from the FM discriminator.
    dc_m: DcBlocker,
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    deemph_m: Deemphasis,
    deemph_l: Deemphasis,
    deemph_r: Deemphasis,
    /// Previous filtered sum/diff composite samples used for linear interpolation.
    prev_sum: f32,
    prev_diff: f32,
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
        deemphasis_us: u32,
    ) -> Self {
        let composite_rate_f = composite_rate.max(1) as f32;
        let output_phase_inc = audio_rate.max(1) as f64 / composite_rate.max(1) as f64;
        let deemphasis_us = deemphasis_us as f32;
        Self {
            output_channels: output_channels.max(1),
            rds_decoder: RdsDecoder::new(composite_rate),
            rds_bpf: BiquadBandPass::new(composite_rate_f, RDS_SUBCARRIER_HZ, RDS_BPF_Q),
            rds_dc: DcBlocker::new(0.995),
            pilot_phase: 0.0,
            pilot_freq: 2.0 * std::f32::consts::PI * PILOT_HZ / composite_rate_f,
            pilot_freq_err: 0.0,
            pilot_i_lp: OnePoleLowPass::new(composite_rate_f, 400.0),
            pilot_q_lp: OnePoleLowPass::new(composite_rate_f, 400.0),
            // 4th-order Butterworth: two cascaded biquads with BW4_Q1/BW4_Q2.
            // At 19 kHz (pilot): ≈ −12 dB; at 38 kHz (DSB carrier): ≈ −32 dB.
            sum_lpf1: BiquadLowPass::new(composite_rate_f, AUDIO_BW_HZ, BW4_Q1),
            sum_lpf2: BiquadLowPass::new(composite_rate_f, AUDIO_BW_HZ, BW4_Q2),
            sum_notch: BiquadNotch::new(composite_rate_f, PILOT_HZ, PILOT_NOTCH_Q),
            diff_lpf1: BiquadLowPass::new(composite_rate_f, AUDIO_BW_HZ, BW4_Q1),
            diff_lpf2: BiquadLowPass::new(composite_rate_f, AUDIO_BW_HZ, BW4_Q2),
            dc_m: DcBlocker::new(0.9999),
            dc_l: DcBlocker::new(0.9999),
            dc_r: DcBlocker::new(0.9999),
            deemph_m: Deemphasis::new(audio_rate.max(1) as f32, deemphasis_us),
            deemph_l: Deemphasis::new(audio_rate.max(1) as f32, deemphasis_us),
            deemph_r: Deemphasis::new(audio_rate.max(1) as f32, deemphasis_us),
            prev_sum: 0.0,
            prev_diff: 0.0,
            output_phase_inc,
            output_phase: 0.0,
        }
    }

    pub fn process_iq(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        let composite = demod_fm(samples);
        if composite.is_empty() {
            return Vec::new();
        }

        let mut output = Vec::with_capacity(
            ((composite.len() as f64 * self.output_phase_inc).ceil() as usize + 1)
                * self.output_channels.max(1),
        );

        for x in composite {
            // --- Pilot PLL ---
            let (sin_p, cos_p) = self.pilot_phase.sin_cos();
            let i = self.pilot_i_lp.process(x * cos_p);
            let q = self.pilot_q_lp.process(x * -sin_p);
            let phase_err = q.atan2(i);
            self.pilot_freq_err = (self.pilot_freq_err + phase_err * 0.00002).clamp(-0.02, 0.02);
            self.pilot_phase += self.pilot_freq + self.pilot_freq_err + phase_err * 0.0015;
            self.pilot_phase = self.pilot_phase.rem_euclid(std::f32::consts::TAU);

            let pilot_mag = (i * i + q * q).sqrt();
            let stereo_blend = (pilot_mag * 40.0).clamp(0.0, 1.0);

            // --- RDS ---
            let rds_quality = (0.35 + pilot_mag * 20.0).clamp(0.35, 1.0);
            let rds_band = self.rds_bpf.process(x);
            let rds_clean = self.rds_dc.process(rds_band);
            let _ = self.rds_decoder.process_sample(rds_clean, rds_quality);

            // --- L+R (sum): 4th-order Butterworth + pilot notch ---
            let sum = self.sum_notch.process(self.sum_lpf2.process(self.sum_lpf1.process(x)));

            // --- L-R (diff): 38 kHz demod + 4th-order Butterworth ---
            let stereo_carrier = (2.0 * self.pilot_phase).cos() * 2.0;
            let diff = self.diff_lpf2.process(self.diff_lpf1.process(x * stereo_carrier))
                * stereo_blend;

            // --- Linear interpolation resampling ---
            // Track previous filtered values every composite sample for interpolation.
            let prev_sum = self.prev_sum;
            let prev_diff = self.prev_diff;
            self.prev_sum = sum;
            self.prev_diff = diff;

            let prev_phase = self.output_phase;
            self.output_phase += self.output_phase_inc;
            if self.output_phase < 1.0 {
                continue;
            }
            self.output_phase -= 1.0;

            // Interpolate: `frac` is the fractional position of the output sample
            // between the previous composite sample (frac≈0) and the current one (frac≈1).
            let frac = ((1.0 - prev_phase) / self.output_phase_inc) as f32;
            let sum_i = prev_sum + frac * (sum - prev_sum);
            let diff_i = prev_diff + frac * (diff - prev_diff);

            // --- Deemphasis + DC block + output ---
            if self.output_channels >= 2 {
                let left = self.dc_l
                    .process(self.deemph_l.process((sum_i + diff_i) * 0.5))
                    .clamp(-1.0, 1.0);
                let right = self.dc_r
                    .process(self.deemph_r.process((sum_i - diff_i) * 0.5))
                    .clamp(-1.0, 1.0);
                output.push(left);
                output.push(right);
            } else {
                output.push(
                    self.dc_m
                        .process(self.deemph_m.process(sum_i))
                        .clamp(-1.0, 1.0),
                );
            }
        }

        output
    }

    pub fn rds_data(&self) -> Option<RdsData> {
        self.rds_decoder.snapshot()
    }

    pub fn reset_rds(&mut self) {
        self.rds_decoder.reset();
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
fn demod_fm(samples: &[Complex<f32>]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    let inv_pi = std::f32::consts::FRAC_1_PI;
    let mut output = Vec::with_capacity(samples.len());
    output.push(0.0_f32);

    for i in 1..samples.len() {
        let product = samples[i] * samples[i - 1].conj();
        let angle = product.im.atan2(product.re);
        output.push(angle * inv_pi);
    }

    output
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

    // Test 9: All demodulators return an empty Vec for empty input without panicking.
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
