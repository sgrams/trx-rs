// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;

/// USB demodulator: take the real part of each IQ sample.
pub(super) fn demod_usb(samples: &[Complex<f32>]) -> Vec<f32> {
    samples.iter().map(|sample| sample.re).collect()
}

/// LSB demodulator: mixing is handled upstream by negating `channel_if_hz`.
pub(super) fn demod_lsb(samples: &[Complex<f32>]) -> Vec<f32> {
    samples.iter().map(|sample| sample.re).collect()
}

/// CW demodulator: take the real part of each baseband IQ sample.
pub(super) fn demod_cw(samples: &[Complex<f32>]) -> Vec<f32> {
    samples.iter().map(|sample| sample.re).collect()
}

#[cfg(test)]
mod tests {
    use super::{demod_cw, demod_lsb, demod_usb};
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
    fn test_usb_passthrough_takes_real_part() {
        let input = vec![
            Complex::new(1.0_f32, 2.0),
            Complex::new(3.0, 4.0),
            Complex::new(-1.0, 0.0),
            Complex::new(0.0, -1.0),
        ];
        let expected = vec![1.0_f32, 3.0, -1.0, 0.0];

        assert_eq!(demod_usb(&input), expected);
        assert_eq!(demod_usb(&input), expected);
    }

    #[test]
    fn test_lsb_takes_real_part() {
        let input = vec![
            Complex::new(1.0_f32, 2.0),
            Complex::new(3.0, 4.0),
            Complex::new(-1.0, 0.0),
            Complex::new(0.0, -1.0),
        ];
        let expected = vec![1.0_f32, 3.0, -1.0, 0.0];

        assert_eq!(demod_lsb(&input), expected);
    }

    #[test]
    fn test_cw_takes_real_part() {
        let input = vec![
            Complex::new(3.0_f32, 4.0),
            Complex::new(0.0, 0.0),
            Complex::new(1.0, 0.0),
        ];
        let out = demod_cw(&input);
        assert_eq!(out.len(), 3);
        assert_approx_eq(out[0], 3.0, 1e-6, "CW sample 0");
        assert_approx_eq(out[1], 0.0, 1e-6, "CW sample 1");
        assert_approx_eq(out[2], 1.0, 1e-6, "CW sample 2");
    }
}
