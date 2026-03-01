// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;

/// AM envelope detector: magnitude of IQ.
pub(super) fn demod_am(samples: &[Complex<f32>]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| (sample.re * sample.re + sample.im * sample.im).sqrt())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::demod_am;
    use num_complex::Complex;

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
    fn test_am_raw_magnitude_constant() {
        let input: Vec<Complex<f32>> = (0..8).map(|_| Complex::new(1.0, 0.0)).collect();
        let out = demod_am(&input);
        assert_eq!(out.len(), 8);
        for (idx, &value) in out.iter().enumerate() {
            assert_approx_eq(value, 1.0, 1e-6, &format!("AM sample {idx}"));
        }
    }

    #[test]
    fn test_am_raw_magnitude_varying() {
        let input = vec![
            Complex::new(0.0_f32, 0.0),
            Complex::new(1.0, 0.0),
            Complex::new(0.0, 0.0),
            Complex::new(1.0, 0.0),
        ];
        let expected = [0.0_f32, 1.0, 0.0, 1.0];
        let out = demod_am(&input);
        assert_eq!(out.len(), 4);
        for (idx, (&got, &exp)) in out.iter().zip(expected.iter()).enumerate() {
            assert_approx_eq(got, exp, 1e-6, &format!("AM sample {idx}"));
        }
    }
}
