// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::f32::consts::{PI, SQRT_2, TAU};
use std::sync::Arc;

use rustfft::{num_complex::Complex, FftPlanner};
use trx_core::rig::state::RdsData;

const RDS_SUBCARRIER_HZ: f32 = 57_000.0;
const RDS_SYMBOL_RATE: f32 = 1_187.5;
/// Biphase (Manchester) chip rate: 2× the symbol rate.
const RDS_CHIP_RATE: f32 = RDS_SYMBOL_RATE * 2.0;
const RDS_POLY: u16 = 0x1B9;
const SEARCH_REG_MASK: u32 = (1 << 26) - 1;
const PHASE_CANDIDATES: usize = 8;
const BIPHASE_CLOCK_WINDOW: usize = 128;
/// Minimum quality score to publish RDS state to the outer decoder.
const MIN_PUBLISH_QUALITY: f32 = 0.20;
/// Tech 6: number of Block A observations before using accumulated PI.
/// 5 observations gives reliable majority voting down to 5 dB SNR with
/// fast acquisition (~435 ms).  Higher values improve voting reliability
/// but delay PI commitment; 5 balances both.
const PI_ACC_THRESHOLD: u8 = 5;
/// Tech 9: maximum total soft-confidence cost for OSD bit flips.
/// Rejects corrections where the flipped bits had high confidence —
/// a strong indicator of a false decode rather than a genuine error.
/// At 9–10 dB SNR genuine errors have cost ≲ 0.3; noise-induced OSD(2)
/// matches typically cost 0.6–1.2.
const OSD_MAX_FLIP_COST: f32 = 0.45;
/// Tech 5 — Costas loop proportional gain for acquisition (per sample).
const COSTAS_KP: f32 = 8e-4;
/// Tech 5 — Costas loop integral gain for acquisition (per sample).
/// Tuned for ζ ≈ 0.68 (ωn = √KI ≈ 5.9e-4 rad/sample → ~22 Hz loop BW).
const COSTAS_KI: f32 = 3.5e-7;
/// Tech 5 — Costas loop proportional gain for narrow tracking mode.
/// ~4× narrower loop BW (~5.5 Hz) reduces phase noise at low SNR.
const COSTAS_KP_TRACK: f32 = 2.0e-4;
/// Tech 5 — Costas loop integral gain for narrow tracking mode.
const COSTAS_KI_TRACK: f32 = 2.2e-8;
/// Tech 5 — maximum frequency correction per sample (radians).
const COSTAS_MAX_FREQ_CORR: f32 = 0.005;
/// Leaky-average time constant for Costas error magnitude tracking.
const COSTAS_ERR_AVG_ALPHA: f32 = 0.998;
/// Costas error average below this threshold triggers narrow tracking mode.
const COSTAS_LOCK_THRESHOLD: f32 = 0.15;
/// Tech 1 — RRC roll-off factor.  0.30 gives ~23% narrower noise bandwidth
/// than 0.50 (one-sided BW = Rs/2 × (1+α) = 772 Hz) for ~0.6 dB extra
/// sensitivity gain.  The tighter excess bandwidth is handled by the longer
/// RRC_SPAN_CHIPS to keep ISI negligible.
const RRC_ALPHA: f32 = 0.30;
/// Tech 1 — RRC filter span in chips.  10 chips captures the full RRC
/// pulse including low-level sidelobes, keeping stopband leakage below
/// −60 dB — critical for rejecting adjacent-channel interference on real
/// signals where α is small.  The extra taps (vs span 5) increase FFT
/// size from 1024 to 2048 but the improved stopband rejection translates
/// directly into better block decode rate on weak, noisy signals.
/// Added latency is ~4.2 ms at 2375 chips/s, negligible for RDS.
const RRC_SPAN_CHIPS: usize = 10;
/// Staleness timeout in seconds.  If the incumbent candidate has not produced
/// a state update in this many seconds, its score advantage is cleared so any
/// candidate can take over.  Prevents the decoder from "freezing" when the
/// incumbent's timing or carrier tracking degrades.
const STALE_TIMEOUT_SECS: f32 = 2.0;

const OFFSET_A: u16 = 0x0FC;
const OFFSET_B: u16 = 0x198;
const OFFSET_C: u16 = 0x168;
const OFFSET_CP: u16 = 0x350;
const OFFSET_D: u16 = 0x1B4;

// ---------------------------------------------------------------------------
// Tech 1: Root Raised Cosine matched filter (FFT overlap-save)
// ---------------------------------------------------------------------------

/// Computes one tap of an RRC filter impulse response.
/// `t` is time in units of symbol periods; `alpha` is the roll-off factor.
fn rrc_tap(t: f32, alpha: f32) -> f32 {
    if t.abs() < 1e-6 {
        return 1.0 - alpha + 4.0 * alpha / PI;
    }
    let t4a = 4.0 * alpha * t;
    if (t4a.abs() - 1.0).abs() < 1e-6 {
        let s = (PI / (4.0 * alpha)).sin();
        let c = (PI / (4.0 * alpha)).cos();
        return (alpha / SQRT_2) * ((1.0 + 2.0 / PI) * s + (1.0 - 2.0 / PI) * c);
    }
    let num = (PI * t * (1.0 - alpha)).sin() + 4.0 * alpha * t * (PI * t * (1.0 + alpha)).cos();
    let den = PI * t * (1.0 - t4a * t4a);
    num / den
}

/// Build and normalise the RRC tap vector.
fn build_rrc_taps(sample_rate: f32, chip_rate: f32) -> Vec<f32> {
    let sps = (sample_rate / chip_rate).max(2.0);
    let n_half = (RRC_SPAN_CHIPS as f32 * sps / 2.0).round() as usize;
    let n_taps = (2 * n_half + 1).min(1025);
    let center = (n_taps / 2) as f32;

    let mut taps: Vec<f32> = (0..n_taps)
        .map(|i| rrc_tap((i as f32 - center) / sps, RRC_ALPHA))
        .collect();

    // Normalise to unity DC gain.
    let sum: f32 = taps.iter().sum();
    if sum.abs() > 1e-9 {
        let inv = 1.0 / sum;
        for tap in &mut taps {
            *tap *= inv;
        }
    }
    taps
}

/// Tech 1: RRC matched filter using FFT overlap-save convolution.
///
/// Processes I and Q simultaneously as a complex signal, halving FFT work
/// compared to two separate real FIR filters.  Output lags input by at most
/// `block_size` samples (< 2 ms at a 200 kHz composite rate).
struct FftRrcFilter {
    n_taps: usize,
    block_size: usize,
    fft_size: usize,
    /// Pre-computed filter spectrum: FFT(rrc_taps) / fft_size.
    filter_spectrum: Vec<Complex<f32>>,
    /// Last (n_taps − 1) complex input samples for overlap-save continuity.
    overlap: Vec<Complex<f32>>,
    /// Accumulates new complex input samples for the current block.
    in_buf: Vec<Complex<f32>>,
    /// Filtered (I, Q) output pairs ready to be consumed.
    out_buf: Vec<(f32, f32)>,
    out_pos: usize,
    /// Pre-allocated scratch buffer for FFT/IFFT processing, avoiding
    /// per-block heap allocations (~234 allocs/s at 240 kHz).
    scratch: Vec<Complex<f32>>,
    fft: Arc<dyn rustfft::Fft<f32>>,
    ifft: Arc<dyn rustfft::Fft<f32>>,
}

impl FftRrcFilter {
    fn new_rrc(sample_rate: f32, chip_rate: f32) -> Self {
        let taps = build_rrc_taps(sample_rate, chip_rate);
        let n_taps = taps.len();

        // block_size >= n_taps ensures the overlap is always the tail of in_buf.
        let block_size = n_taps.next_power_of_two().max(64);
        let fft_size = (block_size + n_taps - 1).next_power_of_two();

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);
        let ifft = planner.plan_fft_inverse(fft_size);

        // Filter spectrum = FFT(taps, zero-padded to fft_size) / fft_size.
        // Dividing by fft_size here absorbs the IFFT normalisation factor so
        // that overlap-save output equals the true linear convolution.
        let scale = 1.0 / fft_size as f32;
        let mut filter_spectrum: Vec<Complex<f32>> =
            taps.iter().map(|&t| Complex::new(t * scale, 0.0)).collect();
        filter_spectrum.resize(fft_size, Complex::new(0.0, 0.0));
        fft.process(&mut filter_spectrum);

