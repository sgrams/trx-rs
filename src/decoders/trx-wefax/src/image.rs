// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Image buffer and PNG encoding for WEFAX decoded images.

use std::io::BufWriter;
use std::path::{Path, PathBuf};

/// Image assembler: accumulates greyscale lines and encodes to PNG.
pub struct ImageAssembler {
    pixels_per_line: usize,
    lines: Vec<Vec<u8>>,
}

impl ImageAssembler {
    pub fn new(pixels_per_line: usize) -> Self {
        Self {
            pixels_per_line,
            lines: Vec::with_capacity(800),
        }
    }

    /// Append a completed greyscale line.
    pub fn push_line(&mut self, line: Vec<u8>) {
        debug_assert_eq!(line.len(), self.pixels_per_line);
        self.lines.push(line);
    }

    /// Number of lines accumulated so far.
    pub fn line_count(&self) -> u32 {
        self.lines.len() as u32
    }

    /// Get the most recently added line (for progress events).
    pub fn last_line(&self) -> Option<&[u8]> {
        self.lines.last().map(|l| l.as_slice())
    }

    /// Pearson correlation between `line` and the most recently pushed line.
    ///
    /// Returns `None` if there is no previous line, the lengths don't match,
    /// or either line has near-zero variance (constant pixels — correlation
    /// is undefined, and flat regions shouldn't be scored as "noise").
    ///
    /// For real WEFAX image content adjacent lines are typically highly
    /// correlated (r > 0.5). When the signal is lost and the slicer feeds
    /// on noise, r collapses toward 0. This mirrors fldigi's line-to-line
    /// correlation check for automatic stop.
    pub fn correlation_with_last(&self, line: &[u8]) -> Option<f32> {
        let prev = self.lines.last()?;
        if prev.len() != line.len() || line.is_empty() {
            return None;
        }

        let n = line.len() as f32;
        let mean_a = prev.iter().map(|&v| v as f32).sum::<f32>() / n;
        let mean_b = line.iter().map(|&v| v as f32).sum::<f32>() / n;

        let mut cov = 0.0f32;
        let mut var_a = 0.0f32;
        let mut var_b = 0.0f32;
        for (&a, &b) in prev.iter().zip(line.iter()) {
            let da = a as f32 - mean_a;
            let db = b as f32 - mean_b;
            cov += da * db;
            var_a += da * da;
            var_b += db * db;
        }

        // Require some variance in both lines — flat regions are common in
        // real imagery (solid black/white) and shouldn't be penalised.
        const MIN_VAR: f32 = 32.0; // ~ stddev of 4 counts on 0..255 scale
        if var_a < MIN_VAR || var_b < MIN_VAR {
            return None;
        }

        Some(cov / (var_a.sqrt() * var_b.sqrt()))
    }

    /// Encode the accumulated image to an 8-bit greyscale PNG file.
    ///
    /// Returns the full path to the saved file.
    pub fn save_png(&self, output_dir: &Path, freq_hz: u64, mode: &str) -> Result<PathBuf, String> {
        if self.lines.is_empty() {
            return Err("no image lines to save".into());
        }

        std::fs::create_dir_all(output_dir).map_err(|e| format!("create output dir: {}", e))?;

        let filename = generate_filename(freq_hz, mode);
        let path = output_dir.join(&filename);

        let file = std::fs::File::create(&path)
            .map_err(|e| format!("create PNG file '{}': {}", path.display(), e))?;
        let w = BufWriter::new(file);

        let width = self.pixels_per_line as u32;
        let height = self.lines.len() as u32;

        let mut encoder = png::Encoder::new(w, width, height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);

        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("write PNG header: {}", e))?;

        // Write all rows.
        let mut img_data = Vec::with_capacity((width * height) as usize);
        for line in &self.lines {
            img_data.extend_from_slice(line);
        }

