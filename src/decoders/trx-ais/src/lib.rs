// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Basic AIS GMSK/HDLC decoder.
//!
//! This decoder operates on narrowband FM-demodulated audio. It uses a simple
//! sign slicer at the symbol rate, HDLC flag detection with NRZI decoding and
//! bit de-stuffing, then parses common AIS position/static messages.

use trx_core::decode::AisMessage;

const AIS_BAUD: f32 = 9_600.0;

const CRC_CCITT_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u16;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0x8408;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

fn crc16ccitt(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in bytes {
        crc = (crc >> 8) ^ CRC_CCITT_TABLE[((crc ^ b as u16) & 0xFF) as usize];
    }
    crc ^ 0xFFFF
}

#[derive(Debug, Clone)]
struct RawFrame {
    payload: Vec<u8>,
    bits: Vec<u8>,
    crc_ok: bool,
}

#[derive(Debug, Clone)]
pub struct AisDecoder {
    sample_rate: f32,
    samples_per_symbol: f32,
    sample_clock: f32,
    dc_state: f32,
    lp_fast: f32,
    lp_slow: f32,
    env_state: f32,
    polarity: i8,
    samples_since_transition: u32,
    clock_locked: bool,
    prev_raw_bit: u8,
    ones: u32,
    in_frame: bool,
    frame_bits: Vec<u8>,
    frames: Vec<RawFrame>,
}

impl AisDecoder {
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1) as f32;
        Self {
            sample_rate,
            samples_per_symbol: sample_rate / AIS_BAUD,
            sample_clock: 0.0,
            dc_state: 0.0,
            lp_fast: 0.0,
            lp_slow: 0.0,
            env_state: 1e-3,
            polarity: 1,
            samples_since_transition: 0,
            clock_locked: false,
            prev_raw_bit: 0,
            ones: 0,
            in_frame: false,
            frame_bits: Vec::new(),
            frames: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.samples_per_symbol = self.sample_rate / AIS_BAUD;
        self.sample_clock = 0.0;
        self.dc_state = 0.0;
        self.lp_fast = 0.0;
        self.lp_slow = 0.0;
        self.env_state = 1e-3;
        self.polarity = 1;
        self.samples_since_transition = 0;
        self.clock_locked = false;
        self.prev_raw_bit = 0;
        self.ones = 0;
        self.in_frame = false;
        self.frame_bits.clear();
        self.frames.clear();
    }

    pub fn process_samples(&mut self, samples: &[f32], channel: &str) -> Vec<AisMessage> {
        for &sample in samples {
            self.process_sample(sample);
        }

        let frames = std::mem::take(&mut self.frames);
        let mut out = Vec::new();
        for frame in frames {
            if let Some(msg) = parse_frame(frame, channel) {
                out.push(msg);
            }
        }
        out
    }

    fn process_sample(&mut self, sample: f32) {
        // Remove slow DC drift from the FM discriminator output.
        self.dc_state += 0.0025 * (sample - self.dc_state);
        let dc_free = sample - self.dc_state;

        // A simple band-pass-ish response makes GMSK symbol transitions stand out
        // without needing a full matched filter.
        self.lp_fast += 0.32 * (dc_free - self.lp_fast);
        self.lp_slow += 0.045 * (dc_free - self.lp_slow);
        let shaped = self.lp_fast - self.lp_slow;

        // Track envelope to keep the slicer stable on weak signals.
        self.env_state += 0.015 * (shaped.abs() - self.env_state);
        let normalized = if self.env_state > 1e-4 {
            shaped / self.env_state
        } else {
            shaped
        };

        let threshold = 0.12;
        let next_polarity = if normalized > threshold {
            1
        } else if normalized < -threshold {
            -1
        } else {
            self.polarity
        };

        self.samples_since_transition = self.samples_since_transition.saturating_add(1);
        if next_polarity != self.polarity {
            self.observe_transition();
            self.polarity = next_polarity;
        }

        if !self.clock_locked {
            return;
        }

        self.sample_clock += 1.0;
        while self.sample_clock >= self.samples_per_symbol {
            self.sample_clock -= self.samples_per_symbol;
            let raw_bit = if self.polarity >= 0 { 1 } else { 0 };
            self.process_symbol(raw_bit);
        }
    }

    fn observe_transition(&mut self) {
        let interval = self.samples_since_transition.max(1) as f32;
        self.samples_since_transition = 0;

        let nominal = (self.sample_rate / AIS_BAUD).max(1.0);
        let symbols = (interval / nominal).round().clamp(1.0, 8.0);
        let estimate = (interval / symbols).clamp(nominal * 0.75, nominal * 1.25);
        self.samples_per_symbol += 0.18 * (estimate - self.samples_per_symbol);
        self.sample_clock = self.samples_per_symbol * 0.5;
        self.clock_locked = true;
    }

    fn process_symbol(&mut self, raw_bit: u8) {
        let decoded_bit = if raw_bit == self.prev_raw_bit { 1 } else { 0 };
        self.prev_raw_bit = raw_bit;

        if decoded_bit == 1 {
            self.ones += 1;
            return;
        }

        // A zero terminates the current run of ones.
        if self.ones >= 7 {
            self.in_frame = false;
            self.frame_bits.clear();
            self.ones = 0;
            return;
        }

        if self.ones == 6 {
            if self.in_frame {
                if let Some(frame) = self.bits_to_frame() {
                    self.frames.push(frame);
                }
            }
            self.frame_bits.clear();
            self.in_frame = true;
            self.ones = 0;
            return;
        }

        if self.ones == 5 {
            if self.in_frame {
                for _ in 0..5 {
                    self.frame_bits.push(1);
                }
            }
            self.ones = 0;
            return;
        }

        if self.in_frame {
            for _ in 0..self.ones {
                self.frame_bits.push(1);
            }
            self.frame_bits.push(0);
        }
        self.ones = 0;
    }

    fn bits_to_frame(&self) -> Option<RawFrame> {
        if self.frame_bits.len() < 24 {
            return None;
        }

        let usable_bits = self.frame_bits.len() - (self.frame_bits.len() % 8);
        if usable_bits < 24 {
            return None;
        }

        let bits = self.frame_bits[..usable_bits].to_vec();
        let mut bytes = Vec::with_capacity(usable_bits / 8);
        for chunk in bits.chunks(8) {
            let mut byte = 0u8;
            for (idx, &bit) in chunk.iter().enumerate() {
                if bit != 0 {
                    byte |= 1 << idx;
                }
            }
            bytes.push(byte);
        }

        if bytes.len() < 3 {
            return None;
        }

        let payload_len = bytes.len() - 2;
        let payload = bytes[..payload_len].to_vec();
        let received_fcs = u16::from_le_bytes([bytes[payload_len], bytes[payload_len + 1]]);
        let crc_ok = crc16ccitt(&payload) == received_fcs;

        Some(RawFrame {
            payload,
            bits,
            crc_ok,
        })
    }
}

