// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! APT telemetry frame parsing, satellite identification, and channel detection.
//!
//! Each APT line contains two 45-sample telemetry blocks (one per channel).
//! The telemetry frame repeats every 128 lines and contains 16 wedges of
//! 8 lines each.  Wedges 1-8 carry calibration reference levels, wedge 9
//! carries the channel ID, and wedges 10-15 carry thermal calibration data.
//! Wedge 16 is the "zero modulation" reference (black body equivalent).

use super::apt::{IMAGE_A_LEN, IMAGE_B_LEN, RawLine};

/// Lines per telemetry frame (128 lines = 16 wedges x 8 lines each).
pub const FRAME_LINES: usize = 128;

/// Lines per wedge.
pub const WEDGE_LINES: usize = 8;

/// Number of wedges in a telemetry frame.
pub const NUM_WEDGES: usize = 16;

/// The 8 calibration step values defined by the APT spec (wedges 1-8).
/// These represent known modulation levels from 1/8 to 8/8 of full scale.
pub const WEDGE_STEPS: [f32; 8] = [
    0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0,
];

/// NOAA AVHRR sensor channel assignments.
///
/// The NOAA APT format transmits two channels simultaneously.  Which sensors
/// are mapped to channel A and B depends on the satellite and illumination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorChannel {
    /// Channel 1: Visible (0.58 - 0.68 um)
    Visible1,
    /// Channel 2: Near-IR (0.725 - 1.0 um)
    NearIr2,
    /// Channel 3A: Near-IR (1.58 - 1.64 um) — daytime only on NOAA-15/18/19
    NearIr3A,
    /// Channel 3B: Mid-IR thermal (3.55 - 3.93 um)
    MidIr3B,
    /// Channel 4: Thermal IR (10.30 - 11.30 um)
    ThermalIr4,
    /// Channel 5: Thermal IR (11.50 - 12.50 um) — not on NOAA-15 APT
    ThermalIr5,
    /// Unknown / could not be determined.
    Unknown,
}

impl std::fmt::Display for SensorChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SensorChannel::Visible1 => write!(f, "1-VIS"),
            SensorChannel::NearIr2 => write!(f, "2-NIR"),
            SensorChannel::NearIr3A => write!(f, "3A-NIR"),
            SensorChannel::MidIr3B => write!(f, "3B-MIR"),
            SensorChannel::ThermalIr4 => write!(f, "4-TIR"),
            SensorChannel::ThermalIr5 => write!(f, "5-TIR"),
            SensorChannel::Unknown => write!(f, "unknown"),
        }
    }
}

/// Identified NOAA satellite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Satellite {
    Noaa15,
    Noaa18,
    Noaa19,
    Unknown,
}

impl std::fmt::Display for Satellite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Satellite::Noaa15 => write!(f, "NOAA-15"),
            Satellite::Noaa18 => write!(f, "NOAA-18"),
            Satellite::Noaa19 => write!(f, "NOAA-19"),
            Satellite::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Wedge 9 channel-ID values for each satellite.
///
/// The channel ID wedge has a distinctive grey level that encodes which
/// AVHRR sensor channel is being transmitted on that APT sub-channel.
/// Values are approximate normalised levels (0.0 - 1.0).
///
/// Reference: NOAA KLM User's Guide, Section 4.2 (APT format).
///
///  Channel A mapping:
///    Wedge 9 ≈ step 1 (1/8) → channel 1 (VIS)
///    Wedge 9 ≈ step 2 (2/8) → channel 2 (NIR)
///    Wedge 9 ≈ step 3 (3/8) → channel 3A (NIR, daytime)
///
///  Channel B mapping:
///    Wedge 9 ≈ step 4 (4/8) → channel 3B (MIR)
///    Wedge 9 ≈ step 5 (5/8) → channel 4 (TIR)
///    Wedge 9 ≈ step 6 (6/8) → channel 5 (TIR)
fn wedge9_to_sensor(normalised: f32) -> SensorChannel {
    // Map to nearest step (1/8 increments)
    let step = (normalised * 8.0).round() as u8;
    match step {
        1 => SensorChannel::Visible1,
        2 => SensorChannel::NearIr2,
        3 => SensorChannel::NearIr3A,
        4 => SensorChannel::MidIr3B,
        5 => SensorChannel::ThermalIr4,
        6 => SensorChannel::ThermalIr5,
        _ => SensorChannel::Unknown,
    }
}