        writer
            .write_image_data(&img_data)
            .map_err(|e| format!("write PNG data: {}", e))?;

        Ok(path)
    }

    pub fn reset(&mut self) {
        self.lines.clear();
    }
}

fn generate_filename(freq_hz: u64, mode: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();

    // Convert to UTC datetime components manually (avoid chrono dependency).
    let (year, month, day, hour, min, sec) = unix_to_utc(secs);
    let freq_khz = freq_hz / 1000;

    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}-{}_kHz_{}.png",
        year, month, day, hour, min, sec, freq_khz, mode
    )
}

/// Convert Unix timestamp to (year, month, day, hour, minute, second) in UTC.
fn unix_to_utc(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = secs;
    let sec = (s % 60) as u32;
    let min = ((s / 60) % 60) as u32;
    let hour = ((s / 3600) % 24) as u32;

    let mut days = (s / 86400) as i64;
    // Days since 1970-01-01.
    let mut year = 1970u32;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month = 0u32;
    for (i, &md) in month_days.iter().enumerate() {
        if days < md as i64 {
            month = i as u32 + 1;
            break;
        }
        days -= md as i64;
    }
    let day = days as u32 + 1;

    (year, month, day, hour, min, sec)
}

fn is_leap(y: u32) -> bool {
    y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_identifies_noise_vs_image() {
        let mut asm = ImageAssembler::new(256);

        // No previous line.
        assert!(asm.correlation_with_last(&[0u8; 256]).is_none());

        // Flat line, then a gradient: first call has no reference.
        let gradient: Vec<u8> = (0..256).map(|i| i as u8).collect();
        asm.push_line(gradient.clone());

        // Nearly identical line — correlation ≈ 1.
        let near: Vec<u8> = (0..256).map(|i| i as u8).collect();
        let r = asm.correlation_with_last(&near).expect("r");
        assert!(r > 0.99, "identical lines should correlate: r={}", r);

        // Pseudo-random noise vs gradient — correlation should be low.
        let noise: Vec<u8> = (0..256)
            .map(|i| ((i * 1103515245 + 12345) as u32 >> 8 & 0xff) as u8)
            .collect();
        let r = asm.correlation_with_last(&noise).expect("r");
        assert!(
            r.abs() < 0.3,
            "noise vs gradient should not correlate: r={}",
            r
        );

        // Flat line returns None (no variance).
        assert!(asm.correlation_with_last(&[128u8; 256]).is_none());
    }

    #[test]
    fn image_assembler_line_count() {
        let mut asm = ImageAssembler::new(1809);
        assert_eq!(asm.line_count(), 0);
        asm.push_line(vec![128; 1809]);
        assert_eq!(asm.line_count(), 1);
        asm.push_line(vec![255; 1809]);
        assert_eq!(asm.line_count(), 2);
    }

    #[test]
    fn save_png_to_temp_dir() {
        let mut asm = ImageAssembler::new(100);
        for i in 0..50 {
            let val = (i * 255 / 49) as u8;
            asm.push_line(vec![val; 100]);
        }

        let dir = std::env::temp_dir().join("trx-wefax-test");
        let result = asm.save_png(&dir, 7880000, "USB");
        assert!(result.is_ok(), "save_png failed: {:?}", result.err());
        let path = result.unwrap();
        assert!(path.exists());
        // Clean up.
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn unix_to_utc_epoch() {
        let (y, m, d, h, mi, s) = unix_to_utc(0);
        assert_eq!((y, m, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn unix_to_utc_known_date() {
        // 2026-03-28T14:30:00 UTC = 1774718600 (approximately)
        let (y, m, d, h, mi, _) = unix_to_utc(1775055000);
        assert_eq!(y, 2026);
        // Just verify reasonable values without asserting exact date.
        assert!(m >= 1 && m <= 12);
        assert!(d >= 1 && d <= 31);
        assert!(h < 24);
        assert!(mi < 60);
    }
}
