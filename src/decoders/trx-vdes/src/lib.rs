// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Early VDES 100 kHz decoder scaffold.
//!
//! This decoder no longer reuses the AIS FM-audio path. It consumes filtered
//! complex baseband for a single 100 kHz channel and performs:
//! - burst energy detection
//! - coarse DC removal / normalization
//! - differential phase extraction
//! - coarse symbol timing at the 76.8 ksps VDE-TER baseline
//! - `pi/4`-QPSK quadrant slicing
//!
//! It intentionally stops at a raw burst payload stage. Full M.2092-1 FEC,
//! interleaving, link-layer parsing, and application payload decoding are not
//! implemented yet.

use num_complex::Complex;
use trx_core::decode::VdesMessage;

const VDES_SYMBOL_RATE: f32 = 76_800.0;
const MIN_BURST_MS: f32 = 2.0;
const BURST_END_MS: f32 = 0.4;
const MIN_BURST_SYMBOLS: usize = 64;
const TER_MCS1_100_BURST_SYMBOLS: usize = 1_984;
const TER_MCS1_100_RAMP_SYMBOLS: usize = 32;
const TER_MCS1_100_SYNC_SYMBOLS: usize = 27;
const TER_MCS1_100_LINK_ID_SYMBOLS: usize = 16;
const TER_MCS1_100_PAYLOAD_SYMBOLS: usize = 1_877;

#[derive(Debug, Clone)]
pub struct VdesDecoder {
    sample_rate: f32,
    noise_floor: f32,
    in_burst: bool,
    quiet_run: u32,
    burst_samples: Vec<Complex<f32>>,
}

impl VdesDecoder {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(1) as f32,
            noise_floor: 1.0e-4,
            in_burst: false,
            quiet_run: 0,
            burst_samples: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.noise_floor = 1.0e-4;
        self.in_burst = false;
        self.quiet_run = 0;
        self.burst_samples.clear();
    }

    pub fn process_samples(&mut self, samples: &[Complex<f32>], channel: &str) -> Vec<VdesMessage> {
        let mut out = Vec::new();
        let min_burst_samples =
            ((self.sample_rate * (MIN_BURST_MS / 1000.0)).round() as usize).max(16);
        let quiet_limit =
            ((self.sample_rate * (BURST_END_MS / 1000.0)).round() as u32).max(4);

        for &sample in samples {
            let power = sample.norm_sqr();
            if !self.in_burst {
                self.noise_floor = 0.995 * self.noise_floor + 0.005 * power;
                let trigger = (self.noise_floor * 8.0).max(2.0e-4);
                if power >= trigger {
                    self.in_burst = true;
                    self.quiet_run = 0;
                    self.burst_samples.clear();
                    self.burst_samples.push(sample);
                }
                continue;
            }

            self.burst_samples.push(sample);
            let sustain = (self.noise_floor * 3.0).max(1.2e-4);
            if power < sustain {
                self.quiet_run = self.quiet_run.saturating_add(1);
            } else {
                self.quiet_run = 0;
            }

            if self.quiet_run >= quiet_limit {
                if self.burst_samples.len() >= min_burst_samples {
                    if let Some(msg) = self.finalize_burst(channel) {
                        out.push(msg);
                    }
                }
                self.in_burst = false;
                self.quiet_run = 0;
                self.burst_samples.clear();
            }
        }

        out
    }

    fn finalize_burst(&self, channel: &str) -> Option<VdesMessage> {
        let samples = self.prepare_burst();
        if samples.len() < 8 {
            return None;
        }

        let symbols = slice_pi4_qpsk_symbols(&samples, self.sample_rate);
        if symbols.len() < MIN_BURST_SYMBOLS {
            return None;
        }

        let framed = extract_candidate_frame(&symbols)?;
        let link_id = decode_link_id_from_symbols(&framed.symbols);
        let payload_symbols = framed.payload_symbols();
        let deinterleaved = deinterleave_100khz_frame(payload_symbols);
        let raw_bytes = pack_dibits_msb(&deinterleaved);
        let rms = burst_rms(&samples);
        let mode = classify_vdes_burst(framed.symbols.len());
        let link_text = link_id
            .map(|value| format!("LID {}", value))
            .unwrap_or_else(|| "LID ?".to_string());

        Some(VdesMessage {
            ts_ms: None,
            channel: channel.to_string(),
            message_type: mode.message_type,
            repeat: 0,
            mmsi: 0,
            crc_ok: false,
            bit_len: deinterleaved.len() * 2,
            raw_bytes,
            lat: None,
            lon: None,
            sog_knots: None,
            cog_deg: None,
            heading_deg: None,
            nav_status: None,
            vessel_name: Some(format!("VDES Frame {} sym", framed.symbols.len())),
            callsign: Some(format!("{} {} @{}", mode.label, link_text, framed.start_offset)),
            destination: Some(format!(
                "TER-MCS-1.100 RMS {:.2} sync {:.2} turbo FEC pending",
                rms, framed.preamble_score
            )),
        })
    }

    fn prepare_burst(&self) -> Vec<Complex<f32>> {
        if self.burst_samples.is_empty() {
            return Vec::new();
        }

        let len = self.burst_samples.len() as f32;
        let mean = self
            .burst_samples
            .iter()
            .copied()
            .fold(Complex::new(0.0_f32, 0.0_f32), |acc, sample| acc + sample)
            / len;

        let mut out: Vec<Complex<f32>> = self
            .burst_samples
            .iter()
            .map(|sample| *sample - mean)
            .collect();

        let rms = burst_rms(&out);
        if rms > 1.0e-6 {
            for sample in &mut out {
                *sample /= rms;
            }
        }

        out
    }
}

