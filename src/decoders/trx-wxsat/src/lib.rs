// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Weather satellite image decoders.
//!
//! This crate provides a decoder for Meteor-M LRPT (Low Rate Picture
//! Transmission) from Meteor-M N2-3/N2-4 using QPSK modulation at 72 kbps
//! with CCSDS framing.

pub mod image_enc;
pub mod lrpt;

/// Current time in milliseconds since UNIX epoch.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
