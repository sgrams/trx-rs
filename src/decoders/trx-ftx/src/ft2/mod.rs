// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! FT2 pipeline orchestration.
//!
//! Implements the full FT2 decode flow: accumulate raw audio, find frequency
//! peaks in the averaged spectrum, downsample each candidate, compute 2D sync
//! scores, extract bit metrics, and run multi-pass LDPC + OSD decode.

pub mod bitmetrics;
pub mod downsample;
pub mod osd;
pub mod sync;

use num_complex::Complex32;

use crate::constants::FT4_XOR_SEQUENCE;
use crate::crc::{ftx_compute_crc, ftx_extract_crc};
use crate::decode::{pack_bits, FtxMessage};
use crate::ldpc;
use crate::protocol::*;

use downsample::DownsampleContext;
use sync::{prepare_sync_waveforms, sync2d_score};

// FT2 DSP constants
pub const FT2_NDOWN: usize = 9;
pub const FT2_NFFT1: usize = 1152;
pub const FT2_NH1: usize = FT2_NFFT1 / 2;
pub const FT2_NSTEP: usize = 288;
pub const FT2_NMAX: usize = 45000;
pub const FT2_MAX_RAW_CANDIDATES: usize = 96;
pub const FT2_MAX_SCAN_HITS: usize = 128;
pub const FT2_SYNC_TWEAK_MIN: i32 = -16;
pub const FT2_SYNC_TWEAK_MAX: i32 = 16;
pub const FT2_NSS: usize = FT2_NSTEP / FT2_NDOWN;
pub const FT2_FRAME_SYMBOLS: usize = FT2_NN - FT2_NR;
pub const FT2_FRAME_SAMPLES: usize = FT2_FRAME_SYMBOLS * FT2_NSS;
pub const FT2_SYMBOL_PERIOD_F: f32 = FT2_SYMBOL_PERIOD;

/// Maximum hard-error count for accepting an OSD result.
const FT2_OSD_MAX_HARD_ERRORS: usize = 36;

/// Frequency offset applied to FT2 candidates.
pub fn ft2_frequency_offset_hz() -> f32 {
    -1.5 / FT2_SYMBOL_PERIOD_F
}

/// Raw frequency peak candidate from the averaged power spectrum.
#[derive(Clone, Copy, Default)]
pub struct RawCandidate {
    pub freq_hz: f32,
    pub score: f32,
}

/// Scan hit with refined sync parameters.
#[derive(Clone, Copy, Default)]
pub struct ScanHit {
    pub freq_hz: f32,
    pub snr0: f32,
    pub sync_score: f32,
    pub start: i32,
    pub idf: i32,
}

/// Statistics from the scan phase.
#[derive(Clone, Default)]
pub struct ScanStats {
    pub peaks_found: usize,
    pub hits_found: usize,
    pub best_peak_score: f32,
    pub best_sync_score: f32,
}

/// Failure stage classification for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailStage {
    None,
    RefinedSync,
    FreqRange,
    FinalDownsample,
    BitMetrics,
    SyncQual,
    Ldpc,
    Crc,
    Unpack,
}

/// Per-pass diagnostic information.
#[derive(Clone)]
pub struct PassDiag {
    pub ntype: [i32; 5],
    pub nharderror: [i32; 5],
    pub dmin: [f32; 5],
}

impl Default for PassDiag {
    fn default() -> Self {
        Self {
            ntype: [0; 5],
            nharderror: [-1; 5],
            dmin: [f32::INFINITY; 5],
        }
    }
}

/// Decoded FT2 result with timing and frequency metadata.
#[derive(Clone)]
pub struct Ft2DecodeResult {
    pub message: FtxMessage,
    pub dt_s: f32,
    pub freq_hz: f32,
    pub snr_db: f32,
}

/// FT2 pipeline state. Accumulates raw audio and runs the full decode flow.
pub struct Ft2Pipeline {
    sample_rate: f32,
    raw_audio: Vec<f32>,
    raw_capacity: usize,
}

impl Ft2Pipeline {
    /// Create a new FT2 pipeline for the given sample rate.
    pub fn new(sample_rate: i32) -> Self {
        Self {
            sample_rate: sample_rate as f32,
            raw_audio: Vec::with_capacity(FT2_NMAX),
            raw_capacity: FT2_NMAX,
        }
    }

