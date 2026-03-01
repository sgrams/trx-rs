// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;

use super::math::demod_fm_with_prev;

/// FM quadrature discriminator: instantaneous frequency via `arg(s[n] * conj(s[n-1]))`.
pub(super) fn demod_fm(samples: &[Complex<f32>]) -> Vec<f32> {
    let mut prev = None;
    demod_fm_with_prev(samples, &mut prev)
}

#[cfg(test)]
mod tests {
    use super::demod_fm;
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

    #[test]
    fn test_fm_tone_frequency() {
        let input = complex_tone(0.25, 16);
        let out = demod_fm(&input);
        assert_eq!(out.len(), 16);
        assert_approx_eq(out[0], 0.0, 1e-6, "FM tone sample 0");
        for (idx, &sample) in out.iter().enumerate().skip(1) {
            assert_approx_eq(sample, 0.5, 0.01, &format!("FM tone sample {idx}"));
        }
    }

    #[test]
    fn test_fm_silence_is_zero() {
        let input: Vec<Complex<f32>> = (0..8).map(|_| Complex::new(1.0, 0.0)).collect();
        let out = demod_fm(&input);
        assert_eq!(out.len(), 8);
        for (idx, &value) in out.iter().enumerate() {
            assert_approx_eq(value, 0.0, 1e-6, &format!("FM silence sample {idx}"));
        }
    }
}
