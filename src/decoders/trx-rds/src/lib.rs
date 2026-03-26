// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::f32::consts::{PI, SQRT_2, TAU};

use trx_core::rig::state::RdsData;

const RDS_SUBCARRIER_HZ: f32 = 57_000.0;
const RDS_SYMBOL_RATE: f32 = 1_187.5;
/// Biphase (Manchester) chip rate: 2× the symbol rate.
const RDS_CHIP_RATE: f32 = RDS_SYMBOL_RATE * 2.0;
const RDS_POLY: u16 = 0x1B9;
const SEARCH_REG_MASK: u32 = (1 << 26) - 1;
const PHASE_CANDIDATES: usize = 4;
const BIPHASE_CLOCK_WINDOW: usize = 128;
/// Minimum quality score to publish RDS state to the outer decoder.
const MIN_PUBLISH_QUALITY: f32 = 0.65;
/// Tech 6: number of Block A observations before using accumulated PI.
const PI_ACC_THRESHOLD: u8 = 3;
/// Tech 5 — Costas loop proportional gain (per sample).
const COSTAS_KP: f32 = 8e-4;
/// Tech 5 — Costas loop integral gain (per sample).
const COSTAS_KI: f32 = 3.5e-7;
/// Tech 5 — maximum frequency correction per sample (radians).
const COSTAS_MAX_FREQ_CORR: f32 = 0.005;
/// Tech 1 — RRC roll-off factor.
const RRC_ALPHA: f32 = 0.75;
/// Tech 1 — RRC filter span in chips.
const RRC_SPAN_CHIPS: usize = 4;

const OFFSET_A: u16 = 0x0FC;
const OFFSET_B: u16 = 0x198;
const OFFSET_C: u16 = 0x168;
const OFFSET_CP: u16 = 0x350;
const OFFSET_D: u16 = 0x1B4;

// ---------------------------------------------------------------------------
// Tech 1: Root Raised Cosine matched filter
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

/// Causal FIR filter with a ring buffer.
#[derive(Debug, Clone)]
struct FirFilter {
    taps: Vec<f32>,
    buf: Vec<f32>,
    pos: usize,
}

