// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! VDES 100 kHz decoder for VDE-TER (ITU-R M.2092-1).
//!
//! This decoder consumes filtered complex baseband for a single 100 kHz
//! channel and performs:
//! - burst energy detection
//! - coarse DC removal / normalization
//! - differential phase extraction (CFO-corrected via 4th-power method)
//! - symbol timing at the 76.8 ksps VDE-TER baseline
//! - π/4-QPSK quadrant slicing
//! - 16-column block deinterleaving
//! - Turbo FEC decoding (dual 8-state RSC with BCJR/MAP, QPP interleaver)
//! - CRC-16-CCITT validation
//! - M.2092-1 link-layer frame parsing (Messages 0–6)

mod crc;
mod link_layer;
mod turbo;

use num_complex::Complex;
use trx_core::decode::VdesMessage;

const VDES_SYMBOL_RATE: f32 = 76_800.0;
const MIN_BURST_MS: f32 = 1.5;
const BURST_END_MS: f32 = 0.35;
const MIN_BURST_SYMBOLS: usize = 64;
const TER_MCS1_100_BURST_SYMBOLS: usize = 1_984;
const TER_MCS1_100_RAMP_SYMBOLS: usize = 32;
const TER_MCS1_100_SYNC_SYMBOLS: usize = 27;
const TER_MCS1_100_LINK_ID_SYMBOLS: usize = 16;
const TER_MCS1_100_PAYLOAD_SYMBOLS: usize = 1_877;
const TER_MCS1_100_FEC_INPUT_SYMBOLS: usize = 1_872;
const TER_MCS1_100_FEC_OUTPUT_BITS: usize = 1_872;
const TER_MCS1_100_FEC_TAIL_BITS: usize = 10;
const TER_MCS1_100_SYNC_BITS: &[u8; TER_MCS1_100_SYNC_SYMBOLS] = b"111111001101010000011001010";
const PI4_QPSK_DIBITS: [u8; 4] = [0b00, 0b01, 0b11, 0b10];
const MIN_SYNC_CANDIDATE_SCORE: f32 = 0.20;
const MIN_SYNC_PARSE_SCORE: f32 = 0.50;
const BURST_TRIGGER_NOISE_MULT: f32 = 3.0;
const BURST_TRIGGER_FLOOR: f32 = 1.0e-10;
const BURST_SUSTAIN_NOISE_MULT: f32 = 1.15;
const BURST_SUSTAIN_FLOOR: f32 = 1.0e-11;

/// Warmup period: number of samples to observe before burst detection starts.
/// This allows the noise-floor EMA (α = 0.005, τ ≈ 200 samples) to converge
/// to the actual SDR noise level.  Without warmup the initial floor of 1e-12
/// is far below any real ADC noise, so the very first sample always triggers a
/// burst whose sustain threshold is also near 1e-12 — meaning quiet_run never
/// reaches quiet_limit and the burst never terminates.
const NOISE_FLOOR_WARMUP_SECS: f32 = 0.05; // 50 ms ≈ 10 EMA time-constants

#[derive(Debug, Clone)]
pub struct VdesDecoder {
    sample_rate: f32,
    noise_floor: f32,
    in_burst: bool,
    quiet_run: u32,
    burst_samples: Vec<Complex<f32>>,
    /// Remaining warmup samples.  Burst detection is suppressed while > 0.
    warmup_remaining: usize,
}

