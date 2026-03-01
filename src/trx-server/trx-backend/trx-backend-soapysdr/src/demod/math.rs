// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;

#[inline]
pub(super) fn fast_atan2(y: f32, x: f32) -> f32 {
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

    if x > 0.0 {
        fast_atan(y / x)
    } else if x < 0.0 {
        if y >= 0.0 {
            fast_atan(y / x) + std::f32::consts::PI
        } else {
            fast_atan(y / x) - std::f32::consts::PI
        }
    } else {
        0.0
    }
}

/// FM quadrature discriminator: instantaneous frequency via `arg(s[n] * conj(s[n-1]))`.
/// Output is in radians/sample, scaled by `1/π` to normalize to `[-1, 1]`.
pub(super) fn demod_fm_with_prev(
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
        output.push(fast_atan2(product.im, product.re) * inv_pi);
    } else {
        output.push(0.0);
    }

    let mut idx = 1usize;

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if std::arch::is_x86_feature_detected!("avx2") {
        idx = super::math_x86::demod_fm_body_avx2(samples, idx, inv_pi, &mut output);
    }

    #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
    {
        idx = super::math_arm::demod_fm_body_neon(samples, idx, inv_pi, &mut output);
    }

    for sample_idx in idx..samples.len() {
        let product = samples[sample_idx] * samples[sample_idx - 1].conj();
        output.push(fast_atan2(product.im, product.re) * inv_pi);
    }

    *prev = samples.last().copied();
    output
}