fn parse_frame(frame: RawFrame, channel: &str) -> Option<AisMessage> {
    if !frame.crc_ok {
        return None;
    }

    let bits = bytes_to_msb_bits(&frame.payload);
    if bits.len() < 40 {
        return None;
    }

    let message_type = get_uint(&bits, 0, 6)? as u8;
    let repeat = get_uint(&bits, 6, 2)? as u8;
    let mmsi = get_uint(&bits, 8, 30)? as u32;

    let mut msg = AisMessage {
        ts_ms: None,
        channel: channel.to_string(),
        message_type,
        repeat,
        mmsi,
        crc_ok: frame.crc_ok,
        bit_len: frame.bits.len(),
        raw_bytes: frame.payload,
        lat: None,
        lon: None,
        sog_knots: None,
        cog_deg: None,
        heading_deg: None,
        nav_status: None,
        vessel_name: None,
        callsign: None,
        destination: None,
    };

    match message_type {
        1..=3 => {
            msg.nav_status = get_uint(&bits, 38, 4).map(|v| v as u8);
            msg.sog_knots = decode_tenths(get_uint(&bits, 50, 10)?, 1023);
            msg.lon = decode_coord(get_int(&bits, 61, 28)?, 181.0);
            msg.lat = decode_coord(get_int(&bits, 89, 27)?, 91.0);
            msg.cog_deg = decode_tenths(get_uint(&bits, 116, 12)?, 3600);
            msg.heading_deg = decode_heading(get_uint(&bits, 128, 9)?);
        }
        18 => {
            msg.sog_knots = decode_tenths(get_uint(&bits, 46, 10)?, 1023);
            msg.lon = decode_coord(get_int(&bits, 57, 28)?, 181.0);
            msg.lat = decode_coord(get_int(&bits, 85, 27)?, 91.0);
            msg.cog_deg = decode_tenths(get_uint(&bits, 112, 12)?, 3600);
            msg.heading_deg = decode_heading(get_uint(&bits, 124, 9)?);
        }
        19 => {
            msg.sog_knots = decode_tenths(get_uint(&bits, 46, 10)?, 1023);
            msg.lon = decode_coord(get_int(&bits, 57, 28)?, 181.0);
            msg.lat = decode_coord(get_int(&bits, 85, 27)?, 91.0);
            msg.cog_deg = decode_tenths(get_uint(&bits, 112, 12)?, 3600);
            msg.heading_deg = decode_heading(get_uint(&bits, 124, 9)?);
            msg.vessel_name = decode_sixbit_text(&bits, 143, 120);
        }
        5 => {
            msg.callsign = decode_sixbit_text(&bits, 70, 42);
            msg.vessel_name = decode_sixbit_text(&bits, 112, 120);
            msg.destination = decode_sixbit_text(&bits, 302, 120);
        }
        _ => {}
    }

    Some(msg)
}

