// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

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
    min_rms: f32,
}

impl WsprDecoder {
    pub fn new() -> Result<Self, String> {
        Ok(Self { min_rms: 0.0005 })
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
        _base_freq_hz: Option<u64>,
    ) -> Result<Vec<WsprDecodeResult>, String> {
        if samples.len() < SLOT_SAMPLES {
            return Ok(Vec::new());
        }

        // Native Rust implementation scaffold:
        // keep a strict "no decode on noise-only slots" gate while protocol/DSP
        // stages are implemented.
        let rms = slot_rms(&samples[..SLOT_SAMPLES]);
        if rms < self.min_rms {
            return Ok(Vec::new());
        }

        Ok(Vec::new())
    }
}

fn slot_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq = samples.iter().map(|s| s * s).sum::<f32>();
    (sum_sq / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_slot_returns_empty() {
        let dec = WsprDecoder::new().expect("decoder");
        let out = dec.decode_slot(&vec![0.0; dec.slot_samples() - 1], None);
        assert!(out.expect("decode").is_empty());
    }

    #[test]
    fn rms_is_zero_for_silence() {
        let rms = slot_rms(&[0.0; 16]);
        assert_eq!(rms, 0.0);
    }
}
