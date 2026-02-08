// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Bell 202 AFSK demodulator + AX.25/APRS decoder.
//!
//! Ported from the browser-side JavaScript implementation.

use trx_core::decode::AprsPacket;

// ---------------------------------------------------------------------------
// CRC-16-CCITT
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Correlation demodulator (one instance)
// ---------------------------------------------------------------------------

const BAUD: f32 = 1200.0;
const MARK: f32 = 1200.0;
const SPACE: f32 = 2200.0;
const TWO_PI: f32 = std::f32::consts::TAU;
const PLL_GAIN: f32 = 0.4;

struct Demodulator {
    samples_per_bit: f32,

    // Energy gate
    energy_acc: f32,
    energy_count: usize,
    energy_window: usize,

    // Oscillator phases
    mark_phase: f32,
    space_phase: f32,
    mark_phase_inc: f32,
    space_phase_inc: f32,

    // Sliding-window correlation filter
    corr_len: usize,
    mark_i_buf: Vec<f32>,
    mark_q_buf: Vec<f32>,
    space_i_buf: Vec<f32>,
    space_q_buf: Vec<f32>,
    corr_idx: usize,
    mark_i_sum: f32,
    mark_q_sum: f32,
    space_i_sum: f32,
    space_q_sum: f32,

    // Clock recovery
    last_bit: u8,
    bit_phase: f32,

    // NRZI
    prev_sampled_bit: u8,

    // HDLC
    ones: u32,
    frame_bits: Vec<u8>,
    in_frame: bool,

    // Results
    frames: Vec<RawFrame>,
}

struct RawFrame {
    payload: Vec<u8>,
    crc_ok: bool,
}

impl Demodulator {
    fn new(sample_rate: u32, window_factor: f32) -> Self {
        let sr = sample_rate as f32;
        let samples_per_bit = sr / BAUD;
        let corr_len = (samples_per_bit * window_factor).round().max(2.0) as usize;
        let energy_window = (sr * 0.05).round() as usize;

        Self {
            samples_per_bit,
            energy_acc: 0.0,
            energy_count: 0,
            energy_window,
            mark_phase: 0.0,
            space_phase: 0.0,
            mark_phase_inc: TWO_PI * MARK / sr,
            space_phase_inc: TWO_PI * SPACE / sr,
            corr_len,
            mark_i_buf: vec![0.0; corr_len],
            mark_q_buf: vec![0.0; corr_len],
            space_i_buf: vec![0.0; corr_len],
            space_q_buf: vec![0.0; corr_len],
            corr_idx: 0,
            mark_i_sum: 0.0,
            mark_q_sum: 0.0,
            space_i_sum: 0.0,
            space_q_sum: 0.0,
            last_bit: 0,
            bit_phase: 0.0,
            prev_sampled_bit: 0,
            ones: 0,
            frame_bits: Vec::new(),
            in_frame: false,
            frames: Vec::new(),
        }
    }

    fn reset_state(&mut self) {
        self.mark_phase = 0.0;
        self.space_phase = 0.0;
        self.mark_i_buf.fill(0.0);
        self.mark_q_buf.fill(0.0);
        self.space_i_buf.fill(0.0);
        self.space_q_buf.fill(0.0);
        self.corr_idx = 0;
        self.mark_i_sum = 0.0;
        self.mark_q_sum = 0.0;
        self.space_i_sum = 0.0;
        self.space_q_sum = 0.0;
        self.last_bit = 0;
        self.bit_phase = 0.0;
        self.prev_sampled_bit = 0;
        self.ones = 0;
        self.frame_bits.clear();
        self.in_frame = false;
    }

    fn process_buffer(&mut self, samples: &[f32]) -> Vec<RawFrame> {
        for &s in samples {
            self.process_sample(s);
        }
        std::mem::take(&mut self.frames)
    }

