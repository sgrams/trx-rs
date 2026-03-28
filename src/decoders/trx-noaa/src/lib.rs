// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! NOAA APT satellite image decoder.
//!
//! Decodes the Automatic Picture Transmission (APT) format broadcast by
//! NOAA-15 (137.620 MHz), NOAA-18 (137.9125 MHz) and NOAA-19 (137.100 MHz).
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

use apt::{AptDemod, SyncTracker};

/// JPEG encoding quality (0–100).
const JPEG_QUALITY: u8 = 85;

/// Completed APT image returned by [`AptDecoder::finalize`].
pub struct AptImage {
    /// JPEG-encoded image bytes.
    pub jpeg: Vec<u8>,
    /// Number of decoded image lines.
    pub line_count: u32,
    /// Millisecond timestamp when the first line was decoded.
    pub first_line_ms: i64,
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
            self.first_line_ms = Some(now_ms());
        }

        new_lines
    }

    /// Total number of lines decoded so far.
    pub fn line_count(&self) -> u32 {
        self.sync.lines.len() as u32
    }

    /// Encode all accumulated lines as a JPEG image and return the result.
    ///
    /// Returns `None` if no lines have been decoded yet.
    /// Does **not** reset the decoder; call [`reset`] afterwards if needed.
    pub fn finalize(&self) -> Option<AptImage> {
        let jpeg = image_enc::encode_jpeg(&self.sync.lines, JPEG_QUALITY)?;
        Some(AptImage {
            jpeg,
            line_count: self.sync.lines.len() as u32,
            first_line_ms: self.first_line_ms.unwrap_or_else(now_ms),
        })
    }

    /// Clear all state; ready to decode a fresh pass.
    pub fn reset(&mut self) {
        self.demod.reset();
        self.sync.reset();
        self.first_line_ms = None;
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
