// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

pub mod protocol;
pub mod constants;
pub mod crc;
pub mod text;
#[allow(clippy::manual_memcpy, clippy::needless_range_loop)]
pub mod ldpc;
#[allow(clippy::needless_range_loop)]
pub mod encode;
pub mod callsign_hash;
#[allow(clippy::explicit_counter_loop, clippy::needless_range_loop)]
pub mod message;
#[allow(dead_code)]
pub mod monitor;
#[allow(dead_code, clippy::needless_range_loop)]
pub mod decode;
#[allow(
    dead_code,
    clippy::manual_memcpy,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]
pub mod ft2;
mod decoder;

pub use decoder::{Ft8Decoder, Ft8DecodeResult};