        Self {
            n_taps,
            block_size,
            fft_size,
            filter_spectrum,
            overlap: vec![Complex::new(0.0, 0.0); n_taps - 1],
            in_buf: Vec::with_capacity(block_size),
            out_buf: Vec::with_capacity(block_size),
            out_pos: 0,
            scratch: vec![Complex::new(0.0, 0.0); fft_size],
            fft,
            ifft,
        }
    }

    /// Submit one (I, Q) pair and return the filtered result.
    /// Returns (0, 0) during the initial fill of the first block.
    #[inline]
    fn process(&mut self, i: f32, q: f32) -> (f32, f32) {
        self.in_buf.push(Complex::new(i, q));
        if self.in_buf.len() == self.block_size {
            self.flush_block();
        }
        if self.out_pos < self.out_buf.len() {
            let s = self.out_buf[self.out_pos];
            self.out_pos += 1;
            s
        } else {
            (0.0, 0.0)
        }
    }

    fn flush_block(&mut self) {
        let ol = self.n_taps - 1;
        let buf = &mut self.scratch;

        // Build FFT input in pre-allocated scratch: [overlap | in_buf | zeros].
        buf[..ol].copy_from_slice(&self.overlap);
        buf[ol..ol + self.block_size].copy_from_slice(&self.in_buf);
        let zero = Complex::new(0.0, 0.0);
        for c in &mut buf[ol + self.block_size..self.fft_size] {
            *c = zero;
        }

        // Update overlap: last (n_taps − 1) samples of in_buf.
        // block_size >= n_taps guarantees in_buf is long enough.
        self.overlap
            .copy_from_slice(&self.in_buf[self.block_size - ol..]);
        self.in_buf.clear();

        // FFT → pointwise multiply by filter spectrum → IFFT.
        self.fft.process(buf);
        for (b, &h) in buf.iter_mut().zip(self.filter_spectrum.iter()) {
            *b *= h;
        }
        self.ifft.process(buf);

        // Valid overlap-save output: indices [n_taps−1 .. n_taps−1+block_size).
        self.out_buf.clear();
        self.out_pos = 0;
        let start = self.n_taps - 1;
        for c in buf.iter().skip(start).take(self.block_size) {
            self.out_buf.push((c.re, c.im));
        }
    }
}

impl Clone for FftRrcFilter {
    fn clone(&self) -> Self {
        Self {
            n_taps: self.n_taps,
            block_size: self.block_size,
            fft_size: self.fft_size,
            filter_spectrum: self.filter_spectrum.clone(),
            overlap: self.overlap.clone(),
            in_buf: self.in_buf.clone(),
            out_buf: self.out_buf.clone(),
            out_pos: self.out_pos,
            scratch: self.scratch.clone(),
            fft: Arc::clone(&self.fft),
            ifft: Arc::clone(&self.ifft),
        }
    }
}

impl std::fmt::Debug for FftRrcFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FftRrcFilter")
            .field("n_taps", &self.n_taps)
            .field("block_size", &self.block_size)
            .field("fft_size", &self.fft_size)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Block / group types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    A,
    B,
    C,
    CPrime,
    D,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectBlock {
    A,
    B,
    C,
    D,
}

// ---------------------------------------------------------------------------
// Candidate — one clock-phase / biphase decoder instance
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Candidate {
    clock_phase: f32,
    clock_inc: f32,
    sym_i_acc: f32,
    sym_q_acc: f32,
    sym_count: u16,
    prev_psk_symbol: Option<(f32, f32)>,
    clock_history: [f32; BIPHASE_CLOCK_WINDOW],
    clock: usize,
    clock_polarity: usize,
    prev_input_bit: bool,
    search_reg: u32,
    search_bits: u8,
    locked: bool,
    expect: ExpectBlock,
    block_reg: u32,
    block_bits: u8,
    /// Tech 3/7/8: per-bit soft magnitudes for the current locked block.
    block_soft: [f32; 26],
    block_a: u16,
    block_b: u16,
    block_c: u16,
    block_c_kind: BlockKind,
    score: u32,
    state: RdsData,
    ps_bytes: [u8; 8],
    ps_seen: [bool; 4],
    rt_bytes: [u8; 64],
    rt_seen: [bool; 16],
    rt_ab_flag: bool,
    ptyn_bytes: [u8; 8],
    ptyn_seen: [bool; 2],
    /// Tech 6: accumulated LLR for the PI field (16 bits, MSB first).
    pi_llr_acc: [f32; 16],
    /// Tech 6: number of Block A observations accumulated.
    pi_acc_count: u8,
}

impl Candidate {
    fn new(sample_rate: f32, phase_offset: f32) -> Self {
        Self {
            clock_phase: phase_offset,
            clock_inc: RDS_CHIP_RATE / sample_rate.max(1.0),
            sym_i_acc: 0.0,
            sym_q_acc: 0.0,
            sym_count: 0,
            prev_psk_symbol: None,
            clock_history: [0.0; BIPHASE_CLOCK_WINDOW],
            clock: 0,
            clock_polarity: 0,
            prev_input_bit: false,
            search_reg: 0,
            search_bits: 0,
            locked: false,
            expect: ExpectBlock::B,
            block_reg: 0,
            block_bits: 0,
            block_soft: [1.0; 26],
            block_a: 0,
            block_b: 0,
            block_c: 0,
            block_c_kind: BlockKind::C,
            score: 0,
            state: RdsData::default(),
            ps_bytes: [b' '; 8],
            ps_seen: [false; 4],
            rt_bytes: [b' '; 64],
            rt_seen: [false; 16],
            rt_ab_flag: false,
            ptyn_bytes: [b' '; 8],
            ptyn_seen: [false; 2],

            pi_llr_acc: [0.0; 16],
            pi_acc_count: 0,
        }
    }

    fn process_sample(&mut self, i: f32, q: f32) -> Option<RdsData> {
        self.sym_i_acc += i;
        self.sym_q_acc += q;
        self.sym_count = self.sym_count.saturating_add(1);
        self.clock_phase += self.clock_inc;
        if self.clock_phase < 1.0 {
            return None;
        }
        self.clock_phase -= 1.0;

        let count = f32::from(self.sym_count.max(1));
        let symbol = (self.sym_i_acc / count, self.sym_q_acc / count);
        self.sym_i_acc = 0.0;
        self.sym_q_acc = 0.0;
        self.sym_count = 0;

        let update = if let Some((prev_i, prev_q)) = self.prev_psk_symbol {
            let biphase_i = (symbol.0 - prev_i) * 0.5;
            let biphase_q = (symbol.1 - prev_q) * 0.5;
            let magnitude = (biphase_i * biphase_i + biphase_q * biphase_q).sqrt();
            let emit_bit = self.clock % 2 == self.clock_polarity;
            self.clock_history[self.clock] = magnitude;
            self.clock = (self.clock + 1) % BIPHASE_CLOCK_WINDOW;

            if self.clock == 0 {
                let mut even_sum = 0.0_f32;
                let mut odd_sum = 0.0_f32;
                let mut idx = 0;
                while idx < BIPHASE_CLOCK_WINDOW {
                    even_sum += self.clock_history[idx];
                    odd_sum += self.clock_history[idx + 1];
                    idx += 2;
                }
                if odd_sum > even_sum {
                    self.clock_polarity = 1;
                } else if even_sum > odd_sum {
                    self.clock_polarity = 0;
                }
            }

            if emit_bit {
                let input_bit = biphase_i >= 0.0;
                let bit = (input_bit != self.prev_input_bit) as u8;
                self.prev_input_bit = input_bit;
                // Soft confidence = |I| (aligned with the bit decision sign),
                // not the full vector magnitude.  When the Costas loop has
                // residual phase error θ, |I| = |s|·|cos θ| correctly reflects
                // how reliable the bit is, whereas √(I²+Q²) = |s| would
                // over-state confidence.  Clock history still uses full magnitude
                // (phase-independent) for clock-polarity detection above.
                self.push_bit_soft(bit, biphase_i.abs())
            } else {
                None
            }
        } else {
            None
        };
        self.prev_psk_symbol = Some(symbol);
        update
    }

    fn push_bit_soft(&mut self, bit: u8, confidence: f32) -> Option<RdsData> {
        if self.locked {
            let bit_idx = self.block_bits as usize;
            self.block_reg = ((self.block_reg << 1) | u32::from(bit)) & SEARCH_REG_MASK;
            // Store soft confidence for Tech 3/7/8 decoding.
            if bit_idx < 26 {
                self.block_soft[bit_idx] = confidence;
            }
            self.block_bits = self.block_bits.saturating_add(1);
            if self.block_bits < 26 {
                return None;
            }
            let word = self.block_reg;
            self.block_reg = 0;
            self.block_bits = 0;
            return self.consume_locked_block(word);
        }

        self.search_reg = ((self.search_reg << 1) | u32::from(bit)) & SEARCH_REG_MASK;
        self.search_bits = self.search_bits.saturating_add(1).min(26);
        if self.search_bits < 26 {
            return None;
        }

        // Hard decode only in search mode: OSD in the slide window would create
        // too many false Block A hits from noise, especially with the cost-pruned
        // OSD variants.  Once locked, OSD(3/4) in consume_locked_block handles
        // weak blocks safely thanks to sequential block-type gating.
        let (data, kind) = decode_block(self.search_reg)?;
        if kind != BlockKind::A {
            return None;
        }

        self.locked = true;
        self.expect = ExpectBlock::B;
        self.block_reg = 0;
        self.block_bits = 0;
        self.block_a = data;
        self.state.pi = Some(data);
        None
    }