struct BurstMode<'a> {
    label: &'a str,
    message_type: u8,
}

struct FrameSlice {
    start_offset: usize,
    preamble_score: f32,
    symbols: Vec<u8>,
}

impl FrameSlice {
    fn payload_symbols(&self) -> &[u8] {
        let payload_start = TER_MCS1_100_RAMP_SYMBOLS + TER_MCS1_100_SYNC_SYMBOLS + TER_MCS1_100_LINK_ID_SYMBOLS;
        let payload_end = payload_start + TER_MCS1_100_PAYLOAD_SYMBOLS;
        if self.symbols.len() <= payload_start {
            return &[];
        }
        &self.symbols[payload_start..self.symbols.len().min(payload_end)]
    }
}

fn classify_vdes_burst(symbols: usize) -> BurstMode<'static> {
    if symbols >= TER_MCS1_100_BURST_SYMBOLS {
        BurstMode {
            label: "TER-MCS-1.100",
            message_type: 101,
        }
    } else {
        BurstMode {
            label: "TER-MCS-1",
            message_type: 100,
        }
    }
}

fn extract_candidate_frame(symbols: &[u8]) -> Option<FrameSlice> {
    if symbols.len() < TER_MCS1_100_SYNC_SYMBOLS {
        return None;
    }

    let search_limit = symbols
        .len()
        .saturating_sub(TER_MCS1_100_BURST_SYMBOLS.saturating_sub(TER_MCS1_100_SYNC_SYMBOLS));
    let mut best_offset = 0usize;
    let mut best_score = f32::MIN;

    for offset in 0..=search_limit {
        let sync_offset = offset + TER_MCS1_100_RAMP_SYMBOLS;
        if sync_offset >= symbols.len() {
            break;
        }
        let score = preamble_like_score(&symbols[sync_offset..]);
        if score > best_score {
            best_score = score;
            best_offset = offset;
        }
    }

    let available = symbols.len().saturating_sub(best_offset);
    if available < MIN_BURST_SYMBOLS {
        return None;
    }
    let take = available.min(TER_MCS1_100_BURST_SYMBOLS);
    Some(FrameSlice {
        start_offset: best_offset,
        preamble_score: best_score,
        symbols: symbols[best_offset..best_offset + take].to_vec(),
    })
}

