// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

#[allow(clippy::needless_range_loop)]
pub mod bitmetrics;
pub mod callsign_hash;
pub mod constants;
pub mod crc;
#[allow(dead_code, clippy::needless_range_loop)]
pub mod decode;
mod decoder;
#[allow(clippy::needless_range_loop)]
pub mod downsample;
#[allow(clippy::needless_range_loop)]
pub mod encode;
#[allow(dead_code, clippy::needless_range_loop, clippy::too_many_arguments)]
pub mod ft2;
#[allow(clippy::needless_range_loop)]
pub mod ft2_sync;
#[allow(clippy::needless_range_loop)]
pub mod ldpc;
#[allow(clippy::explicit_counter_loop, clippy::needless_range_loop)]
pub mod message;
#[allow(dead_code)]
pub mod monitor;
#[allow(dead_code, clippy::needless_range_loop, clippy::too_many_arguments)]
pub mod osd;
pub mod protocol;
pub mod text;

pub use decoder::{Ft8DecodeResult, Ft8Decoder};
