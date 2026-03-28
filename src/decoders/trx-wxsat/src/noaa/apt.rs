// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! APT (Automatic Picture Transmission) demodulator and line decoder.
//!
//! Weather satellite APT signal chain:
//!   FM-demodulated audio → 2400 Hz AM subcarrier → envelope → 4160 Hz image
//!
//! Frame layout at 4160 Hz (2080 samples = 0.5 s per line, 2 lines/sec):
//!   [SyncA 39][SpaceA 47][ImageA 909][TelA 45][SyncB 39][SpaceB 47][ImageB 909][TelB 45]

use num_complex::Complex;
use rustfft::FftPlanner;
use std::sync::Arc;

pub const APT_RATE: u32 = 4160;
pub const LINE_SAMPLES: usize = 2080;

// Line layout offsets (samples into a line at APT_RATE Hz)
pub const SYNC_A_LEN: usize = 39;
const SPACE_A_LEN: usize = 47;
pub const IMAGE_A_LEN: usize = 909;
const TEL_A_LEN: usize = 45;
const SYNC_B_LEN: usize = 39;
const SPACE_B_LEN: usize = 47;
pub const IMAGE_B_LEN: usize = 909;

pub const IMAGE_A_OFFSET: usize = SYNC_A_LEN + SPACE_A_LEN; // 86
pub const IMAGE_B_OFFSET: usize =
    IMAGE_A_OFFSET + IMAGE_A_LEN + TEL_A_LEN + SYNC_B_LEN + SPACE_B_LEN; // 1126

// FFT block size for Hilbert-based AM demodulation
const BLOCK_SIZE: usize = 4096;

// Sync detection parameters
const SYNC_THRESHOLD: f32 = 0.15;
const SYNC_SEARCH_LOCKED: usize = 12; // ±samples around expected sync position when locked
const MAX_BAD_SYNC_LINES: u32 = 8; // unlock after this many low-confidence lines

/// Telemetry block length (samples per channel).
pub const TEL_LEN: usize = 45;

/// A decoded APT line: raw pixel arrays for both image channels plus telemetry.
#[derive(Clone)]
pub struct RawLine {
    pub pixels_a: Box<[u8; IMAGE_A_LEN]>,
    pub pixels_b: Box<[u8; IMAGE_B_LEN]>,
    /// Telemetry block A (45 samples, normalised to 0-255).
    pub tel_a: Box<[u8; TEL_LEN]>,
    /// Telemetry block B (45 samples, normalised to 0-255).
    pub tel_b: Box<[u8; TEL_LEN]>,
    pub line_no: u32,
}

/// Sync A reference pattern at APT_RATE Hz.
///
/// 1040 Hz square wave (period = 4 samples): alternating pairs hi/lo.
fn sync_a_ref() -> [f32; SYNC_A_LEN] {
    let mut p = [0.0f32; SYNC_A_LEN];
    for (i, v) in p.iter_mut().enumerate() {
        // 7 cycles of alternating pairs, rest is zero (end-of-sync blank)
        *v = if i < 28 && (i % 4) < 2 { 1.0 } else { -1.0 };
    }
    p
}

/// Compute normalised cross-correlation of `buf[offset..]` with the sync A
/// reference pattern.  Returns a value approximately in `[-1.0, 1.0]`.
fn sync_score(buf: &[f32], offset: usize) -> f32 {
    if offset + SYNC_A_LEN > buf.len() {
        return 0.0;
    }
    let ref_pat = sync_a_ref();
    let window = &buf[offset..offset + SYNC_A_LEN];
    let mean = window.iter().sum::<f32>() / SYNC_A_LEN as f32;
    let rms =
        (window.iter().map(|&x| (x - mean) * (x - mean)).sum::<f32>() / SYNC_A_LEN as f32).sqrt();
    if rms < 1e-7 {
        return 0.0;
    }
    window
        .iter()
        .zip(ref_pat.iter())
        .map(|(&s, &r)| (s - mean) * r)
        .sum::<f32>()
        / (SYNC_A_LEN as f32 * rms)
}

/// Find the offset in `buf[0..search_len]` with the highest sync A score.
/// Returns `(offset, score)`.
fn find_best_sync(buf: &[f32], search_len: usize) -> (usize, f32) {
    let limit = search_len.min(buf.len().saturating_sub(SYNC_A_LEN));
    let mut best_off = 0usize;
    let mut best_score = f32::NEG_INFINITY;
    for off in 0..=limit {
        let s = sync_score(buf, off);
        if s > best_score {
            best_score = s;
            best_off = off;
        }
    }
    (best_off, best_score)
}

