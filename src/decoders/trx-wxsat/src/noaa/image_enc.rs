// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! APT image assembly and JPEG encoding.
//!
//! Standard output layout: channel A (visible / IR-A) on the left half and
//! channel B (IR-B / IR) on the right half, stacked vertically by line number.

use std::io::Cursor;

use image::{DynamicImage, GrayImage};

use super::apt::{RawLine, IMAGE_A_LEN, IMAGE_B_LEN};

/// Assemble decoded lines into a JPEG image.
///
/// Returns the JPEG bytes, or `None` if `lines` is empty or encoding fails.
/// Width = `IMAGE_A_LEN + IMAGE_B_LEN` (1818 px), height = number of lines.
pub fn encode_jpeg(lines: &[RawLine], quality: u8) -> Option<Vec<u8>> {
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

    let gray = GrayImage::from_raw(width, height, pixels)?;
    let dynamic = DynamicImage::ImageLuma8(gray);

    let mut cursor = Cursor::new(Vec::new());
    dynamic
        .write_to(&mut cursor, image::ImageOutputFormat::Jpeg(quality))
        .ok()?;

    Some(cursor.into_inner())
}
