// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

pub mod common;
mod decoder;
#[cfg(feature = "ft2")]
pub mod ft2;
pub mod ft4;
pub mod ft8;

pub use decoder::{Ft8DecodeResult, Ft8Decoder};
