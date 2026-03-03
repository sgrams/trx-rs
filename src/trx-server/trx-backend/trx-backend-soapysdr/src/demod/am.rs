// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;

/// AM demodulator using a limiter-derived coherent reference.
///
/// This is a practical DSP analogue of the patent's split/limit/mix approach:
/// derive a fixed-amplitude carrier reference from the incoming IQ, smooth that
/// reference to follow carrier phase/frequency, then mix the original branch by
/// the conjugate reference and take the in-phase projection.
pub(super) fn demod_am(samples: &[Complex<f32>]) -> Vec<f32> {
    const EPSILON: f32 = 1.0e-12;
    const REF_BLEND: f32 = 0.08;

    let mut out = Vec::with_capacity(samples.len());
    let mut carrier_ref = Complex::new(1.0_f32, 0.0);

    for &sample in samples {
        let mag_sq = sample.re * sample.re + sample.im * sample.im;
        if mag_sq <= EPSILON {
            out.push(0.0);
            continue;
        }

        let mag = mag_sq.sqrt();
        let limited = sample / mag;
        let blended = carrier_ref * (1.0 - REF_BLEND) + limited * REF_BLEND;
        let blended_mag_sq = blended.re * blended.re + blended.im * blended.im;
        if blended_mag_sq > EPSILON {
            carrier_ref = blended / blended_mag_sq.sqrt();
        } else {
            carrier_ref = limited;
        }

        // Project the original signal onto the limiter-derived carrier phase.
        let mixed = sample * carrier_ref.conj();
        out.push(mixed.re.max(0.0));
    }

    out
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

    #[test]
    fn test_am_tracks_rotating_carrier_reference() {
        let input: Vec<Complex<f32>> = (0..16)
            .map(|idx| {
                let angle = idx as f32 * 0.05;
                Complex::new(angle.cos(), angle.sin())
            })
            .collect();
        let out = demod_am(&input);
        assert_eq!(out.len(), input.len());
        let avg = out.iter().skip(1).copied().sum::<f32>() / (out.len().saturating_sub(1) as f32);
        assert!(
            avg > 0.95,
            "AM rotating carrier: expected strong average coherent output, got {avg}"
        );
    }
}
