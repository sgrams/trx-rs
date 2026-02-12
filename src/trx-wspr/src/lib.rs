// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

const WSPR_SAMPLE_RATE: u32 = 12_000;
const SLOT_SAMPLES: usize = 120 * WSPR_SAMPLE_RATE as usize;

#[derive(Debug, Clone)]
pub struct WsprDecodeResult {
    pub message: String,
    pub snr_db: f32,
    pub dt_s: f32,
    pub freq_hz: f32,
}

pub struct WsprDecoder {
    binary: String,
}

impl WsprDecoder {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            binary: "wsprd".to_string(),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        WSPR_SAMPLE_RATE
    }

    pub fn slot_samples(&self) -> usize {
        SLOT_SAMPLES
    }

    pub fn decode_slot(
        &self,
        samples: &[f32],
        base_freq_hz: Option<u64>,
    ) -> Result<Vec<WsprDecodeResult>, String> {
        if samples.len() < SLOT_SAMPLES {
            return Ok(Vec::new());
        }

        let wav_path = self.write_temp_wav(samples)?;
        let output = Command::new(&self.binary)
            .arg(&wav_path)
            .output()
            .map_err(|e| format!("failed to run {}: {}", self.binary, e))?;

        let _ = fs::remove_file(&wav_path);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "wsprd failed with status {}: {}",
                output.status,
                stderr.trim()
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_wsprd_output(&stdout, base_freq_hz))
    }

    fn write_temp_wav(&self, samples: &[f32]) -> Result<PathBuf, String> {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "trx-wspr-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis()
        );
        path.push(unique);

        let mut file = fs::File::create(&path)
            .map_err(|e| format!("failed to create temp wav {}: {}", path.display(), e))?;

        let num_samples = samples.len() as u32;
        let data_bytes = num_samples * 2;
        let riff_size = 36 + data_bytes;
        let byte_rate = WSPR_SAMPLE_RATE * 2;

        file.write_all(b"RIFF").map_err(|e| e.to_string())?;
        file.write_all(&riff_size.to_le_bytes())
            .map_err(|e| e.to_string())?;
        file.write_all(b"WAVE").map_err(|e| e.to_string())?;
        file.write_all(b"fmt ").map_err(|e| e.to_string())?;
        file.write_all(&16u32.to_le_bytes())
            .map_err(|e| e.to_string())?; // PCM fmt size
        file.write_all(&1u16.to_le_bytes())
            .map_err(|e| e.to_string())?; // PCM format
        file.write_all(&1u16.to_le_bytes())
            .map_err(|e| e.to_string())?; // channels
        file.write_all(&WSPR_SAMPLE_RATE.to_le_bytes())
            .map_err(|e| e.to_string())?;
        file.write_all(&byte_rate.to_le_bytes())
            .map_err(|e| e.to_string())?;
        file.write_all(&2u16.to_le_bytes())
            .map_err(|e| e.to_string())?; // block align
        file.write_all(&16u16.to_le_bytes())
            .map_err(|e| e.to_string())?; // bits/sample
        file.write_all(b"data").map_err(|e| e.to_string())?;
        file.write_all(&data_bytes.to_le_bytes())
            .map_err(|e| e.to_string())?;

        for &sample in samples.iter().take(SLOT_SAMPLES) {
            let clamped = sample.clamp(-1.0, 1.0);
            let pcm = (clamped * i16::MAX as f32) as i16;
            file.write_all(&pcm.to_le_bytes())
                .map_err(|e| e.to_string())?;
        }

        Ok(path)
    }
}

fn parse_wsprd_output(output: &str, base_freq_hz: Option<u64>) -> Vec<WsprDecodeResult> {
    output
        .lines()
        .filter_map(|line| parse_wsprd_line(line, base_freq_hz))
        .collect()
}

fn parse_wsprd_line(line: &str, base_freq_hz: Option<u64>) -> Option<WsprDecodeResult> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 6 {
        return None;
    }

    let snr_db: f32 = fields.get(1)?.parse().ok()?;
    let dt_s: f32 = fields.get(2)?.parse().ok()?;
    let decoded_freq_hz: f32 = fields.get(3)?.parse().ok()?;

    let message = fields.iter().skip(5).copied().collect::<Vec<_>>().join(" ");
    if message.is_empty() {
        return None;
    }

    let freq_hz = if let Some(base) = base_freq_hz {
        decoded_freq_hz - base as f32
    } else {
        decoded_freq_hz
    };

    Some(WsprDecodeResult {
        message,
        snr_db,
        dt_s,
        freq_hz,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_basic() {
        let line = "0001 -24 0.3 14097100 -1 CQ TEST FN20 37";
        let parsed = parse_wsprd_line(line, Some(14_097_000)).expect("parse");
        assert_eq!(parsed.message, "CQ TEST FN20 37");
        assert_eq!(parsed.snr_db, -24.0);
        assert_eq!(parsed.dt_s, 0.3);
        assert_eq!(parsed.freq_hz, 100.0);
    }
}
