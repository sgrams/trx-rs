// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! WEFAX decoder configuration.

/// Configuration for the WEFAX decoder.
#[derive(Debug, Clone)]
pub struct WefaxConfig {
    /// Lines per minute: 60, 90, 120, 240. `None` = auto-detect from APT.
    pub lpm: Option<u16>,
    /// Index of Cooperation: 288 or 576. `None` = auto-detect from start tone.
    pub ioc: Option<u16>,
    /// Centre frequency of the FM subcarrier (default 1900 Hz).
    pub center_freq_hz: f32,
    /// Deviation (default ±400 Hz, so black=1500, white=2300).
    pub deviation_hz: f32,
    /// Directory for saving decoded images.
    pub output_dir: Option<String>,
    /// Whether to emit line-by-line progress events.
    pub emit_progress: bool,
    /// Whether to continuously track and correct sample-clock drift
    /// (line-to-line cross-correlation) to remove image slant.
    pub slant_correction: bool,
}

impl Default for WefaxConfig {
    fn default() -> Self {
        Self {
            lpm: None,
            ioc: None,
            center_freq_hz: 1900.0,
            deviation_hz: 400.0,
            output_dir: None,
            emit_progress: true,
            slant_correction: true,
        }
    }
}

impl WefaxConfig {
    /// Pixels per line for a given IOC value: `IOC × π`, rounded.
    pub fn pixels_per_line(ioc: u16) -> u16 {
        (f64::from(ioc) * std::f64::consts::PI).round() as u16
    }

    /// Line duration in seconds for a given LPM value.
    pub fn line_duration_s(lpm: u16) -> f32 {
        60.0 / lpm as f32
    }

    /// Samples per line at the internal sample rate.
    pub fn samples_per_line(lpm: u16, sample_rate: u32) -> usize {
        (Self::line_duration_s(lpm) * sample_rate as f32).round() as usize
    }
}
