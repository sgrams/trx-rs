// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;
use super::DcBlocker;

/// C-QUAM (Compatible Quadrature AM) stereo demodulator.
///
/// Tracks the AM carrier phase using a first-order IIR filter on the baseband
/// DC component (τ ≈ 50 ms), rotates each sample to align I with the sum
/// audio and Q with the difference audio, then reconstructs L/R stereo.
///
/// Input: AGC-normalised baseband IQ samples at audio sample rate.
/// Output: interleaved stereo PCM [L0, R0, L1, R1, …]
pub struct CquamDemod {
    /// IIR-tracked in-phase carrier estimate.
    carrier_re: f32,
    /// IIR-tracked quadrature carrier estimate.
    carrier_im: f32,
    /// IIR smoothing coefficient (close to 1 → slow tracking).
    alpha: f32,
    /// DC blocker for left channel output (removes the carrier-level DC).
    dc_l: DcBlocker,
    /// DC blocker for right channel output.
    dc_r: DcBlocker,
}

impl CquamDemod {
    /// Create a new C-QUAM demodulator for the given audio sample rate.
    pub fn new(audio_sample_rate: u32) -> Self {
        let sr = audio_sample_rate.max(1) as f32;
        // 50 ms tracking time constant — slow enough not to follow audio
        // modulation (lowest speech fundamental ~100 Hz → period 10 ms),
        // fast enough to follow SDR frequency offset drift.
        let alpha = (-1.0f32 / (0.05 * sr)).exp();
        Self {
            carrier_re: 1.0,
            carrier_im: 0.0,
            alpha,
            dc_l: DcBlocker::new(0.999),
            dc_r: DcBlocker::new(0.999),
        }
    }

    /// Demodulate a block of AGC-normalised baseband IQ samples into
    /// interleaved stereo audio.
    pub fn demodulate_stereo(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        let mut out = Vec::with_capacity(samples.len() * 2);
        let alpha = self.alpha;
        let one_minus_alpha = 1.0 - alpha;

        for &s in samples {
            // Advance the carrier IIR tracker.  In steady state the DC
            // component of s is the carrier phasor e^{jφ}.
            self.carrier_re = alpha * self.carrier_re + one_minus_alpha * s.re;
            self.carrier_im = alpha * self.carrier_im + one_minus_alpha * s.im;

            // Rotate s by −φ to phase-align I with (1 + m_s) and Q with m_d.
            let mag_sq =
                self.carrier_re * self.carrier_re + self.carrier_im * self.carrier_im;
            let (i_corr, q_corr) = if mag_sq > 1e-8 {
                let inv = mag_sq.sqrt().recip();
                let cos_phi = self.carrier_re * inv;
                let sin_phi = self.carrier_im * inv;
                // s · e^{-jφ}
                (
                    s.re * cos_phi + s.im * sin_phi,
                    -s.re * sin_phi + s.im * cos_phi,
                )
            } else {
                (s.re, s.im)
            };

            // Stereo decode.
            // I ≈ 1 + (L+R)/2,  Q ≈ (L−R)/2
            // L_raw = I + Q = 1 + L  →  DC-block → L audio
            // R_raw = I − Q = 1 + R  →  DC-block → R audio
            out.push(self.dc_l.process(i_corr + q_corr));
            out.push(self.dc_r.process(i_corr - q_corr));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cquam_silence_is_silent() {
        let mut demod = CquamDemod::new(8_000);
        let samples = vec![Complex::new(0.0f32, 0.0); 256];
        let out = demod.demodulate_stereo(&samples);
        assert_eq!(out.len(), 512);
        for &s in &out {
            assert!(s.abs() < 1e-5, "silence should produce near-zero output, got {s}");
        }
    }

    #[test]
    fn test_cquam_pure_am_mono() {
        // A pure AM carrier (no Q modulation) should produce equal L and R.
        let mut demod = CquamDemod::new(8_000);
        // Let the carrier tracker settle for 1 s worth of samples.
        let settle: Vec<Complex<f32>> = (0..8_000)
            .map(|i| {
                let t = i as f32 / 8_000.0;
                let audio = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
                Complex::new(1.0 + audio, 0.0)
            })
            .collect();
        let out = demod.demodulate_stereo(&settle);
        // After settling, L and R should be roughly equal (within 0.02 amplitude).
        for chunk in out.chunks_exact(2).skip(4_000) {
            let l = chunk[0];
            let r = chunk[1];
            assert!(
                (l - r).abs() < 0.02,
                "pure AM mono should have L ≈ R, got L={l:.4} R={r:.4}"
            );
        }
    }
}