    fn process_sample(&mut self, s: f32) {
        // Energy gate
        self.energy_acc += s * s;
        self.energy_count += 1;
        if self.energy_count >= self.energy_window {
            let rms = (self.energy_acc / self.energy_count as f32).sqrt();
            if rms < 0.001 {
                self.reset_state();
            }
            self.energy_acc = 0.0;
            self.energy_count = 0;
        }

        // Mix with reference oscillators
        let m_i = s * self.mark_phase.cos();
        let m_q = s * self.mark_phase.sin();
        let s_i = s * self.space_phase.cos();
        let s_q = s * self.space_phase.sin();
        self.mark_phase += self.mark_phase_inc;
        self.space_phase += self.space_phase_inc;
        if self.mark_phase > TWO_PI {
            self.mark_phase -= TWO_PI;
        }
        if self.space_phase > TWO_PI {
            self.space_phase -= TWO_PI;
        }

        // Sliding-window integration
        let idx = self.corr_idx;
        self.mark_i_sum += m_i - self.mark_i_buf[idx];
        self.mark_q_sum += m_q - self.mark_q_buf[idx];
        self.space_i_sum += s_i - self.space_i_buf[idx];
        self.space_q_sum += s_q - self.space_q_buf[idx];
        self.mark_i_buf[idx] = m_i;
        self.mark_q_buf[idx] = m_q;
        self.space_i_buf[idx] = s_i;
        self.space_q_buf[idx] = s_q;
        self.corr_idx = (idx + 1) % self.corr_len;

        // Compare mark vs space energy
        let mark_energy =
            self.mark_i_sum * self.mark_i_sum + self.mark_q_sum * self.mark_q_sum;
        let space_energy =
            self.space_i_sum * self.space_i_sum + self.space_q_sum * self.space_q_sum;
        let bit: u8 = if mark_energy > space_energy { 1 } else { 0 };

        // PLL clock recovery
        if bit != self.last_bit {
            self.last_bit = bit;
            let error = self.bit_phase - self.samples_per_bit / 2.0;
            self.bit_phase -= PLL_GAIN * error;
        }

        self.bit_phase -= 1.0;
        if self.bit_phase <= 0.0 {
            self.bit_phase += self.samples_per_bit;
            self.process_bit(bit);
        }
    }

    fn process_bit(&mut self, raw_bit: u8) {
        // NRZI decode: no transition = 1, transition = 0
        let decoded_bit: u8 = if raw_bit == self.prev_sampled_bit {
            1
        } else {
            0
        };
        self.prev_sampled_bit = raw_bit;

        if decoded_bit == 1 {
            self.ones += 1;
            return;
        }

        // decoded_bit == 0
        if self.ones >= 7 {
            // Abort
            self.in_frame = false;
            self.frame_bits.clear();
            self.ones = 0;
            return;
        }
        if self.ones == 6 {
            // Flag
            if self.in_frame && self.frame_bits.len() >= 136 {
                if let Some(frame) = self.bits_to_bytes() {
                    self.frames.push(frame);
                }
            }
            self.frame_bits.clear();
            self.in_frame = true;
            self.ones = 0;
            return;
        }
        if self.ones == 5 {
            // Bit stuffing — flush 5 ones, discard stuffed zero
            if self.in_frame {
                for _ in 0..5 {
                    self.frame_bits.push(1);
                }
            }
            self.ones = 0;
            return;
        }

        // Normal data
        if self.in_frame {
            for _ in 0..self.ones {
                self.frame_bits.push(1);
            }
            self.frame_bits.push(0);
        }
        self.ones = 0;
    }

    fn bits_to_bytes(&self) -> Option<RawFrame> {
        let byte_len = self.frame_bits.len() / 8;
        if byte_len < 17 {
            return None;
        }
        let mut bytes = vec![0u8; byte_len];
        for i in 0..byte_len {
            let mut b: u8 = 0;
            for j in 0..8 {
                b |= self.frame_bits[i * 8 + j] << j;
            }
            bytes[i] = b;
        }

        let payload = &bytes[..byte_len - 2];
        let fcs = bytes[byte_len - 2] as u16 | ((bytes[byte_len - 1] as u16) << 8);
        let computed = crc16ccitt(payload);
        let crc_ok = computed == fcs;

        Some(RawFrame {
            payload: payload.to_vec(),
            crc_ok,
        })
    }
}

