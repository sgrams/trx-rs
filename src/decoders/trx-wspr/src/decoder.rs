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
const BASE_SEARCH_STEP_HZ: f32 = 2.0;
const FINE_SEARCH_STEP_HZ: f32 = 0.25;

// Timing offset search: search ±2s in 0.5s steps (4800 samples at 12 kHz)
const DT_SEARCH_RANGE_SAMPLES: isize = 2 * WSPR_SAMPLE_RATE as isize;
const DT_SEARCH_STEP_SAMPLES: isize = (WSPR_SAMPLE_RATE as isize) / 2;

// Number of top frequency candidates to try full decode on
const MAX_FREQ_CANDIDATES: usize = 8;

// Minimum sync correlation score to attempt a full decode.  Candidates below
// this threshold are almost certainly noise and skipping them avoids expensive
// Fano decode attempts that would produce false positives.
const MIN_SYNC_SCORE: f32 = 10.0;

/// WSPR sync vector (162 bits). symbol = sync[i] + 2*data[i].
/// The LSB of each received symbol should match this pattern.
#[rustfmt::skip]
const SYNC_VECTOR: [u8; 162] = [
    1,1,0,0,0,0,0,0,1,0,0,0,1,1,1,0,0,0,1,0,0,1,0,1,1,1,1,0,0,0,
    0,0,0,0,1,0,0,1,0,1,0,0,0,0,0,0,1,0,1,1,0,0,1,1,0,1,0,0,0,1,
    1,0,1,0,0,0,0,1,1,0,1,0,1,0,1,0,1,0,0,1,0,0,1,0,1,1,0,0,0,1,
    1,0,1,0,1,0,0,0,1,0,0,0,0,0,1,0,0,1,0,0,1,1,1,0,1,1,0,0,1,1,
    0,1,0,0,0,1,1,1,0,0,0,0,0,1,0,1,0,0,1,1,0,0,0,0,0,0,0,1,1,0,
    1,0,1,1,0,0,0,1,1,0,0,1,
];

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

        // Collect top frequency candidates across timing offsets
        let mut candidates: Vec<(f32, isize, f32)> = Vec::new(); // (freq, dt_samples, score)

        let mut dt = -DT_SEARCH_RANGE_SAMPLES;
        while dt <= DT_SEARCH_RANGE_SAMPLES {
            let start = EXPECTED_SIGNAL_START_SAMPLES as isize + dt;
            if start < 0 || (start as usize) + WSPR_SIGNAL_SAMPLES > samples.len() {
                dt += DT_SEARCH_STEP_SAMPLES;
                continue;
            }
            let signal = &samples[start as usize..start as usize + WSPR_SIGNAL_SAMPLES];

            // Coarse frequency search using sync vector correlation
            let mut freq_scores: Vec<(f32, f32)> = Vec::new();
            let mut freq = BASE_SEARCH_MIN_HZ;
            while freq <= BASE_SEARCH_MAX_HZ {
                let score = sync_correlation_score(signal, freq);
                freq_scores.push((freq, score));
                freq += BASE_SEARCH_STEP_HZ;
            }

            // Keep top candidates from coarse search
            freq_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for &(coarse_freq, _) in freq_scores.iter().take(3) {
                // Fine-tune frequency around each coarse candidate
                let mut best_fine_freq = coarse_freq;
                let mut best_fine_score = f32::MIN;
                let mut fine_freq = coarse_freq - BASE_SEARCH_STEP_HZ;
                while fine_freq <= coarse_freq + BASE_SEARCH_STEP_HZ {
                    let score = sync_correlation_score(signal, fine_freq);
                    if score > best_fine_score {
                        best_fine_score = score;
                        best_fine_freq = fine_freq;
                    }
                    fine_freq += FINE_SEARCH_STEP_HZ;
                }
                candidates.push((best_fine_freq, dt, best_fine_score));
            }
            dt += DT_SEARCH_STEP_SAMPLES;
        }

        // Sort candidates by score (best first) and try to decode each
        candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        let mut results = Vec::new();
        let mut seen_messages = std::collections::HashSet::new();

        for &(freq, dt_samples, score) in candidates.iter().take(MAX_FREQ_CANDIDATES) {
            if score < MIN_SYNC_SCORE {
                break; // candidates are sorted by score, no point continuing
            }
            let start = (EXPECTED_SIGNAL_START_SAMPLES as isize + dt_samples) as usize;
            let signal = &samples[start..start + WSPR_SIGNAL_SAMPLES];

            let demod = demodulate_symbols(signal, freq);
            if let Some(decoded) = protocol::decode_symbols(&demod.symbols) {
                if seen_messages.insert(decoded.message.clone()) {
                    let dt_s = dt_samples as f32 / WSPR_SAMPLE_RATE as f32;
                    results.push(WsprDecodeResult {
                        message: decoded.message,
                        snr_db: demod.snr_db,
                        dt_s,
                        freq_hz: freq,
                    });
                }
            }
        }

        Ok(results)
    }
}

