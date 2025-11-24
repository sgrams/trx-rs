// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use crate::radio::freq::Band;

const SPEED_OF_LIGHT_M_PER_S: f64 = 299_792_458.0;

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
