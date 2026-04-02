// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Phasing signal detector and line-start alignment for WEFAX.
//!
//! During the phasing period, each line is >95% white (luminance ≈ 1.0) with
//! a narrow black pulse (~5% of line width) marking the line-start position.
//! This module detects the pulse position via cross-correlation against
//! a synthetic phasing template, and averages over multiple lines to
//! establish a stable phase offset.

use crate::config::WefaxConfig;

/// Minimum number of phasing lines needed to establish phase lock.
const MIN_PHASING_LINES: usize = 10;

/// Maximum variance (in samples²) of pulse position for phase to be considered stable.
const MAX_PHASE_VARIANCE: f32 = 16.0;

/// Fraction of line width occupied by the black pulse in phasing signal.
const PULSE_WIDTH_FRACTION: f32 = 0.05;

/// Phasing signal detector.
pub struct PhasingDetector {
    samples_per_line: usize,
    pulse_width: usize,
    /// Collected pulse positions from each phasing line.
    pub(crate) pulse_positions: Vec<usize>,
    /// Luminance sample accumulator for the current line.
    line_buffer: Vec<f32>,
    /// Established phase offset (samples from buffer start to line start).
    phase_offset: Option<usize>,
}

impl PhasingDetector {
    pub fn new(lpm: u16, sample_rate: u32) -> Self {
        let samples_per_line = WefaxConfig::samples_per_line(lpm, sample_rate);
        let pulse_width = (samples_per_line as f32 * PULSE_WIDTH_FRACTION).round() as usize;

        Self {
            samples_per_line,
            pulse_width,
            pulse_positions: Vec::new(),
            line_buffer: Vec::with_capacity(samples_per_line),
            phase_offset: None,
        }
    }

    /// Feed luminance samples. Returns `Some(offset)` once phase is locked.
    pub fn process(&mut self, lum_samples: &[f32]) -> Option<usize> {
        if self.phase_offset.is_some() {
            return self.phase_offset;
        }

        for &s in lum_samples {
            self.line_buffer.push(s);

            if self.line_buffer.len() >= self.samples_per_line {
                self.analyze_phasing_line();
                self.line_buffer.clear();
            }
        }

        self.phase_offset
    }

    /// Return the established phase offset, if locked.
    pub fn offset(&self) -> Option<usize> {
        self.phase_offset
    }

    /// Check if phasing is complete and offset is stable.
    pub fn is_locked(&self) -> bool {
        self.phase_offset.is_some()
    }

    pub fn reset(&mut self) {
        self.pulse_positions.clear();
        self.line_buffer.clear();
        self.phase_offset = None;
    }

    fn analyze_phasing_line(&mut self) {
        let line = &self.line_buffer;

        // Verify this looks like a phasing line: >90% should be high luminance.
        let white_count = line.iter().filter(|&&v| v > 0.7).count();
        if white_count < line.len() * 85 / 100 {
            // Not a phasing line; reset accumulated positions.
            self.pulse_positions.clear();
            return;
        }

        // Find the black pulse position via minimum-energy sliding window.
        let pw = self.pulse_width.max(1);
        let mut min_energy = f32::MAX;
        let mut min_pos = 0;

        // Running sum for efficiency.
        let mut sum: f32 = line[..pw].iter().sum();
        if sum < min_energy {
            min_energy = sum;
            min_pos = 0;
        }

        for i in 1..=(line.len() - pw) {
            sum += line[i + pw - 1] - line[i - 1];
            if sum < min_energy {
                min_energy = sum;
                min_pos = i;
            }
        }

        // The black pulse should be significantly darker than the average.
        let avg_pulse = min_energy / pw as f32;
        if avg_pulse > 0.3 {
            // Pulse not dark enough, skip this line.
            return;
        }

        // Record pulse position (centre of the pulse window).
        self.pulse_positions.push(min_pos + pw / 2);

        // Check if we have enough samples and the variance is low.
        if self.pulse_positions.len() >= MIN_PHASING_LINES {
            let mean = self.pulse_positions.iter().sum::<usize>() as f32
                / self.pulse_positions.len() as f32;
            let variance = self
                .pulse_positions
                .iter()
                .map(|&p| {
                    let d = p as f32 - mean;
                    d * d
                })
                .sum::<f32>()
                / self.pulse_positions.len() as f32;

            if variance < MAX_PHASE_VARIANCE {
                self.phase_offset = Some(mean.round() as usize);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_phasing_pulse() {
        let lpm = 120;
        let sr = 11025;
        let spl = WefaxConfig::samples_per_line(lpm, sr);
        let mut det = PhasingDetector::new(lpm, sr);

        // Create 20 phasing lines with a black pulse at ~10% of line width.
        let pw = (spl as f32 * PULSE_WIDTH_FRACTION).round() as usize;
        let pulse_start = spl / 10;
        let pulse_center = pulse_start + pw / 2;

        for line_idx in 0..20 {
            let mut line = vec![1.0f32; spl];
            for j in pulse_start..pulse_start + pw {
                if j < spl {
                    line[j] = 0.0;
                }
            }
            let result = det.process(&line);
            if let Some(offset) = result {
                assert!(
                    (offset as i32 - pulse_center as i32).unsigned_abs() <= 3,
                    "phase offset {} too far from expected {} (line {})",
                    offset,
                    pulse_center,
                    line_idx,
                );
                return;
            }
        }

        panic!(
            "phasing should have locked after 20 lines (spl={}, pw={}, positions={:?})",
            spl,
            pw,
            det.pulse_positions
        );
    }
}
