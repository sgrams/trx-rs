// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! FM discriminator for WEFAX demodulation.
//!
//! Computes instantaneous frequency from the analytic signal produced by a
//! Hilbert transform FIR, then maps the frequency to a 0.0–1.0 luminance
//! value (1500 Hz = black, 2300 Hz = white).
//!
//! Uses block-based linear processing for auto-vectorisation of the FIR
//! convolution, consistent with `docs/Optimization-Guidelines.md`.

use std::f32::consts::PI;

/// Number of taps for the Hilbert transform FIR.
const HILBERT_TAPS: usize = 65;

/// Half the Hilbert FIR length (group delay in samples).
const HILBERT_DELAY: usize = HILBERT_TAPS / 2;

/// FM discriminator producing luminance values from audio samples.
pub struct FmDiscriminator {
    /// Hilbert FIR coefficients (odd-length, anti-symmetric).
    hilbert_coeffs: [f32; HILBERT_TAPS],
    /// Tail buffer: last `HILBERT_TAPS - 1` input samples from the previous
    /// block (used to prime the next convolution without modular indexing).
    tail: Vec<f32>,
    /// Previous analytic signal sample for frequency differentiation.
    prev_i: f32,
    prev_q: f32,
    /// Pre-computed constants.
    inv_2pi_ts: f32,
    black_hz: f32,
    inv_range_hz: f32,
}

impl FmDiscriminator {
    pub fn new(sample_rate: u32, center_hz: f32, deviation_hz: f32) -> Self {
        let coeffs = design_hilbert_fir();
        let sr = sample_rate as f32;
        Self {
            hilbert_coeffs: coeffs,
            tail: vec![0.0; HILBERT_TAPS - 1],
            prev_i: 0.0,
            prev_q: 0.0,
            inv_2pi_ts: sr / (2.0 * PI),
            black_hz: center_hz - deviation_hz,
            inv_range_hz: 1.0 / (2.0 * deviation_hz),
        }
    }

    /// Process a block of real-valued audio samples, returning luminance
    /// values in the range 0.0 (black / 1500 Hz) to 1.0 (white / 2300 Hz).
    ///
    /// The Hilbert FIR is evaluated on a contiguous linear buffer
    /// (`[tail | samples]`) so the inner loop uses straight indexing—no
    /// modular arithmetic—and the compiler can auto-vectorise.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let n = HILBERT_TAPS;
        let half = HILBERT_DELAY;
        let tail_len = n - 1;

        // Build contiguous work buffer: [tail from previous block | new samples].
        let work_len = tail_len + samples.len();
        let mut work = Vec::with_capacity(work_len);
        work.extend_from_slice(&self.tail);
        work.extend_from_slice(samples);

        let mut output = Vec::with_capacity(samples.len());
        let coeffs = &self.hilbert_coeffs;

        for i in 0..samples.len() {
            // Linear FIR convolution — window is work[i..i+n].
            let window = &work[i..i + n];
            let mut q = 0.0f32;
            for k in 0..n {
                q += coeffs[k] * window[n - 1 - k];
            }

            // In-phase component is the delayed input (group delay = half).
            let i_val = work[i + half];

            // Instantaneous frequency via phase differentiation:
            // f = |arg(z[n] · conj(z[n-1]))| / (2π·Ts)
            let di = i_val * self.prev_i + q * self.prev_q;
            let dq = q * self.prev_i - i_val * self.prev_q;
            let freq = dq.atan2(di).abs() * self.inv_2pi_ts;

            // Map frequency to luminance.
            let lum = ((freq - self.black_hz) * self.inv_range_hz).clamp(0.0, 1.0);
            output.push(lum);

            self.prev_i = i_val;
            self.prev_q = q;
        }

        // Save tail for next call.
        self.tail.copy_from_slice(&work[work_len - tail_len..]);

        output
    }

    pub fn reset(&mut self) {
        self.tail.fill(0.0);
        self.prev_i = 0.0;
        self.prev_q = 0.0;
    }
}

/// Design a Hilbert transform FIR filter (odd-length, type III).
///
/// The impulse response is: h[n] = 2/(πn) for odd n (relative to centre),
/// 0 for even n, windowed with a Blackman window.
fn design_hilbert_fir() -> [f32; HILBERT_TAPS] {
    let num_taps = HILBERT_TAPS;
    let mut coeffs = [0.0f32; HILBERT_TAPS];
    let m = (num_taps - 1) as f64;
    let mid = m / 2.0;

    let mut i = 0;
    while i < num_taps {
        let n = i as f64 - mid;
        let ni = n.round() as i64;
        if ni != 0 && ni % 2 != 0 {
            // Hilbert kernel: 2/(π·n) for odd offsets.
            let h = 2.0 / (std::f64::consts::PI * n);
            // Blackman window.
            let w = 0.42
                - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / m).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / m).cos();
            coeffs[i] = (h * w) as f32;
        }
        i += 1;
    }

    coeffs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminator_white_tone() {
        // Feed a pure 2300 Hz tone, expect luminance ≈ 1.0.
        let sr = 11025;
        let mut disc = FmDiscriminator::new(sr, 1900.0, 400.0);
        let n = 2000;
        let tone: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 2300.0 * i as f32 / sr as f32).sin())
            .collect();
        let lum = disc.process(&tone);
        // Skip initial transient (Hilbert FIR settling).
        let tail = &lum[lum.len() / 2..];
        let avg: f32 = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(
            (avg - 1.0).abs() < 0.05,
            "expected ~1.0 for white tone, got {}",
            avg
        );
    }

    #[test]
    fn discriminator_black_tone() {
        // Feed a pure 1500 Hz tone, expect luminance ≈ 0.0.
        let sr = 11025;
        let mut disc = FmDiscriminator::new(sr, 1900.0, 400.0);
        let n = 2000;
        let tone: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 1500.0 * i as f32 / sr as f32).sin())
            .collect();
        let lum = disc.process(&tone);
        let tail = &lum[lum.len() / 2..];
        let avg: f32 = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(
            avg < 0.05,
            "expected ~0.0 for black tone, got {}",
            avg
        );
    }

    #[test]
    fn discriminator_center_tone() {
        // Feed 1900 Hz (center), expect luminance ≈ 0.5.
        let sr = 11025;
        let mut disc = FmDiscriminator::new(sr, 1900.0, 400.0);
        let n = 2000;
        let tone: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 1900.0 * i as f32 / sr as f32).sin())
            .collect();
        let lum = disc.process(&tone);
        let tail = &lum[lum.len() / 2..];
        let avg: f32 = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(
            (avg - 0.5).abs() < 0.05,
            "expected ~0.5 for center tone, got {}",
            avg
        );
    }
}
