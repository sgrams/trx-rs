// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Shared PNG image encoding for weather satellite decoders.
//!
//! Both NOAA APT and Meteor-M LRPT decoders produce PNG output through
//! this common module.

use std::io::Cursor;

use image::DynamicImage;

/// Encode a grayscale pixel buffer as PNG.
///
/// Returns `None` if the buffer is empty or encoding fails.
pub fn encode_grayscale_png(width: u32, height: u32, pixels: Vec<u8>) -> Option<Vec<u8>> {
    let gray = image::GrayImage::from_raw(width, height, pixels)?;
    let dynamic = DynamicImage::ImageLuma8(gray);
    encode_dynamic_png(&dynamic)
}

/// Encode an RGB pixel buffer as PNG.
///
/// `pixels` must contain `width * height * 3` bytes in R, G, B order.
/// Returns `None` if the buffer is empty or encoding fails.
pub fn encode_rgb_png(width: u32, height: u32, pixels: Vec<u8>) -> Option<Vec<u8>> {
    let rgb = image::RgbImage::from_raw(width, height, pixels)?;
    let dynamic = DynamicImage::ImageRgb8(rgb);
    encode_dynamic_png(&dynamic)
}

fn encode_dynamic_png(img: &DynamicImage) -> Option<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageOutputFormat::Png)
        .ok()?;
    Some(cursor.into_inner())
}