// ---------------------------------------------------------------------------
// AX.25 address decoding
// ---------------------------------------------------------------------------

struct Ax25Address {
    call: String,
    ssid: u8,
    last: bool,
}

fn decode_ax25_address(bytes: &[u8], offset: usize) -> Ax25Address {
    let mut call = String::with_capacity(6);
    for i in 0..6 {
        let ch = bytes[offset + i] >> 1;
        if ch > 32 {
            call.push(ch as char);
        }
    }
    let call = call.trim_end().to_string();
    let ssid = (bytes[offset + 6] >> 1) & 0x0F;
    let last = (bytes[offset + 6] & 0x01) == 1;
    Ax25Address { call, ssid, last }
}

struct Ax25Frame {
    src: Ax25Address,
    dest: Ax25Address,
    digis: Vec<Ax25Address>,
    info: Vec<u8>,
}

fn parse_ax25(frame: &[u8]) -> Option<Ax25Frame> {
    if frame.len() < 16 {
        return None;
    }
    let dest = decode_ax25_address(frame, 0);
    let src = decode_ax25_address(frame, 7);

    let mut offset = 14;
    let mut digis = Vec::new();
    let mut last_addr = src.last;
    while !last_addr && offset + 7 <= frame.len() {
        let digi = decode_ax25_address(frame, offset);
        last_addr = digi.last;
        digis.push(digi);
        offset += 7;
    }

    if offset + 2 > frame.len() {
        return None;
    }
    // Skip control + PID bytes
    let info = frame[offset + 2..].to_vec();

    Some(Ax25Frame {
        src,
        dest,
        digis,
        info,
    })
}

// ---------------------------------------------------------------------------
// APRS parser
// ---------------------------------------------------------------------------

fn format_call(addr: &Ax25Address) -> String {
    if addr.ssid != 0 {
        format!("{}-{}", addr.call, addr.ssid)
    } else {
        addr.call.clone()
    }
}

fn parse_aprs(ax25: &Ax25Frame) -> AprsPacket {
    let src_call = format_call(&ax25.src);
    let dest_call = format_call(&ax25.dest);
    let path = ax25
        .digis
        .iter()
        .map(|d| format_call(d))
        .collect::<Vec<_>>()
        .join(",");
    let info_str = String::from_utf8_lossy(&ax25.info).to_string();

    let packet_type = if !info_str.is_empty() {
        match info_str.as_bytes()[0] {
            b'!' | b'=' | b'/' | b'@' => "Position",
            b':' => "Message",
            b'>' => "Status",
            b'T' => "Telemetry",
            b';' => "Object",
            b')' => "Item",
            b'`' | b'\'' => "Mic-E",
            _ => "Unknown",
        }
    } else {
        "Unknown"
    };

    let mut lat = None;
    let mut lon = None;
    let mut symbol_table = None;
    let mut symbol_code = None;

    if packet_type == "Position" {
        if let Some(pos) = parse_aprs_position(&info_str) {
            lat = Some(pos.0);
            lon = Some(pos.1);
            symbol_table = Some(pos.2.to_string());
            symbol_code = Some(pos.3.to_string());
        }
    }

    AprsPacket {
        src_call,
        dest_call,
        path,
        info: info_str,
        packet_type: packet_type.to_string(),
        crc_ok: false, // set by caller
        lat,
        lon,
        symbol_table,
        symbol_code,
    }
}