    /// Reset the pipeline, clearing all accumulated audio.
    pub fn reset(&mut self) {
        self.raw_audio.clear();
    }

    /// Accumulate raw audio samples. Returns true when the buffer is full.
    pub fn accumulate(&mut self, samples: &[f32]) -> bool {
        let remaining = self.raw_capacity.saturating_sub(self.raw_audio.len());
        if remaining > 0 {
            let n = remaining.min(samples.len());
            self.raw_audio.extend_from_slice(&samples[..n]);
        }
        self.raw_audio.len() >= self.raw_capacity
    }

    /// Returns true when enough audio has been accumulated for decoding.
    pub fn is_ready(&self) -> bool {
        self.raw_audio.len() >= self.raw_capacity
    }

    /// Number of raw audio samples accumulated so far.
    pub fn raw_len(&self) -> usize {
        self.raw_audio.len()
    }

    /// Run the full FT2 decode pipeline. Returns decoded messages.
    pub fn decode(&self, max_results: usize) -> Vec<Ft2DecodeResult> {
        if self.raw_audio.len() < FT2_NFFT1 {
            return Vec::new();
        }

        let ctx = match DownsampleContext::new(&self.raw_audio, self.sample_rate) {
            Some(ctx) => ctx,
            None => return Vec::new(),
        };

        let hits = self.find_scan_hits(&ctx);
        if hits.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::new();
        let mut seen_hashes: Vec<(u16, [u8; FTX_PAYLOAD_LENGTH_BYTES])> = Vec::new();

        for hit in &hits {
            if results.len() >= max_results {
                break;
            }
            if let Some(result) = self.decode_hit(&ctx, hit) {
                // Dedup
                let dominated = seen_hashes.iter().any(|(h, p)| {
                    *h == result.message.hash && *p == result.message.payload
                });
                if dominated {
                    continue;
                }
                seen_hashes.push((result.message.hash, result.message.payload));
                results.push(result);
            }
        }

        results
    }

    /// Find frequency peaks from averaged power spectrum.
    fn find_frequency_peaks(&self) -> Vec<RawCandidate> {
        if self.raw_audio.len() < FT2_NFFT1 {
            return Vec::new();
        }

        let fs = self.sample_rate;
        let df = fs / FT2_NFFT1 as f32;
        let n_frames = 1 + (self.raw_audio.len() - FT2_NFFT1) / FT2_NSTEP;

        // Compute Nuttall window
        let window = nuttall_window(FT2_NFFT1);

        // Forward real FFT setup
        let mut real_planner = realfft::RealFftPlanner::<f32>::new();
        let fft = real_planner.plan_fft_forward(FT2_NFFT1);
        let mut fft_input = fft.make_input_vec();
        let mut fft_output = fft.make_output_vec();
        let mut fft_scratch = fft.make_scratch_vec();

        // Average power spectrum across frames
        let mut avg = vec![0.0f32; FT2_NH1];

        for frame in 0..n_frames {
            let start = frame * FT2_NSTEP;
            for i in 0..FT2_NFFT1 {
                fft_input[i] = self.raw_audio[start + i] * window[i];
            }
            fft.process_with_scratch(&mut fft_input, &mut fft_output, &mut fft_scratch)
                .expect("FFT failed");

            for bin in 1..FT2_NH1 {
                if bin < fft_output.len() {
                    let c = fft_output[bin];
                    let power = c.re * c.re + c.im * c.im;
                    avg[bin] += power;
                }
            }
        }

        for bin in 1..FT2_NH1 {
            avg[bin] /= n_frames as f32;
        }

        // Smooth with 15-point moving average
        let mut smooth = vec![0.0f32; FT2_NH1];
        for bin in 8..FT2_NH1.saturating_sub(8) {
            let mut sum = 0.0f32;
            for i in (bin.saturating_sub(7))..=(bin + 7).min(FT2_NH1 - 1) {
                sum += avg[i];
            }
            smooth[bin] = sum / 15.0;
        }

        // Baseline with 63-point moving average
        let mut baseline = vec![0.0f32; FT2_NH1];
        for bin in 32..FT2_NH1.saturating_sub(32) {
            let mut sum = 0.0f32;
            for i in (bin.saturating_sub(31))..=(bin + 31).min(FT2_NH1 - 1) {
                sum += smooth[i];
            }
            baseline[bin] = sum / 63.0 + 1e-9;
        }

        // Find peaks
        let min_bin = (200.0 / df).round() as usize;
        let max_bin = (4910.0 / df).round() as usize;
        let mut candidates = Vec::new();

        let mut bin = min_bin + 1;
        while bin < max_bin.saturating_sub(1) && candidates.len() < FT2_MAX_RAW_CANDIDATES {
            if baseline[bin] <= 0.0 {
                bin += 1;
                continue;
            }
            let value = smooth[bin] / baseline[bin];
            if value < 1.03 {
                bin += 1;
                continue;
            }

            let left = smooth[bin.saturating_sub(1)] / baseline[bin.saturating_sub(1)].max(1e-9);
            let right = if bin + 1 < FT2_NH1 {
                smooth[bin + 1] / baseline[bin + 1].max(1e-9)
            } else {
                0.0
            };

            if value < left || value < right {
                bin += 1;
                continue;
            }

            let den = left - 2.0 * value + right;
            let delta = if den.abs() > 1e-6 {
                0.5 * (left - right) / den
            } else {
                0.0
            };

            let freq_hz = (bin as f32 + delta) * df + ft2_frequency_offset_hz();
            if !(200.0..=4910.0).contains(&freq_hz) {
                bin += 1;
                continue;
            }

            candidates.push(RawCandidate {
                freq_hz,
                score: value,
            });
            bin += 1;
        }

        // Sort by score descending
        candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        candidates
    }