fn preamble_like_score(symbols: &[u8]) -> f32 {
    if symbols.len() < TER_MCS1_100_SYNC_SYMBOLS {
        return f32::MIN;
    }
    let window = &symbols[..TER_MCS1_100_SYNC_SYMBOLS];
    let mut score = 0.0_f32;
    for (idx, &dibit) in window.iter().enumerate() {
        if dibit == 0b00 || dibit == 0b11 {
            score += 1.0;
        } else {
            score -= 1.5;
        }
        if idx > 0 {
            if dibit != window[idx - 1] {
                score += 0.4;
            } else {
                score -= 0.2;
            }
        }
    }
    score / TER_MCS1_100_SYNC_SYMBOLS as f32
}

fn deinterleave_100khz_frame(symbols: &[u8]) -> Vec<u8> {
    if symbols.len() < 8 {
        return symbols.to_vec();
    }
    let cols = 16usize;
    let rows = symbols.len().div_ceil(cols);
    let mut out = vec![0u8; symbols.len()];
    for idx in 0..symbols.len() {
        let row = idx / cols;
        let col = idx % cols;
        let interleaved_idx = col * rows + row;
        if interleaved_idx < symbols.len() {
            out[idx] = symbols[interleaved_idx];
        } else {
            out[idx] = symbols[idx];
        }
    }
    out
}

fn decode_link_id_from_symbols(symbols: &[u8]) -> Option<u8> {
    let start = TER_MCS1_100_RAMP_SYMBOLS + TER_MCS1_100_SYNC_SYMBOLS;
    let end = start + TER_MCS1_100_LINK_ID_SYMBOLS;
    if symbols.len() < end {
        return None;
    }
    let bits = dibits_to_bits(&symbols[start..end]);
    if bits.len() != 32 {
        return None;
    }
    decode_rm_1_5(&bits)
}

fn dibits_to_bits(symbols: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(symbols.len() * 2);
    for &dibit in symbols {
        out.push((dibit >> 1) & 1);
        out.push(dibit & 1);
    }
    out
}

fn decode_rm_1_5(bits: &[u8]) -> Option<u8> {
    if bits.len() != 32 {
        return None;
    }
    let mut best_id = 0u8;
    let mut best_dist = usize::MAX;
    for id in 0u8..64 {
        let code = rm_1_5_codeword(id);
        let dist = code
            .iter()
            .zip(bits.iter())
            .filter(|(a, b)| a != b)
            .count();
        if dist < best_dist {
            best_dist = dist;
            best_id = id;
        }
    }
    if best_dist <= 8 {
        Some(best_id)
    } else {
        None
    }
}

fn rm_1_5_codeword(value: u8) -> [u8; 32] {
    let a0 = (value >> 5) & 1;
    let a1 = (value >> 4) & 1;
    let a2 = (value >> 3) & 1;
    let a3 = (value >> 2) & 1;
    let a4 = (value >> 1) & 1;
    let a5 = value & 1;
    let mut out = [0u8; 32];
    for idx in 0..32 {
        let x1 = ((idx >> 4) & 1) as u8;
        let x2 = ((idx >> 3) & 1) as u8;
        let x3 = ((idx >> 2) & 1) as u8;
        let x4 = ((idx >> 1) & 1) as u8;
        let x5 = (idx & 1) as u8;
        out[idx] = a0 ^ (a1 & x1) ^ (a2 & x2) ^ (a3 & x3) ^ (a4 & x4) ^ (a5 & x5);
    }
    out
}

fn burst_rms(samples: &[Complex<f32>]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let power = samples.iter().map(|sample| sample.norm_sqr()).sum::<f32>() / samples.len() as f32;
    power.sqrt()
}

