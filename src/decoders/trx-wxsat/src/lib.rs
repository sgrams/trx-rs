// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Weather satellite image decoders.
//!
//! This crate provides decoders for two weather satellite transmission formats:
//!
//! - **NOAA APT** ([`noaa`]): Automatic Picture Transmission from NOAA-15/18/19
//!   on 137 MHz using FM/AM subcarrier modulation at 4160 samples/sec.
//!
//! - **Meteor-M LRPT** ([`lrpt`]): Low Rate Picture Transmission from
//!   Meteor-M N2-3/N2-4 using QPSK modulation at 72 kbps with CCSDS framing.

pub mod lrpt;
pub mod noaa;

/// Current time in milliseconds since UNIX epoch.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
