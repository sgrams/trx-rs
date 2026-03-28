// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! NOAA APT (Automatic Picture Transmission) weather satellite image decoder.
//!
//! Decodes the APT format broadcast by NOAA-15 (137.620 MHz),
//! NOAA-18 (137.9125 MHz) and NOAA-19 (137.100 MHz).
//!
//! # Signal chain
//!
//! The input is FM-demodulated audio containing a 2400 Hz AM subcarrier.
//! The decoder:
//! 1. Extracts the AM envelope via a FFT-based Hilbert transform (rustfft).
//! 2. Resamples to 4160 Hz (the APT image sample rate).
//! 3. Detects line sync markers (1040 Hz alternating pattern).
//! 4. Assembles image lines (2080 samples each) and extracts both channels.
//!
//! Call [`AptDecoder::process_samples`] with each audio batch, then
//! [`AptDecoder::finalize`] when the pass ends to obtain JPEG bytes.

pub mod apt;
mod image_enc;
pub mod telemetry;

use apt::{AptDemod, SyncTracker};
use telemetry::{Satellite, SensorChannel};

/// JPEG encoding quality (0-100).
const JPEG_QUALITY: u8 = 85;

/// Completed APT image returned by [`AptDecoder::finalize`].
pub struct AptImage {
    /// JPEG-encoded image bytes.
    pub jpeg: Vec<u8>,
    /// Number of decoded image lines.
    pub line_count: u32,
    /// Millisecond timestamp when the first line was decoded.
    pub first_line_ms: i64,
    /// Identified satellite, if telemetry was decodable.
    pub satellite: Satellite,
    /// Detected sensor channel for sub-channel A.
    pub sensor_a: SensorChannel,
    /// Detected sensor channel for sub-channel B.
    pub sensor_b: SensorChannel,
}

/// Top-level NOAA APT decoder.
///
/// Feed audio samples with [`process_samples`] and call [`finalize`] at
/// pass end to retrieve the assembled JPEG.
pub struct AptDecoder {
    demod: AptDemod,
    sync: SyncTracker,
    first_line_ms: Option<i64>,
}

impl AptDecoder {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            demod: AptDemod::new(sample_rate),
            sync: SyncTracker::new(),
            first_line_ms: None,
        }
    }

    /// Process a batch of PCM samples (float32, mono or will be treated as-is).
    ///
    /// Returns the number of new lines decoded in this batch.
    pub fn process_samples(&mut self, samples: &[f32]) -> u32 {
        self.demod.push(samples);

        let before = self.sync.lines.len() as u32;

        // Move accumulated envelope output into the sync tracker
        if !self.demod.out.is_empty() {
            let envelope = std::mem::take(&mut self.demod.out);
            self.sync.push(&envelope);
        }

        let after = self.sync.lines.len() as u32;
        let new_lines = after - before;

        if new_lines > 0 && self.first_line_ms.is_none() {
            self.first_line_ms = Some(crate::now_ms());
        }

        new_lines
    }

    /// Total number of lines decoded so far.
    pub fn line_count(&self) -> u32 {
        self.sync.lines.len() as u32
    }

    /// Encode all accumulated lines as a JPEG image and return the result.
    ///
    /// Performs telemetry extraction, radiometric calibration (when enough
    /// lines are available for a full 128-line telemetry frame), and
    /// histogram equalisation before JPEG encoding.
    ///
    /// Returns `None` if no lines have been decoded yet.
    /// Does **not** reset the decoder; call [`reset`] afterwards if needed.
    pub fn finalize(&self) -> Option<AptImage> {
        if self.sync.lines.is_empty() {
            return None;
        }

        // Extract telemetry for calibration and satellite identification
        let tel = telemetry::extract_telemetry(&self.sync.lines);

        // Clone lines so we can apply calibration without mutating decoder state
        let mut lines = self.sync.lines.clone();

        let (satellite, sensor_a, sensor_b) = if let Some(ref tf) = tel {
            // Apply radiometric calibration using telemetry wedge LUTs
            for line in &mut lines {
                telemetry::calibrate_line_a(&mut line.pixels_a, &tf.cal_lut_a);
                telemetry::calibrate_line_b(&mut line.pixels_b, &tf.cal_lut_b);
            }
            (tf.satellite, tf.sensor_a, tf.sensor_b)
        } else {
            (Satellite::Unknown, SensorChannel::Unknown, SensorChannel::Unknown)
        };

        // Apply histogram equalisation per-channel for contrast enhancement
        let mut all_a: Vec<u8> = lines.iter().flat_map(|l| l.pixels_a.iter().copied()).collect();
        let mut all_b: Vec<u8> = lines.iter().flat_map(|l| l.pixels_b.iter().copied()).collect();
        telemetry::histogram_equalize(&mut all_a);
        telemetry::histogram_equalize(&mut all_b);

        // Write equalised pixels back
        let width_a = apt::IMAGE_A_LEN;
        let width_b = apt::IMAGE_B_LEN;
        for (i, line) in lines.iter_mut().enumerate() {
            line.pixels_a.copy_from_slice(&all_a[i * width_a..(i + 1) * width_a]);
            line.pixels_b.copy_from_slice(&all_b[i * width_b..(i + 1) * width_b]);
        }

        let jpeg = image_enc::encode_jpeg(&lines, JPEG_QUALITY)?;
        Some(AptImage {
            jpeg,
            line_count: lines.len() as u32,
            first_line_ms: self.first_line_ms.unwrap_or_else(crate::now_ms),
            satellite,
            sensor_a,
            sensor_b,
        })
    }

    /// Clear all state; ready to decode a fresh pass.
    pub fn reset(&mut self) {
        self.demod.reset();
        self.sync.reset();
        self.first_line_ms = None;
    }
}