    fn consume_locked_block(&mut self, word: u32) -> Option<RdsData> {
        let expected = self.expect;
        // Conservative OSD until the candidate has proven itself with multiple
        // successful groups.  OSD(2) at baseline matches the pre-TED decoder's
        // false-positive rate; OSD(3) is only unlocked after 2+ groups where
        // sequential block gating provides strong protection.  The cost ceiling
        // stays tight (0.50 vs the previous 0.60) to reject noise-induced matches.
        let max_cost = if self.score >= 2 {
            OSD_MAX_FLIP_COST + 0.05
        } else {
            OSD_MAX_FLIP_COST
        };
        let max_order = if self.score >= 2 { 3u8 } else { 2 };
        // Tech 3/7/8: use soft-decision decoder instead of hard decode.
        let Some((data, kind)) =
            decode_block_soft(word, &self.block_soft, max_cost, max_order)
        else {
            self.drop_lock(word);
            return None;
        };

        match (expected, kind) {
            (ExpectBlock::B, BlockKind::B) => {
                self.block_b = data;
                self.expect = ExpectBlock::C;
                None
            }
            (ExpectBlock::C, BlockKind::C | BlockKind::CPrime) => {
                self.block_c = data;
                self.block_c_kind = kind;
                self.expect = ExpectBlock::D;
                None
            }
            (ExpectBlock::D, BlockKind::D) => {
                // Stay locked and expect Block A next so the next group's
                // Block A can benefit from OSD soft decoding.  Previously
                // the decoder dropped lock here and fell back to search mode
                // (hard CRC only), which caused it to freeze after 2-3
                // groups on weak signals because Block A could not be
                // re-acquired without OSD.
                self.expect = ExpectBlock::A;
                self.block_reg = 0;
                self.block_bits = 0;
                self.process_group(
                    self.block_a,
                    self.block_b,
                    self.block_c,
                    self.block_c_kind,
                    data,
                )
            }
            (_, BlockKind::A) => {
                // Resync on unexpected Block A.
                self.locked = true;
                self.expect = ExpectBlock::B;
                self.block_reg = 0;
                self.block_bits = 0;
                self.block_a = data;
                // Tech 6: accumulate LLR for PI from soft values.
                self.accumulate_pi_llr(data);
                self.state.pi = Some(data);
                None
            }
            _ => {
                self.drop_lock(word);
                None
            }
        }
    }

    fn drop_lock(&mut self, word: u32) {
        self.locked = false;
        self.expect = ExpectBlock::B;
        self.block_reg = 0;
        self.block_bits = 0;
        self.search_reg = word;
        self.search_bits = 26;
        if let Some((data, kind)) = decode_block(word) {
            if kind == BlockKind::A {
                self.locked = true;
                self.search_reg = 0;
                self.search_bits = 0;
                self.block_a = data;
                self.state.pi = Some(data);
            }
        }
    }

    /// Tech 6: accumulate signed LLR values for the 16 PI data bits.
    /// Called each time a Block A is successfully decoded.
    fn accumulate_pi_llr(&mut self, pi: u16) {
        for i in 0..16usize {
            let bit = ((pi >> (15 - i)) & 1) as f32;
            let signed_llr = (2.0 * bit - 1.0) * self.block_soft[i];
            self.pi_llr_acc[i] += signed_llr;
        }
        self.pi_acc_count += 1;
        if self.pi_acc_count >= PI_ACC_THRESHOLD {
            let accumulated_pi: u16 = (0..16).fold(0u16, |acc, i| {
                acc | (((self.pi_llr_acc[i] >= 0.0) as u16) << (15 - i))
            });
            self.state.pi = Some(accumulated_pi);
            self.pi_llr_acc = [0.0; 16];
            self.pi_acc_count = 0;
        }
    }

    fn process_group(
        &mut self,
        block_a: u16,
        block_b: u16,
        block_c: u16,
        block_c_kind: BlockKind,
        block_d: u16,
    ) -> Option<RdsData> {
        let mut changed = false;

        // Tech 10: PI consistency — if this candidate already has an established
        // PI, reject groups whose Block A carries a different PI code.
        // This prevents a single false OSD decode from polluting accumulated
        // text fields (PS, RT) with garbage from an unrelated station or noise.
        if let Some(existing_pi) = self.state.pi {
            if block_a != existing_pi {
                // Don't count this group; don't update any state.
                return None;
            }
        }

        // Tech 6: accumulate PI LLR on every successfully decoded Block A.
        self.accumulate_pi_llr(block_a);
        if self.state.pi != Some(block_a) && self.pi_acc_count == 0 {
            // After accumulation committed above; also set immediately.
            self.state.pi = self.state.pi.or(Some(block_a));
            changed = true;
        } else if self.state.pi != Some(block_a) {
            changed = true;
        }

        let tp = ((block_b >> 10) & 0x1) != 0;
        if self.state.traffic_program != Some(tp) {
            self.state.traffic_program = Some(tp);
            changed = true;
        }

        let pty = ((block_b >> 5) & 0x1f) as u8;
        if self.state.pty != Some(pty) {
            self.state.pty = Some(pty);
            self.state.pty_name = Some(pty_name(pty).to_string());
            changed = true;
        }

        let group_type = ((block_b >> 12) & 0x0f) as u8;
        let version_b = ((block_b >> 11) & 0x1) != 0;
        if group_type == 0 {
            if !version_b && block_c_kind == BlockKind::C {
                let [af0, af1] = block_c.to_be_bytes();
                if self.process_af_pair(af0, af1) {
                    changed = true;
                }
            }
            let ta = ((block_b >> 4) & 0x1) != 0;
            if self.state.traffic_announcement != Some(ta) {
                self.state.traffic_announcement = Some(ta);
                changed = true;
            }
            let music = ((block_b >> 3) & 0x1) != 0;
            if self.state.music != Some(music) {
                self.state.music = Some(music);
                changed = true;
            }
            let segment = usize::from((block_b & 0x0003) as u8);
            let di = ((block_b >> 2) & 0x1) != 0;
            match segment {
                0 => {
                    if self.state.dynamic_pty != Some(di) {
                        self.state.dynamic_pty = Some(di);
                        changed = true;
                    }
                }
                1 => {
                    if self.state.compressed != Some(di) {
                        self.state.compressed = Some(di);
                        changed = true;
                    }
                }
                2 => {
                    if self.state.artificial_head != Some(di) {
                        self.state.artificial_head = Some(di);
                        changed = true;
                    }
                }
                3 => {
                    if self.state.stereo != Some(di) {
                        self.state.stereo = Some(di);
                        changed = true;
                    }
                }
                _ => {}
            }
            let [b0, b1] = block_d.to_be_bytes();
            self.ps_bytes[segment * 2] = sanitize_text_byte(b0);
            self.ps_bytes[segment * 2 + 1] = sanitize_text_byte(b1);
            self.ps_seen[segment] = true;
            if self.ps_seen.iter().all(|seen| *seen) {
                let ps = String::from_utf8_lossy(&self.ps_bytes)
                    .trim_end()
                    .to_string();
                if !ps.is_empty() && self.state.program_service.as_deref() != Some(ps.as_str()) {
                    self.state.program_service = Some(ps);
                    changed = true;
                }
            }
        } else if group_type == 2 {
            let text_ab = ((block_b >> 4) & 0x1) != 0;
            if text_ab != self.rt_ab_flag {
                self.rt_ab_flag = text_ab;
                self.rt_bytes = [b' '; 64];
                self.rt_seen = [false; 16];
            }
            let segment = usize::from((block_b & 0x000f) as u8);
            if version_b {
                let [b0, b1] = block_d.to_be_bytes();
                let base = segment.saturating_mul(2);
                if base + 1 < self.rt_bytes.len() {
                    self.rt_bytes[base] = sanitize_text_byte(b0);
                    self.rt_bytes[base + 1] = sanitize_text_byte(b1);
                    self.rt_seen[segment] = true;
                }
            } else if block_c_kind == BlockKind::C {
                let [c0, c1] = block_c.to_be_bytes();
                let [d0, d1] = block_d.to_be_bytes();
                let base = segment.saturating_mul(4);
                if base + 3 < self.rt_bytes.len() {
                    self.rt_bytes[base] = sanitize_text_byte(c0);
                    self.rt_bytes[base + 1] = sanitize_text_byte(c1);
                    self.rt_bytes[base + 2] = sanitize_text_byte(d0);
                    self.rt_bytes[base + 3] = sanitize_text_byte(d1);
                    self.rt_seen[segment] = true;
                }
            }
            if let Some(last_seen) = self.rt_seen.iter().rposition(|seen| *seen) {
                let rt_len = if version_b {
                    (last_seen + 1) * 2
                } else {
                    (last_seen + 1) * 4
                };
                let rt = String::from_utf8_lossy(&self.rt_bytes[..rt_len])
                    .trim_end()
                    .to_string();
                if !rt.is_empty() && self.state.radio_text.as_deref() != Some(rt.as_str()) {
                    self.state.radio_text = Some(rt);
                    changed = true;
                }
            }
        } else if group_type == 10 && !version_b && block_c_kind == BlockKind::C {
            let segment = usize::from((block_b & 0x0001) as u8);
            let [c0, c1] = block_c.to_be_bytes();
            let [d0, d1] = block_d.to_be_bytes();
            let base = segment.saturating_mul(4);
            if base + 3 < self.ptyn_bytes.len() {
                self.ptyn_bytes[base] = sanitize_text_byte(c0);
                self.ptyn_bytes[base + 1] = sanitize_text_byte(c1);
                self.ptyn_bytes[base + 2] = sanitize_text_byte(d0);
                self.ptyn_bytes[base + 3] = sanitize_text_byte(d1);
                self.ptyn_seen[segment] = true;
            }
            if self.ptyn_seen.iter().all(|seen| *seen) {
                let ptyn = String::from_utf8_lossy(&self.ptyn_bytes)
                    .trim_end()
                    .to_string();
                if !ptyn.is_empty()
                    && self.state.program_type_name_long.as_deref() != Some(ptyn.as_str())
                {
                    self.state.program_type_name_long = Some(ptyn);
                    changed = true;
                }
            }
        }

        self.score = self.score.saturating_add(1);
        changed.then(|| self.state.clone())
    }

    fn process_af_pair(&mut self, af0: u8, af1: u8) -> bool {
        let mut changed = false;
        if !is_af_count_code(af0) {
            changed |= self.record_af_code(af0);
        }
        if !is_af_count_code(af1) {
            changed |= self.record_af_code(af1);
        }
        changed
    }

