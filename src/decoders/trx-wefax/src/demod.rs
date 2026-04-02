// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! FM discriminator for WEFAX demodulation.
//!
//! Computes instantaneous frequency from the analytic signal produced by a
//! Hilbert transform FIR, then maps the frequency to a 0.0–1.0 luminance
//! value (1500 Hz = black, 2300 Hz = white).

use std::f32::consts::PI;

/// Number of taps for the Hilbert transform FIR.
const HILBERT_TAPS: usize = 65;

/// Half the Hilbert FIR length (group delay in samples).
const HILBERT_DELAY: usize = HILBERT_TAPS / 2;

/// FM discriminator producing luminance values from audio samples.
pub struct FmDiscriminator {
    sample_rate: f32,
    /// Hilbert FIR coefficients (odd-length, anti-symmetric).
    hilbert_coeffs: Vec<f32>,
    /// Input sample delay line for FIR convolution.
    delay_line: Vec<f32>,
    /// Write position in delay line (circular buffer).
    write_pos: usize,
    /// Previous analytic signal sample for frequency differentiation.
    prev_i: f32,
    prev_q: f32,
    /// Centre frequency for luminance mapping.
    center_hz: f32,
    /// Deviation for luminance mapping.
    deviation_hz: f32,
}

impl FmDiscriminator {
    pub fn new(sample_rate: u32, center_hz: f32, deviation_hz: f32) -> Self {
        let coeffs = design_hilbert_fir(HILBERT_TAPS);
        Self {
            sample_rate: sample_rate as f32,
            hilbert_coeffs: coeffs,
            delay_line: vec![0.0; HILBERT_TAPS],
            write_pos: 0,
            prev_i: 0.0,
            prev_q: 0.0,
            center_hz,
            deviation_hz,
        }
    }

    /// Process a block of real-valued audio samples, returning luminance
    /// values in the range 0.0 (black / 1500 Hz) to 1.0 (white / 2300 Hz).
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(samples.len());
        let n = HILBERT_TAPS;
        let half = HILBERT_DELAY;
        let inv_2pi_ts = self.sample_rate / (2.0 * PI);
        let black_hz = self.center_hz - self.deviation_hz; // 1500
        let range_hz = 2.0 * self.deviation_hz; // 800

        for &sample in samples {
            // Write into circular delay line.
            self.delay_line[self.write_pos] = sample;
            self.write_pos = (self.write_pos + 1) % n;

            // Compute Hilbert-transformed (quadrature) output via FIR.
            let mut q = 0.0f32;
            for k in 0..n {
                let idx = (self.write_pos + k) % n;
                q += self.hilbert_coeffs[k] * self.delay_line[idx];
            }

            // The in-phase component is the delayed input (centre tap of the
            // Hilbert FIR corresponds to the group delay).
            let i = self.delay_line[(self.write_pos + half) % n];

            // Instantaneous frequency via phase differentiation:
            // f = arg(z[n] · conj(z[n-1])) / (2π·Ts)
            // z[n] · conj(z[n-1]) = (i + jq)(prev_i - j·prev_q)
            let di = i * self.prev_i + q * self.prev_q;
            let dq = q * self.prev_i - i * self.prev_q;
            let phase_diff = dq.atan2(di);
            let freq = phase_diff.abs() * inv_2pi_ts;

            // Map frequency to luminance.
            let lum = ((freq - black_hz) / range_hz).clamp(0.0, 1.0);
            output.push(lum);

            self.prev_i = i;
            self.prev_q = q;
        }

        output
    }

    pub fn reset(&mut self) {
        self.delay_line.fill(0.0);
        self.write_pos = 0;
        self.prev_i = 0.0;
        self.prev_q = 0.0;
    }
}

/// Design a Hilbert transform FIR filter (odd-length, type III).
///
/// The impulse response is: h[n] = 2/(πn) for odd n (relative to centre),
/// 0 for even n, windowed with a Blackman window.
#[allow(clippy::needless_range_loop)]
fn design_hilbert_fir(num_taps: usize) -> Vec<f32> {
    assert!(num_taps % 2 == 1, "Hilbert FIR must have odd length");
    let mut coeffs = vec![0.0f32; num_taps];
    let mid = (num_taps - 1) as f64 / 2.0;

    for i in 0..num_taps {
        let n = i as f64 - mid;
        let ni = n.round() as i64;
        if ni == 0 {
            coeffs[i] = 0.0;
        } else if ni % 2 != 0 {
            // Hilbert kernel: 2/(π·n) for odd offsets.
            let h = 2.0 / (std::f64::consts::PI * n);
            // Blackman window.
            let w = 0.42
                - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (num_taps - 1) as f64).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / (num_taps - 1) as f64).cos();
            coeffs[i] = (h * w) as f32;
        }
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