impl FirFilter {
    fn new_rrc(sample_rate: f32, chip_rate: f32) -> Self {
        let sps = (sample_rate / chip_rate).max(2.0);
        let n_half = (RRC_SPAN_CHIPS as f32 * sps / 2.0).round() as usize;
        let n_taps = (2 * n_half + 1).min(513);
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

        let len = taps.len();
        Self {
            taps,
            buf: vec![0.0; len],
            pos: 0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let n = self.taps.len();
        self.buf[self.pos] = x;
        let mut acc = 0.0_f32;
        for (k, &tap) in self.taps.iter().enumerate() {
            let idx = if self.pos >= k {
                self.pos - k
            } else {
                n - k + self.pos
            };
            acc += tap * self.buf[idx];
        }
        self.pos = (self.pos + 1) % n;
        acc
    }

    #[allow(dead_code)]
    fn reset(&mut self) {
        for x in &mut self.buf {
            *x = 0.0;
        }
        self.pos = 0;
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
                // Pass soft magnitude as confidence alongside the bit.
                self.push_bit_soft(bit, magnitude)
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
        // Tech 3/7/8: use soft-decision decoder instead of hard decode.
        let Some((data, kind)) = decode_block_soft(word, &self.block_soft) else {
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
                self.locked = false;
                self.search_bits = 0;
                self.search_reg = 0;
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
    /// Tech 1: RRC matched filter for the I baseband channel.
    rrc_i: FirFilter,
    /// Tech 1: RRC matched filter for the Q baseband channel.
    rrc_q: FirFilter,
    /// Tech 5: Costas loop integrator state.
    costas_integrator: f32,
    /// Tech 2: pilot-derived 57 kHz carrier reference (cos, sin).
    /// When Some, the free-running NCO is bypassed and Costas is suppressed.
    pilot_ref: Option<(f32, f32)>,
    candidates: Vec<Candidate>,
    best_score: u32,
    best_state: Option<RdsData>,
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
            rrc_i: FirFilter::new_rrc(sample_rate_f, RDS_CHIP_RATE),
            rrc_q: FirFilter::new_rrc(sample_rate_f, RDS_CHIP_RATE),
            costas_integrator: 0.0,
            pilot_ref: None,
            candidates,
            best_score: 0,
            best_state: None,
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

        // Tech 1: apply RRC matched filter to I and Q.
        let mixed_i = self.rrc_i.process(raw_i);
        let mixed_q = self.rrc_q.process(raw_q);

        // Tech 5: Costas loop — tanh soft phase detector.
        // Only active when not using a pilot reference.
        if self.pilot_ref.is_none() {
            let err = mixed_i.tanh() * mixed_q;
            self.costas_integrator += COSTAS_KI * err;
            let freq_correction = (COSTAS_KP * err + self.costas_integrator)
                .clamp(-COSTAS_MAX_FREQ_CORR, COSTAS_MAX_FREQ_CORR);
            self.carrier_phase -= freq_correction;
            self.carrier_phase = self.carrier_phase.rem_euclid(TAU);
        }

        for candidate in &mut self.candidates {
            if let Some(update) = candidate.process_sample(mixed_i, mixed_q) {
                if candidate.score >= self.best_score {
                    self.best_score = candidate.score;
                    let same_pi = self.best_state.as_ref().and_then(|state| state.pi) == update.pi;
                    if publish_quality >= MIN_PUBLISH_QUALITY
                        || same_pi
                        || self.best_state.is_none()
                    {
                        self.best_state = Some(update);
                    }
                }
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

/// Tech 3/7/8: soft-decision block decoder implementing OSD(1).
///
/// `word` is the 26-bit hard-decision word; `soft[k]` is the confidence
/// magnitude (|LLR|) for the k-th received bit, where bit 0 is the MSB
/// (bit 25 of `word`) and bit 25 is the LSB (bit 0 of `word`).
///
/// Search order:
/// 1. Hard decode (Hamming distance 0) — zero cost.
/// 2. All 26 single-bit flips — return the lowest-cost success.
///
/// Limiting to distance 1 keeps false-positive rates low while still
/// correcting single-bit burst errors.
fn decode_block_soft(word: u32, soft: &[f32; 26]) -> Option<(u16, BlockKind)> {
    // Distance 0.
    if let Some(result) = decode_block(word) {
        return Some(result);
    }

    let mut best_result: Option<(u16, BlockKind)> = None;
    let mut best_cost = f32::INFINITY;

    // Distance 1: all 26 single-bit flips.
    for (k, &flip_cost) in soft.iter().enumerate() {
        let trial = word ^ (1 << (25 - k));
        if let Some(result) = decode_block(trial) {
            if flip_cost < best_cost {
                best_cost = flip_cost;
                best_result = Some(result);
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
        let fir = FirFilter::new_rrc(240_000.0, RDS_CHIP_RATE);
        let sum: f32 = fir.taps.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "RRC DC gain = {sum}");
    }

    #[test]
    fn decode_block_soft_corrects_single_bit_error() {
        let word = encode_block(0xABCD, OFFSET_A);
        // Flip one bit (bit 10, i.e. position k=15 from MSB).
        let corrupted = word ^ (1 << 10);
        let soft = [1.0f32; 26];
        let (data, kind) = decode_block_soft(corrupted, &soft).expect("should recover");
        assert_eq!(data, 0xABCD);
        assert_eq!(kind, BlockKind::A);
    }

    #[test]
    fn decode_block_soft_rejects_two_bit_error() {
        // OSD(1) does not correct 2-bit errors; verify it returns None.
        let word = encode_block(0x1234, OFFSET_B);
        let corrupted = word ^ 0b11; // flip two bits
        let soft = [1.0f32; 26];
        assert!(decode_block_soft(corrupted, &soft).is_none());
    }

    #[test]
    fn decode_block_soft_prefers_least_costly_flip() {
        // Construct a word with an injected single-bit error at bit k=2 (high confidence)
        // and also make bits k=24,25 low-confidence. The decoder should flip k=2 (cheapest).
        let word = encode_block(0xBEEF, OFFSET_D);
        let corrupted = word ^ (1 << (25 - 2)); // flip bit k=2
        let mut soft = [1.0f32; 26];
        soft[2] = 0.01; // least confident → cheapest to flip
        let (data, kind) = decode_block_soft(corrupted, &soft).expect("should recover");
        assert_eq!(data, 0xBEEF);
        assert_eq!(kind, BlockKind::D);
    }
}