// ---------------------------------------------------------------------------
// AM demodulator (Hilbert-based via rustfft)
// ---------------------------------------------------------------------------

/// Converts PCM at `sample_rate` to APT envelope samples at `APT_RATE` Hz.
///
/// Uses an FFT-based Hilbert transform to obtain the analytic signal, then
/// extracts the AM envelope.  Processes input in non-overlapping blocks of
/// `BLOCK_SIZE` samples.
pub struct AptDemod {
    fft_fwd: Arc<dyn rustfft::Fft<f32>>,
    fft_inv: Arc<dyn rustfft::Fft<f32>>,
    /// Lower bin index of the 2400 Hz ± 1040 Hz bandpass filter.
    k_lo: usize,
    /// Upper bin index of the 2400 Hz ± 1040 Hz bandpass filter.
    k_hi: usize,
    /// Input sample accumulation buffer.
    in_buf: Vec<f32>,
    /// Fractional position into the next input block for the resampler.
    resamp_phase: f64,
    /// Input samples consumed per APT_RATE output sample.
    resamp_step: f64,
    /// Output envelope buffer at APT_RATE Hz.
    pub out: Vec<f32>,
}

impl AptDemod {
    pub fn new(sample_rate: u32) -> Self {
        let mut planner = FftPlanner::new();
        let fft_fwd = planner.plan_fft_forward(BLOCK_SIZE);
        let fft_inv = planner.plan_fft_inverse(BLOCK_SIZE);

        let fs = sample_rate as f64;
        // Bandpass around 2400 Hz carrier, ±1040 Hz (APT image bandwidth)
        let k_lo = ((1360.0 * BLOCK_SIZE as f64 / fs).floor() as usize).max(1);
        let k_hi = ((3440.0 * BLOCK_SIZE as f64 / fs).ceil() as usize).min(BLOCK_SIZE / 2);

        Self {
            fft_fwd,
            fft_inv,
            k_lo,
            k_hi,
            in_buf: Vec::new(),
            resamp_phase: 0.0,
            resamp_step: sample_rate as f64 / APT_RATE as f64,
            out: Vec::new(),
        }
    }

    /// Push raw PCM samples; envelope output accumulates in `self.out`.
    pub fn push(&mut self, samples: &[f32]) {
        self.in_buf.extend_from_slice(samples);
        while self.in_buf.len() >= BLOCK_SIZE {
            // Drain exactly BLOCK_SIZE samples into a stack array
            let block: Vec<f32> = self.in_buf.drain(..BLOCK_SIZE).collect();
            self.process_block(&block);
        }
    }

    fn process_block(&mut self, block: &[f32]) {
        // Forward FFT
        let mut spectrum: Vec<Complex<f32>> = block.iter().map(|&s| Complex::new(s, 0.0)).collect();
        self.fft_fwd.process(&mut spectrum);

        // Bandpass + analytic signal:
        //   - Keep positive-freq band [k_lo, k_hi], doubled (for single-sideband power).
        //   - Zero all other bins (negative freqs and out-of-band positives).
        // IFFT of the resulting one-sided spectrum ≈ analytic signal of the
        // bandpass-filtered input; its magnitude is the AM envelope.
        for (k, bin) in spectrum.iter_mut().enumerate() {
            if k >= self.k_lo && k <= self.k_hi {
                *bin *= 2.0;
            } else {
                *bin = Complex::ZERO;
            }
        }

        // Inverse FFT → complex analytic signal
        self.fft_inv.process(&mut spectrum);
        let scale = 1.0_f32 / BLOCK_SIZE as f32;

        // Magnitude = AM envelope; resample to APT_RATE via linear interpolation
        let n = BLOCK_SIZE as f64;
        while self.resamp_phase + 1.0 < n {
            let i = self.resamp_phase as usize;
            let frac = (self.resamp_phase - i as f64) as f32;
            let s0 = spectrum[i].norm() * scale;
            let s1 = spectrum[i + 1].norm() * scale;
            self.out.push(s0 + frac * (s1 - s0));
            self.resamp_phase += self.resamp_step;
        }
        self.resamp_phase -= n;
        if self.resamp_phase < 0.0 {
            self.resamp_phase = 0.0;
        }
    }