impl VdesDecoder {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(1) as f32;
        Self {
            sample_rate: sr,
            noise_floor: 1.0e-12,
            in_burst: false,
            quiet_run: 0,
            burst_samples: Vec::new(),
            warmup_remaining: (sr * NOISE_FLOOR_WARMUP_SECS) as usize,
        }
    }

    pub fn reset(&mut self) {
        self.noise_floor = 1.0e-12;
        self.in_burst = false;
        self.quiet_run = 0;
        self.burst_samples.clear();
        self.warmup_remaining = (self.sample_rate * NOISE_FLOOR_WARMUP_SECS) as usize;
    }

    pub fn process_samples(&mut self, samples: &[Complex<f32>], channel: &str) -> Vec<VdesMessage> {
        let mut out = Vec::new();
        let min_burst_samples =
            ((self.sample_rate * (MIN_BURST_MS / 1000.0)).round() as usize).max(16);
        let quiet_limit = ((self.sample_rate * (BURST_END_MS / 1000.0)).round() as u32).max(4);

        // Warmup phase: feed samples into the noise-floor EMA only.
        if self.warmup_remaining > 0 {
            let n = self.warmup_remaining.min(samples.len());
            for &sample in &samples[..n] {
                let power = sample.norm_sqr();
                self.noise_floor = 0.995 * self.noise_floor + 0.005 * power;
            }
            self.warmup_remaining -= n;
            if n == samples.len() {
                return out;
            }
            // Process any remaining samples after warmup completes.
            return out
                .into_iter()
                .chain(self.process_samples(&samples[n..], channel))
                .collect();
        }

        for &sample in samples {
            let power = sample.norm_sqr();
            if !self.in_burst {
                self.noise_floor = 0.995 * self.noise_floor + 0.005 * power;
                let trigger =
                    (self.noise_floor * BURST_TRIGGER_NOISE_MULT).max(BURST_TRIGGER_FLOOR);
                if power >= trigger {
                    self.in_burst = true;
                    self.quiet_run = 0;
                    self.burst_samples.clear();
                    self.burst_samples.push(sample);
                }
                continue;
            }

            self.burst_samples.push(sample);
            let sustain = (self.noise_floor * BURST_SUSTAIN_NOISE_MULT).max(BURST_SUSTAIN_FLOOR);
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

        let framed =
            extract_candidate_frame(&symbols).unwrap_or_else(|| fallback_frame_slice(&symbols));
        let rms = burst_rms(&samples);
        let mode = classify_vdes_burst(framed.symbols.len());
        let payload_symbols = framed.payload_symbols();
        let deinterleaved = deinterleave_100khz_frame(payload_symbols);
        if framed.sync_score < MIN_SYNC_PARSE_SCORE {
            return Some(build_unsynced_message(
                channel,
                &framed,
                &mode,
                rms,
                &framed.symbols,
            ));
        }

        let link_id = decode_link_id_from_symbols(&framed.symbols);
        let (fec_input_symbols, fec_tail_symbols) = split_fec_frame(&deinterleaved);
        let coded_bits = dibits_to_bits(fec_input_symbols);

        // --- Turbo FEC decode (primary path) ---
        // The info block length is half the coded bits for rate 1/2.
        let info_len = coded_bits.len() / 2;
        let (turbo_bits, turbo_reliability) = turbo::turbo_decode(&coded_bits, info_len);

        // --- Try link-layer parse on turbo-decoded bits ---
        let turbo_frame = if !turbo_bits.is_empty() {
            link_layer::parse_link_layer(&turbo_bits)
        } else {
            None
        };

        // If turbo decode + link-layer parse succeeds with valid CRC, use it.
        if let Some(ref ll_frame) = turbo_frame {
            if ll_frame.crc_ok {
                return Some(build_link_layer_message(
                    channel,
                    ll_frame,
                    &framed,
                    &mode,
                    rms,
                    link_id,
                    turbo_reliability,
                ));
            }
        }

        // --- Fallback: Viterbi decode (legacy path) ---
        let decoded_bits = viterbi_decode_rate_half(&coded_bits);
        if decoded_bits.is_empty() {
            return Some(build_unsynced_message(
                channel,
                &framed,
                &mode,
                rms,
                &framed.symbols,
            ));
        }

        // Try link-layer parse on Viterbi-decoded bits too
        let viterbi_frame = link_layer::parse_link_layer(&decoded_bits);
        if let Some(ref ll_frame) = viterbi_frame {
            if ll_frame.crc_ok {
                return Some(build_link_layer_message(
                    channel,
                    ll_frame,
                    &framed,
                    &mode,
                    rms,
                    link_id,
                    0.0,
                ));
            }
        }

        // --- Neither FEC produced a valid CRC: fall back to plausibility scoring ---
        // Prefer turbo result if it had a valid link-layer parse (even without CRC).
        let (use_turbo, decoded_ref) = if let Some(ref ll_frame) = turbo_frame {
            if ll_frame.message_id <= 6 {
                (true, turbo_bits.as_slice())
            } else {
                (false, decoded_bits.as_slice())
            }
        } else {
            (false, decoded_bits.as_slice())
        };

        let parsed = parse_vdes_payload(decoded_ref);
        let payload_bits = if parsed.payload_bits.is_empty() {
            decoded_ref
        } else {
            parsed.payload_bits.as_slice()
        };
        let raw_bytes = pack_bits_msb(payload_bits);
        let link_text = link_id
            .map(|value| format!("LID {}", value))
            .unwrap_or_else(|| "LID ?".to_string());
        let tail_zero_bits = dibits_to_bits(fec_tail_symbols)
            .into_iter()
            .filter(|bit| *bit == 0)
            .count();
        let plausibility = vdes_plausibility_score(&parsed, link_id, tail_zero_bits);
        if plausibility < -35 {
            return Some(build_unsynced_message(
                channel,
                &framed,
                &mode,
                rms,
                &framed.symbols,
            ));
        }

        // Check CRC on decoded bits (no link-layer wrapper)
        let crc_ok = crc::check_crc16(decoded_ref);

        let fec_label = if use_turbo {
            format!(
                "Turbo FEC (8-iter BCJR), reliability {:.2}{}",
                turbo_reliability,
                if plausibility < 15 {
                    " · Low confidence"
                } else {
                    ""
                }
            )
        } else {
            format!(
                "Hard-decision 1/2 Viterbi, tail {} / {} zero bits{}",
                tail_zero_bits,
                TER_MCS1_100_FEC_TAIL_BITS,
                if plausibility < 15 {
                    " · Low confidence"
                } else {
                    ""
                }
            )
        };
        let destination = parsed.summary.clone().or_else(|| {
            Some(format!(
                "TER-MCS-1.100 RMS {:.2} sync {:.0}% rot {}",
                rms,
                framed.sync_score * 100.0,
                framed.phase_rotation
            ))
        });

        Some(VdesMessage {
            rig_id: None,
            ts_ms: None,
            channel: channel.to_string(),
            message_type: parsed.message_id.unwrap_or(mode.message_type),
            repeat: parsed.repeat,
            mmsi: parsed.source_id.unwrap_or(0),
            crc_ok,
            bit_len: payload_bits.len(),
            raw_bytes,
            lat: parsed.lat,
            lon: parsed.lon,
            sog_knots: None,
            cog_deg: None,
            heading_deg: None,
            nav_status: None,
            vessel_name: Some(format!(
                "{} {} sym",
                parsed.message_label.unwrap_or("VDES Frame"),
                framed.symbols.len()
            )),
            callsign: Some(format!(
                "{} {} @{}",
                mode.label, link_text, framed.start_offset
            )),
            destination,
            message_label: parsed.message_label.map(str::to_string),
            session_id: parsed.session_id,
            source_id: parsed.source_id,
            destination_id: parsed.destination_id,
            data_count: parsed.data_count,
            asm_identifier: parsed.asm_identifier,
            ack_nack_mask: parsed.ack_nack_mask,
            channel_quality: parsed.channel_quality,
            payload_preview: parsed.payload_preview,
            link_id,
            sync_score: Some(framed.sync_score),
            sync_errors: Some(framed.sync_errors),
            phase_rotation: Some(framed.phase_rotation),
            fec_state: Some(fec_label),
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

#[derive(Default)]
struct ParsedPayload {
    message_id: Option<u8>,
    message_label: Option<&'static str>,
    repeat: u8,
    session_id: Option<u8>,
    source_id: Option<u32>,
    destination_id: Option<u32>,
    data_count: Option<u16>,
    asm_identifier: Option<u16>,
    ack_nack_mask: Option<u16>,
    channel_quality: Option<u8>,
    payload_bits: Vec<u8>,
    payload_preview: Option<String>,
    summary: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
}

struct FrameSlice {
    start_offset: usize,
    sync_score: f32,
    sync_errors: u8,
    phase_rotation: u8,
    symbols: Vec<u8>,
}

impl FrameSlice {
    fn payload_symbols(&self) -> &[u8] {
        let payload_start =
            TER_MCS1_100_RAMP_SYMBOLS + TER_MCS1_100_SYNC_SYMBOLS + TER_MCS1_100_LINK_ID_SYMBOLS;
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
    if symbols.len() < TER_MCS1_100_RAMP_SYMBOLS + TER_MCS1_100_SYNC_SYMBOLS {
        return None;
    }

    let search_limit = symbols
        .len()
        .saturating_sub(TER_MCS1_100_RAMP_SYMBOLS + TER_MCS1_100_SYNC_SYMBOLS);
    let mut best_offset = 0usize;
    let mut best_score = 0.0_f32;
    let mut best_errors = u8::MAX;
    let mut best_rotation = 0u8;

    for offset in 0..=search_limit {
        let sync_offset = offset + TER_MCS1_100_RAMP_SYMBOLS;
        let sync_window = &symbols[sync_offset..sync_offset + TER_MCS1_100_SYNC_SYMBOLS];
        for rotation in 0..4 {
            let (score, errors) = syncword_score(sync_window, rotation);
            if score > best_score || (score == best_score && errors < best_errors) {
                best_score = score;
                best_errors = errors;
                best_rotation = rotation;
                best_offset = offset;
            }
        }
    }
    if best_score <= MIN_SYNC_CANDIDATE_SCORE {
        return None;
    }

    let available = symbols.len().saturating_sub(best_offset);
    if available < MIN_BURST_SYMBOLS {
        return None;
    }
    let take = available.min(TER_MCS1_100_BURST_SYMBOLS);
    let rotated = rotate_pi4_stream(&symbols[best_offset..best_offset + take], best_rotation);
    Some(FrameSlice {
        start_offset: best_offset,
        sync_score: best_score,
        sync_errors: best_errors,
        phase_rotation: best_rotation,
        symbols: rotated,
    })
}

fn fallback_frame_slice(symbols: &[u8]) -> FrameSlice {
    let take = symbols.len().min(TER_MCS1_100_BURST_SYMBOLS);
    FrameSlice {
        start_offset: 0,
        sync_score: 0.0,
        sync_errors: (TER_MCS1_100_SYNC_SYMBOLS * 2) as u8,
        phase_rotation: 0,
        symbols: symbols[..take].to_vec(),
    }
}

fn build_unsynced_message(
    channel: &str,
    framed: &FrameSlice,
    mode: &BurstMode<'_>,
    rms: f32,
    raw_symbols: &[u8],
) -> VdesMessage {
    let raw_bytes = pack_dibits_msb(raw_symbols);
    let sync_pct = framed.sync_score * 100.0;
    VdesMessage {
        rig_id: None,
        ts_ms: None,
        channel: channel.to_string(),
        message_type: mode.message_type,
        repeat: 0,
        mmsi: 0,
        crc_ok: false,
        bit_len: raw_symbols.len() * 2,
        raw_bytes,
        lat: None,
        lon: None,
        sog_knots: None,
        cog_deg: None,
        heading_deg: None,
        nav_status: None,
        vessel_name: Some(format!("Unsynced {} sym", framed.symbols.len())),
        callsign: Some(format!("{} raw @{}", mode.label, framed.start_offset)),
        destination: Some(format!(
            "Weak sync {:.0}% ({}) · RMS {:.2} · raw symbol dump",
            sync_pct, framed.sync_errors, rms
        )),
        message_label: Some("Unsynced".to_string()),
        session_id: None,
        source_id: None,
        destination_id: None,
        data_count: None,
        asm_identifier: None,
        ack_nack_mask: None,
        channel_quality: None,
        payload_preview: None,
        link_id: None,
        sync_score: Some(framed.sync_score),
        sync_errors: Some(framed.sync_errors),
        phase_rotation: Some(framed.phase_rotation),
        fec_state: Some("Sync below parse threshold".to_string()),
    }
}

fn build_link_layer_message(
    channel: &str,
    ll: &link_layer::LinkLayerFrame,
    framed: &FrameSlice,
    mode: &BurstMode<'_>,
    _rms: f32,
    link_id: Option<u8>,
    turbo_reliability: f32,
) -> VdesMessage {
    let link_text = link_id
        .map(|value| format!("LID {}", value))
        .unwrap_or_else(|| "LID ?".to_string());

    let (lat, lon) = match &ll.geo_box {
        Some(geo) => (Some(geo.center_lat()), Some(geo.center_lon())),
        None => (None, None),
    };

    let payload_preview = ascii_preview(&ll.payload_bits);
    let raw_bytes = pack_bits_msb(&ll.payload_bits);

    let summary = match ll.message_id {
        0 => format!(
            "Broadcast from {} · {} data bits",
            ll.source_id,
            ll.payload_bits.len()
        ),
        1 => format!(
            "Scheduled ASM {} · {} data bits",
            ll.asm_identifier.unwrap_or(0),
            ll.payload_bits.len()
        ),
        2 => format!(
            "Scheduled ITDMA ASM {} · {} data bits",
            ll.asm_identifier.unwrap_or(0),
            ll.payload_bits.len()
        ),
        3 => format!(
            "{} -> {} · ASM {} · {} data bits",
            ll.source_id,
            ll.destination_id.unwrap_or(0),
            ll.asm_identifier.unwrap_or(0),
            ll.payload_bits.len()
        ),
        4 => format!(
            "{} -> {} · ITDMA ASM {} · {} data bits",
            ll.source_id,
            ll.destination_id.unwrap_or(0),
            ll.asm_identifier.unwrap_or(0),
            ll.payload_bits.len()
        ),
        5 => format!(
            "{} -> {} · ack 0x{:04X} · CQ {}",
            ll.source_id,
            ll.destination_id.unwrap_or(0),
            ll.ack_nack_mask.unwrap_or(0),
            ll.channel_quality.unwrap_or(0)
        ),
        6 => {
            if let Some(ref geo) = ll.geo_box {
                format!(
                    "Geo ASM {} · {} data bits · box {:.3},{:.3} to {:.3},{:.3}",
                    ll.asm_identifier.unwrap_or(0),
                    ll.payload_bits.len(),
                    geo.sw_lat,
                    geo.sw_lon,
                    geo.ne_lat,
                    geo.ne_lon
                )
            } else {
                format!(
                    "Geo ASM {} · {} data bits",
                    ll.asm_identifier.unwrap_or(0),
                    ll.payload_bits.len()
                )
            }
        }
        _ => format!("Message {} · {} bits", ll.message_id, ll.payload_bits.len()),
    };

    let fec_state = if turbo_reliability > 0.0 {
        format!(
            "Turbo FEC (8-iter BCJR), reliability {:.2}, CRC OK",
            turbo_reliability
        )
    } else {
        "Viterbi 1/2-rate, CRC OK".to_string()
    };

    VdesMessage {
        rig_id: None,
        ts_ms: None,
        channel: channel.to_string(),
        message_type: ll.message_id,
        repeat: ll.repeat,
        mmsi: ll.source_id,
        crc_ok: true,
        bit_len: ll.payload_bits.len(),
        raw_bytes,
        lat,
        lon,
        sog_knots: None,
        cog_deg: None,
        heading_deg: None,
        nav_status: None,
        vessel_name: Some(format!("{} {} sym", ll.label, framed.symbols.len())),
        callsign: Some(format!(
            "{} {} @{}",
            mode.label, link_text, framed.start_offset
        )),
        destination: Some(summary),
        message_label: Some(ll.label.to_string()),
        session_id: Some(ll.session_id),
        source_id: Some(ll.source_id),
        destination_id: ll.destination_id,
        data_count: ll.data_count,
        asm_identifier: ll.asm_identifier,
        ack_nack_mask: ll.ack_nack_mask,
        channel_quality: ll.channel_quality,
        payload_preview,
        link_id,
        sync_score: Some(framed.sync_score),
        sync_errors: Some(framed.sync_errors),
        phase_rotation: Some(framed.phase_rotation),
        fec_state: Some(fec_state),
    }
}

fn syncword_score(symbols: &[u8], rotation: u8) -> (f32, u8) {
    if symbols.len() < TER_MCS1_100_SYNC_SYMBOLS {
        return (0.0, u8::MAX);
    }
    let mut bit_errors = 0u8;
    for (idx, &dibit) in symbols.iter().take(TER_MCS1_100_SYNC_SYMBOLS).enumerate() {
        let rotated = rotate_pi4_dibit(dibit, rotation);
        let expected = sync_reference_dibit(idx);
        bit_errors = bit_errors.saturating_add(dibit_bit_distance(rotated, expected) as u8);
    }
    let max_bits = (TER_MCS1_100_SYNC_SYMBOLS * 2) as f32;
    let score = 1.0 - (bit_errors as f32 / max_bits);
    (score.clamp(0.0, 1.0), bit_errors)
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

fn split_fec_frame(symbols: &[u8]) -> (&[u8], &[u8]) {
    let input_end = symbols.len().min(TER_MCS1_100_FEC_INPUT_SYMBOLS);
    let tail_end = symbols
        .len()
        .min(TER_MCS1_100_FEC_INPUT_SYMBOLS + (TER_MCS1_100_FEC_TAIL_BITS / 2));
    (&symbols[..input_end], &symbols[input_end..tail_end])
}

fn parse_vdes_payload(bits: &[u8]) -> ParsedPayload {
    let Some(message_id) = read_bits_u8(bits, 0, 4) else {
        return ParsedPayload::default();
    };
    let repeat = read_bits_u8(bits, 5, 2).unwrap_or(0);
    let session_id = read_bits_u8(bits, 7, 6);
    let source_id = read_bits_u32(bits, 13, 32);
    let common = ParsedPayload {
        message_id: Some(message_id),
        repeat,
        session_id,
        source_id,
        ..Default::default()
    };

    match message_id {
        0 => parse_msg_0(bits, common),
        1 => parse_msg_1(bits, common),
        2 => parse_msg_2(bits, common),
        3 => parse_msg_3(bits, common),
        4 => parse_msg_4(bits, common),
        5 => parse_msg_5(bits, common),
        6 => parse_msg_6(bits, common),
        _ => parse_unknown_msg(bits, common),
    }
}

fn parse_msg_0(bits: &[u8], mut parsed: ParsedPayload) -> ParsedPayload {
    parsed.message_label = Some("Broadcast");
    parsed.data_count = read_bits_u16(bits, 45, 11);
    parsed.payload_bits = extract_counted_payload(bits, 56, parsed.data_count);
    parsed.payload_preview = ascii_preview(&parsed.payload_bits);
    parsed.summary = Some(format!(
        "Broadcast from {} · {} data bits",
        parsed.source_id.unwrap_or(0),
        parsed.payload_bits.len()
    ));
    parsed
}

fn parse_msg_1(bits: &[u8], mut parsed: ParsedPayload) -> ParsedPayload {
    parsed.message_label = Some("Scheduled");
    parsed.data_count = read_bits_u16(bits, 45, 11);
    parsed.asm_identifier = read_bits_u16(bits, 56, 16);
    parsed.payload_bits = extract_counted_payload(bits, 72, parsed.data_count);
    parsed.payload_preview = ascii_preview(&parsed.payload_bits);
    parsed.summary = Some(format!(
        "Scheduled ASM {} · {} data bits",
        parsed.asm_identifier.unwrap_or(0),
        parsed.payload_bits.len()
    ));
    parsed
}

fn parse_msg_2(bits: &[u8], mut parsed: ParsedPayload) -> ParsedPayload {
    parsed.message_label = Some("Scheduled");
    parsed.data_count = read_bits_u16(bits, 45, 11);
    parsed.asm_identifier = read_bits_u16(bits, 56, 16);
    parsed.payload_bits = extract_counted_payload(bits, 72, parsed.data_count);
    parsed.payload_preview = ascii_preview(&parsed.payload_bits);
    parsed.summary = Some(format!(
        "Scheduled ITDMA ASM {} · {} data bits",
        parsed.asm_identifier.unwrap_or(0),
        parsed.payload_bits.len()
    ));
    parsed
}

fn parse_msg_3(bits: &[u8], mut parsed: ParsedPayload) -> ParsedPayload {
    parsed.message_label = Some("Addressed");
    parsed.destination_id = read_bits_u32(bits, 45, 32);
    parsed.data_count = read_bits_u16(bits, 77, 11);
    parsed.asm_identifier = read_bits_u16(bits, 88, 16);
    parsed.payload_bits = extract_counted_payload(bits, 104, parsed.data_count);
    parsed.payload_preview = ascii_preview(&parsed.payload_bits);
    parsed.summary = Some(format!(
        "{} -> {} · ASM {} · {} data bits",
        parsed.source_id.unwrap_or(0),
        parsed.destination_id.unwrap_or(0),
        parsed.asm_identifier.unwrap_or(0),
        parsed.payload_bits.len()
    ));
    parsed
}

fn parse_msg_4(bits: &[u8], mut parsed: ParsedPayload) -> ParsedPayload {
    parsed.message_label = Some("Addressed");
    parsed.destination_id = read_bits_u32(bits, 45, 32);
    parsed.data_count = read_bits_u16(bits, 77, 11);
    parsed.asm_identifier = read_bits_u16(bits, 88, 16);
    parsed.payload_bits = extract_counted_payload(bits, 104, parsed.data_count);
    parsed.payload_preview = ascii_preview(&parsed.payload_bits);
    parsed.summary = Some(format!(
        "{} -> {} · ITDMA ASM {} · {} data bits",
        parsed.source_id.unwrap_or(0),
        parsed.destination_id.unwrap_or(0),
        parsed.asm_identifier.unwrap_or(0),
        parsed.payload_bits.len()
    ));
    parsed
}

fn parse_msg_5(bits: &[u8], mut parsed: ParsedPayload) -> ParsedPayload {
    parsed.message_label = Some("Acknowledge");
    parsed.destination_id = read_bits_u32(bits, 45, 32);
    parsed.ack_nack_mask = read_bits_u16(bits, 77, 16);
    parsed.channel_quality = read_bits_u8(bits, 95, 8);
    parsed.summary = Some(format!(
        "{} -> {} · ack 0x{:04X} · CQ {}",
        parsed.source_id.unwrap_or(0),
        parsed.destination_id.unwrap_or(0),
        parsed.ack_nack_mask.unwrap_or(0),
        parsed.channel_quality.unwrap_or(0)
    ));
    parsed
}

fn parse_msg_6(bits: &[u8], mut parsed: ParsedPayload) -> ParsedPayload {
    parsed.message_label = Some("Geo");
    let ne_lon = read_signed_bits(bits, 45, 18);
    let ne_lat = read_signed_bits(bits, 63, 17);
    let sw_lon = read_signed_bits(bits, 80, 18);
    let sw_lat = read_signed_bits(bits, 98, 17);
    parsed.data_count = read_bits_u16(bits, 115, 11);
    parsed.asm_identifier = read_bits_u16(bits, 128, 16);
    parsed.payload_bits = extract_counted_payload(bits, 144, parsed.data_count);
    parsed.payload_preview = ascii_preview(&parsed.payload_bits);
    if let (Some(ne_lon), Some(ne_lat), Some(sw_lon), Some(sw_lat)) =
        (ne_lon, ne_lat, sw_lon, sw_lat)
    {
        let first_lon_deg = ne_lon as f64 / 600.0;
        let first_lat_deg = ne_lat as f64 / 600.0;
        let second_lon_deg = sw_lon as f64 / 600.0;
        let second_lat_deg = sw_lat as f64 / 600.0;
        if let Some((sw_lat_deg, sw_lon_deg, ne_lat_deg, ne_lon_deg)) =
            normalize_geo_box(first_lat_deg, first_lon_deg, second_lat_deg, second_lon_deg)
        {
            parsed.lon = Some((sw_lon_deg + ne_lon_deg) * 0.5);
            parsed.lat = Some((sw_lat_deg + ne_lat_deg) * 0.5);
            parsed.summary = Some(format!(
                "Geo ASM {} · {} data bits · box {:.3},{:.3} to {:.3},{:.3}",
                parsed.asm_identifier.unwrap_or(0),
                parsed.payload_bits.len(),
                sw_lat_deg,
                sw_lon_deg,
                ne_lat_deg,
                ne_lon_deg
            ));
        } else {
            parsed.summary = Some(format!(
                "Geo ASM {} · {} data bits · invalid box",
                parsed.asm_identifier.unwrap_or(0),
                parsed.payload_bits.len()
            ));
        }
    } else {
        parsed.summary = Some(format!(
            "Geo ASM {} · {} data bits",
            parsed.asm_identifier.unwrap_or(0),
            parsed.payload_bits.len()
        ));
    }
    parsed
}

fn parse_unknown_msg(bits: &[u8], mut parsed: ParsedPayload) -> ParsedPayload {
    parsed.message_label = Some("Unknown");
    parsed.payload_bits = bits.to_vec();
    parsed.payload_preview = ascii_preview(&parsed.payload_bits);
    parsed.summary = Some(format!(
        "Message {} · {} bits",
        parsed.message_id.unwrap_or(255),
        parsed.payload_bits.len()
    ));
    parsed
}

fn vdes_plausibility_score(
    parsed: &ParsedPayload,
    link_id: Option<u8>,
    tail_zero_bits: usize,
) -> i32 {
    let mut score = 0i32;

    match parsed.message_id {
        Some(0..=6) => score += 30,
        Some(_) | None => score -= 10,
    }

    if valid_station_id(parsed.source_id) {
        score += 20;
    } else {
        score -= 20;
    }

    if link_id.is_some() {
        score += 10;
    }

    score += match tail_zero_bits {
        4.. => 20,
        2..=3 => 8,
        _ => -8,
    };

    if !parsed.payload_bits.is_empty() {
        score += i32::try_from((parsed.payload_bits.len() / 128).min(12)).unwrap_or(0);
    }

    match parsed.message_id {
        Some(0) => {
            score += counted_payload_score(parsed, 56);
        }
        Some(1 | 2) => {
            score += counted_payload_score(parsed, 72);
        }
        Some(3 | 4) => {
            score += counted_payload_score(parsed, 104);
            if valid_station_id(parsed.destination_id) {
                score += 15;
            } else {
                score -= 8;
            }
        }
        Some(5) => {
            if valid_station_id(parsed.destination_id) {
                score += 15;
            } else {
                score -= 8;
            }
            if parsed.ack_nack_mask.is_some() {
                score += 5;
            }
        }
        Some(6) => {
            score += counted_payload_score(parsed, 144);
            if parsed.lat.is_some() && parsed.lon.is_some() {
                score += 20;
            } else {
                score -= 8;
            }
        }
        _ => {}
    }

    if parsed.message_label == Some("Unknown") {
        score -= 8;
    }

    score
}

fn counted_payload_score(parsed: &ParsedPayload, payload_start_bit: usize) -> i32 {
    let Some(data_count) = parsed.data_count else {
        return -8;
    };
    let Some(expected_end) = payload_start_bit.checked_add(usize::from(data_count)) else {
        return -8;
    };
    if parsed.payload_bits.len() == usize::from(data_count) && expected_end >= payload_start_bit {
        15
    } else {
        -8
    }
}

fn valid_station_id(id: Option<u32>) -> bool {
    matches!(id, Some(value) if value != 0 && value != u32::MAX)
}

fn valid_geo_coord(lat: f64, lon: f64) -> bool {
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

fn normalize_geo_box(
    first_lat: f64,
    first_lon: f64,
    second_lat: f64,
    second_lon: f64,
) -> Option<(f64, f64, f64, f64)> {
    if !valid_geo_coord(first_lat, first_lon) || !valid_geo_coord(second_lat, second_lon) {
        return None;
    }

    let south = first_lat.min(second_lat);
    let north = first_lat.max(second_lat);
    let west = first_lon.min(second_lon);
    let east = first_lon.max(second_lon);
    Some((south, west, north, east))
}

fn viterbi_decode_rate_half(coded_bits: &[u8]) -> Vec<u8> {
    if coded_bits.len() < 2 {
        return Vec::new();
    }

    let pair_count = coded_bits.len() / 2;
    let mut metrics = [u16::MAX; 64];
    let mut next_metrics = [u16::MAX; 64];
    let mut predecessors = vec![[0u8; 64]; pair_count];
    metrics[0] = 0;

    for step in 0..pair_count {
        next_metrics.fill(u16::MAX);
        let recv0 = coded_bits[step * 2] & 1;
        let recv1 = coded_bits[step * 2 + 1] & 1;

        for (state, &metric) in metrics.iter().enumerate() {
            if metric == u16::MAX {
                continue;
            }
            for input_bit in 0..=1u8 {
                let reg = ((state as u8) << 1) | input_bit;
                let out = conv_encode_output(reg);
                let branch = dibit_bit_distance(out, (recv0 << 1) | recv1) as u16;
                let next_state = (reg & 0x3f) as usize;
                let candidate = metric.saturating_add(branch);
                if candidate < next_metrics[next_state] {
                    next_metrics[next_state] = candidate;
                    predecessors[step][next_state] = state as u8;
                }
            }
        }

        metrics = next_metrics;
    }

    let mut best_state = 0usize;
    let mut best_metric = u16::MAX;
    for (state, &metric) in metrics.iter().enumerate() {
        if metric < best_metric {
            best_metric = metric;
            best_state = state;
        }
    }
    if best_metric == u16::MAX {
        return Vec::new();
    }

    let mut decoded = vec![0u8; pair_count];
    let mut state = best_state;
    for step in (0..pair_count).rev() {
        let bit = (state as u8) & 1;
        decoded[step] = bit;
        state = predecessors[step][state] as usize;
    }

    decoded.truncate(TER_MCS1_100_FEC_OUTPUT_BITS.min(decoded.len()));
    decoded
}

fn conv_encode_output(reg: u8) -> u8 {
    let g0 = parity6_7(reg & 0o171);
    let g1 = parity6_7(reg & 0o133);
    (g0 << 1) | g1
}

fn parity6_7(value: u8) -> u8 {
    (value.count_ones() as u8) & 1
}

fn extract_counted_payload(bits: &[u8], start: usize, count: Option<u16>) -> Vec<u8> {
    let Some(count) = count.map(usize::from) else {
        return Vec::new();
    };
    let end = start.saturating_add(count).min(bits.len());
    if start >= end {
        return Vec::new();
    }
    bits[start..end].to_vec()
}

fn ascii_preview(bits: &[u8]) -> Option<String> {
    let bytes = pack_bits_msb(bits);
    let mut out = String::new();
    for &byte in bytes.iter().take(24) {
        let ch = if byte.is_ascii_graphic() || byte == b' ' {
            byte as char
        } else {
            '.'
        };
        out.push(ch);
    }
    let trimmed = out.trim_matches('.').trim();
    if trimmed.is_empty() {
        None
    } else if bytes.len() > 24 {
        Some(format!("{}...", trimmed))
    } else {
        Some(trimmed.to_string())
    }
}

fn read_bits_u8(bits: &[u8], start: usize, len: usize) -> Option<u8> {
    read_bits_u32(bits, start, len).and_then(|value| u8::try_from(value).ok())
}

fn read_bits_u16(bits: &[u8], start: usize, len: usize) -> Option<u16> {
    read_bits_u32(bits, start, len).and_then(|value| u16::try_from(value).ok())
}

fn read_bits_u32(bits: &[u8], start: usize, len: usize) -> Option<u32> {
    if len == 0 || len > 32 {
        return None;
    }
    let end = start.checked_add(len)?;
    let slice = bits.get(start..end)?;
    let mut value = 0u32;
    for &bit in slice {
        value = (value << 1) | u32::from(bit & 1);
    }
    Some(value)
}

fn read_signed_bits(bits: &[u8], start: usize, len: usize) -> Option<i32> {
    let raw = read_bits_u32(bits, start, len)?;
    if len == 0 || len > 31 {
        return None;
    }
    let sign_mask = 1u32 << (len - 1);
    if raw & sign_mask == 0 {
        Some(raw as i32)
    } else {
        let extended = raw | (!0u32 << len);
        Some(extended as i32)
    }
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

fn sync_reference_dibit(idx: usize) -> u8 {
    match TER_MCS1_100_SYNC_BITS[idx] {
        b'1' => 0b11,
        _ => 0b00,
    }
}

fn rotate_pi4_dibit(dibit: u8, rotation: u8) -> u8 {
    let pos = PI4_QPSK_DIBITS
        .iter()
        .position(|candidate| *candidate == (dibit & 0b11))
        .unwrap_or(0);
    PI4_QPSK_DIBITS[(pos + rotation as usize) % PI4_QPSK_DIBITS.len()]
}

fn rotate_pi4_stream(symbols: &[u8], rotation: u8) -> Vec<u8> {
    if rotation == 0 {
        return symbols.to_vec();
    }
    symbols
        .iter()
        .map(|dibit| rotate_pi4_dibit(*dibit, rotation))
        .collect()
}

fn dibit_bit_distance(a: u8, b: u8) -> usize {
    ((a ^ b) & 0b11).count_ones() as usize
}

fn dibits_to_bits(symbols: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(symbols.len() * 2);
    for &dibit in symbols {
        out.push((dibit >> 1) & 1);
        out.push(dibit & 1);
    }
    out
}

fn bits_to_dibits(bits: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bits.len().div_ceil(2));
    let mut idx = 0usize;
    while idx < bits.len() {
        let hi = bits[idx] & 1;
        let lo = bits.get(idx + 1).copied().unwrap_or(0) & 1;
        out.push((hi << 1) | lo);
        idx += 2;
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
        let dist = code.iter().zip(bits.iter()).filter(|(a, b)| a != b).count();
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
    for (idx, slot) in out.iter_mut().enumerate() {
        let x1 = ((idx >> 4) & 1) as u8;
        let x2 = ((idx >> 3) & 1) as u8;
        let x3 = ((idx >> 2) & 1) as u8;
        let x4 = ((idx >> 1) & 1) as u8;
        let x5 = (idx & 1) as u8;
        *slot = a0 ^ (a1 & x1) ^ (a2 & x2) ^ (a3 & x3) ^ (a4 & x4) ^ (a5 & x5);
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

/// Demodulate π/4-QPSK burst samples into a dibit stream.
///
/// The function:
/// 1. Uses linear interpolation to resample the IQ stream to exactly one
///    sample per symbol epoch (fractional-delay matched to the nominal symbol
///    rate), which gives cleaner decision points than a free-running phase
///    counter at the typical ~1.25 samples-per-symbol IQ rate.
/// 2. Estimates the average carrier-frequency offset (CFO) for the burst via
///    the 4th-power method and removes it before differential decoding.
///    This corrects constant-frequency offsets of up to ±symbol_rate/8.
///
/// # Limitations
/// Closed-loop symbol timing recovery (Gardner / Mueller–Müller) would need
/// ≥2 samples/symbol and so cannot be applied at the current IQ tap rate.
/// The open-loop linear interpolation used here is the best approximation
/// achievable within the existing decimation architecture.
fn slice_pi4_qpsk_symbols(samples: &[Complex<f32>], sample_rate: f32) -> Vec<u8> {
    if samples.len() < 4 {
        return Vec::new();
    }

    let sps = sample_rate / VDES_SYMBOL_RATE;

    // --- Pass 1: resample to one IQ sample per symbol epoch ---
    let sym_count = ((samples.len() as f32 - 1.0) / sps) as usize;
    let mut sym_iq: Vec<Complex<f32>> = Vec::with_capacity(sym_count + 4);
    let n = samples.len();
    let mut epoch = 0.0f64;
    loop {
        let i0 = epoch.floor() as usize;
        if i0 + 1 >= n {
            break;
        }
        let frac = (epoch - i0 as f64) as f32;
        sym_iq.push(samples[i0] * (1.0 - frac) + samples[i0 + 1] * frac);
        epoch += sps as f64;
    }

    if sym_iq.len() < 2 {
        return Vec::new();
    }

    // --- Pass 2: estimate CFO using the 4th-power method ---
    // For π/4-QPSK differential symbols {±π/4, ±3π/4}, diff^4 collapses all
    // constellation points to the same angle, leaving only 4*CFO_per_symbol.
    let cfo_inc = estimate_differential_cfo(&sym_iq);

    // --- Pass 3: apply CFO correction and differential decode ---
    let mut symbols: Vec<u8> = Vec::with_capacity(sym_iq.len());
    let mut prev: Option<Complex<f32>> = None;
    for (idx, &s) in sym_iq.iter().enumerate() {
        let angle = -(idx as f32) * cfo_inc;
        let correction = Complex::new(angle.cos(), angle.sin());
        let corrected = s * correction;
        if let Some(p) = prev {
            symbols.push(quantize_pi4_qpsk(corrected * p.conj()));
        }
        prev = Some(corrected);
    }

    symbols
}

/// Estimate the per-symbol carrier-frequency offset from a sequence of
/// symbol-rate IQ samples using the 4th-power method.
///
/// Returns the phase increment (radians per symbol) to subtract from the
/// received phase.
fn estimate_differential_cfo(sym_iq: &[Complex<f32>]) -> f32 {
    let mut sum = Complex::new(0.0f32, 0.0f32);
    for w in sym_iq.windows(2) {
        let diff = w[1] * w[0].conj();
        // diff^4 removes π/4-QPSK modulation phase (multiples of π/2)
        let d2 = diff * diff;
        let d4 = d2 * d2;
        sum += d4;
    }
    if sum.norm_sqr() < 1.0e-12 {
        return 0.0;
    }
    // Unwrap: angle(sum)/4 = average CFO per symbol period
    sum.im.atan2(sum.re) / 4.0
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
    let mut out = Vec::with_capacity(symbols.len().div_ceil(4));
    let mut byte = 0u8;
    let mut count = 0usize;

    for &dibit in symbols {
        let shift = 6usize.saturating_sub((count % 4) * 2);
        byte |= (dibit & 0b11) << shift;
        count += 1;
        if count.is_multiple_of(4) {
            out.push(byte);
            byte = 0;
        }
    }

    if !count.is_multiple_of(4) {
        out.push(byte);
    }

    out
}

fn pack_bits_msb(bits: &[u8]) -> Vec<u8> {
    let dibits = bits_to_dibits(bits);
    pack_dibits_msb(&dibits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(angle: f32) -> Complex<f32> {
        Complex::new(angle.cos(), angle.sin())
    }

    fn write_bits(bits: &mut [u8], start: usize, len: usize, value: u32) {
        for idx in 0..len {
            let shift = len - idx - 1;
            bits[start + idx] = ((value >> shift) & 1) as u8;
        }
    }

    fn write_signed_bits(bits: &mut [u8], start: usize, len: usize, value: i32) {
        let mask = if len >= 32 {
            u32::MAX
        } else {
            (1u32 << len) - 1
        };
        write_bits(bits, start, len, (value as u32) & mask);
    }

    #[test]
    fn packs_dibits_msb_first() {
        assert_eq!(
            pack_dibits_msb(&[0b00, 0b01, 0b10, 0b11]),
            vec![0b0001_1011]
        );
    }

    #[test]
    fn quantizes_pi_over_four_steps() {
        assert_eq!(quantize_pi4_qpsk(phase(std::f32::consts::FRAC_PI_4)), 0b00);
        assert_eq!(
            quantize_pi4_qpsk(phase(3.0 * std::f32::consts::FRAC_PI_4)),
            0b01
        );
        assert_eq!(
            quantize_pi4_qpsk(phase(-3.0 * std::f32::consts::FRAC_PI_4)),
            0b11
        );
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
    fn syncword_score_prefers_correct_rotation() {
        let sync: Vec<u8> = (0..TER_MCS1_100_SYNC_SYMBOLS)
            .map(sync_reference_dibit)
            .collect();
        let rotated = rotate_pi4_stream(&sync, 2);
        let (wrong_score, wrong_errors) = syncword_score(&rotated, 0);
        let (right_score, right_errors) = syncword_score(&rotated, 2);
        assert!(right_score > wrong_score);
        assert!(right_errors < wrong_errors);
        assert_eq!(right_errors, 0);
    }

    #[test]
    fn deinterleave_preserves_length() {
        let symbols: Vec<u8> = (0..127).map(|idx| (idx % 4) as u8).collect();
        let out = deinterleave_100khz_frame(&symbols);
        assert_eq!(out.len(), symbols.len());
    }

    #[test]
    fn viterbi_decodes_k7_rate_half_stream() {
        let input: Vec<u8> = (0..TER_MCS1_100_FEC_OUTPUT_BITS)
            .map(|idx| ((idx * 5 + 1) % 2) as u8)
            .collect();
        let mut state = 0u8;
        let mut coded = Vec::with_capacity(input.len() * 2);
        for &bit in &input {
            state = ((state << 1) | bit) & 0x7f;
            let dibit = conv_encode_output(state);
            coded.push((dibit >> 1) & 1);
            coded.push(dibit & 1);
        }
        let decoded = viterbi_decode_rate_half(&coded);
        assert_eq!(decoded, input);
    }

    #[test]
    fn parses_geo_box_even_when_corners_arrive_reversed() {
        let mut bits = vec![0u8; 160];
        write_bits(&mut bits, 0, 4, 6);
        write_bits(&mut bits, 13, 32, 12345);
        write_signed_bits(&mut bits, 45, 18, (10.0_f64 * 600.0) as i32);
        write_signed_bits(&mut bits, 63, 17, (20.0_f64 * 600.0) as i32);
        write_signed_bits(&mut bits, 80, 18, (-5.0_f64 * 600.0) as i32);
        write_signed_bits(&mut bits, 98, 17, (15.0_f64 * 600.0) as i32);

        let parsed = parse_vdes_payload(&bits);

        assert_eq!(parsed.message_id, Some(6));
        assert_eq!(parsed.lat, Some(17.5));
        assert_eq!(parsed.lon, Some(2.5));
    }
}