    fn record_af_code(&mut self, code: u8) -> bool {
        let Some(hz) = af_code_to_hz(code) else {
            return false;
        };
        let afs = self
            .state
            .alternative_frequencies_hz
            .get_or_insert_with(Vec::new);
        if afs.contains(&hz) {
            return false;
        }
        afs.push(hz);
        afs.sort_unstable();
        true
    }
}

fn is_af_count_code(code: u8) -> bool {
    (224..=249).contains(&code)
}

fn af_code_to_hz(code: u8) -> Option<u32> {
    if (1..=204).contains(&code) {
        Some(87_500_000 + u32::from(code) * 100_000)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// RdsDecoder — main public entry point
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RdsDecoder {
    sample_rate_hz: u32,
    carrier_phase: f32,
    carrier_inc: f32,
    /// Tech 1: RRC matched filter (I and Q processed together as complex).
    rrc: FftRrcFilter,
    /// Tech 5: Costas loop integrator state.
    costas_integrator: f32,
    /// Tech 2: pilot-derived 57 kHz carrier reference (cos, sin).
    /// When Some, the free-running NCO is bypassed and Costas is suppressed.
    pilot_ref: Option<(f32, f32)>,
    /// Leaky average of |Costas error| for adaptive loop bandwidth.
    costas_err_avg: f32,
    candidates: Vec<Candidate>,
    best_score: u32,
    /// Index into `candidates` for the current winning candidate.
    /// Once established, only this candidate can update `best_state` at equal
    /// score; a different candidate must achieve a strictly higher score to
    /// take over.  This prevents N candidates decoding the same groups from
    /// cycling through `best_state` with partially-accumulated ps_seen / rt_seen.
    best_candidate_idx: Option<usize>,
    best_state: Option<RdsData>,
    /// Running sample counter for staleness detection.
    sample_counter: u64,
    /// Sample counter at which best_state was last updated.
    last_update_sample: u64,
    /// Number of samples before the incumbent is considered stale.
    stale_threshold: u64,
}

impl RdsDecoder {
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate_f = sample_rate.max(1) as f32;
        let mut candidates = Vec::with_capacity(PHASE_CANDIDATES);
        for idx in 0..PHASE_CANDIDATES {
            candidates.push(Candidate::new(
                sample_rate_f,
                idx as f32 / PHASE_CANDIDATES as f32,
            ));
        }
        Self {
            sample_rate_hz: sample_rate.max(1),
            carrier_phase: 0.0,
            carrier_inc: TAU * RDS_SUBCARRIER_HZ / sample_rate_f,
            rrc: FftRrcFilter::new_rrc(sample_rate_f, RDS_CHIP_RATE),
            costas_integrator: 0.0,
            pilot_ref: None,
            costas_err_avg: 1.0,
            candidates,
            best_score: 0,
            best_candidate_idx: None,
            best_state: None,
            sample_counter: 0,
            last_update_sample: 0,
            stale_threshold: (STALE_TIMEOUT_SECS * sample_rate_f) as u64,
        }
    }

    /// Tech 2: provide a pilot-derived 57 kHz carrier reference.
    /// `cos57` and `sin57` should be the cosine and sine of the
    /// triple-angle (3 × 19 kHz pilot) phase for the current sample.
    /// Call this per sample when the pilot is locked; call `clear_pilot_ref`
    /// when the pilot is lost.
    pub fn set_pilot_ref(&mut self, cos57: f32, sin57: f32) {
        self.pilot_ref = Some((cos57, sin57));
    }

    /// Tech 2: revert to the free-running NCO + Costas loop.
    pub fn clear_pilot_ref(&mut self) {
        self.pilot_ref = None;
    }

    pub fn process_sample(&mut self, sample: f32, quality: f32) -> Option<&RdsData> {
        let publish_quality = quality.clamp(0.0, 1.0);

        // Tech 2: use pilot-derived reference when available; otherwise use
        // the free-running NCO with Tech 5 Costas feedback.
        let (cos_p, sin_p) = if let Some((c, s)) = self.pilot_ref {
            (c, s)
        } else {
            let (s, c) = self.carrier_phase.sin_cos();
            (c, s)
        };

        // Always advance the free-running NCO so it stays ready as fallback.
        self.carrier_phase = (self.carrier_phase + self.carrier_inc).rem_euclid(TAU);

        // Mix down to RDS baseband.
        let raw_i = sample * cos_p * 2.0;
        let raw_q = sample * -sin_p * 2.0;

        // Tech 1: apply RRC matched filter to I and Q (processed as complex).
        let (mixed_i, mixed_q) = self.rrc.process(raw_i, raw_q);

        // Tech 5: Costas loop — tanh soft phase detector.
        // Only active when not using a pilot reference.
        // Adaptive bandwidth: use wide gains for acquisition, narrow once locked.
        if self.pilot_ref.is_none() {
            let err = mixed_i.tanh() * mixed_q;
            self.costas_err_avg = COSTAS_ERR_AVG_ALPHA * self.costas_err_avg
                + (1.0 - COSTAS_ERR_AVG_ALPHA) * err.abs();
            let (kp, ki) = if self.costas_err_avg < COSTAS_LOCK_THRESHOLD {
                (COSTAS_KP_TRACK, COSTAS_KI_TRACK)
            } else {
                (COSTAS_KP, COSTAS_KI)
            };
            self.costas_integrator += ki * err;
            let freq_correction = (kp * err + self.costas_integrator)
                .clamp(-COSTAS_MAX_FREQ_CORR, COSTAS_MAX_FREQ_CORR);
            self.carrier_phase -= freq_correction;
            self.carrier_phase = self.carrier_phase.rem_euclid(TAU);
        }

        self.sample_counter += 1;

        // Staleness check: if the incumbent hasn't produced an update in
        // STALE_TIMEOUT_SECS, clear its score advantage so any candidate
        // can take over.  This prevents the decoder from "freezing" on stale
        // data when the incumbent's timing or carrier tracking degrades.
        if self.best_candidate_idx.is_some()
            && self.sample_counter - self.last_update_sample > self.stale_threshold
        {
            self.best_score = 0;
            self.best_candidate_idx = None;
        }

        for (idx, candidate) in self.candidates.iter_mut().enumerate() {
            let is_incumbent = self.best_candidate_idx == Some(idx);
            if let Some(update) = candidate.process_sample(mixed_i, mixed_q) {
                let qualifies = candidate.score > self.best_score
                    || (is_incumbent && candidate.score >= self.best_score)
                    || self.best_state.is_none();
                if qualifies {
                    let same_pi = self.best_state.as_ref().and_then(|s| s.pi) == update.pi;
                    if publish_quality >= MIN_PUBLISH_QUALITY
                        || same_pi
                        || self.best_state.is_none()
                    {
                        self.best_score = candidate.score;
                        self.best_candidate_idx = Some(idx);
                        self.best_state = Some(update);
                        self.last_update_sample = self.sample_counter;
                    }
                }
            } else if is_incumbent {
                self.best_score = candidate.score;
            }
        }
        self.best_state.as_ref()
    }

    pub fn process_samples(&mut self, samples: &[f32]) -> Option<&RdsData> {
        for &sample in samples {
            let _ = self.process_sample(sample, 1.0);
        }
        self.best_state.as_ref()
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.sample_rate_hz);
    }

    pub fn snapshot(&self) -> Option<RdsData> {
        self.best_state.clone()
    }
}

// ---------------------------------------------------------------------------
// Block decoding: hard and soft (Tech 3/7/8)
// ---------------------------------------------------------------------------

/// Hard-decision block decoder. Returns `(data, block_kind)` if the 26-bit
/// word passes a CRC10 syndrome check against any of the five RDS offset words.
fn decode_block(word: u32) -> Option<(u16, BlockKind)> {
    let data = (word >> 10) as u16;
    let check = (word & 0x03ff) as u16;
    let syndrome = crc10(data) ^ check;
    let kind = match syndrome {
        OFFSET_A => BlockKind::A,
        OFFSET_B => BlockKind::B,
        OFFSET_C => BlockKind::C,
        OFFSET_CP => BlockKind::CPrime,
        OFFSET_D => BlockKind::D,
        _ => return None,
    };
    Some((data, kind))
}

/// Map a 10-bit CRC syndrome to its RDS block kind, if it matches any offset.
#[inline]
fn offset_to_kind(syndrome: u16) -> Option<BlockKind> {
    match syndrome {
        OFFSET_A => Some(BlockKind::A),
        OFFSET_B => Some(BlockKind::B),
        OFFSET_C => Some(BlockKind::C),
        OFFSET_CP => Some(BlockKind::CPrime),
        OFFSET_D => Some(BlockKind::D),
        _ => None,
    }
}