    pub fn reset(&mut self) {
        self.in_buf.clear();
        self.out.clear();
        self.resamp_phase = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Sync tracker and line assembler
// ---------------------------------------------------------------------------

/// Consumes resampled APT envelope samples (at `APT_RATE` Hz), detects
/// sync A markers, and assembles decoded image lines.
pub struct SyncTracker {
    buf: Vec<f32>,
    locked: bool,
    bad_sync_count: u32,
    line_no: u32,
    pub lines: Vec<RawLine>,
}

impl Default for SyncTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncTracker {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            locked: false,
            bad_sync_count: 0,
            line_no: 0,
            lines: Vec::new(),
        }
    }

    /// Push envelope samples; fully assembled lines accumulate in `self.lines`.
    pub fn push(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);
        self.drain();
    }

    fn drain(&mut self) {
        loop {
            if !self.locked {
                // Need 2 × LINE_SAMPLES to have room for a full line after scan
                if self.buf.len() < 2 * LINE_SAMPLES {
                    break;
                }
                let (pos, score) = find_best_sync(&self.buf, LINE_SAMPLES);
                if score >= SYNC_THRESHOLD {
                    self.buf.drain(0..pos);
                    self.locked = true;
                    // Fall through to locked extraction below
                } else {
                    // No convincing sync yet; discard half a line and retry
                    self.buf.drain(0..LINE_SAMPLES / 2);
                    continue;
                }
            }

            // Locked mode: need LINE_SAMPLES + search window to be available
            if self.buf.len() < LINE_SAMPLES + SYNC_SEARCH_LOCKED {
                break;
            }

            // Refine within a small window around the expected line start
            let (drift, score) = find_best_sync(&self.buf, SYNC_SEARCH_LOCKED);
            if score >= SYNC_THRESHOLD * 0.5 {
                // Accept refined position
                if drift > 0 {
                    self.buf.drain(0..drift);
                }
                self.bad_sync_count = 0;
            } else {
                self.bad_sync_count += 1;
                if self.bad_sync_count >= MAX_BAD_SYNC_LINES {
                    self.locked = false;
                    self.bad_sync_count = 0;
                    // Discard the current suspect region and restart search
                    self.buf.drain(0..LINE_SAMPLES / 2);
                    continue;
                }
                // Keep going at the expected position (don't drift on bad sync)
            }

            if self.buf.len() < LINE_SAMPLES {
                break;
            }
            self.extract_line();
        }
    }

    fn extract_line(&mut self) {
        let samples: Vec<f32> = self.buf.drain(0..LINE_SAMPLES).collect();

        // Per-line normalisation: scale to [0, 255] using the 2nd–98th percentile
        // of this line's values to clip noise and hot pixels.
        let mut sorted: Vec<f32> = samples.clone();
        sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p_lo = sorted[(sorted.len() * 2 / 100).max(0)];
        let p_hi = sorted[(sorted.len() * 98 / 100).min(sorted.len() - 1)];
        let range = (p_hi - p_lo).max(1e-6);

        let norm = |v: f32| -> u8 { ((v - p_lo) / range * 255.0).round().clamp(0.0, 255.0) as u8 };

        let mut pixels_a = Box::new([0u8; IMAGE_A_LEN]);
        for (i, p) in pixels_a.iter_mut().enumerate() {
            *p = norm(samples[IMAGE_A_OFFSET + i]);
        }
        let mut pixels_b = Box::new([0u8; IMAGE_B_LEN]);
        for (i, p) in pixels_b.iter_mut().enumerate() {
            *p = norm(samples[IMAGE_B_OFFSET + i]);
        }

        // Extract telemetry blocks (adjacent to image data)
        let tel_a_offset = IMAGE_A_OFFSET + IMAGE_A_LEN; // right after image A
        let tel_b_offset = IMAGE_B_OFFSET + IMAGE_B_LEN; // right after image B
        let mut tel_a = Box::new([0u8; TEL_LEN]);
        for (i, p) in tel_a.iter_mut().enumerate() {
            if tel_a_offset + i < LINE_SAMPLES {
                *p = norm(samples[tel_a_offset + i]);
            }
        }
        let mut tel_b = Box::new([0u8; TEL_LEN]);
        for (i, p) in tel_b.iter_mut().enumerate() {
            if tel_b_offset + i < LINE_SAMPLES {
                *p = norm(samples[tel_b_offset + i]);
            }
        }

        self.lines.push(RawLine {
            pixels_a,
            pixels_b,
            tel_a,
            tel_b,
            line_no: self.line_no,
        });
        self.line_no += 1;
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.locked = false;
        self.bad_sync_count = 0;
        self.line_no = 0;
        self.lines.clear();
    }
}