fn bytes_to_msb_bits(bytes: &[u8]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(bytes.len() * 8);
    for &byte in bytes {
        for shift in (0..8).rev() {
            bits.push((byte >> shift) & 1);
        }
    }
    bits
}

fn get_uint(bits: &[u8], start: usize, len: usize) -> Option<u32> {
    if len == 0 || start.checked_add(len)? > bits.len() || len > 32 {
        return None;
    }
    let mut out = 0u32;
    for &bit in &bits[start..start + len] {
        out = (out << 1) | u32::from(bit);
    }
    Some(out)
}

fn get_int(bits: &[u8], start: usize, len: usize) -> Option<i32> {
    let raw = get_uint(bits, start, len)?;
    if len == 0 || len > 31 {
        return None;
    }
    let sign_mask = 1u32 << (len - 1);
    if raw & sign_mask == 0 {
        Some(raw as i32)
    } else {
        Some((raw as i32) - ((1u32 << len) as i32))
    }
}

fn decode_tenths(raw: u32, invalid: u32) -> Option<f32> {
    if raw == invalid {
        None
    } else {
        Some(raw as f32 / 10.0)
    }
}

fn decode_heading(raw: u32) -> Option<u16> {
    if raw >= 360 {
        None
    } else {
        Some(raw as u16)
    }
}

fn decode_coord(raw: i32, invalid_abs: f64) -> Option<f64> {
    let value = raw as f64 / 600_000.0;
    if value.abs() >= invalid_abs {
        None
    } else {
        Some(value)
    }
}

fn decode_sixbit_text(bits: &[u8], start: usize, len: usize) -> Option<String> {
    if start.checked_add(len)? > bits.len() || len % 6 != 0 {
        return None;
    }

    let mut out = String::new();
    for offset in (0..len).step_by(6) {
        let value = get_uint(bits, start + offset, 6)? as u8;
        let ch = if value < 32 {
            char::from(value + 64)
        } else {
            char::from(value)
        };
        if ch != '@' {
            out.push(ch);
        }
    }

    let trimmed = out.trim().trim_matches('@').trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload_with_crc(payload: &[u8]) -> Vec<u8> {
        let mut out = payload.to_vec();
        out.extend_from_slice(&crc16ccitt(payload).to_le_bytes());
        out
    }

    fn bytes_to_lsb_bits(bytes: &[u8]) -> Vec<u8> {
        let mut bits = Vec::with_capacity(bytes.len() * 8);
        for &byte in bytes {
            for shift in 0..8 {
                bits.push((byte >> shift) & 1);
            }
        }
        bits
    }

    fn bitstuff(bits: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bits.len() + bits.len() / 5);
        let mut ones = 0u32;
        for &bit in bits {
            out.push(bit);
            if bit == 1 {
                ones += 1;
                if ones == 5 {
                    out.push(0);
                    ones = 0;
                }
            } else {
                ones = 0;
            }
        }
        out
    }

    fn nrzi_encode(bits: &[u8]) -> Vec<u8> {
        let mut state = 0u8;
        let mut out = Vec::with_capacity(bits.len());
        for &bit in bits {
            if bit == 0 {
                state ^= 1;
            }
            out.push(state);
        }
        out
    }

    #[test]
    fn decodes_signed_coordinates() {
        assert_eq!(decode_coord(60_000, 181.0), Some(0.1));
        assert_eq!(decode_coord(-60_000, 181.0), Some(-0.1));
    }

    #[test]
    fn decodes_sixbit_name() {
        let bytes = [0x10_u8, 0x41_u8, 0x11_u8, 0x92_u8, 0x08_u8, 0x00_u8];
        let bits = bytes_to_msb_bits(&bytes);
        let text = decode_sixbit_text(&bits, 0, 36);
        assert!(text.is_some());
    }

    #[test]
    fn recovers_hdlc_frame_from_raw_nrzi_bits() {
        let payload = [0x11_u8, 0x22_u8, 0x7E_u8, 0x00_u8, 0xF0_u8];
        let frame_bytes = payload_with_crc(&payload);
        let mut hdlc_bits = bytes_to_lsb_bits(&[0x7E]);
        hdlc_bits.extend(bitstuff(&bytes_to_lsb_bits(&frame_bytes)));
        hdlc_bits.extend(bytes_to_lsb_bits(&[0x7E]));
        let raw_bits = nrzi_encode(&hdlc_bits);

        let mut decoder = AisDecoder::new(48_000);
        for raw_bit in raw_bits {
            decoder.process_symbol(raw_bit);
        }

        assert_eq!(decoder.frames.len(), 1);
        let frame = &decoder.frames[0];
        assert!(frame.crc_ok);
        assert_eq!(frame.payload, payload);
    }
}