    /// Find scan hits by downsampling each frequency peak and computing sync scores.
    fn find_scan_hits(&self, ctx: &DownsampleContext) -> Vec<ScanHit> {
        let peaks = self.find_frequency_peaks();
        if peaks.is_empty() {
            return Vec::new();
        }

        let nfft2 = ctx.nfft2();
        let waveforms = prepare_sync_waveforms();

        let mut hits = Vec::new();

        for peak in &peaks {
            if hits.len() >= FT2_MAX_SCAN_HITS {
                break;
            }

            let mut down = vec![Complex32::new(0.0, 0.0); nfft2];
            let produced = ctx.downsample(peak.freq_hz, &mut down);
            if produced == 0 {
                continue;
            }
            normalize_downsampled(&mut down[..produced], produced);

            // Coarse search
            let mut best_score: f32 = -1.0;
            let mut best_start: i32 = 0;
            let mut best_idf: i32 = 0;

            let mut idf = -12i32;
            while idf <= 12 {
                let mut start = -688i32;
                while start <= 2024 {
                    let score = sync2d_score(
                        &down[..produced],
                        start,
                        idf,
                        &waveforms,
                    );
                    if score > best_score {
                        best_score = score;
                        best_start = start;
                        best_idf = idf;
                    }
                    start += 4;
                }
                idf += 3;
            }

            if best_score < 0.50 {
                continue;
            }

            // Fine refinement
            for idf in (best_idf - 4)..=(best_idf + 4) {
                if !(FT2_SYNC_TWEAK_MIN..=FT2_SYNC_TWEAK_MAX).contains(&idf) {
                    continue;
                }
                for start in (best_start - 5)..=(best_start + 5) {
                    let score = sync2d_score(
                        &down[..produced],
                        start,
                        idf,
                        &waveforms,
                    );
                    if score > best_score {
                        best_score = score;
                        best_start = start;
                        best_idf = idf;
                    }
                }
            }

            if best_score < 0.50 {
                continue;
            }

            hits.push(ScanHit {
                freq_hz: peak.freq_hz,
                snr0: peak.score - 1.0,
                sync_score: best_score,
                start: best_start,
                idf: best_idf,
            });
        }

        // Sort by sync score descending
        hits.sort_by(|a, b| {
            b.sync_score
                .partial_cmp(&a.sync_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits
    }

    /// Attempt to decode a single scan hit through the full pipeline.
    fn decode_hit(&self, ctx: &DownsampleContext, hit: &ScanHit) -> Option<Ft2DecodeResult> {
        let nfft2 = ctx.nfft2();
        let waveforms = prepare_sync_waveforms();

        // Initial downsample for sync refinement
        let mut cd2 = vec![Complex32::new(0.0, 0.0); nfft2];
        let produced = ctx.downsample(hit.freq_hz, &mut cd2);
        if produced == 0 {
            return None;
        }
        normalize_downsampled(&mut cd2[..produced], produced);

        // Refine sync
        let mut best_score: f32 = -1.0;
        let mut best_start = hit.start;
        let mut best_idf = hit.idf;

        for idf in (hit.idf - 4)..=(hit.idf + 4) {
            if !(FT2_SYNC_TWEAK_MIN..=FT2_SYNC_TWEAK_MAX).contains(&idf) {
                continue;
            }
            for start in (hit.start - 5)..=(hit.start + 5) {
                let score = sync2d_score(&cd2[..produced], start, idf, &waveforms);
                if score > best_score {
                    best_score = score;
                    best_start = start;
                    best_idf = idf;
                }
            }
        }

        if best_score < 0.65 {
            return None;
        }

        // Frequency correction
        let corrected_freq_hz = hit.freq_hz + best_idf as f32;
        if corrected_freq_hz <= 10.0 || corrected_freq_hz >= 4990.0 {
            return None;
        }

        // Final downsample at corrected frequency
        let mut cb = vec![Complex32::new(0.0, 0.0); nfft2];
        let produced2 = ctx.downsample(corrected_freq_hz, &mut cb);
        if produced2 == 0 {
            return None;
        }
        normalize_downsampled(&mut cb[..produced2], FT2_FRAME_SAMPLES);

        // Extract signal region
        let mut signal = vec![Complex32::new(0.0, 0.0); FT2_FRAME_SAMPLES];
        extract_signal_region(&cb[..produced2], best_start, &mut signal);

        // Extract bit metrics
        let bitmetrics = bitmetrics::extract_bitmetrics_raw(&signal)?;

        // Sync quality check using known Costas bit patterns
        let sync_bits_a: [u8; 8] = [0, 0, 0, 1, 1, 0, 1, 1];
        let sync_bits_b: [u8; 8] = [0, 1, 0, 0, 1, 1, 1, 0];
        let sync_bits_c: [u8; 8] = [1, 1, 1, 0, 0, 1, 0, 0];
        let sync_bits_d: [u8; 8] = [1, 0, 1, 1, 0, 0, 0, 1];
        let mut sync_qual = 0;
        for i in 0..8 {
            sync_qual += if (bitmetrics[i][0] >= 0.0) as u8 == sync_bits_a[i] { 1 } else { 0 };
            sync_qual += if (bitmetrics[66 + i][0] >= 0.0) as u8 == sync_bits_b[i] { 1 } else { 0 };
            sync_qual += if (bitmetrics[132 + i][0] >= 0.0) as u8 == sync_bits_c[i] { 1 } else { 0 };
            sync_qual += if (bitmetrics[198 + i][0] >= 0.0) as u8 == sync_bits_d[i] { 1 } else { 0 };
        }
        if sync_qual < 10 {
            return None;
        }

        // Build 5 LLR passes from the 3 metric scales
        let mut llr_passes = [[0.0f32; FTX_LDPC_N]; 5];
        for i in 0..58 {
            llr_passes[0][i] = bitmetrics[8 + i][0];
            llr_passes[0][58 + i] = bitmetrics[74 + i][0];
            llr_passes[0][116 + i] = bitmetrics[140 + i][0];

            llr_passes[1][i] = bitmetrics[8 + i][1];
            llr_passes[1][58 + i] = bitmetrics[74 + i][1];
            llr_passes[1][116 + i] = bitmetrics[140 + i][1];

            llr_passes[2][i] = bitmetrics[8 + i][2];
            llr_passes[2][58 + i] = bitmetrics[74 + i][2];
            llr_passes[2][116 + i] = bitmetrics[140 + i][2];
        }

        // Scale and derive combined passes
        for i in 0..FTX_LDPC_N {
            llr_passes[0][i] *= 3.2;
            llr_passes[1][i] *= 3.2;
            llr_passes[2][i] *= 3.2;

            let a = llr_passes[0][i];
            let b = llr_passes[1][i];
            let c = llr_passes[2][i];

            // Pass 3: max-abs metric
            llr_passes[3][i] = if a.abs() >= b.abs() && a.abs() >= c.abs() {
                a
            } else if b.abs() >= c.abs() {
                b
            } else {
                c
            };

            // Pass 4: average
            llr_passes[4][i] = (a + b + c) / 3.0;
        }

        // Multi-pass LDPC decode
        let mut ok = false;
        let mut message = FtxMessage::default();
        let mut global_best_errors = FTX_LDPC_M as i32;

        for pass in 0..5 {
            if ok {
                break;
            }
            let mut log174 = llr_passes[pass];
            normalize_log174(&mut log174);

            let mut nharderror = FTX_LDPC_M as i32;

            // BP decode
            let mut bp_plain = [0u8; FTX_LDPC_N];
            let bp_errors = ldpc::bp_decode(&log174, 50, &mut bp_plain);
            if bp_errors < nharderror {
                nharderror = bp_errors;
            }
            if bp_errors == 0 {
                if let Some(msg) = unpack_message(&bp_plain) {
                    message = msg;
                    ok = true;
                    nharderror = 0;
                }
            }

            // Sum-product decode (fallback)
            if !ok {
                let mut sp_log174 = llr_passes[pass];
                normalize_log174(&mut sp_log174);
                let mut sp_plain = [0u8; FTX_LDPC_N];
                let sp_errors = ldpc::ldpc_decode(&mut sp_log174, 50, &mut sp_plain);
                if sp_errors < nharderror {
                    nharderror = sp_errors;
                }
                if sp_errors == 0 {
                    if let Some(msg) = unpack_message(&sp_plain) {
                        message = msg;
                        ok = true;
                        nharderror = 0;
                    }
                }
            }

            if nharderror < global_best_errors {
                global_best_errors = nharderror;
            }
        }

        // CRC-based OSD-1/OSD-2 fallback when LDPC was close to converging
        if !ok && global_best_errors <= 6 {
            for pass in 0..5 {
                if ok {
                    break;
                }
                let mut osd_log174 = llr_passes[pass];
                normalize_log174(&mut osd_log174);
                if let Some(msg) = osd_lite_decode(&osd_log174) {
                    message = msg;
                    ok = true;
                }
            }
        }

        if !ok {
            return None;
        }

        // Compute refined timing via parabolic interpolation
        let sm1 = sync2d_score(&cd2[..produced], best_start - 1, best_idf, &waveforms);
        let sp1 = sync2d_score(&cd2[..produced], best_start + 1, best_idf, &waveforms);
        let mut xstart = best_start as f32;
        let den = sm1 - 2.0 * best_score + sp1;
        if den.abs() > 1e-6 {
            xstart += 0.5 * (sm1 - sp1) / den;
        }

        let dt_s = xstart / (12000.0 / FT2_NDOWN as f32) - 0.5;
        let snr_db = if hit.snr0 > 0.0 {
            (10.0 * hit.snr0.log10() - 13.0).max(-21.0)
        } else {
            -21.0
        };

        Some(Ft2DecodeResult {
            message,
            dt_s,
            freq_hz: corrected_freq_hz,
            snr_db,
        })
    }
}

/// Compute a Nuttall window of length `n`.
fn nuttall_window(n: usize) -> Vec<f32> {
    let a0: f32 = 0.355768;
    let a1: f32 = 0.487396;
    let a2: f32 = 0.144232;
    let a3: f32 = 0.012604;
    (0..n)
        .map(|i| {
            let phase = 2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32;
            a0 - a1 * phase.cos() + a2 * (2.0 * phase).cos() - a3 * (3.0 * phase).cos()
        })
        .collect()
}

/// Normalize complex downsampled signal to unit power.
fn normalize_downsampled(samples: &mut [Complex32], ref_count: usize) {
    let power: f32 = samples.iter().map(|s| s.norm_sqr()).sum();
    if power <= 0.0 {
        return;
    }
    let rc = if ref_count == 0 { samples.len() } else { ref_count };
    let scale = (rc as f32 / power).sqrt();
    for s in samples.iter_mut() {
        *s *= scale;
    }
}

/// Extract a signal region starting at `start` into `out_signal`.
fn extract_signal_region(input: &[Complex32], start: i32, out_signal: &mut [Complex32]) {
    for i in 0..out_signal.len() {
        let src = start + i as i32;
        out_signal[i] = if src >= 0 && (src as usize) < input.len() {
            input[src as usize]
        } else {
            Complex32::new(0.0, 0.0)
        };
    }
}

/// Normalize LLR array (divide by standard deviation).
fn normalize_log174(log174: &mut [f32; FTX_LDPC_N]) {
    let mut sum = 0.0f32;
    let mut sum2 = 0.0f32;
    for &v in log174.iter() {
        sum += v;
        sum2 += v * v;
    }
    let inv_n = 1.0 / FTX_LDPC_N as f32;
    let variance = (sum2 - sum * sum * inv_n) * inv_n;
    if variance <= 1e-12 {
        return;
    }
    let sigma = variance.sqrt();
    for v in log174.iter_mut() {
        *v /= sigma;
    }
}

/// Unpack a 174-bit plaintext into an FtxMessage, verifying CRC and applying XOR sequence.
fn unpack_message(plain174: &[u8; FTX_LDPC_N]) -> Option<FtxMessage> {
    let mut a91 = [0u8; FTX_LDPC_K_BYTES];
    pack_bits(plain174, FTX_LDPC_K, &mut a91);

    let crc_extracted = ftx_extract_crc(&a91);
    a91[9] &= 0xF8;
    a91[10] = 0x00;
    let crc_calculated = ftx_compute_crc(&a91, 96 - 14);

    if crc_extracted != crc_calculated {
        return None;
    }

    // Re-read a91 since we modified it for CRC check
    pack_bits(plain174, FTX_LDPC_K, &mut a91);

    let mut msg = FtxMessage {
        hash: crc_calculated,
        payload: [0; FTX_PAYLOAD_LENGTH_BYTES],
    };
    for i in 0..10 {
        msg.payload[i] = a91[i] ^ FT4_XOR_SEQUENCE[i];
    }
    Some(msg)
}

/// Encode a packed 91-bit message into a 174-bit codeword (bit array).
fn encode_codeword_from_a91(a91: &[u8; FTX_LDPC_K_BYTES]) -> [u8; FTX_LDPC_N] {
    let mut codeword = [0u8; FTX_LDPC_N];
    // Systematic part
    for i in 0..FTX_LDPC_K {
        codeword[i] = (a91[i / 8] >> (7 - (i % 8))) & 0x01;
    }
    // Parity part using generator matrix
    for i in 0..FTX_LDPC_M {
        let mut nsum: u8 = 0;
        for j in 0..FTX_LDPC_K_BYTES {
            let x = a91[j] & crate::constants::FTX_LDPC_GENERATOR[i][j];
            nsum ^= parity8(x);
        }
        codeword[FTX_LDPC_K + i] = nsum & 0x01;
    }
    codeword
}

/// Count parity of a byte.
fn parity8(x: u8) -> u8 {
    let x = x ^ (x >> 4);
    let x = x ^ (x >> 2);
    let x = x ^ (x >> 1);
    x & 1
}

/// Count hard errors between LLR signs and a candidate codeword.
fn count_hard_errors_vs_llr(log174: &[f32; FTX_LDPC_N], codeword: &[u8; FTX_LDPC_N]) -> usize {
    let mut errors = 0;
    for i in 0..FTX_LDPC_N {
        let received = if log174[i] >= 0.0 { 1u8 } else { 0u8 };
        if received != codeword[i] {
            errors += 1;
        }
    }
    errors
}

/// Try a CRC candidate: encode the packed message, verify CRC and hard-error count.
fn try_crc_candidate(
    a91: &[u8; FTX_LDPC_K_BYTES],
    log174: &[f32; FTX_LDPC_N],
) -> Option<FtxMessage> {
    let codeword = encode_codeword_from_a91(a91);

    // Check CRC via unpack
    let mut plain174 = [0u8; FTX_LDPC_N];
    plain174.copy_from_slice(&codeword);
    let msg = unpack_message(&plain174)?;

    // Verify consistency with received LLRs
    if count_hard_errors_vs_llr(log174, &codeword) > FT2_OSD_MAX_HARD_ERRORS {
        return None;
    }

    Some(msg)
}

/// Reliability entry for OSD-lite sorting.
struct ReliabilityEntry {
    index: usize,
    reliability: f32,
}

/// CRC-guided OSD-1/OSD-2 lite decoder.
///
/// Tries flipping each of the 16 least-reliable systematic bits (OSD-1),
/// then all pairs (OSD-2). Returns decoded message on CRC match.
fn osd_lite_decode(log174: &[f32; FTX_LDPC_N]) -> Option<FtxMessage> {
    // Build base hard decision from systematic bits
    let mut base_a91 = [0u8; FTX_LDPC_K_BYTES];
    for i in 0..FTX_LDPC_K {
        if log174[i] >= 0.0 {
            base_a91[i / 8] |= 0x80u8 >> (i % 8);
        }
    }

    // Try base (zero flips)
    if let Some(msg) = try_crc_candidate(&base_a91, log174) {
        return Some(msg);
    }

    // Sort systematic bits by reliability (ascending = least reliable first)
    let mut rel: Vec<ReliabilityEntry> = (0..FTX_LDPC_K)
        .map(|i| ReliabilityEntry {
            index: i,
            reliability: log174[i].abs(),
        })
        .collect();
    rel.sort_by(|a, b| {
        a.reliability
            .partial_cmp(&b.reliability)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let max_candidates = 16.min(FTX_LDPC_K);

    // OSD-1: single bit flips
    for i in 0..max_candidates {
        let mut trial = base_a91;
        let b0 = rel[i].index;
        trial[b0 / 8] ^= 0x80u8 >> (b0 % 8);
        if let Some(msg) = try_crc_candidate(&trial, log174) {
            return Some(msg);
        }
    }

    // OSD-2: all pairs
    for i in 0..max_candidates {
        for j in (i + 1)..max_candidates {
            let mut trial = base_a91;
            let b0 = rel[i].index;
            let b1 = rel[j].index;
            trial[b0 / 8] ^= 0x80u8 >> (b0 % 8);
            trial[b1 / 8] ^= 0x80u8 >> (b1 % 8);
            if let Some(msg) = try_crc_candidate(&trial, log174) {
                return Some(msg);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nuttall_window_length() {
        let w = nuttall_window(64);
        assert_eq!(w.len(), 64);
    }

    #[test]
    fn nuttall_window_symmetric() {
        let w = nuttall_window(128);
        for i in 0..64 {
            assert!(
                (w[i] - w[127 - i]).abs() < 1e-6,
                "Window not symmetric at index {}",
                i
            );
        }
    }

    #[test]
    fn pipeline_accumulate() {
        let mut pipe = Ft2Pipeline::new(12000);
        let samples = vec![0.0f32; 1000];
        assert!(!pipe.accumulate(&samples));
        assert_eq!(pipe.raw_len(), 1000);
    }

    #[test]
    fn pipeline_ready() {
        let mut pipe = Ft2Pipeline::new(12000);
        let samples = vec![0.0f32; FT2_NMAX];
        assert!(pipe.accumulate(&samples));
        assert!(pipe.is_ready());
    }

    #[test]
    fn normalize_downsampled_zero_power() {
        let mut samples = vec![Complex32::new(0.0, 0.0); 16];
        normalize_downsampled(&mut samples, 16);
        // Should not crash or produce NaN
        for s in &samples {
            assert!(!s.re.is_nan());
            assert!(!s.im.is_nan());
        }
    }

    #[test]
    fn parity8_basic() {
        assert_eq!(parity8(0x00), 0);
        assert_eq!(parity8(0x01), 1);
        assert_eq!(parity8(0x03), 0);
        assert_eq!(parity8(0xFF), 0);
    }

    #[test]
    fn encode_codeword_all_zeros() {
        let a91 = [0u8; FTX_LDPC_K_BYTES];
        let cw = encode_codeword_from_a91(&a91);
        for &b in &cw {
            assert_eq!(b, 0);
        }
    }
}
