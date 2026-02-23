// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use crate::protocol;

const WSPR_SAMPLE_RATE: u32 = 12_000;
const SLOT_SAMPLES: usize = 120 * WSPR_SAMPLE_RATE as usize;
const WSPR_SYMBOL_COUNT: usize = 162;
const WSPR_SYMBOL_SAMPLES: usize = 8192;
const WSPR_SIGNAL_SAMPLES: usize = WSPR_SYMBOL_COUNT * WSPR_SYMBOL_SAMPLES;
const EXPECTED_SIGNAL_START_SAMPLES: usize = WSPR_SAMPLE_RATE as usize; // 1s
const TONE_SPACING_HZ: f32 = WSPR_SAMPLE_RATE as f32 / WSPR_SYMBOL_SAMPLES as f32; // 1.46484375

// Coarse search range for base tone. This matches common WSPR audio passband.
const BASE_SEARCH_MIN_HZ: f32 = 1200.0;
const BASE_SEARCH_MAX_HZ: f32 = 1800.0;
const BASE_SEARCH_STEP_HZ: f32 = 4.0;
const COARSE_SYMBOLS: usize = 48;

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

        let rms = slot_rms(&samples[..SLOT_SAMPLES]);
        if rms < self.min_rms {
            return Ok(Vec::new());
        }

        let start = EXPECTED_SIGNAL_START_SAMPLES;
        if start + WSPR_SIGNAL_SAMPLES > samples.len() {
            return Ok(Vec::new());
        }
        let signal = &samples[start..start + WSPR_SIGNAL_SAMPLES];

        let Some(base_hz) = estimate_base_tone_hz(signal) else {
            return Ok(Vec::new());
        };
        let demod = demodulate_symbols(signal, base_hz);
        let Some(decoded) = protocol::decode_symbols(&demod.symbols) else {
            return Ok(Vec::new());
        };

        Ok(vec![WsprDecodeResult {
            message: decoded.message,
            snr_db: demod.snr_db,
            dt_s: 0.0,
            freq_hz: base_hz,
        }])
    }
}

#[derive(Debug, Clone)]
struct DemodOutput {
    symbols: Vec<u8>,
    snr_db: f32,
}

fn estimate_base_tone_hz(signal: &[f32]) -> Option<f32> {
    if signal.len() < WSPR_SYMBOL_SAMPLES * COARSE_SYMBOLS {
        return None;
    }

    let mut best_freq = BASE_SEARCH_MIN_HZ;
    let mut best_score = f32::MIN;
    let mut freq = BASE_SEARCH_MIN_HZ;
    while freq <= BASE_SEARCH_MAX_HZ {
        let score = coarse_score(signal, freq);
        if score > best_score {
            best_score = score;
            best_freq = freq;
        }
        freq += BASE_SEARCH_STEP_HZ;
    }
    Some(best_freq)
}

fn coarse_score(signal: &[f32], base_hz: f32) -> f32 {
    let mut score = 0.0_f32;
    for sym in 0..COARSE_SYMBOLS {
        let off = sym * WSPR_SYMBOL_SAMPLES;
        let frame = &signal[off..off + WSPR_SYMBOL_SAMPLES];
        let mut best = 0.0_f32;
        for tone in 0..4 {
            let hz = base_hz + tone as f32 * TONE_SPACING_HZ;
            let p = goertzel_power(frame, hz, WSPR_SAMPLE_RATE as f32);
            if p > best {
                best = p;
            }
        }
        score += best;
    }
    score
}

fn demodulate_symbols(signal: &[f32], base_hz: f32) -> DemodOutput {
    let mut symbols = Vec::with_capacity(WSPR_SYMBOL_COUNT);
    let mut signal_sum = 0.0_f32;
    let mut noise_sum = 0.0_f32;

    for sym in 0..WSPR_SYMBOL_COUNT {
        let off = sym * WSPR_SYMBOL_SAMPLES;
        let frame = &signal[off..off + WSPR_SYMBOL_SAMPLES];

        let mut tone_power = [0.0_f32; 4];
        for (i, power) in tone_power.iter_mut().enumerate() {
            let hz = base_hz + i as f32 * TONE_SPACING_HZ;
            *power = goertzel_power(frame, hz, WSPR_SAMPLE_RATE as f32);
        }

        let mut best_idx = 0_u8;
        let mut best_pow = tone_power[0];
        for (idx, p) in tone_power.iter().enumerate().skip(1) {
            if *p > best_pow {
                best_pow = *p;
                best_idx = idx as u8;
            }
        }

        symbols.push(best_idx);
        signal_sum += best_pow;

        let noise_a = goertzel_power(
            frame,
            base_hz - 8.0 * TONE_SPACING_HZ,
            WSPR_SAMPLE_RATE as f32,
        );
        let noise_b = goertzel_power(
            frame,
            base_hz + 12.0 * TONE_SPACING_HZ,
            WSPR_SAMPLE_RATE as f32,
        );
        noise_sum += (noise_a + noise_b) * 0.5;
    }

    let signal_avg = signal_sum / WSPR_SYMBOL_COUNT as f32;
    let noise_avg = (noise_sum / WSPR_SYMBOL_COUNT as f32).max(1e-12);
    let snr_db = 10.0 * (signal_avg / noise_avg).max(1e-12).log10();

    DemodOutput { symbols, snr_db }
}

fn goertzel_power(frame: &[f32], target_hz: f32, sample_rate: f32) -> f32 {
    let n = frame.len() as f32;
    let k = (0.5 + (n * target_hz / sample_rate)).floor();
    let w = 2.0 * std::f32::consts::PI * k / n;
    let coeff = 2.0 * w.cos();

    let mut s_prev = 0.0_f32;
    let mut s_prev2 = 0.0_f32;
    for (idx, &x) in frame.iter().enumerate() {
        let win = 0.5_f32 - 0.5_f32 * (2.0_f32 * std::f32::consts::PI * idx as f32 / n).cos();
        let s = x * win + coeff * s_prev - s_prev2;
        s_prev2 = s_prev;
        s_prev = s;
    }

    s_prev2 * s_prev2 + s_prev * s_prev - coeff * s_prev * s_prev2
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

    #[test]
    fn base_search_finds_synthetic_signal() {
        let mut slot = vec![0.0_f32; SLOT_SAMPLES];
        let base_hz = 1496.0_f32;
        let start = EXPECTED_SIGNAL_START_SAMPLES;

        for sym in 0..WSPR_SYMBOL_COUNT {
            let tone = (sym % 4) as f32;
            let freq = base_hz + tone * TONE_SPACING_HZ;
            let begin = start + sym * WSPR_SYMBOL_SAMPLES;
            for i in 0..WSPR_SYMBOL_SAMPLES {
                let t = i as f32 / WSPR_SAMPLE_RATE as f32;
                slot[begin + i] = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.2;
            }
        }

        let signal = &slot[start..start + WSPR_SIGNAL_SAMPLES];
        let estimated = estimate_base_tone_hz(signal).expect("base tone");
        assert!((estimated - base_hz).abs() <= BASE_SEARCH_STEP_HZ);
    }
}
