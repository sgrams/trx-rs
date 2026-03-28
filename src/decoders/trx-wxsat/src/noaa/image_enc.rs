// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! APT image assembly.
//!
//! Standard output layout: channel A (visible / IR-A) on the left half and
//! channel B (IR-B / IR) on the right half, stacked vertically by line number.

use super::apt::{RawLine, IMAGE_A_LEN, IMAGE_B_LEN};

/// Assemble decoded APT lines into a PNG image.
///
/// Returns the PNG bytes, or `None` if `lines` is empty or encoding fails.
/// Width = `IMAGE_A_LEN + IMAGE_B_LEN` (1818 px), height = number of lines.
pub fn encode_png(lines: &[RawLine]) -> Option<Vec<u8>> {
    if lines.is_empty() {
        return None;
    }

    let width = (IMAGE_A_LEN + IMAGE_B_LEN) as u32;
    let height = lines.len() as u32;
    let mut pixels: Vec<u8> = Vec::with_capacity((width * height) as usize);

    for line in lines {
        pixels.extend_from_slice(line.pixels_a.as_ref());
        pixels.extend_from_slice(line.pixels_b.as_ref());
    }

    crate::image_enc::encode_grayscale_png(width, height, pixels)
}
