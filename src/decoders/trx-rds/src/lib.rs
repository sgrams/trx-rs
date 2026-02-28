// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::f32::consts::TAU;

use trx_core::rig::state::RdsData;

const RDS_SUBCARRIER_HZ: f32 = 57_000.0;
const RDS_SYMBOL_RATE: f32 = 1_187.5;
const RDS_PSK_SYMBOL_RATE: f32 = RDS_SYMBOL_RATE * 2.0;
const RDS_POLY: u16 = 0x1B9;
const SEARCH_REG_MASK: u32 = (1 << 26) - 1;
const PHASE_CANDIDATES: usize = 8;
const BIPHASE_CLOCK_WINDOW: usize = 128;
const RDS_BASEBAND_LP_HZ: f32 = 3_000.0;
const MIN_PUBLISH_QUALITY: f32 = 0.45;

const OFFSET_A: u16 = 0x0FC;
const OFFSET_B: u16 = 0x198;
const OFFSET_C: u16 = 0x168;
const OFFSET_CP: u16 = 0x350;
const OFFSET_D: u16 = 0x1B4;

#[derive(Debug, Clone)]
struct OnePoleLowPass {
    alpha: f32,
    y: f32,
}

impl OnePoleLowPass {
    fn new(sample_rate: f32, cutoff_hz: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let cutoff = cutoff_hz.clamp(1.0, sr * 0.49);
        let dt = 1.0 / sr;
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
        let alpha = dt / (rc + dt);
        Self { alpha, y: 0.0 }
    }

    fn process(&mut self, x: f32) -> f32 {
        self.y += self.alpha * (x - self.y);
        self.y
    }
}

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
}

impl Candidate {
    fn new(sample_rate: f32, phase_offset: f32) -> Self {
        Self {
            clock_phase: phase_offset,
            clock_inc: RDS_PSK_SYMBOL_RATE / sample_rate.max(1.0),
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
                let mut even_sum = 0.0;
                let mut odd_sum = 0.0;
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
                self.push_bit(bit)
            } else {
                None
            }
        } else {
            None
        };
        self.prev_psk_symbol = Some(symbol);
        update
    }

    fn push_bit(&mut self, bit: u8) -> Option<RdsData> {
        if self.locked {
            self.block_reg = ((self.block_reg << 1) | u32::from(bit)) & SEARCH_REG_MASK;
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
        let Some((data, kind)) = decode_block(word) else {
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
                self.process_group(self.block_a, self.block_b, self.block_c, self.block_c_kind, data)
            }
            (_, BlockKind::A) => {
                self.locked = true;
                self.expect = ExpectBlock::B;
                self.block_reg = 0;
                self.block_bits = 0;
                self.block_a = data;
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

    fn process_group(
        &mut self,
        block_a: u16,
        block_b: u16,
        block_c: u16,
        block_c_kind: BlockKind,
        block_d: u16,
    ) -> Option<RdsData> {
        let mut changed = false;
        if self.state.pi != Some(block_a) {
            self.state.pi = Some(block_a);
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
                let ps = String::from_utf8_lossy(&self.ps_bytes).trim_end().to_string();
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
                let rt = String::from_utf8_lossy(&self.rt_bytes[..rt_len]).trim_end().to_string();
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
                let ptyn = String::from_utf8_lossy(&self.ptyn_bytes).trim_end().to_string();
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

#[derive(Debug, Clone)]
pub struct RdsDecoder {
    sample_rate_hz: u32,
    carrier_phase: f32,
    carrier_inc: f32,
    i_lp: OnePoleLowPass,
    q_lp: OnePoleLowPass,
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
            i_lp: OnePoleLowPass::new(sample_rate_f, RDS_BASEBAND_LP_HZ),
            q_lp: OnePoleLowPass::new(sample_rate_f, RDS_BASEBAND_LP_HZ),
            candidates,
            best_score: 0,
            best_state: None,
        }
    }

    pub fn process_sample(&mut self, sample: f32, quality: f32) -> Option<&RdsData> {
        let publish_quality = quality.clamp(0.0, 1.0);
        let (sin_p, cos_p) = self.carrier_phase.sin_cos();
        self.carrier_phase = (self.carrier_phase + self.carrier_inc).rem_euclid(TAU);
        let mixed_i = self.i_lp.process(sample * cos_p * 2.0);
        let mixed_q = self.q_lp.process(sample * -sin_p * 2.0);

        for candidate in &mut self.candidates {
            if let Some(update) = candidate.process_sample(mixed_i, mixed_q) {
                if candidate.score >= self.best_score {
                    self.best_score = candidate.score;
                    let same_pi = self.best_state.as_ref().and_then(|state| state.pi) == update.pi;
                    if publish_quality >= MIN_PUBLISH_QUALITY || same_pi || self.best_state.is_none() {
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

fn sanitize_text_byte(byte: u8) -> u8 {
    if (0x20..=0x7e).contains(&byte) {
        byte
    } else {
        b' '
    }
}

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
        let block_b = encode_block((10 << 5) | 0, OFFSET_B);
        let block_d = encode_block(u16::from_be_bytes(*b"AB"), OFFSET_D);

        for bit_idx in (0..26).rev() {
            let bit = ((block_a >> bit_idx) & 1) as u8;
            let _ = candidate.push_bit(bit);
        }
        for bit_idx in (0..26).rev() {
            let bit = ((block_b >> bit_idx) & 1) as u8;
            let _ = candidate.push_bit(bit);
        }
        let filler = encode_block(0, OFFSET_C);
        for bit_idx in (0..26).rev() {
            let bit = ((filler >> bit_idx) & 1) as u8;
            let _ = candidate.push_bit(bit);
        }
        let mut last = None;
        for bit_idx in (0..26).rev() {
            let bit = ((block_d >> bit_idx) & 1) as u8;
            last = candidate.push_bit(bit);
        }

        assert!(last.is_some());
        let state = last.unwrap();
        assert_eq!(state.pi, Some(pi));
        assert_eq!(state.pty, Some(10));
        assert_eq!(state.pty_name.as_deref(), Some("Pop Music"));
    }
}