fn slice_pi4_qpsk_symbols(samples: &[Complex<f32>], sample_rate: f32) -> Vec<u8> {
    if samples.len() < 2 {
        return Vec::new();
    }

    let mut phase_clock = 0.0_f32;
    let mut prev = samples[0];
    let mut symbols = Vec::with_capacity(((samples.len() as f32) * VDES_SYMBOL_RATE / sample_rate) as usize + 4);

    for &sample in &samples[1..] {
        phase_clock += VDES_SYMBOL_RATE;
        let diff = sample * prev.conj();
        prev = sample;

        while phase_clock >= sample_rate {
            phase_clock -= sample_rate;
            symbols.push(quantize_pi4_qpsk(diff));
        }
    }

    symbols
}

fn quantize_pi4_qpsk(sample: Complex<f32>) -> u8 {
    let angle = sample.im.atan2(sample.re);
    let candidates = [
        (std::f32::consts::FRAC_PI_4, 0b00),
        (3.0 * std::f32::consts::FRAC_PI_4, 0b01),
        (-3.0 * std::f32::consts::FRAC_PI_4, 0b11),
        (-std::f32::consts::FRAC_PI_4, 0b10),
    ];

    let mut best = 0b00;
    let mut best_err = f32::MAX;
    for (ref_angle, dibit) in candidates {
        let mut err = angle - ref_angle;
        while err > std::f32::consts::PI {
            err -= std::f32::consts::TAU;
        }
        while err < -std::f32::consts::PI {
            err += std::f32::consts::TAU;
        }
        let abs_err = err.abs();
        if abs_err < best_err {
            best_err = abs_err;
            best = dibit;
        }
    }

    best
}

fn pack_dibits_msb(symbols: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((symbols.len() + 3) / 4);
    let mut byte = 0u8;
    let mut count = 0usize;

    for &dibit in symbols {
        let shift = 6usize.saturating_sub((count % 4) * 2);
        byte |= (dibit & 0b11) << shift;
        count += 1;
        if count % 4 == 0 {
            out.push(byte);
            byte = 0;
        }
    }

    if count % 4 != 0 {
        out.push(byte);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(angle: f32) -> Complex<f32> {
        Complex::new(angle.cos(), angle.sin())
    }

    #[test]
    fn packs_dibits_msb_first() {
        assert_eq!(pack_dibits_msb(&[0b00, 0b01, 0b10, 0b11]), vec![0b0001_1011]);
    }

    #[test]
    fn quantizes_pi_over_four_steps() {
        assert_eq!(quantize_pi4_qpsk(phase(std::f32::consts::FRAC_PI_4)), 0b00);
        assert_eq!(quantize_pi4_qpsk(phase(3.0 * std::f32::consts::FRAC_PI_4)), 0b01);
        assert_eq!(quantize_pi4_qpsk(phase(-3.0 * std::f32::consts::FRAC_PI_4)), 0b11);
        assert_eq!(quantize_pi4_qpsk(phase(-std::f32::consts::FRAC_PI_4)), 0b10);
    }

    #[test]
    fn slices_simple_symbol_stream() {
        let sample_rate = 96_000.0;
        let mut samples = Vec::new();
        let mut current = phase(0.0);
        for angle in [
            std::f32::consts::FRAC_PI_4,
            3.0 * std::f32::consts::FRAC_PI_4,
            -3.0 * std::f32::consts::FRAC_PI_4,
            -std::f32::consts::FRAC_PI_4,
        ] {
            current *= phase(angle);
            samples.push(current);
            samples.push(current);
        }
        let symbols = slice_pi4_qpsk_symbols(&samples, sample_rate);
        assert!(!symbols.is_empty());
    }

    #[test]
    fn extracts_candidate_frame_window() {
        let mut symbols = vec![0u8; 40];
        symbols.extend((0..TER_MCS1_100_BURST_SYMBOLS).map(|idx| (idx % 4) as u8));
        let frame = extract_candidate_frame(&symbols).expect("frame should be found");
        assert!(frame.symbols.len() >= MIN_BURST_SYMBOLS);
    }

    #[test]
    fn deinterleave_preserves_length() {
        let symbols: Vec<u8> = (0..127).map(|idx| (idx % 4) as u8).collect();
        let out = deinterleave_100khz_frame(&symbols);
        assert_eq!(out.len(), symbols.len());
    }
}