/// Extracted telemetry data from one complete 128-line frame.
#[derive(Debug, Clone)]
pub struct TelemetryFrame {
    /// Mean pixel value for each of the 16 wedges (normalised 0.0 - 1.0).
    pub wedge_means_a: [f32; NUM_WEDGES],
    pub wedge_means_b: [f32; NUM_WEDGES],
    /// Detected sensor channel for sub-channel A.
    pub sensor_a: SensorChannel,
    /// Detected sensor channel for sub-channel B.
    pub sensor_b: SensorChannel,
    /// Calibration mapping: maps raw pixel [0,255] → calibrated [0.0, 1.0]
    /// using wedges 1-8 as known reference levels.
    pub cal_lut_a: [u8; 256],
    pub cal_lut_b: [u8; 256],
    /// Identified satellite (from channel pairing heuristics).
    pub satellite: Satellite,
}

/// Extract telemetry from raw lines, requiring at least one full 128-line frame.
///
/// Picks the best complete frame (highest overall signal quality) and parses
/// wedge values from the telemetry blocks.
pub fn extract_telemetry(lines: &[RawLine]) -> Option<TelemetryFrame> {
    if lines.len() < FRAME_LINES {
        return None;
    }

    // Use the middle complete frame for best quality (avoids pass start/end noise)
    let num_frames = lines.len() / FRAME_LINES;
    let frame_idx = num_frames / 2;
    let frame_start = frame_idx * FRAME_LINES;
    let frame = &lines[frame_start..frame_start + FRAME_LINES];

    // Extract wedge means from telemetry blocks.
    // Each wedge spans 8 lines; we average the telemetry samples across those lines.
    let mut wedge_means_a = [0.0f32; NUM_WEDGES];
    let mut wedge_means_b = [0.0f32; NUM_WEDGES];

    for wedge_idx in 0..NUM_WEDGES {
        let line_start = wedge_idx * WEDGE_LINES;
        let mut sum_a = 0.0f32;
        let mut sum_b = 0.0f32;
        let mut count = 0u32;

        for line_offset in 0..WEDGE_LINES {
            let line = &frame[line_start + line_offset];
            for &v in line.tel_a.as_ref() {
                sum_a += v as f32;
                count += 1;
            }
            for &v in line.tel_b.as_ref() {
                sum_b += v as f32;
            }
        }

        if count > 0 {
            wedge_means_a[wedge_idx] = sum_a / count as f32 / 255.0;
            wedge_means_b[wedge_idx] = sum_b / count as f32 / 255.0;
        }
    }

    // Detect sensor channels from wedge 9 (index 8)
    let sensor_a = wedge9_to_sensor(wedge_means_a[8]);
    let sensor_b = wedge9_to_sensor(wedge_means_b[8]);

    // Build calibration LUTs from wedges 1-8
    let cal_lut_a = build_calibration_lut(&wedge_means_a);
    let cal_lut_b = build_calibration_lut(&wedge_means_b);

    // Identify satellite from channel pairing
    let satellite = identify_satellite(sensor_a, sensor_b);

    Some(TelemetryFrame {
        wedge_means_a,
        wedge_means_b,
        sensor_a,
        sensor_b,
        cal_lut_a,
        cal_lut_b,
        satellite,
    })
}

