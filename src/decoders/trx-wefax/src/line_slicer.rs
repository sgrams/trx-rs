// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Line slicer: pixel clock recovery and line buffer assembly.
//!
//! Once the phasing detector has established a line-start phase offset,
//! the line slicer accumulates demodulated luminance samples and extracts
//! complete image lines at the configured LPM rate.
//!
//! When `slant_correction` is enabled, the slicer tracks line-to-line
//! drift via cross-correlation with the previous line and nudges the
//! extraction cursor by ±`MAX_DRIFT_SAMPLES` per line. This compensates
//! for the small mismatch between the transmitter's and receiver's
//! sample clocks that would otherwise skew the assembled image.

use crate::config::WefaxConfig;

/// Maximum per-line drift (in samples at the internal rate) searched for
/// when slant correction is enabled. At 120 LPM / 11025 Hz there are
/// ~5512 samples per line, so ±6 samples is ~0.1% drift per line — more
/// than enough for any real-world sample-clock mismatch.
const MAX_DRIFT_SAMPLES: usize = 6;

/// Line slicer for WEFAX image assembly.
pub struct LineSlicer {
    /// Samples per line at the internal sample rate.
    samples_per_line: usize,
    /// Pixels per line (IOC × π).
    pixels_per_line: usize,
    /// Phase offset in samples from the phasing detector.
    phase_offset: usize,
    /// Accumulated luminance samples. While `slant_correction` is on,
    /// the buffer anchor is the *start of the previous line* (so the
    /// first `samples_per_line` samples are the reference for drift
    /// tracking). Without slant correction the anchor is simply the
    /// start of the next line to extract.
    buffer: Vec<f32>,
    /// Whether we have aligned to the phase offset yet.
    aligned: bool,
    /// Whether a reference (previous) line is held at the buffer anchor.
    has_reference: bool,
    /// Enable line-to-line drift tracking.
    slant_correction: bool,
    /// Cumulative drift applied so far (samples). Diagnostic.
    pub(crate) total_drift: i64,
}

impl LineSlicer {
    pub fn new(lpm: u16, ioc: u16, sample_rate: u32, phase_offset: usize) -> Self {
        Self::with_slant(lpm, ioc, sample_rate, phase_offset, true)
    }

