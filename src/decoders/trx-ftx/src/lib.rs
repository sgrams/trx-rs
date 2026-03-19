// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

pub mod common;
mod decoder;
#[allow(clippy::needless_range_loop)]
pub mod ft2;
#[allow(clippy::needless_range_loop)]
pub mod ft4;
#[allow(clippy::needless_range_loop)]
pub mod ft8;

pub use decoder::{Ft8DecodeResult, Ft8Decoder};
