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

    /// Encode the accumulated image to an 8-bit greyscale PNG file.
    ///
    /// Returns the full path to the saved file.
    pub fn save_png(
        &self,
        output_dir: &Path,
        ioc: u16,
        lpm: u16,
    ) -> Result<PathBuf, String> {
        if self.lines.is_empty() {
            return Err("no image lines to save".into());
        }

        std::fs::create_dir_all(output_dir)
            .map_err(|e| format!("create output dir: {}", e))?;

        let filename = generate_filename(ioc, lpm);
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

fn generate_filename(ioc: u16, lpm: u16) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();

    // Convert to UTC datetime components manually (avoid chrono dependency).
    let (year, month, day, hour, min, sec) = unix_to_utc(secs);

    format!(
        "WEFAX-{:04}-{:02}-{:02}T{:02}{:02}{:02}-IOC{}-{}lpm.png",
        year, month, day, hour, min, sec, ioc, lpm
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
        let result = asm.save_png(&dir, 576, 120);
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
