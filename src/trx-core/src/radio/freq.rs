// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use serde::{Deserialize, Serialize};

const SPEED_OF_LIGHT_M_PER_S: f64 = 299_792_458.0;

/// Supported band range in Hz.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Band {
    pub low_hz: u64,
    pub high_hz: u64,
    pub tx_allowed: bool,
}

impl Band {
    /// Midpoint frequency of the band in Hz.
    #[must_use]
    pub fn center_hz(&self) -> u64 {
        u64::midpoint(self.low_hz, self.high_hz)
    }
}

/// Frequency wrapper (Hz).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Freq {
    pub hz: u64,
}

impl Freq {
    #[must_use]
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

/// Convert a frequency in Hz to a human-friendly wavelength string.
///
/// Values above one meter are rounded to the nearest meter; shorter wavelengths
/// are shown in centimeters.
pub fn wavelength_label(freq_hz: u64) -> String {
    if freq_hz == 0 {
        return "-".to_string();
    }

    let wavelength_m = SPEED_OF_LIGHT_M_PER_S / (freq_hz as f64);
    if wavelength_m >= 1.0 {
        format!("{:.0}m", wavelength_m.round())
    } else {
        format!("{:.0}cm", (wavelength_m * 100.0).round())
    }
}

/// Derive a human-friendly band label from a band's wavelength.
///
/// The label is computed from the wavelength at the band's center frequency.
pub fn band_name(band: &Band) -> String {
    wavelength_label(band.center_hz())
}