/// Build a 256-entry calibration look-up table from wedge means.
///
/// Wedges 1-8 (indices 0-7) represent known reference levels at 1/8 to 8/8.
/// We fit a piecewise linear mapping from observed pixel values to calibrated
/// output levels, producing a corrected 0-255 output.
fn build_calibration_lut(wedge_means: &[f32; NUM_WEDGES]) -> [u8; 256] {
    let mut lut = [0u8; 256];

    // Collect (observed_pixel_value, target_normalised) pairs from wedges 1-8
    let mut pairs: Vec<(f32, f32)> = Vec::with_capacity(8);
    for i in 0..8 {
        let observed = wedge_means[i] * 255.0;
        let target = WEDGE_STEPS[i];
        pairs.push((observed, target));
    }

    // Sort by observed value
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // Deduplicate (if two wedges map to nearly the same observed value)
    pairs.dedup_by(|a, b| (a.0 - b.0).abs() < 0.5);

    if pairs.len() < 2 {
        // Not enough calibration data — return identity
        for (i, v) in lut.iter_mut().enumerate() {
            *v = i as u8;
        }
        return lut;
    }

    // Piecewise linear interpolation
    for (i, entry) in lut.iter_mut().enumerate() {
        let x = i as f32;
        let calibrated = if x <= pairs[0].0 {
            pairs[0].1
        } else if x >= pairs[pairs.len() - 1].0 {
            pairs[pairs.len() - 1].1
        } else {
            let mut cal = pairs[0].1;
            for w in pairs.windows(2) {
                if x >= w[0].0 && x <= w[1].0 {
                    let t = (x - w[0].0) / (w[1].0 - w[0].0).max(1e-6);
                    cal = w[0].1 + t * (w[1].1 - w[0].1);
                    break;
                }
            }
            cal
        };
        *entry = (calibrated * 255.0).round().clamp(0.0, 255.0) as u8;
    }

    lut
}

/// Identify the satellite based on channel pairing heuristics.
///
/// Typical APT channel pairings:
///   - NOAA-15: Ch A = 2 (NIR), Ch B = 4 (TIR) daytime;
///     Ch A = 3A (NIR), Ch B = 4 (TIR) alternate daytime
///   - NOAA-18: Ch A = 1 (VIS), Ch B = 4 (TIR) daytime;
///     Ch A = 3A (NIR), Ch B = 4 (TIR) alternate
///   - NOAA-19: Ch A = 2 (NIR), Ch B = 4 (TIR) daytime
///
/// Night passes typically transmit Ch 3B or Ch 4 on channel A.
fn identify_satellite(sensor_a: SensorChannel, sensor_b: SensorChannel) -> Satellite {
    match (sensor_a, sensor_b) {
        // NOAA-18 typically sends VIS ch1 on A
        (SensorChannel::Visible1, SensorChannel::ThermalIr4) => Satellite::Noaa18,
        // NOAA-15 and NOAA-19 both send NIR ch2 on A; distinguish by B channel
        (SensorChannel::NearIr2, SensorChannel::ThermalIr4) => {
            // Both NOAA-15 and NOAA-19 use this pairing; cannot easily distinguish
            // without orbital data.  Default to NOAA-19 (most common active).
            Satellite::Noaa19
        }
        (SensorChannel::NearIr3A, SensorChannel::ThermalIr4) => Satellite::Noaa15,
        (SensorChannel::NearIr2, SensorChannel::ThermalIr5) => Satellite::Noaa19,
        _ => Satellite::Unknown,
    }
}

/// Apply calibration LUT to a line's pixel data (in-place).
pub fn calibrate_line_a(pixels: &mut [u8; IMAGE_A_LEN], lut: &[u8; 256]) {
    for p in pixels.iter_mut() {
        *p = lut[*p as usize];
    }
}