    pub fn with_slant(
        lpm: u16,
        ioc: u16,
        sample_rate: u32,
        phase_offset: usize,
        slant_correction: bool,
    ) -> Self {
        let samples_per_line = WefaxConfig::samples_per_line(lpm, sample_rate);
        let pixels_per_line = WefaxConfig::pixels_per_line(ioc) as usize;

        Self {
            samples_per_line,
            pixels_per_line,
            phase_offset,
            buffer: Vec::with_capacity(samples_per_line * 3),
            aligned: false,
            has_reference: false,
            slant_correction,
            total_drift: 0,
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

        let spl = self.samples_per_line;

        if !self.slant_correction {
            // Simple fixed-period extraction.
            let mut offset = 0;
            while offset + spl <= self.buffer.len() {
                let line_samples = &self.buffer[offset..offset + spl];
                let pixels = self.resample_line(line_samples);
                lines.push(pixels);
                offset += spl;
            }
            if offset > 0 {
                self.buffer.drain(..offset);
            }
            return lines;
        }

        // Slant-corrected extraction.
        let max_shift = MAX_DRIFT_SAMPLES;

        // Bootstrap: the very first line has no previous reference.
        // Extract it naively and keep it in the buffer as the reference.
        if !self.has_reference {
            if self.buffer.len() < spl {
                return lines;
            }
            let first = self.buffer[0..spl].to_vec();
            let pixels = self.resample_line(&first);
            lines.push(pixels);
            self.has_reference = true;
            // Do NOT drain: the first `spl` samples remain as the
            // reference for the next line's drift search.
        }

        // Subsequent lines: for each iteration, buffer[0..spl] is the
        // reference line, and we search for the best starting position
        // of the NEXT line in the range [spl - max_shift, spl + max_shift].
        while self.buffer.len() >= 2 * spl + max_shift {
            let prev = &self.buffer[0..spl];
            let (best_d, _best_r) = search_best_shift(prev, &self.buffer, spl, max_shift);

            let start = (spl as i32 + best_d) as usize;
            let next_line = self.buffer[start..start + spl].to_vec();
            let pixels = self.resample_line(&next_line);
            lines.push(pixels);

            // Advance the anchor to the start of the line we just
            // emitted — it becomes the reference for the next iteration.
            self.buffer.drain(..start);
            self.total_drift += best_d as i64;
        }

        lines
    }

    pub fn pixels_per_line(&self) -> usize {
        self.pixels_per_line
    }

    /// Samples per line at the internal rate (for diagnostics).
    pub fn samples_per_line(&self) -> usize {
        self.samples_per_line
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.aligned = false;
        self.has_reference = false;
        self.total_drift = 0;
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

/// Search for the drift `d ∈ [-max_shift, +max_shift]` that maximises
/// the Pearson correlation between `reference` and
/// `buffer[spl+d .. spl+d+spl]`.
///
/// Returns `(best_d, best_r)`. A correlation-peak deadband prefers
/// `d = 0` when the peak is only marginally better than at zero, which
/// keeps tracking stable on quiet lines.
fn search_best_shift(
    reference: &[f32],
    buffer: &[f32],
    spl: usize,
    max_shift: usize,
) -> (i32, f32) {
    debug_assert!(buffer.len() >= 2 * spl + max_shift);
    debug_assert_eq!(reference.len(), spl);

    // Pre-compute reference mean + variance.
    let n = spl as f32;
    let mean_r = reference.iter().sum::<f32>() / n;
    let mut var_r = 0.0f32;
    for &v in reference {
        let d = v - mean_r;
        var_r += d * d;
    }

    // Guard against a flat reference line — drift tracking is useless.
    const MIN_VAR: f32 = 32.0;
    if var_r < MIN_VAR {
        return (0, 0.0);
    }

    let ms = max_shift as i32;
    let mut best_d = 0i32;
    let mut best_r = f32::NEG_INFINITY;
    let mut r_at_zero = 0.0f32;

    for d in -ms..=ms {
        let start = (spl as i32 + d) as usize;
        let candidate = &buffer[start..start + spl];

        let mean_c = candidate.iter().sum::<f32>() / n;
        let mut var_c = 0.0f32;
        let mut cov = 0.0f32;
        for (i, &v) in candidate.iter().enumerate() {
            let dr = reference[i] - mean_r;
            let dc = v - mean_c;
            cov += dr * dc;
            var_c += dc * dc;
        }

        let r = if var_c < MIN_VAR {
            // Skip flat candidate slices.
            f32::NEG_INFINITY
        } else {
            cov / (var_r.sqrt() * var_c.sqrt())
        };

        if d == 0 {
            r_at_zero = r;
        }
        if r > best_r {
            best_r = r;
            best_d = d;
        }
    }

    // Deadband: if the peak is only marginally better than `d = 0`,
    // stick with zero. This avoids per-line jitter when drift is small.
    const DEADBAND: f32 = 0.01;
    if r_at_zero.is_finite() && best_r - r_at_zero < DEADBAND {
        return (0, r_at_zero);
    }

    (best_d, best_r)
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

        // Slant correction off for deterministic line count.
        let mut slicer = LineSlicer::with_slant(lpm, ioc, sr, 0, false);
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

        let mut slicer = LineSlicer::with_slant(lpm, ioc, sr, 0, false);
        // Feed a linear ramp from 0.0 to 1.0.
        let samples: Vec<f32> = (0..spl).map(|i| i as f32 / spl as f32).collect();
        let lines = slicer.process(&samples);
        assert_eq!(lines.len(), 1);
        // First pixel should be near 0, last pixel near 255.
        assert!(lines[0][0] < 5);
        assert!(lines[0].last().copied().unwrap_or(0) > 250);
    }

    /// Synthesise a noisy-ish gradient line that repeats with a small
    /// per-line offset, simulating a sample-clock mismatch. The slant
    /// tracker should follow the drift.
    #[test]
    fn slant_tracker_follows_drift() {
        let lpm = 120;
        let ioc = 576;
        let sr = 11025;
        let spl = WefaxConfig::samples_per_line(lpm, sr);

        // Build a signal where each real line is `spl + 3` samples long
        // (i.e. transmitter clock is slower than expected → positive drift
        // of +3 samples per line). The content needs high-frequency
        // structure for a few-sample shift to be detectable against the
        // deadband.
        let true_line_len = spl + 3;
        let mut signal: Vec<f32> = Vec::new();
        let base: Vec<f32> = (0..true_line_len)
            .map(|i| {
                // Pseudo-random-but-repeatable content with a narrow
                // bright stripe — sharp features make sub-line shifts
                // easy to localise.
                let x = ((i as u32).wrapping_mul(2654435761)) >> 16;
                let noise = (x & 0xff) as f32 / 255.0;
                let stripe = if i == true_line_len / 3 { 1.0 } else { 0.0 };
                0.3 + 0.4 * noise + stripe
            })
            .collect();
        // 20 lines, each identical.
        for _ in 0..20 {
            signal.extend_from_slice(&base);
        }

        let mut slicer = LineSlicer::with_slant(lpm, ioc, sr, 0, true);
        let lines = slicer.process(&signal);

        // Expect ~ (20*true_line_len - spl) / (spl+drift) lines with
        // drift absorbing the extra 2 samples per line.
        assert!(
            lines.len() >= 15,
            "slant-corrected slicer produced only {} lines",
            lines.len()
        );
        // Should have tracked positive drift.
        assert!(
            slicer.total_drift > 0,
            "expected positive drift, got {}",
            slicer.total_drift
        );
        // Roughly +3 per line (after the first bootstrap line); allow wide tolerance.
        let per_line = slicer.total_drift as f32 / (lines.len() - 1) as f32;
        assert!(
            per_line > 1.5 && per_line < 4.0,
            "per-line drift {:.2} out of range (total {}, lines {})",
            per_line,
            slicer.total_drift,
            lines.len()
        );
    }

    #[test]
    fn slant_tracker_deadband_on_no_drift() {
        let lpm = 120;
        let ioc = 576;
        let sr = 11025;
        let spl = WefaxConfig::samples_per_line(lpm, sr);

        // Perfectly aligned lines → drift should stay at zero.
        let line: Vec<f32> = (0..spl)
            .map(|i| {
                let t = i as f32 / spl as f32;
                0.5 + 0.4 * (t * 9.0 * std::f32::consts::PI).sin()
            })
            .collect();
        let mut signal = Vec::new();
        for _ in 0..10 {
            signal.extend_from_slice(&line);
        }

        let mut slicer = LineSlicer::with_slant(lpm, ioc, sr, 0, true);
        let _ = slicer.process(&signal);
        // Deadband should keep drift at 0.
        assert_eq!(
            slicer.total_drift, 0,
            "no drift expected for identical lines"
        );
    }
}