fn parse_aprs_position(info_str: &str) -> Option<(f64, f64, char, char)> {
    if info_str.is_empty() {
        return None;
    }
    let bytes = info_str.as_bytes();
    let dt = bytes[0];

    let pos_str = match dt {
        b'!' | b'=' => &info_str[1..],
        b'/' | b'@' => {
            if info_str.len() < 9 {
                return None;
            }
            &info_str[8..]
        }
        _ => return None,
    };

    if pos_str.is_empty() {
        return None;
    }

    let first = pos_str.as_bytes()[0];
    if first < b'0' || first > b'9' {
        return parse_aprs_compressed(pos_str);
    }

    // Uncompressed: DDMM.MMN/DDDMM.MMEsYYY
    if pos_str.len() < 19 {
        return None;
    }

    let lat_str = &pos_str[..8];
    let sym_table = pos_str.as_bytes()[8] as char;
    let lon_str = &pos_str[9..18];
    let sym_code = pos_str.as_bytes()[18] as char;

    let lat = parse_aprs_lat(lat_str)?;
    let lon = parse_aprs_lon(lon_str)?;

    Some((lat, lon, sym_table, sym_code))
}

fn parse_aprs_compressed(pos_str: &str) -> Option<(f64, f64, char, char)> {
    if pos_str.len() < 10 {
        return None;
    }
    let bytes = pos_str.as_bytes();
    let sym_table = bytes[0] as char;

    let mut lat_val: u32 = 0;
    let mut lon_val: u32 = 0;
    for i in 0..4 {
        let lc = bytes[1 + i] as i32 - 33;
        let xc = bytes[5 + i] as i32 - 33;
        if lc < 0 || lc > 90 || xc < 0 || xc > 90 {
            return None;
        }
        lat_val = lat_val * 91 + lc as u32;
        lon_val = lon_val * 91 + xc as u32;
    }

    let lat = 90.0 - lat_val as f64 / 380926.0;
    let lon = -180.0 + lon_val as f64 / 190463.0;

    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }

    let sym_code = bytes[9] as char;
    let lat = (lat * 1e6).round() / 1e6;
    let lon = (lon * 1e6).round() / 1e6;

    Some((lat, lon, sym_table, sym_code))
}

fn parse_aprs_lat(s: &str) -> Option<f64> {
    if s.len() < 8 {
        return None;
    }
    let deg: f64 = s[..2].parse().ok()?;
    let min: f64 = s[2..7].parse().ok()?;
    let ns = s.as_bytes()[7];
    let mut lat = deg + min / 60.0;
    match ns {
        b'S' | b's' => lat = -lat,
        b'N' | b'n' => {}
        _ => return None,
    }
    Some((lat * 1e6).round() / 1e6)
}

fn parse_aprs_lon(s: &str) -> Option<f64> {
    if s.len() < 9 {
        return None;
    }
    let deg: f64 = s[..3].parse().ok()?;
    let min: f64 = s[3..8].parse().ok()?;
    let ew = s.as_bytes()[8];
    let mut lon = deg + min / 60.0;
    match ew {
        b'W' | b'w' => lon = -lon,
        b'E' | b'e' => {}
        _ => return None,
    }
    Some((lon * 1e6).round() / 1e6)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub struct AprsDecoder {
    demodulators: Vec<Demodulator>,
}

impl AprsDecoder {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            demodulators: vec![
                Demodulator::new(sample_rate, 1.0),
                Demodulator::new(sample_rate, 0.5),
            ],
        }
    }

    pub fn process_samples(&mut self, samples: &[f32]) -> Vec<AprsPacket> {
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();

        for demod in &mut self.demodulators {
            for frame in demod.process_buffer(samples) {
                // Dedup by address prefix + payload length
                let key_len = frame.payload.len().min(14);
                let mut key = Vec::with_capacity(key_len + 4);
                key.extend_from_slice(&frame.payload[..key_len]);
                key.extend_from_slice(&(frame.payload.len() as u32).to_le_bytes());
                if !seen.insert(key) {
                    continue;
                }

                if let Some(ax25) = parse_ax25(&frame.payload) {
                    let mut pkt = parse_aprs(&ax25);
                    pkt.crc_ok = frame.crc_ok;
                    results.push(pkt);
                }
            }
        }

        results
    }

    pub fn reset(&mut self) {
        for demod in &mut self.demodulators {
            demod.reset_state();
            demod.energy_acc = 0.0;
            demod.energy_count = 0;
            demod.frames.clear();
        }
    }
}
