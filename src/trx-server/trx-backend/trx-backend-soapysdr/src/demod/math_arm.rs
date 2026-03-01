// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
use num_complex::Complex;

/// Placeholder hook for future ARM/NEON FM discriminator vectorization.
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
pub(super) fn demod_fm_body_neon(
    _samples: &[Complex<f32>],
    start: usize,
    _inv_pi: f32,
    _output: &mut Vec<f32>,
) -> usize {
    start
}