#[derive(Debug, Clone)]
struct DemodOutput {
    symbols: Vec<u8>,
    snr_db: f32,
}

/// Score a candidate base frequency by correlating detected symbol LSBs with
/// the known WSPR sync vector. Higher score = better match.
fn sync_correlation_score(signal: &[f32], base_hz: f32) -> f32 {
    let nsyms = WSPR_SYMBOL_COUNT.min(signal.len() / WSPR_SYMBOL_SAMPLES);
    let mut score = 0.0_f32;
    for (sym, &sync_bit) in SYNC_VECTOR.iter().enumerate().take(nsyms) {
        let off = sym * WSPR_SYMBOL_SAMPLES;
        let frame = &signal[off..off + WSPR_SYMBOL_SAMPLES];
        // Sum power in even tones (0,2) vs odd tones (1,3)
        let p0 = goertzel_power(frame, base_hz, WSPR_SAMPLE_RATE as f32);
        let p2 = goertzel_power(
            frame,
            base_hz + 2.0 * TONE_SPACING_HZ,
            WSPR_SAMPLE_RATE as f32,
        );
        let p1 = goertzel_power(frame, base_hz + TONE_SPACING_HZ, WSPR_SAMPLE_RATE as f32);
        let p3 = goertzel_power(
            frame,
            base_hz + 3.0 * TONE_SPACING_HZ,
            WSPR_SAMPLE_RATE as f32,
        );

        let even_power = p0 + p2; // tones with LSB=0
        let odd_power = p1 + p3; // tones with LSB=1

        // Correlate with sync vector: sync=1 means odd tone expected
        if sync_bit == 1 {
            score += odd_power - even_power;
        } else {
            score += even_power - odd_power;
        }
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
            let tone = SYNC_VECTOR[sym] + 2 * ((sym % 2) as u8);
            let freq = base_hz + tone as f32 * TONE_SPACING_HZ;
            let begin = start + sym * WSPR_SYMBOL_SAMPLES;
            for i in 0..WSPR_SYMBOL_SAMPLES {
                let t = i as f32 / WSPR_SAMPLE_RATE as f32;
                slot[begin + i] = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.2;
            }
        }

        let signal = &slot[start..start + WSPR_SIGNAL_SAMPLES];
        let candidates = find_candidates(signal);
        assert!(!candidates.is_empty());
        let (estimated, _) = candidates[0];
        assert!(
            (estimated - base_hz).abs() <= 1.0,
            "estimated {estimated} Hz, expected {base_hz} Hz"
        );
    }

    /// Helper: run the candidate search on a signal slice
    fn find_candidates(signal: &[f32]) -> Vec<(f32, f32)> {
        let mut freq_scores: Vec<(f32, f32)> = Vec::new();
        let mut freq = BASE_SEARCH_MIN_HZ;
        while freq <= BASE_SEARCH_MAX_HZ {
            let score = sync_correlation_score(signal, freq);
            freq_scores.push((freq, score));
            freq += BASE_SEARCH_STEP_HZ;
        }
        freq_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Fine-tune top result
        if let Some(&(coarse_freq, _)) = freq_scores.first() {
            let mut best_fine_freq = coarse_freq;
            let mut best_fine_score = f32::MIN;
            let mut fine_freq = coarse_freq - BASE_SEARCH_STEP_HZ;
            while fine_freq <= coarse_freq + BASE_SEARCH_STEP_HZ {
                let score = sync_correlation_score(signal, fine_freq);
                if score > best_fine_score {
                    best_fine_score = score;
                    best_fine_freq = fine_freq;
                }
                fine_freq += FINE_SEARCH_STEP_HZ;
            }
            vec![(best_fine_freq, best_fine_score)]
        } else {
            vec![]
        }
    }

    #[test]
    fn sync_correlation_prefers_correct_frequency() {
        let base_hz = 1500.0_f32;
        let wrong_hz = 1400.0_f32;

        // Generate a synthetic WSPR-like signal using the sync vector
        let mut signal = vec![0.0_f32; WSPR_SIGNAL_SAMPLES];
        for sym in 0..WSPR_SYMBOL_COUNT {
            let tone = SYNC_VECTOR[sym]; // just sync, no data
            let freq = base_hz + tone as f32 * TONE_SPACING_HZ;
            let begin = sym * WSPR_SYMBOL_SAMPLES;
            for i in 0..WSPR_SYMBOL_SAMPLES {
                let t = i as f32 / WSPR_SAMPLE_RATE as f32;
                signal[begin + i] = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.2;
            }
        }

        let correct_score = sync_correlation_score(&signal, base_hz);
        let wrong_score = sync_correlation_score(&signal, wrong_hz);
        assert!(
            correct_score > wrong_score,
            "correct={correct_score}, wrong={wrong_score}"
        );
    }
}
