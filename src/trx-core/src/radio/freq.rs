// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use serde::{Deserialize, Serialize};

use super::band::band_name;

/// Supported band range in Hz.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Band {
    pub low_hz: u64,
    pub high_hz: u64,
    pub tx_allowed: bool,
}

impl Band {
    /// Midpoint frequency of the band in Hz.
    pub fn center_hz(&self) -> u64 {
        (self.low_hz + self.high_hz) / 2
    }
}

/// Frequency wrapper (Hz).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Freq {
    pub hz: u64,
}

impl Freq {
    pub fn new(hz: u64) -> Self {
        Self { hz }
    }

    /// Return the band name for this frequency, if any, using the provided band list.
    pub fn band_name(&self, bands: &[Band]) -> Option<String> {
        band_for_freq(bands, self).map(band_name)
    }
}

/// Find the band that contains the given frequency (inclusive), if any.
pub fn band_for_freq<'a>(bands: &'a [Band], freq: &Freq) -> Option<&'a Band> {
    bands
        .iter()
        .find(|b| freq.hz >= b.low_hz && freq.hz <= b.high_hz)
}
