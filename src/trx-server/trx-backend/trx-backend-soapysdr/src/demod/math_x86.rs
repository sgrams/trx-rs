// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use num_complex::Complex;

/// 7th-order minimax atan approximation for |z| <= 1.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn atan_poly_avx2(z: std::arch::x86_64::__m256) -> std::arch::x86_64::__m256 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let c0 = _mm256_set1_ps(0.999_999_5_f32);
    let c1 = _mm256_set1_ps(-0.333_326_1_f32);
    let c2 = _mm256_set1_ps(0.199_777_1_f32);
    let c3 = _mm256_set1_ps(-0.138_776_8_f32);

    let z2 = _mm256_mul_ps(z, z);
    let p = _mm256_add_ps(c2, _mm256_mul_ps(z2, c3));
    let p = _mm256_add_ps(c1, _mm256_mul_ps(z2, p));
    let p = _mm256_add_ps(c0, _mm256_mul_ps(z2, p));
    _mm256_mul_ps(z, p)
}

/// Branchless AVX2 atan2 using argument reduction and polynomial evaluation.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn fast_atan2_8_avx2(
    y: std::arch::x86_64::__m256,
    x: std::arch::x86_64::__m256,
) -> std::arch::x86_64::__m256 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let abs_mask = _mm256_castsi256_ps(_mm256_set1_epi32(0x7FFF_FFFF_u32 as i32));
    let sign_mask = _mm256_castsi256_ps(_mm256_set1_epi32(0x8000_0000_u32 as i32));
    let pi = _mm256_set1_ps(std::f32::consts::PI);
    let pi_2 = _mm256_set1_ps(std::f32::consts::FRAC_PI_2);

    let abs_y = _mm256_and_ps(y, abs_mask);
    let abs_x = _mm256_and_ps(x, abs_mask);

    let swap_mask = _mm256_cmp_ps(abs_y, abs_x, _CMP_GT_OS);
    let num = _mm256_blendv_ps(y, x, swap_mask);
    let den = _mm256_blendv_ps(x, y, swap_mask);

    let eps = _mm256_set1_ps(1.0e-30);
    let safe_den = _mm256_or_ps(
        den,
        _mm256_and_ps(_mm256_cmp_ps(den, _mm256_setzero_ps(), _CMP_EQ_OQ), eps),
    );
    let atan_input = _mm256_div_ps(num, safe_den);
    let mut result = atan_poly_avx2(atan_input);

    let adj = _mm256_sub_ps(
        _mm256_or_ps(pi_2, _mm256_and_ps(atan_input, sign_mask)),
        result,
    );
    result = _mm256_blendv_ps(result, adj, swap_mask);

    let x_sign_mask = _mm256_castsi256_ps(_mm256_srai_epi32(_mm256_castps_si256(x), 31));
    let correction = _mm256_and_ps(_mm256_xor_ps(pi, _mm256_and_ps(sign_mask, y)), x_sign_mask);
    _mm256_add_ps(result, correction)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub(super) fn demod_fm_body_avx2(
    samples: &[Complex<f32>],
    start: usize,
    inv_pi: f32,
    output: &mut Vec<f32>,
) -> usize {
    unsafe { demod_fm_body_avx2_impl(samples, start, inv_pi, output) }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn demod_fm_body_avx2_impl(
    samples: &[Complex<f32>],
    start: usize,
    inv_pi: f32,
    output: &mut Vec<f32>,
) -> usize {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let len = samples.len();
    let mut idx = start;
    let mut cur_re = [0.0_f32; 8];
    let mut cur_im = [0.0_f32; 8];
    let mut prev_re = [0.0_f32; 8];
    let mut prev_im = [0.0_f32; 8];
    let mut angles = [0.0_f32; 8];
    let inv_pi_v = _mm256_set1_ps(inv_pi);

    while idx + 8 <= len {
        for lane in 0..8 {
            let cur = samples[idx + lane];
            let prev = samples[idx + lane - 1];
            cur_re[lane] = cur.re;
            cur_im[lane] = cur.im;
            prev_re[lane] = prev.re;
            prev_im[lane] = prev.im;
        }

        let cur_re_v = _mm256_loadu_ps(cur_re.as_ptr());
        let cur_im_v = _mm256_loadu_ps(cur_im.as_ptr());
        let prev_re_v = _mm256_loadu_ps(prev_re.as_ptr());
        let prev_im_v = _mm256_loadu_ps(prev_im.as_ptr());

        let re_v = _mm256_add_ps(
            _mm256_mul_ps(cur_re_v, prev_re_v),
            _mm256_mul_ps(cur_im_v, prev_im_v),
        );
        let im_v = _mm256_sub_ps(
            _mm256_mul_ps(cur_im_v, prev_re_v),
            _mm256_mul_ps(cur_re_v, prev_im_v),
        );

        let angle_v = _mm256_mul_ps(fast_atan2_8_avx2(im_v, re_v), inv_pi_v);
        _mm256_storeu_ps(angles.as_mut_ptr(), angle_v);
        output.extend_from_slice(&angles);

        idx += 8;
    }

    idx
}
