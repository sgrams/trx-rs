// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Line slicer: pixel clock recovery and line buffer assembly.
//!
//! Once the phasing detector has established a line-start phase offset,
//! the line slicer accumulates demodulated luminance samples and extracts
//! complete image lines at the configured LPM rate.

use crate::config::WefaxConfig;

/// Line slicer for WEFAX image assembly.
pub struct LineSlicer {
    /// Samples per line at the internal sample rate.
    samples_per_line: usize,
    /// Pixels per line (IOC × π).
    pixels_per_line: usize,
    /// Phase offset in samples from the phasing detector.
    phase_offset: usize,
    /// Accumulated luminance samples.
    buffer: Vec<f32>,
    /// Number of samples consumed since the last phase alignment point.
    consumed: usize,
    /// Whether we have aligned to the phase offset yet.
    aligned: bool,
}

impl LineSlicer {
    pub fn new(lpm: u16, ioc: u16, sample_rate: u32, phase_offset: usize) -> Self {
        let samples_per_line = WefaxConfig::samples_per_line(lpm, sample_rate);
        let pixels_per_line = WefaxConfig::pixels_per_line(ioc) as usize;

        Self {
            samples_per_line,
            pixels_per_line,
            phase_offset,
            buffer: Vec::with_capacity(samples_per_line * 2),
            consumed: 0,
            aligned: false,
        }
    }

    /// Feed luminance samples and extract complete image lines.
    ///
    /// Returns a vector of completed lines, each as a `Vec<u8>` of
    /// greyscale pixel values (0–255).
    pub fn process(&mut self, lum_samples: &[f32]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(lum_samples);
        let mut lines = Vec::new();

        // On first call, skip samples to align to the phase offset.
        if !self.aligned {
            if self.buffer.len() < self.phase_offset {
                return lines;
            }
            self.buffer.drain(..self.phase_offset);
            self.aligned = true;
        }

        // Extract complete lines (single drain at the end to avoid O(n²)).
        let mut offset = 0;
        while offset + self.samples_per_line <= self.buffer.len() {
            let line_samples = &self.buffer[offset..offset + self.samples_per_line];
            let pixels = self.resample_line(line_samples);
            lines.push(pixels);
            offset += self.samples_per_line;
            self.consumed += self.samples_per_line;
        }
        if offset > 0 {
            self.buffer.drain(..offset);
        }

        lines
    }

    pub fn pixels_per_line(&self) -> usize {
        self.pixels_per_line
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.consumed = 0;
        self.aligned = false;
    }

    /// Resample a line's worth of luminance samples to the target pixel count
    /// using linear interpolation.
    fn resample_line(&self, samples: &[f32]) -> Vec<u8> {
        let n_samples = samples.len() as f32;
        let n_pixels = self.pixels_per_line;
        let mut pixels = Vec::with_capacity(n_pixels);

        for px in 0..n_pixels {
            // Map pixel index to sample position.
            let pos = (px as f32 + 0.5) * n_samples / n_pixels as f32;
            let idx = pos.floor() as usize;
            let frac = pos - idx as f32;

            let v = if idx + 1 < samples.len() {
                samples[idx] * (1.0 - frac) + samples[idx + 1] * frac
            } else if idx < samples.len() {
                samples[idx]
            } else {
                0.0
            };

            pixels.push((v * 255.0).clamp(0.0, 255.0) as u8);
        }

        pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slicer_extracts_correct_line_count() {
        let lpm = 120;
        let ioc = 576;
        let sr = 11025;
        let spl = WefaxConfig::samples_per_line(lpm, sr);
        let ppl = WefaxConfig::pixels_per_line(ioc) as usize;

        let mut slicer = LineSlicer::new(lpm, ioc, sr, 0);
        // Feed exactly 3 lines worth of white.
        let samples = vec![1.0f32; spl * 3];
        let lines = slicer.process(&samples);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].len(), ppl);
        // All pixels should be white (255).
        assert!(lines[0].iter().all(|&p| p == 255));
    }

    #[test]
    fn slicer_linear_interpolation() {
        let lpm = 120;
        let ioc = 576;
        let sr = 11025;
        let spl = WefaxConfig::samples_per_line(lpm, sr);

        let mut slicer = LineSlicer::new(lpm, ioc, sr, 0);
        // Feed a linear ramp from 0.0 to 1.0.
        let samples: Vec<f32> = (0..spl).map(|i| i as f32 / spl as f32).collect();
        let lines = slicer.process(&samples);
        assert_eq!(lines.len(), 1);
        // First pixel should be near 0, last pixel near 255.
        assert!(lines[0][0] < 5);
        assert!(lines[0].last().copied().unwrap_or(0) > 250);
    }
}