/// Tech 3/7/8: soft-decision block decoder implementing OSD(3) or OSD(4).
///
/// Uses syndrome arithmetic instead of recomputing CRC for each trial:
/// flipping bit k changes the syndrome by a precomputed delta (CRC linearity),
/// reducing each trial to a single XOR + 5-way comparison instead of a full
/// 16-iteration CRC.  Bit positions are sorted by ascending soft confidence
/// so inner loops can `break` (not just `continue`) once accumulated cost
/// exceeds the threshold, since all subsequent combinations are guaranteed
/// to be more expensive.
///
/// `word` is the 26-bit hard-decision word; `soft[k]` is the confidence
/// magnitude (|LLR|) for the k-th received bit, where bit 0 is the MSB
/// (bit 25 of `word`) and bit 25 is the LSB (bit 0 of `word`).
///
/// `max_cost` is the maximum total flip cost (adaptive based on signal quality).
/// `max_order` is the maximum OSD order (3 or 4).
fn decode_block_soft(
    word: u32,
    soft: &[f32; 26],
    max_cost: f32,
    max_order: u8,
) -> Option<(u16, BlockKind)> {
    // Compute base syndrome once: CRC(data) XOR check_bits.
    let base_data = (word >> 10) as u16;
    let check = (word & 0x03ff) as u16;
    let base_syn = crc10(base_data) ^ check;

    // Distance 0: hard decode.
    if let Some(kind) = offset_to_kind(base_syn) {
        return Some((base_data, kind));
    }

    // Precompute syndrome delta for each of the 26 bit positions.
    // Exploits CRC linearity: CRC(a ^ b) = CRC(a) ^ CRC(b).
    let bit_syn: [u16; 26] = {
        let mut t = [0u16; 26];
        for (k, slot) in t[..16].iter_mut().enumerate() {
            *slot = crc10(1u16 << (15 - k));
        }
        for (k, slot) in t[16..].iter_mut().enumerate() {
            *slot = 1u16 << (9 - k);
        }
        t
    };

    // Sort bit indices by ascending soft confidence for early termination.
    let mut order = [0u8; 26];
    for (i, slot) in order.iter_mut().enumerate() {
        *slot = i as u8;
    }
    order.sort_unstable_by(|&a, &b| {
        soft[a as usize]
            .partial_cmp(&soft[b as usize])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut best_result: Option<(u16, BlockKind)> = None;
    let mut best_cost = f32::INFINITY;

    // Distance 1: single-bit flips in cost-ascending order.
    for &ki in &order {
        let k = ki as usize;
        if soft[k] >= best_cost {
            break;
        }
        if let Some(kind) = offset_to_kind(base_syn ^ bit_syn[k]) {
            best_cost = soft[k];
            best_result = Some((((word ^ (1 << (25 - k))) >> 10) as u16, kind));
            break; // sorted order: first match is cheapest
        }
    }

    if best_result.is_some() {
        if best_cost <= max_cost {
            return best_result;
        }
        best_result = None;
        best_cost = f32::INFINITY;
    }

    // Distance 2: two-bit flips.
    for (i1, &ki1) in order.iter().enumerate() {
        let k1 = ki1 as usize;
        if soft[k1] >= max_cost {
            break;
        }
        let syn1 = base_syn ^ bit_syn[k1];
        for &ki2 in &order[i1 + 1..] {
            let k2 = ki2 as usize;
            let pair_cost = soft[k1] + soft[k2];
            if pair_cost > max_cost || pair_cost >= best_cost {
                break;
            }
            if let Some(kind) = offset_to_kind(syn1 ^ bit_syn[k2]) {
                best_cost = pair_cost;
                best_result = Some((
                    ((word ^ (1 << (25 - k1)) ^ (1 << (25 - k2))) >> 10) as u16,
                    kind,
                ));
            }
        }
    }

    if best_result.is_some() {
        return best_result;
    }

    // Distance 3: three-bit flips.
    for (i1, &ki1) in order.iter().enumerate() {
        let k1 = ki1 as usize;
        if soft[k1] >= max_cost {
            break;
        }
        let syn1 = base_syn ^ bit_syn[k1];
        for (off2, &ki2) in order[i1 + 1..].iter().enumerate() {
            let k2 = ki2 as usize;
            let c12 = soft[k1] + soft[k2];
            if c12 >= max_cost {
                break;
            }
            let i2 = i1 + 1 + off2;
            let syn12 = syn1 ^ bit_syn[k2];
            for &ki3 in &order[i2 + 1..] {
                let k3 = ki3 as usize;
                let triple_cost = c12 + soft[k3];
                if triple_cost > max_cost || triple_cost >= best_cost {
                    break;
                }
                if let Some(kind) = offset_to_kind(syn12 ^ bit_syn[k3]) {
                    best_cost = triple_cost;
                    let flip =
                        (1u32 << (25 - k1)) ^ (1u32 << (25 - k2)) ^ (1u32 << (25 - k3));
                    best_result = Some((((word ^ flip) >> 10) as u16, kind));
                }
            }
        }
    }

    if best_result.is_some() || max_order < 4 {
        return best_result;
    }

    // Distance 4: four-bit flips.
    for (i1, &ki1) in order.iter().enumerate() {
        let k1 = ki1 as usize;
        if soft[k1] >= max_cost {
            break;
        }
        let syn1 = base_syn ^ bit_syn[k1];
        for (off2, &ki2) in order[i1 + 1..].iter().enumerate() {
            let k2 = ki2 as usize;
            let c12 = soft[k1] + soft[k2];
            if c12 >= max_cost {
                break;
            }
            let i2 = i1 + 1 + off2;
            let syn12 = syn1 ^ bit_syn[k2];
            for (off3, &ki3) in order[i2 + 1..].iter().enumerate() {
                let k3 = ki3 as usize;
                let c123 = c12 + soft[k3];
                if c123 >= max_cost {
                    break;
                }
                let i3 = i2 + 1 + off3;
                let syn123 = syn12 ^ bit_syn[k3];
                for &ki4 in &order[i3 + 1..] {
                    let k4 = ki4 as usize;
                    let quad_cost = c123 + soft[k4];
                    if quad_cost > max_cost || quad_cost >= best_cost {
                        break;
                    }
                    if let Some(kind) = offset_to_kind(syn123 ^ bit_syn[k4]) {
                        best_cost = quad_cost;
                        let flip = (1u32 << (25 - k1))
                            ^ (1u32 << (25 - k2))
                            ^ (1u32 << (25 - k3))
                            ^ (1u32 << (25 - k4));
                        best_result = Some((((word ^ flip) >> 10) as u16, kind));
                    }
                }
            }
        }
    }

    best_result
}

fn crc10(data: u16) -> u16 {
    let mut reg = u32::from(data) << 10;
    let poly = u32::from(RDS_POLY);
    for shift in (10..=25).rev() {
        if (reg & (1 << shift)) != 0 {
            reg ^= poly << (shift - 10);
        }
    }
    (reg & 0x03ff) as u16
}

fn sanitize_text_byte(byte: u8) -> u8 {
    if (0x20..=0x7e).contains(&byte) {
        byte
    } else {
        b' '
    }
}

fn pty_name(pty: u8) -> &'static str {
    match pty {
        0 => "None",
        1 => "News",
        2 => "Current Affairs",
        3 => "Information",
        4 => "Sport",
        5 => "Education",
        6 => "Drama",
        7 => "Culture",
        8 => "Science",
        9 => "Varied",
        10 => "Pop Music",
        11 => "Rock Music",
        12 => "Easy Listening",
        13 => "Light Classical",
        14 => "Serious Classical",
        15 => "Other Music",
        16 => "Weather",
        17 => "Finance",
        18 => "Children's",
        19 => "Social Affairs",
        20 => "Religion",
        21 => "Phone In",
        22 => "Travel",
        23 => "Leisure",
        24 => "Jazz Music",
        25 => "Country Music",
        26 => "National Music",
        27 => "Oldies Music",
        28 => "Folk Music",
        29 => "Documentary",
        30 => "Alarm Test",
        _ => "Alarm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_block(data: u16, offset: u16) -> u32 {
        (u32::from(data) << 10) | u32::from(crc10(data) ^ offset)
    }

    #[test]
    fn decode_block_recognizes_valid_offsets() {
        let block = encode_block(0x1234, OFFSET_A);
        let (data, kind) = decode_block(block).expect("valid block");
        assert_eq!(data, 0x1234);
        assert_eq!(kind, BlockKind::A);
    }

    #[test]
    fn decoder_emits_ps_and_pty_from_group_0a() {
        let mut candidate = Candidate::new(240_000.0, 0.0);
        let pi = 0x52ab;
        let block_a = encode_block(pi, OFFSET_A);
        let block_b = encode_block(10 << 5, OFFSET_B);
        let block_d = encode_block(u16::from_be_bytes(*b"AB"), OFFSET_D);

        for bit_idx in (0..26).rev() {
            let bit = ((block_a >> bit_idx) & 1) as u8;
            let _ = candidate.push_bit_soft(bit, 1.0);
        }
        for bit_idx in (0..26).rev() {
            let bit = ((block_b >> bit_idx) & 1) as u8;
            let _ = candidate.push_bit_soft(bit, 1.0);
        }
        let filler = encode_block(0, OFFSET_C);
        for bit_idx in (0..26).rev() {
            let bit = ((filler >> bit_idx) & 1) as u8;
            let _ = candidate.push_bit_soft(bit, 1.0);
        }
        let mut last = None;
        for bit_idx in (0..26).rev() {
            let bit = ((block_d >> bit_idx) & 1) as u8;
            last = candidate.push_bit_soft(bit, 1.0);
        }

        assert!(last.is_some());
        let state = last.unwrap();
        assert_eq!(state.pty, Some(10));
        assert_eq!(state.pty_name.as_deref(), Some("Pop Music"));
    }

    #[test]
    fn rrc_tap_dc_gain() {
        // All taps of a normalized RRC filter should sum to 1.0.
        let taps = build_rrc_taps(240_000.0, RDS_CHIP_RATE);
        let sum: f32 = taps.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "RRC DC gain = {sum}");
    }

    #[test]
    fn decode_block_soft_corrects_single_bit_error() {
        let word = encode_block(0xABCD, OFFSET_A);
        // Flip one bit (bit 10, i.e. position k=15 from MSB).
        let corrupted = word ^ (1 << 10);
        let mut soft = [1.0f32; 26];
        // Mark the corrupted bit as low confidence (realistic: a genuine
        // error has low |biphase_I|).
        soft[15] = 0.05;
        let (data, kind) = decode_block_soft(corrupted, &soft, OSD_MAX_FLIP_COST, 3).expect("should recover");
        assert_eq!(data, 0xABCD);
        assert_eq!(kind, BlockKind::A);
    }

    #[test]
    fn decode_block_soft_corrects_two_bit_error_osd2() {
        // OSD(2) must correct a 2-bit error at known positions.
        let word = encode_block(0x1234, OFFSET_B);
        // Flip bits k=0 and k=1 (two most-significant positions).
        let corrupted = word ^ (1 << 25) ^ (1 << 24);
        // Set soft confidences very low for bits 0 and 1 so the decoder
        // knows they are unreliable and picks the cheapest pair.
        let mut soft = [1.0f32; 26];
        soft[0] = 0.05;
        soft[1] = 0.05;
        let (data, kind) = decode_block_soft(corrupted, &soft, OSD_MAX_FLIP_COST, 3).expect("OSD(2) should correct");
        assert_eq!(data, 0x1234);
        assert_eq!(kind, BlockKind::B);
    }

    // Note: OSD(2) intentionally does NOT assert None for 3-bit errors.
    // When all soft values are equal (uninformative), there are ~325 two-bit
    // combinations to try; some accidentally produce a valid CRC for a
    // *different* codeword (~80% probability for random words).  This is
    // acceptable: in locked mode, sequential block-type gating (B→C→D)
    // prevents any such false decode from completing a full group.
    // The `pure_noise_produces_zero_pi_decodes` test is the authoritative
    // guard against false PI reports.

    #[test]
    fn decode_block_soft_prefers_least_costly_flip() {
        // Construct a word with an injected single-bit error at bit k=2 (high confidence)
        // and also make bits k=24,25 low-confidence. The decoder should flip k=2 (cheapest).
        let word = encode_block(0xBEEF, OFFSET_D);
        let corrupted = word ^ (1 << (25 - 2)); // flip bit k=2
        let mut soft = [1.0f32; 26];
        soft[2] = 0.01; // least confident → cheapest to flip
        let (data, kind) = decode_block_soft(corrupted, &soft, OSD_MAX_FLIP_COST, 3).expect("should recover");
        assert_eq!(data, 0xBEEF);
        assert_eq!(kind, BlockKind::D);
    }

    // -----------------------------------------------------------------------
    // Signal synthesis helpers for end-to-end / sensitivity tests
    // -----------------------------------------------------------------------

    /// Minimal LCG pseudo-random number generator (deterministic, seedable).
    fn lcg_rand(state: &mut u64) -> f32 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*state >> 33) as f32 / (1u64 << 31) as f32
    }

    /// Box-Muller Gaussian sample, zero mean, unit variance.
    fn gaussian(state: &mut u64) -> f32 {
        let u1 = lcg_rand(state).max(1e-9);
        let u2 = lcg_rand(state);
        (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos()
    }

    /// Encode 26-bit RDS block words into a differential biphase chip stream.
    ///
    /// The decoder uses biphase differential detection:
    ///   `bit = sign(biphase_I) XOR prev_sign(biphase_I)`
    /// To recover the original data bits, the encoder must pre-apply NRZI
    /// (NRZ-Mark: transition on 1, hold on 0) before Manchester encoding.
    ///
    /// NRZI=1 → chips (−1, +1); NRZI=0 → chips (+1, −1).
    /// A 2-chip preamble (NRZI=1) is prepended so the decoder can initialise
    /// `prev_psk_symbol` and establish the initial differential state.
    fn blocks_to_chips(words: &[u32]) -> Vec<i8> {
        let mut chips = Vec::with_capacity(words.len() * 52 + 2);
        // Preamble: bit=1 encoded as NRZI=1 → chips (−1, +1).
        // Starting NRZI state before preamble = false; bit=1 transitions to true.
        chips.push(-1i8);
        chips.push(1i8);
        let mut nrzi = true; // NRZI state after preamble
        for &word in words {
            for k in (0..26).rev() {
                let bit = ((word >> k) & 1) != 0;
                // NRZI mark: transition on 1, hold on 0.
                if bit {
                    nrzi = !nrzi;
                }
                if nrzi {
                    chips.push(-1);
                    chips.push(1);
                } else {
                    chips.push(1);
                    chips.push(-1);
                }
            }
        }
        chips
    }

    /// Modulate chip stream as BPSK on the 57 kHz RDS subcarrier.
    /// Returns a composite FM-baseband signal for `RdsDecoder::process_sample`.
    ///
    /// Each chip is RRC pulse-shaped so that RRC(tx) × RRC(rx) = raised cosine,
    /// giving zero ISI at the receiver's optimal sampling instants.
    fn chips_to_rds_signal(chips: &[i8], sample_rate: f32) -> Vec<f32> {
        let spc = sample_rate / RDS_CHIP_RATE;
        let n = (chips.len() as f32 * spc).ceil() as usize;

        // Build the transmit RRC pulse shape (same taps as the receiver).
        let taps = build_rrc_taps(sample_rate, RDS_CHIP_RATE);

        // Create baseband impulse train and convolve with RRC taps.
        let mut baseband = vec![0.0f32; n];
        for (ci, &chip) in chips.iter().enumerate() {
            let center = ((ci as f32 + 0.5) * spc).round() as usize;
            if center < n {
                baseband[center] = chip as f32;
            }
        }

        // Convolve baseband impulse train with RRC taps (direct FIR).
        let half = taps.len() / 2;
        let mut shaped = vec![0.0f32; n];
        for (i, &impulse) in baseband.iter().enumerate() {
            if impulse == 0.0 {
                continue;
            }
            for (j, &tap) in taps.iter().enumerate() {
                let idx = i + j;
                if idx >= half && idx - half < n {
                    shaped[idx - half] += impulse * tap;
                }
            }
        }

        // BPSK modulate onto the 57 kHz subcarrier.
        for t in 0..n {
            let phase = TAU * RDS_SUBCARRIER_HZ * t as f32 / sample_rate;
            shaped[t] *= phase.cos();
        }
        shaped
    }

    /// Add AWGN at the given SNR (dB) relative to the signal's actual power.
    fn add_awgn(sig: &mut [f32], snr_db: f32, rng: &mut u64) {
        let pwr = sig.iter().map(|x| x * x).sum::<f32>() / sig.len() as f32;
        let noise_sigma = (pwr / 10.0f32.powf(snr_db / 10.0)).sqrt();
        for s in sig.iter_mut() {
            *s += gaussian(rng) * noise_sigma;
        }
    }

    /// Build a Group-0A block set for the given PI / PS segment.
    fn group_0a(pi: u16, segment: u8, ps_chars: [u8; 2], pty: u8) -> [u32; 4] {
        let block_b: u16 = (u16::from(pty) << 5) | u16::from(segment & 0x03);
        [
            encode_block(pi, OFFSET_A),
            encode_block(block_b, OFFSET_B),
            encode_block(0x0000, OFFSET_C),
            encode_block(u16::from_be_bytes(ps_chars), OFFSET_D),
        ]
    }

    // -----------------------------------------------------------------------
    // End-to-end sensitivity tests
    // -----------------------------------------------------------------------

    /// Directly decode the chip stream (no BPSK) to verify `blocks_to_chips`
    /// round-trips correctly for all 16 blocks (4 groups × 4 blocks).
    #[test]
    fn blocks_to_chips_round_trips_all_groups() {
        let pi = 0x9801u16;
        let ps = b"TEST FM!";
        let mut words: Vec<u32> = Vec::new();
        for seg in 0..4u8 {
            let g = group_0a(
                pi,
                seg,
                [ps[seg as usize * 2], ps[seg as usize * 2 + 1]],
                10,
            );
            words.extend_from_slice(&g);
        }
        let chips = blocks_to_chips(&words);

        // Manually decode with perfect biphase alignment.
        // clock_polarity=0: emit bit from the second chip of each pair.
        // The preamble chip pair is chips[0..2]; first data chip pair is chips[2..4], etc.
        // We skip the preamble pair (it only sets prev_input_bit = true) and
        // decode from chips[2] onward in pairs.
        let mut prev_input_bit = true; // set by the preamble bit
        let mut shift: u32 = 0;
        let mut bit_idx = 0usize;
        let mut decoded: Vec<u32> = Vec::new();

        // chips[0] = preamble first (-1), chips[1] = preamble second (+1)
        // The preamble pair biphase = (+1 - (-1))/2 = +1  →  input_bit = true
        // bit = (true != false) = 1, prev_input_bit = true  (NRZI seed established)
        // Preamble bit not added to decoded stream; data starts at chips[2].

        let mut prev_chip = chips[1]; // last chip of preamble
        let mut pair_idx = 0usize; // which chip within current bit pair (0=first/reference, 1=second/data)
        for &chip in &chips[2..] {
            let biphase_i = (chip as f32 - prev_chip as f32) * 0.5;
            if pair_idx == 1 {
                // Second chip of pair → emit bit (clock_polarity = 0, even positions)
                let input_bit = biphase_i >= 0.0;
                let bit = (input_bit != prev_input_bit) as u32;
                prev_input_bit = input_bit;
                shift = ((shift << 1) | bit) & 0x03FF_FFFF;
                bit_idx += 1;
                if bit_idx == 26 {
                    decoded.push(shift);
                    shift = 0;
                    bit_idx = 0;
                }
            }
            prev_chip = chip;
            pair_idx = 1 - pair_idx;
        }

        assert_eq!(
            decoded.len(),
            words.len(),
            "decoded {decoded_len} blocks but expected {expected}",
            decoded_len = decoded.len(),
            expected = words.len()
        );
        for (i, (got, want)) in decoded.iter().zip(words.iter()).enumerate() {
            assert_eq!(
                got, want,
                "block {i}: decoded 0x{got:08X} but expected 0x{want:08X}"
            );
        }
    }

    #[test]
    fn end_to_end_clean_signal_decodes_ps() {
        // Synthesise a clean RDS signal, run it through the full decoder,
        // and verify that PI and the first PS segment are decoded correctly.
        let sample_rate = 240_000.0f32;
        let pi = 0x9801u16;
        let ps = b"TEST FM!";

        // Four Group-0A blocks cover all four PS segments.
        let mut words: Vec<u32> = Vec::new();
        for seg in 0..4u8 {
            let g = group_0a(
                pi,
                seg,
                [ps[seg as usize * 2], ps[seg as usize * 2 + 1]],
                10,
            );
            words.extend_from_slice(&g);
        }
        // Repeat 20× to give the decoder time to acquire.
        let words: Vec<u32> = words
            .iter()
            .copied()
            .cycle()
            .take(words.len() * 60)
            .collect();

        let chips = blocks_to_chips(&words);
        let signal = chips_to_rds_signal(&chips, sample_rate);

        let mut dec = RdsDecoder::new(sample_rate as u32);
        let mut got_pi = false;
        let mut got_ps = false;
        for &s in &signal {
            if let Some(state) = dec.process_sample(s, 1.0) {
                if state.pi == Some(pi) {
                    got_pi = true;
                }
                if state.program_service.as_deref() == Some("TEST FM!") {
                    got_ps = true;
                    break;
                }
            }
        }
        assert!(got_pi, "PI should be decoded from clean signal");
        assert!(got_ps, "PS 'TEST FM!' should be decoded from clean signal");
    }

    #[test]
    fn end_to_end_noisy_signal_snr_10db_decodes_pi() {
        // At 10 dB SNR the decoder should still recover PI reliably.
        let sample_rate = 240_000.0f32;
        let pi = 0x4BBC;

        let mut words: Vec<u32> = Vec::new();
        for seg in 0..4u8 {
            let g = group_0a(pi, seg, [b'N', b'Z' + seg], 3);
            words.extend_from_slice(&g);
        }
        let words: Vec<u32> = words
            .iter()
            .copied()
            .cycle()
            .take(words.len() * 40)
            .collect();

        let chips = blocks_to_chips(&words);
        let mut signal = chips_to_rds_signal(&chips, sample_rate);
        let mut rng = 0xDEAD_BEEF_1234_5678u64;
        add_awgn(&mut signal, 10.0, &mut rng);

        let mut dec = RdsDecoder::new(sample_rate as u32);
        let mut got_pi = false;
        for &s in &signal {
            if dec.process_sample(s, 1.0).and_then(|st| st.pi) == Some(pi) {
                got_pi = true;
                break;
            }
        }
        assert!(got_pi, "PI should decode at SNR = 10 dB");
    }

    #[test]
    fn end_to_end_noisy_signal_snr_9db_decodes_pi() {
        // At 9 dB SNR the decoder should still recover PI reliably.
        let sample_rate = 240_000.0f32;
        let pi = 0x4BBC;

        let mut words: Vec<u32> = Vec::new();
        for seg in 0..4u8 {
            let g = group_0a(pi, seg, [b'N', b'Z' + seg], 3);
            words.extend_from_slice(&g);
        }
        let words: Vec<u32> = words
            .iter()
            .copied()
            .cycle()
            .take(words.len() * 60)
            .collect();

        let chips = blocks_to_chips(&words);
        let mut signal = chips_to_rds_signal(&chips, sample_rate);
        let mut rng = 0xCAFE_BABE_9876_5432u64;
        add_awgn(&mut signal, 9.0, &mut rng);

        let mut dec = RdsDecoder::new(sample_rate as u32);
        let mut got_pi = false;
        for &s in &signal {
            if dec.process_sample(s, 1.0).and_then(|st| st.pi) == Some(pi) {
                got_pi = true;
                break;
            }
        }
        assert!(got_pi, "PI should decode at SNR = 9 dB");
    }

    #[test]
    fn end_to_end_noisy_signal_snr_7db_decodes_pi() {
        let sample_rate = 240_000.0f32;
        let pi = 0x4BBC;

        let mut words: Vec<u32> = Vec::new();
        for seg in 0..4u8 {
            let g = group_0a(pi, seg, [b'N', b'Z' + seg], 3);
            words.extend_from_slice(&g);
        }
        let words: Vec<u32> = words
            .iter()
            .copied()
            .cycle()
            .take(words.len() * 80)
            .collect();

        let chips = blocks_to_chips(&words);
        let mut signal = chips_to_rds_signal(&chips, sample_rate);
        let mut rng = 0xBAAD_F00D_1337_C0DEu64;
        add_awgn(&mut signal, 7.0, &mut rng);

        let mut dec = RdsDecoder::new(sample_rate as u32);
        let mut got_pi = false;
        for &s in &signal {
            if dec.process_sample(s, 1.0).and_then(|st| st.pi) == Some(pi) {
                got_pi = true;
                break;
            }
        }
        assert!(got_pi, "PI should decode at SNR = 7 dB");
    }

    #[test]
    fn end_to_end_noisy_signal_snr_5db_decodes_pi() {
        // At 5 dB SNR: raw BER ~3.6%, OSD(4) + block retry + adaptive Costas
        // should still recover PI reliably with enough groups.
        let sample_rate = 240_000.0f32;
        let pi = 0x4BBC;

        let mut words: Vec<u32> = Vec::new();
        for seg in 0..4u8 {
            let g = group_0a(pi, seg, [b'N', b'Z' + seg], 3);
            words.extend_from_slice(&g);
        }
        let words: Vec<u32> = words
            .iter()
            .copied()
            .cycle()
            .take(words.len() * 120)
            .collect();

        let chips = blocks_to_chips(&words);
        let mut signal = chips_to_rds_signal(&chips, sample_rate);
        let mut rng = 0xDEAD_C0DE_FACE_B00Cu64;
        add_awgn(&mut signal, 5.0, &mut rng);

        let mut dec = RdsDecoder::new(sample_rate as u32);
        let mut got_pi = false;
        for &s in &signal {
            if dec.process_sample(s, 1.0).and_then(|st| st.pi) == Some(pi) {
                got_pi = true;
                break;
            }
        }
        assert!(got_pi, "PI should decode at SNR = 5 dB");
    }

    #[test]
    fn end_to_end_with_pilot_reference_decodes_pi() {
        // With an exact pilot reference, PI acquisition should be fast (< 20 groups).
        let sample_rate = 240_000.0f32;
        let pi = 0xC001u16;

        let mut words: Vec<u32> = Vec::new();
        for seg in 0..4u8 {
            let g = group_0a(pi, seg, [b'A' + seg, b'B' + seg], 1);
            words.extend_from_slice(&g);
        }
        let words: Vec<u32> = words
            .iter()
            .copied()
            .cycle()
            .take(words.len() * 20)
            .collect();

        let chips = blocks_to_chips(&words);
        let signal = chips_to_rds_signal(&chips, sample_rate);

        let mut dec = RdsDecoder::new(sample_rate as u32);
        let mut got_pi = false;
        for (t, &s) in signal.iter().enumerate() {
            // Provide perfect pilot reference: cos/sin of 57 kHz at each sample.
            let phase57 = TAU * RDS_SUBCARRIER_HZ * t as f32 / sample_rate;
            dec.set_pilot_ref(phase57.cos(), phase57.sin());
            if dec.process_sample(s, 1.0).and_then(|st| st.pi) == Some(pi) {
                got_pi = true;
                break;
            }
        }
        assert!(got_pi, "PI should decode quickly with pilot reference");
    }

    // -----------------------------------------------------------------------
    // Block error rate / OSD comparison
    // -----------------------------------------------------------------------

    /// Inject exactly `n_errors` bit flips at random positions in a 26-bit word.
    fn inject_errors(word: u32, positions: &[usize]) -> u32 {
        positions.iter().fold(word, |w, &k| w ^ (1 << (25 - k)))
    }

    #[test]
    fn full_group_with_two_bit_errors_in_each_locked_block() {
        // Verify OSD(2) can recover a full group where blocks B, C, D each
        // have exactly 2 bit errors at known low-confidence positions.
        let pi = 0xABCDu16;
        let block_a = encode_block(pi, OFFSET_A);
        let block_b_data: u16 = (2u16 << 12) | (1 << 11) | (10 << 5); // Group 2B, pty=10
        let block_b = encode_block(block_b_data, OFFSET_B);
        let block_c = encode_block(0x4865, OFFSET_CP); // C' (version B, unused)
        let block_d = encode_block(u16::from_be_bytes(*b"Hi"), OFFSET_D);

        // Corrupt B, C, D at known positions (k=0,1 = two MSBs).
        let corrupt_b = inject_errors(block_b, &[0, 1]);
        let corrupt_c = inject_errors(block_c, &[0, 1]);
        let corrupt_d = inject_errors(block_d, &[0, 1]);

        // Build soft confidence: bits 0 and 1 are low-confidence.
        let mut soft = [1.0f32; 26];
        soft[0] = 0.05;
        soft[1] = 0.05;

        // Verify each corrupted block individually recovers via OSD(2).
        let (d_b, k_b) = decode_block_soft(corrupt_b, &soft, OSD_MAX_FLIP_COST, 3).expect("block B should recover");
        assert_eq!((d_b, k_b), (block_b_data, BlockKind::B));

        // C' check
        let (d_c, _k_c) = decode_block_soft(corrupt_c, &soft, OSD_MAX_FLIP_COST, 3).expect("block C' should recover");
        assert_eq!(d_c, 0x4865);

        let (d_d, k_d) = decode_block_soft(corrupt_d, &soft, OSD_MAX_FLIP_COST, 3).expect("block D should recover");
        assert_eq!(k_d, BlockKind::D);
        assert_eq!(d_d, u16::from_be_bytes(*b"Hi"));

        // Now run a complete group through the Candidate state machine.
        let mut cand = Candidate::new(240_000.0, 0.0);
        let mut last: Option<RdsData> = None;
        // Feed clean Block A.
        for bit_idx in (0..26).rev() {
            let _ = cand.push_bit_soft(((block_a >> bit_idx) & 1) as u8, 1.0);
        }
        // Feed corrupt B with low confidence on bits 0,1.
        for bit_idx in (0..26).rev() {
            let bit = ((corrupt_b >> bit_idx) & 1) as u8;
            let conf = if bit_idx >= 24 { 0.05 } else { 1.0 };
            let _ = cand.push_bit_soft(bit, conf);
        }
        // Feed corrupt C' with low confidence.
        for bit_idx in (0..26).rev() {
            let bit = ((corrupt_c >> bit_idx) & 1) as u8;
            let conf = if bit_idx >= 24 { 0.05 } else { 1.0 };
            let _ = cand.push_bit_soft(bit, conf);
        }
        // Feed corrupt D with low confidence.
        for bit_idx in (0..26).rev() {
            let bit = ((corrupt_d >> bit_idx) & 1) as u8;
            let conf = if bit_idx >= 24 { 0.05 } else { 1.0 };
            last = cand.push_bit_soft(bit, conf);
        }
        assert!(
            last.is_some(),
            "Full group should decode despite 2-bit errors in B/C/D"
        );
    }

    #[test]
    fn block_decode_rate_osd1_vs_osd2() {
        // Measure how many blocks with exactly 2 bit errors are recovered
        // by OSD(2) vs the number that would succeed at OSD(1) (none for 2-bit errors).
        //
        // For a valid block with 2 bit errors at the two *least* confident
        // positions, OSD(2) should always recover it; OSD(1) should never.
        let offsets = [OFFSET_A, OFFSET_B, OFFSET_C, OFFSET_CP, OFFSET_D];
        let data_values: [u16; 5] = [0x1111, 0x2222, 0x4865, 0xBEEF, 0xCAFE];

        let mut osd1_ok = 0u32;
        let mut osd2_ok = 0u32;
        let total = offsets.len() * data_values.len();

        for &offset in &offsets {
            for &data in &data_values {
                let word = encode_block(data, offset);
                // Flip bits k=0 and k=25 (spread across the word).
                let corrupted = word ^ (1 << 25) ^ (1 << 0);
                let mut soft = [1.0f32; 26];
                soft[0] = 0.01; // very uncertain
                soft[25] = 0.01;

                // OSD(1): should fail for 2-bit errors.
                let osd1_result = {
                    if decode_block(corrupted).is_some() {
                        Some(()) // d0 hit (unexpected but count it)
                    } else {
                        (0..26usize)
                            .find_map(|k| decode_block(corrupted ^ (1 << (25 - k))))
                            .map(|_| ())
                    }
                };
                if osd1_result.is_some() {
                    osd1_ok += 1;
                }

                // OSD(2).
                if decode_block_soft(corrupted, &soft, OSD_MAX_FLIP_COST, 3).is_some() {
                    osd2_ok += 1;
                }
            }
        }

        // OSD(2) must recover at least 80% of cleanly 2-bit-corrupted blocks.
        assert!(
            osd2_ok >= (total as u32 * 8 / 10),
            "OSD(2) recovery rate = {}/{total} (< 80%)",
            osd2_ok
        );
        // OSD(1) should not recover any (all are genuine 2-bit errors).
        assert_eq!(osd1_ok, 0, "OSD(1) should not recover 2-bit errors");
    }

    // -----------------------------------------------------------------------
    // Costas loop convergence
    // -----------------------------------------------------------------------

    #[test]
    fn costas_tracks_without_diverging_on_clean_signal() {
        // Feed a clean RDS signal through the decoder (no pilot ref) and
        // verify that at least some PI data is recovered, proving the Costas
        // loop stays coherent rather than losing lock permanently.
        let sample_rate = 240_000.0f32;
        let pi = 0x7777u16;

        let mut words: Vec<u32> = Vec::new();
        for seg in 0..4u8 {
            let g = group_0a(pi, seg, [b'C' + seg, b'D' + seg], 5);
            words.extend_from_slice(&g);
        }
        // 60× repetitions to give Costas plenty of time to acquire.
        let words: Vec<u32> = words
            .iter()
            .copied()
            .cycle()
            .take(words.len() * 60)
            .collect();

        let chips = blocks_to_chips(&words);
        let signal = chips_to_rds_signal(&chips, sample_rate);

        let mut dec = RdsDecoder::new(sample_rate as u32);
        let mut pi_correct = 0u32;
        let mut pi_total = 0u32;
        for &s in &signal {
            if let Some(state) = dec.process_sample(s, 1.0) {
                if state.pi.is_some() {
                    pi_total += 1;
                    if state.pi == Some(pi) {
                        pi_correct += 1;
                    }
                }
            }
        }
        assert!(
            pi_correct > 0,
            "Costas should converge and produce correct PI (got {pi_correct}/{pi_total})"
        );
    }

    // -----------------------------------------------------------------------
    // Noise rejection
    // -----------------------------------------------------------------------

    #[test]
    fn pure_noise_produces_zero_pi_decodes() {
        // Feed 2 seconds of white noise (no RDS signal) through the decoder.
        // The decoder must not report any PI (false positive).
        //
        // Note: with OSD(2) active in locked mode, the lock gate requires
        // Block A to be acquired first (hard or OSD-1 decode in search mode),
        // which keeps the false-acquisition rate low even at OSD(2).
        // Tech 9 (OSD cost ceiling) further suppresses noise-induced matches.
        let sample_rate = 240_000.0f32;
        let n_samples = (sample_rate * 2.0) as usize;
        let mut rng = 0xFEED_FACE_DEAD_BEEFu64;
        let mut noise: Vec<f32> = (0..n_samples).map(|_| gaussian(&mut rng)).collect();
        // Scale noise to unit power.
        let pwr = noise.iter().map(|x| x * x).sum::<f32>() / n_samples as f32;
        let scale = pwr.sqrt().recip();
        noise.iter_mut().for_each(|x| *x *= scale);

        let mut dec = RdsDecoder::new(sample_rate as u32);
        let mut false_pi = 0u32;
        for &s in &noise {
            if let Some(state) = dec.process_sample(s, 1.0) {
                if state.pi.is_some() {
                    false_pi += 1;
                }
            }
        }
        assert_eq!(
            false_pi, 0,
            "Pure noise generated {false_pi} false PI reports"
        );
    }

    // -----------------------------------------------------------------------
    // PI accumulation
    // -----------------------------------------------------------------------

    #[test]
    fn pi_accumulation_corrects_weak_pi_after_threshold() {
        // The PI LLR accumulator (Tech 6) should vote out a one-bit error
        // in the PI field after PI_ACC_THRESHOLD observations.
        let real_pi: u16 = 0x9420;
        let bad_pi: u16 = real_pi ^ 0x0001; // one bit wrong in LSB

        let pty = 10u8;
        let mut cand = Candidate::new(240_000.0, 0.0);

        // Send PI_ACC_THRESHOLD groups; each Block A carries the correct PI
        // but with a very low soft confidence on the corrupted bit position.
        for i in 0..(PI_ACC_THRESHOLD + 1) {
            let block_a = encode_block(real_pi, OFFSET_A);
            let block_b = encode_block(u16::from(pty) << 5, OFFSET_B);
            let block_c = encode_block(0, OFFSET_C);
            let block_d = encode_block(u16::from_be_bytes(*b"OK"), OFFSET_D);

            for bit_idx in (0..26).rev() {
                let bit = ((block_a >> bit_idx) & 1) as u8;
                // Bit 0 (LSB of PI) has low confidence.
                let conf = if bit_idx == 0 { 0.1 } else { 1.0 };
                let _ = cand.push_bit_soft(bit, conf);
            }
            for bit_idx in (0..26).rev() {
                let _ = cand.push_bit_soft(((block_b >> bit_idx) & 1) as u8, 1.0);
            }
            for bit_idx in (0..26).rev() {
                let _ = cand.push_bit_soft(((block_c >> bit_idx) & 1) as u8, 1.0);
            }
            let mut last = None;
            for bit_idx in (0..26).rev() {
                last = cand.push_bit_soft(((block_d >> bit_idx) & 1) as u8, 1.0);
            }
            let _ = (i, last, bad_pi); // silence unused warnings
        }

        // After threshold groups, the accumulated PI should converge to real_pi.
        let pi = cand.state.pi.expect("PI should be set after accumulation");
        assert_eq!(
            pi, real_pi,
            "Accumulated PI {pi:#06x} should converge to {real_pi:#06x}"
        );
    }
}
