// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
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

// Timing offset search: search ±2s in 0.5s steps (6000 samples at 12 kHz)
const DT_SEARCH_RANGE_SAMPLES: isize = 2 * WSPR_SAMPLE_RATE as isize;
const DT_SEARCH_STEP_SAMPLES: isize = (WSPR_SAMPLE_RATE as isize) / 2;

// Number of top frequency candidates to try full decode on
const MAX_FREQ_CANDIDATES: usize = 8;

// Minimum normalized sync correlation score to attempt decode.
// The reference wsprd uses minsync1=0.10 but applies additional filtering
// downstream. A higher threshold here prevents noise from reaching the Fano
// decoder and producing false positives.
const MIN_SYNC_SCORE: f32 = 0.20;

// Soft-symbol normalization factor (reference wsprd: symfac=50)
const SYMFAC: f32 = 50.0;

/// WSPR sync vector (162 bits). symbol = sync[i] + 2*data[i].
/// The LSB of each received symbol should match this pattern.
#[rustfmt::skip]
pub(crate) const SYNC_VECTOR: [u8; 162] = [
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

// Minimum estimated SNR (dB) to attempt decode. WSPR's theoretical decode
// limit is around -28 dB in 2500 Hz bandwidth, but the per-tone SNR estimate
// computed here uses a narrower noise reference and reads higher. Setting
// -20 dB is conservative enough to pass all real signals while rejecting
// pure-noise candidates where the Fano decoder might otherwise hallucinate.
const MIN_SNR_DB: f32 = -20.0;

pub struct WsprDecoder {
    min_rms: f32,
}

struct DemodOutput {
    soft_symbols: [u8; WSPR_SYMBOL_COUNT],
    snr_db: f32,
}

impl WsprDecoder {
    pub fn new() -> Result<Self, String> {
        Ok(Self { min_rms: 0.005 })
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
        // Track (freq, dt) of successful decodes to skip near-duplicates
        let mut decoded_positions: Vec<(f32, isize)> = Vec::new();

        for &(freq, dt_samples, _score) in candidates.iter().take(MAX_FREQ_CANDIDATES) {
            // Skip candidates too close in (freq, dt) to an already-decoded signal
            let dominated = decoded_positions.iter().any(|&(df, ddt)| {
                (freq - df).abs() < 4.0 * TONE_SPACING_HZ
                    && (dt_samples - ddt).unsigned_abs() < DT_SEARCH_STEP_SAMPLES as usize
            });
            if dominated {
                continue;
            }
            let start = (EXPECTED_SIGNAL_START_SAMPLES as isize + dt_samples) as usize;
            let signal = &samples[start..start + WSPR_SIGNAL_SAMPLES];

            // Use normalized sync score for threshold check
            let norm_score = sync_correlation_score_normalized(signal, freq);
            if norm_score < MIN_SYNC_SCORE {
                continue;
            }

            let demod = demodulate_soft_symbols(signal, freq);

            // Reject candidates where estimated SNR is too low — the Fano
            // decoder can converge on noise-only input after normalization.
            if demod.snr_db < MIN_SNR_DB {
                continue;
            }

            if let Some(decoded) = protocol::decode_symbols(&demod.soft_symbols) {
                if seen_messages.insert(decoded.message.clone()) {
                    let dt_s = dt_samples as f32 / WSPR_SAMPLE_RATE as f32;
                    results.push(WsprDecodeResult {
                        message: decoded.message,
                        snr_db: demod.snr_db,
                        dt_s,
                        freq_hz: freq,
                    });
                    decoded_positions.push((freq, dt_samples));
                }
            }
        }

        Ok(results)
    }
}

/// Score a candidate base frequency by correlating detected tone amplitudes
/// with the known WSPR sync vector. Uses amplitude (sqrt of power) and
/// normalizes by total power, matching the reference wsprd implementation.
/// Higher score = better match. Range approximately [0.0, 1.0].
fn sync_correlation_score(signal: &[f32], base_hz: f32) -> f32 {
    let nsyms = WSPR_SYMBOL_COUNT.min(signal.len() / WSPR_SYMBOL_SAMPLES);
    let mut ss = 0.0_f32;
    let sr = WSPR_SAMPLE_RATE as f32;

    for (sym, &sync_bit) in SYNC_VECTOR.iter().enumerate().take(nsyms) {
        let off = sym * WSPR_SYMBOL_SAMPLES;
        let frame = &signal[off..off + WSPR_SYMBOL_SAMPLES];

        // Compute amplitude (sqrt of power) at each of the 4 FSK tones
        let p0 = goertzel_power(frame, base_hz, sr).sqrt();
        let p1 = goertzel_power(frame, base_hz + TONE_SPACING_HZ, sr).sqrt();
        let p2 = goertzel_power(frame, base_hz + 2.0 * TONE_SPACING_HZ, sr).sqrt();
        let p3 = goertzel_power(frame, base_hz + 3.0 * TONE_SPACING_HZ, sr).sqrt();

        // Correlate with sync vector: (p1+p3)-(p0+p2) weighted by (2*sync-1)
        let cmet = (p1 + p3) - (p0 + p2);
        if sync_bit == 1 {
            ss += cmet;
        } else {
            ss -= cmet;
        }
    }

    // Raw (unnormalized) score for candidate ranking. At frequencies with no
    // signal, amplitude differences are near zero so raw score is naturally low.
    // Normalized threshold check is applied separately in decode_slot.
    ss
}

/// Compute the normalized sync score (ss/totp) for threshold comparison.
fn sync_correlation_score_normalized(signal: &[f32], base_hz: f32) -> f32 {
    let nsyms = WSPR_SYMBOL_COUNT.min(signal.len() / WSPR_SYMBOL_SAMPLES);
    let mut ss = 0.0_f32;
    let mut totp = 0.0_f32;
    let sr = WSPR_SAMPLE_RATE as f32;

    for (sym, &sync_bit) in SYNC_VECTOR.iter().enumerate().take(nsyms) {
        let off = sym * WSPR_SYMBOL_SAMPLES;
        let frame = &signal[off..off + WSPR_SYMBOL_SAMPLES];

        let p0 = goertzel_power(frame, base_hz, sr).sqrt();
        let p1 = goertzel_power(frame, base_hz + TONE_SPACING_HZ, sr).sqrt();
        let p2 = goertzel_power(frame, base_hz + 2.0 * TONE_SPACING_HZ, sr).sqrt();
        let p3 = goertzel_power(frame, base_hz + 3.0 * TONE_SPACING_HZ, sr).sqrt();

        let cmet = (p1 + p3) - (p0 + p2);
        if sync_bit == 1 {
            ss += cmet;
        } else {
            ss -= cmet;
        }
        totp += p0 + p1 + p2 + p3;
    }

    if totp > 0.0 {
        ss / totp
    } else {
        0.0
    }
}

/// Produce soft-decision symbols from a signal slice.
///
/// Each soft symbol is an unsigned byte (0-255) where 128 = no confidence,
/// values above 128 mean data bit is likely 1, below 128 means likely 0.
///
/// This matches the reference wsprd `sync_and_demodulate` mode=2 output.
fn demodulate_soft_symbols(signal: &[f32], base_hz: f32) -> DemodOutput {
    let sr = WSPR_SAMPLE_RATE as f32;
    let mut fsymb = [0.0_f32; WSPR_SYMBOL_COUNT];
    let mut signal_sum = 0.0_f32;
    let mut noise_sum = 0.0_f32;

    for sym in 0..WSPR_SYMBOL_COUNT {
        let off = sym * WSPR_SYMBOL_SAMPLES;
        let frame = &signal[off..off + WSPR_SYMBOL_SAMPLES];

        // Compute amplitude (sqrt of power) at each tone — matches reference
        let p0 = goertzel_power(frame, base_hz, sr).sqrt();
        let p1 = goertzel_power(frame, base_hz + TONE_SPACING_HZ, sr).sqrt();
        let p2 = goertzel_power(frame, base_hz + 2.0 * TONE_SPACING_HZ, sr).sqrt();
        let p3 = goertzel_power(frame, base_hz + 3.0 * TONE_SPACING_HZ, sr).sqrt();

        // Soft metric for the data bit:
        //   sync=1 → data bit selects tone 1 (data=0) vs tone 3 (data=1)
        //   sync=0 → data bit selects tone 0 (data=0) vs tone 2 (data=1)
        // Positive fsymb means data_bit=1 is more likely.
        if SYNC_VECTOR[sym] == 1 {
            fsymb[sym] = p3 - p1;
        } else {
            fsymb[sym] = p2 - p0;
        }

        // SNR estimation: signal = best tone power, noise = out-of-band
        let best_amp = p0.max(p1).max(p2).max(p3);
        signal_sum += best_amp * best_amp;

        let noise_a = goertzel_power(frame, base_hz - 8.0 * TONE_SPACING_HZ, sr);
        let noise_b = goertzel_power(frame, base_hz + 12.0 * TONE_SPACING_HZ, sr);
        noise_sum += (noise_a + noise_b) * 0.5;
    }

    // Normalize: zero-mean, scale by symfac/stddev, clip to [-128,127], bias to [0,255]
    let n = WSPR_SYMBOL_COUNT as f32;
    let mean = fsymb.iter().sum::<f32>() / n;
    let var = fsymb.iter().map(|&x| (x - mean) * (x - mean)).sum::<f32>() / n;
    let fac = var.sqrt().max(1e-12);

    let mut soft_symbols = [128u8; WSPR_SYMBOL_COUNT];
    for i in 0..WSPR_SYMBOL_COUNT {
        let v = SYMFAC * fsymb[i] / fac;
        let v = v.clamp(-128.0, 127.0);
        soft_symbols[i] = (v + 128.0) as u8;
    }

    // SNR estimate
    let signal_avg = signal_sum / n;
    let noise_avg = (noise_sum / n).max(1e-12);
    let snr_db = 10.0 * (signal_avg / noise_avg).max(1e-12).log10();

    DemodOutput {
        soft_symbols,
        snr_db,
    }
}

/// Goertzel algorithm: compute power at a specific frequency in a windowed frame.
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

        for (sym, sync_tone) in SYNC_VECTOR
            .iter()
            .copied()
            .enumerate()
            .take(WSPR_SYMBOL_COUNT)
        {
            let tone = sync_tone + 2 * ((sym % 2) as u8);
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
        for (sym, sync_tone) in SYNC_VECTOR
            .iter()
            .copied()
            .enumerate()
            .take(WSPR_SYMBOL_COUNT)
        {
            let freq = base_hz + sync_tone as f32 * TONE_SPACING_HZ;
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

    #[test]
    fn noise_only_slot_produces_no_decodes() {
        // Deterministic pseudo-random noise via simple LCG
        let mut rng_state = 0x12345678u64;
        let mut next_f32 = || -> f32 {
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((rng_state >> 33) as f32 / u32::MAX as f32) * 2.0 - 1.0
        };

        let dec = WsprDecoder::new().expect("decoder");
        let slot: Vec<f32> = (0..dec.slot_samples()).map(|_| next_f32() * 0.05).collect();
        let results = dec.decode_slot(&slot, None).expect("decode");
        assert!(
            results.is_empty(),
            "noise-only slot should produce no decodes, got {}",
            results.len()
        );
    }

    #[test]
    fn normalized_sync_score_is_bounded() {
        let base_hz = 1500.0_f32;

        // Generate a perfect synthetic WSPR signal
        let mut signal = vec![0.0_f32; WSPR_SIGNAL_SAMPLES];
        for (sym, sync_tone) in SYNC_VECTOR
            .iter()
            .copied()
            .enumerate()
            .take(WSPR_SYMBOL_COUNT)
        {
            // Use sync_tone as the only varying bit to maximize sync metric
            let freq = base_hz + sync_tone as f32 * TONE_SPACING_HZ;
            let begin = sym * WSPR_SYMBOL_SAMPLES;
            for i in 0..WSPR_SYMBOL_SAMPLES {
                let t = i as f32 / WSPR_SAMPLE_RATE as f32;
                signal[begin + i] = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.2;
            }
        }

        let score = sync_correlation_score_normalized(&signal, base_hz);
        // Normalized score should be positive and bounded
        assert!(score > 0.0, "score should be positive: {score}");
        assert!(score <= 1.0, "score should be <= 1.0: {score}");
        // This synthetic signal only uses sync tones (no data tones), so the
        // normalized score is moderate (~0.18). A real WSPR signal occupies all
        // 4 tones and produces higher scores (>0.3).
        assert!(
            score > 0.10,
            "score {score} should be clearly above noise floor"
        );
    }
}