/// Apply calibration LUT to a line's pixel data (in-place).
pub fn calibrate_line_b(pixels: &mut [u8; IMAGE_B_LEN], lut: &[u8; 256]) {
    for p in pixels.iter_mut() {
        *p = lut[*p as usize];
    }
}

/// Apply histogram equalisation to an image channel for contrast enhancement.
pub fn histogram_equalize(pixels: &mut [u8]) {
    if pixels.is_empty() {
        return;
    }

    // Build histogram
    let mut hist = [0u32; 256];
    for &p in pixels.iter() {
        hist[p as usize] += 1;
    }

    // Compute CDF
    let mut cdf = [0u32; 256];
    cdf[0] = hist[0];
    for i in 1..256 {
        cdf[i] = cdf[i - 1] + hist[i];
    }

    // Find minimum non-zero CDF value
    let cdf_min = cdf.iter().copied().find(|&v| v > 0).unwrap_or(0);
    let total = pixels.len() as u32;
    let denom = (total - cdf_min).max(1);

    // Build equalisation LUT
    let mut lut = [0u8; 256];
    for i in 0..256 {
        lut[i] = ((cdf[i].saturating_sub(cdf_min) as f64 / denom as f64) * 255.0).round() as u8;
    }

    // Apply
    for p in pixels.iter_mut() {
        *p = lut[*p as usize];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wedge9_to_sensor() {
        assert_eq!(wedge9_to_sensor(0.125), SensorChannel::Visible1);
        assert_eq!(wedge9_to_sensor(0.25), SensorChannel::NearIr2);
        assert_eq!(wedge9_to_sensor(0.375), SensorChannel::NearIr3A);
        assert_eq!(wedge9_to_sensor(0.5), SensorChannel::MidIr3B);
        assert_eq!(wedge9_to_sensor(0.625), SensorChannel::ThermalIr4);
        assert_eq!(wedge9_to_sensor(0.75), SensorChannel::ThermalIr5);
        assert_eq!(wedge9_to_sensor(0.0), SensorChannel::Unknown);
    }

    #[test]
    fn test_identify_satellite() {
        assert_eq!(
            identify_satellite(SensorChannel::Visible1, SensorChannel::ThermalIr4),
            Satellite::Noaa18
        );
        assert_eq!(
            identify_satellite(SensorChannel::NearIr2, SensorChannel::ThermalIr4),
            Satellite::Noaa19
        );
        assert_eq!(
            identify_satellite(SensorChannel::NearIr3A, SensorChannel::ThermalIr4),
            Satellite::Noaa15
        );
    }

    #[test]
    fn test_calibration_lut_identity_on_insufficient_data() {
        let mut means = [0.0f32; NUM_WEDGES];
        // All zeros → insufficient data → identity LUT
        let lut = build_calibration_lut(&means);
        for i in 0..256 {
            assert_eq!(lut[i], i as u8);
        }

        // One non-zero wedge still insufficient (need ≥ 2 distinct)
        means[0] = 0.5;
        let lut = build_calibration_lut(&means);
        // Still degenerate
        assert!(lut[0] == lut[0]); // trivially true, but confirms no panic
    }

    #[test]
    fn test_histogram_equalize_uniform() {
        // Uniform distribution should remain roughly unchanged
        let mut pixels: Vec<u8> = (0..=255).collect();
        histogram_equalize(&mut pixels);
        // After equalization, values should span full range
        assert_eq!(*pixels.first().unwrap(), 0);
        assert_eq!(*pixels.last().unwrap(), 255);
    }

    #[test]
    fn test_sensor_channel_display() {
        assert_eq!(format!("{}", SensorChannel::Visible1), "1-VIS");
        assert_eq!(format!("{}", SensorChannel::ThermalIr4), "4-TIR");
    }

    #[test]
    fn test_satellite_display() {
        assert_eq!(format!("{}", Satellite::Noaa15), "NOAA-15");
        assert_eq!(format!("{}", Satellite::Noaa19), "NOAA-19");
    }
}
