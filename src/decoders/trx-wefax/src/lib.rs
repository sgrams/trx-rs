// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! WEFAX (Weather Facsimile) decoder.
//!
//! Pure Rust implementation supporting 60/90/120/240 LPM, IOC 288 and 576,
//! with automatic APT tone detection and phase alignment.

pub mod config;
pub mod decoder;
pub mod demod;
pub mod image;
pub mod line_slicer;
pub mod phase;
pub mod resampler;
pub mod tone_detect;

pub use config::WefaxConfig;
pub use decoder::{WefaxDecoder, WefaxEvent};
