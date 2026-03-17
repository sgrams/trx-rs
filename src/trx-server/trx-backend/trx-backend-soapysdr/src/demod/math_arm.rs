// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
use num_complex::Complex;

/// 7th-order minimax atan approximation for |z| <= 1.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn atan_poly_neon(z: std::arch::aarch64::float32x4_t) -> std::arch::aarch64::float32x4_t {
    use std::arch::aarch64::*;
    let c0 = vdupq_n_f32(0.999_999_5_f32);
    let c1 = vdupq_n_f32(-0.333_326_1_f32);
    let c2 = vdupq_n_f32(0.199_777_1_f32);
    let c3 = vdupq_n_f32(-0.138_776_8_f32);
    let z2 = vmulq_f32(z, z);
    let p = vaddq_f32(c2, vmulq_f32(z2, c3));
    let p = vaddq_f32(c1, vmulq_f32(z2, p));
    let p = vaddq_f32(c0, vmulq_f32(z2, p));
    vmulq_f32(z, p)
}

/// Branchless NEON atan2 using argument reduction and polynomial evaluation.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn fast_atan2_4_neon(
    y: std::arch::aarch64::float32x4_t,
    x: std::arch::aarch64::float32x4_t,
) -> std::arch::aarch64::float32x4_t {
    use std::arch::aarch64::*;
    let abs_mask = vdupq_n_u32(0x7FFF_FFFF_u32);
    let sign_mask = vdupq_n_u32(0x8000_0000_u32);
    let pi = vdupq_n_f32(std::f32::consts::PI);
    let pi_2 = vdupq_n_f32(std::f32::consts::FRAC_PI_2);

    let abs_y = vreinterpretq_f32_u32(vandq_u32(vreinterpretq_u32_f32(y), abs_mask));
    let abs_x = vreinterpretq_f32_u32(vandq_u32(vreinterpretq_u32_f32(x), abs_mask));

    let swap_mask = vcgtq_f32(abs_y, abs_x);
    let num = vbslq_f32(swap_mask, x, y);
    let den = vbslq_f32(swap_mask, y, x);

    let eps = vdupq_n_f32(1.0e-30_f32);
    let den_is_zero = vceqq_f32(den, vdupq_n_f32(0.0));
    let safe_den = vreinterpretq_f32_u32(vorrq_u32(
        vreinterpretq_u32_f32(den),
        vandq_u32(den_is_zero, vreinterpretq_u32_f32(eps)),
    ));
    let atan_input = vdivq_f32(num, safe_den);
    let mut result = atan_poly_neon(atan_input);

    let pi_2_with_sign = vreinterpretq_f32_u32(vorrq_u32(
        vreinterpretq_u32_f32(pi_2),
        vandq_u32(vreinterpretq_u32_f32(atan_input), sign_mask),
    ));
    let adj = vsubq_f32(pi_2_with_sign, result);
    result = vbslq_f32(swap_mask, adj, result);

    let x_sign_mask = vreinterpretq_f32_s32(vshrq_n_s32::<31>(vreinterpretq_s32_f32(x)));
    let pi_xor_y_sign = vreinterpretq_f32_u32(veorq_u32(
        vreinterpretq_u32_f32(pi),
        vandq_u32(sign_mask, vreinterpretq_u32_f32(y)),
    ));
    let correction = vreinterpretq_f32_u32(vandq_u32(
        vreinterpretq_u32_f32(pi_xor_y_sign),
        vreinterpretq_u32_f32(x_sign_mask),
    ));
    vaddq_f32(result, correction)
}

/// NEON FM discriminator: processes 4 samples per iteration.
#[cfg(target_arch = "aarch64")]
pub(super) fn demod_fm_body_neon(
    samples: &[Complex<f32>],
    start: usize,
    inv_pi: f32,
    output: &mut Vec<f32>,
) -> usize {
    unsafe { demod_fm_body_neon_impl(samples, start, inv_pi, output) }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn demod_fm_body_neon_impl(
    samples: &[Complex<f32>],
    start: usize,
    inv_pi: f32,
    output: &mut Vec<f32>,
) -> usize {
    use std::arch::aarch64::*;

    let len = samples.len();
    let mut idx = start;
    let mut cur_re = [0.0_f32; 4];
    let mut cur_im = [0.0_f32; 4];
    let mut prev_re = [0.0_f32; 4];
    let mut prev_im = [0.0_f32; 4];
    let mut angles = [0.0_f32; 4];
    let inv_pi_v = vdupq_n_f32(inv_pi);

    while idx + 4 <= len {
        for lane in 0..4 {
            let cur = samples[idx + lane];
            let prev = samples[idx + lane - 1];
            cur_re[lane] = cur.re;
            cur_im[lane] = cur.im;
            prev_re[lane] = prev.re;
            prev_im[lane] = prev.im;
        }

        let cur_re_v = vld1q_f32(cur_re.as_ptr());
        let cur_im_v = vld1q_f32(cur_im.as_ptr());
        let prev_re_v = vld1q_f32(prev_re.as_ptr());
        let prev_im_v = vld1q_f32(prev_im.as_ptr());

        // Conjugate multiply: s[n] * conj(s[n-1])
        // re = cur_re * prev_re + cur_im * prev_im
        // im = cur_im * prev_re - cur_re * prev_im
        let re_v = vaddq_f32(
            vmulq_f32(cur_re_v, prev_re_v),
            vmulq_f32(cur_im_v, prev_im_v),
        );
        let im_v = vsubq_f32(
            vmulq_f32(cur_im_v, prev_re_v),
            vmulq_f32(cur_re_v, prev_im_v),
        );

        let angle_v = vmulq_f32(fast_atan2_4_neon(im_v, re_v), inv_pi_v);
        vst1q_f32(angles.as_mut_ptr(), angle_v);
        output.extend_from_slice(&angles);

        idx += 4;
    }

    idx
}

/// On 32-bit ARM, fall back to the scalar path.
#[cfg(target_arch = "arm")]
pub(super) fn demod_fm_body_neon(
    _samples: &[Complex<f32>],
    start: usize,
    _inv_pi: f32,
    _output: &mut Vec<f32>,
) -> usize {
    start
}
