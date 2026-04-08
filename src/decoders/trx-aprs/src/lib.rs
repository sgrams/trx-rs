// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
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
    fn new(sample_rate: u32, baud: f32, mark_hz: f32, space_hz: f32, window_factor: f32) -> Self {
        let sr = sample_rate as f32;
        let samples_per_bit = sr / baud;
        let corr_len = (samples_per_bit * window_factor).round().max(2.0) as usize;
        let energy_window = (sr * 0.05).round() as usize;

        Self {
            samples_per_bit,
            energy_acc: 0.0,
            energy_count: 0,
            energy_window,
            mark_phase: 0.0,
            space_phase: 0.0,
            mark_phase_inc: TWO_PI * mark_hz / sr,
            space_phase_inc: TWO_PI * space_hz / sr,
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
        let mark_energy = self.mark_i_sum * self.mark_i_sum + self.mark_q_sum * self.mark_q_sum;
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
        for (i, out) in bytes.iter_mut().enumerate() {
            let mut b: u8 = 0;
            for j in 0..8 {
                b |= self.frame_bits[i * 8 + j] << j;
            }
            *out = b;
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
        .map(format_call)
        .collect::<Vec<_>>()
        .join(",");
    let info = &ax25.info;
    let info_str = String::from_utf8_lossy(info).to_string();

    let packet_type = if !info.is_empty() {
        match info[0] {
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
        if let Some(pos) = parse_aprs_position(info) {
            lat = Some(pos.0);
            lon = Some(pos.1);
            symbol_table = Some(pos.2.to_string());
            symbol_code = Some(pos.3.to_string());
        }
    }

    AprsPacket {
        rig_id: None,
        ts_ms: None,
        src_call,
        dest_call,
        path,
        info: info_str,
        info_bytes: info.to_vec(),
        packet_type: packet_type.to_string(),
        crc_ok: false, // set by caller
        lat,
        lon,
        symbol_table,
        symbol_code,
    }
}

fn parse_aprs_position(info: &[u8]) -> Option<(f64, f64, char, char)> {
    if info.is_empty() {
        return None;
    }
    let dt = info[0];

    let pos = match dt {
        b'!' | b'=' => &info[1..],
        b'/' | b'@' => {
            if info.len() < 9 {
                return None;
            }
            &info[8..]
        }
        _ => return None,
    };

    if pos.is_empty() {
        return None;
    }

    if pos[0] < b'0' || pos[0] > b'9' {
        return parse_aprs_compressed(pos);
    }

    // Uncompressed: DDMM.MMN/DDDMM.MMEsYYY
    if pos.len() < 19 {
        return None;
    }

    let sym_table = pos[8] as char;
    let sym_code = pos[18] as char;

    let lat = parse_aprs_lat(&pos[..8])?;
    let lon = parse_aprs_lon(&pos[9..18])?;

    Some((lat, lon, sym_table, sym_code))
}

fn parse_aprs_compressed(pos: &[u8]) -> Option<(f64, f64, char, char)> {
    if pos.len() < 10 {
        return None;
    }
    let sym_table = pos[0] as char;

    let mut lat_val: u32 = 0;
    let mut lon_val: u32 = 0;
    for i in 0..4 {
        let lc = pos[1 + i] as i32 - 33;
        let xc = pos[5 + i] as i32 - 33;
        if !(0..=90).contains(&lc) || !(0..=90).contains(&xc) {
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

    let sym_code = pos[9] as char;
    let lat = (lat * 1e6).round() / 1e6;
    let lon = (lon * 1e6).round() / 1e6;

    Some((lat, lon, sym_table, sym_code))
}

fn parse_aprs_lat(b: &[u8]) -> Option<f64> {
    if b.len() < 8 {
        return None;
    }
    let deg: f64 = std::str::from_utf8(&b[..2]).ok()?.parse().ok()?;
    let min: f64 = std::str::from_utf8(&b[2..7]).ok()?.parse().ok()?;
    let mut lat = deg + min / 60.0;
    match b[7] {
        b'S' | b's' => lat = -lat,
        b'N' | b'n' => {}
        _ => return None,
    }
    Some((lat * 1e6).round() / 1e6)
}

fn parse_aprs_lon(b: &[u8]) -> Option<f64> {
    if b.len() < 9 {
        return None;
    }
    let deg: f64 = std::str::from_utf8(&b[..3]).ok()?.parse().ok()?;
    let min: f64 = std::str::from_utf8(&b[3..8]).ok()?.parse().ok()?;
    let mut lon = deg + min / 60.0;
    match b[8] {
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
    /// VHF APRS: Bell 202, 1200 baud, mark=1200 Hz, space=2200 Hz.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            demodulators: vec![
                Demodulator::new(sample_rate, 1200.0, 1200.0, 2200.0, 1.0),
                Demodulator::new(sample_rate, 1200.0, 1200.0, 2200.0, 0.5),
            ],
        }
    }

    /// HF APRS: 300 baud, mark=1600 Hz, space=1800 Hz (200 Hz shift).
    pub fn new_hf(sample_rate: u32) -> Self {
        Self {
            demodulators: vec![
                Demodulator::new(sample_rate, 300.0, 1600.0, 1800.0, 1.0),
                Demodulator::new(sample_rate, 300.0, 1600.0, 1800.0, 0.5),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ======================================================================
    // CRC-16-CCITT
    // ======================================================================

    #[test]
    fn crc16_empty() {
        // CRC of empty input = 0xFFFF ^ 0xFFFF = 0x0000
        assert_eq!(crc16ccitt(&[]), 0x0000);
    }

    #[test]
    fn crc16_known_vector() {
        // "123456789" has well-known CCITT (x25) CRC = 0x906E
        assert_eq!(crc16ccitt(b"123456789"), 0x906E);
    }

    #[test]
    fn crc16_frame_with_appended_fcs_is_zero() {
        // When the FCS is appended to the payload, the CRC of the whole
        // sequence should yield the residue constant 0x0F47.
        let payload = b"123456789";
        let fcs = crc16ccitt(payload);
        let mut with_fcs = payload.to_vec();
        with_fcs.push(fcs as u8);
        with_fcs.push((fcs >> 8) as u8);
        assert_eq!(crc16ccitt(&with_fcs), 0x0F47);
    }

    // ======================================================================
    // AX.25 address decoding
    // ======================================================================

    #[test]
    fn decode_ax25_address_basic() {
        // AX.25 addresses are left-shifted by 1 bit. "N0CALL" → bytes shifted.
        let mut addr = [0u8; 7];
        for (i, &ch) in b"N0CALL".iter().enumerate() {
            addr[i] = ch << 1;
        }
        addr[6] = (0 << 1) | 1; // SSID=0, last=true

        let decoded = decode_ax25_address(&addr, 0);
        assert_eq!(decoded.call, "N0CALL");
        assert_eq!(decoded.ssid, 0);
        assert!(decoded.last);
    }

    #[test]
    fn decode_ax25_address_with_ssid() {
        let mut addr = [0u8; 7];
        for (i, &ch) in b"SP2SJG".iter().enumerate() {
            addr[i] = ch << 1;
        }
        addr[6] = (5 << 1) | 0; // SSID=5, last=false

        let decoded = decode_ax25_address(&addr, 0);
        assert_eq!(decoded.call, "SP2SJG");
        assert_eq!(decoded.ssid, 5);
        assert!(!decoded.last);
    }

    #[test]
    fn decode_ax25_address_short_call() {
        // Short callsign "W1AW" padded with spaces (0x20)
        let mut addr = [0u8; 7];
        for (i, &ch) in b"W1AW  ".iter().enumerate() {
            addr[i] = ch << 1;
        }
        addr[6] = (0 << 1) | 1;

        let decoded = decode_ax25_address(&addr, 0);
        assert_eq!(decoded.call, "W1AW");
    }

    // ======================================================================
    // AX.25 frame parsing
    // ======================================================================

    /// Build a minimal valid AX.25 UI frame from src/dest callsigns and info.
    fn build_ax25_frame(dest: &str, src: &str, info: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        // Destination address (7 bytes)
        let dest_bytes = format!("{:<6}", dest);
        for &ch in dest_bytes.as_bytes().iter().take(6) {
            frame.push(ch << 1);
        }
        frame.push(0 << 1); // SSID=0, last=false
        // Source address (7 bytes)
        let src_bytes = format!("{:<6}", src);
        for &ch in src_bytes.as_bytes().iter().take(6) {
            frame.push(ch << 1);
        }
        frame.push((0 << 1) | 1); // SSID=0, last=true
        // Control + PID
        frame.push(0x03); // UI frame
        frame.push(0xF0); // No layer-3 protocol
        // Info field
        frame.extend_from_slice(info);
        frame
    }

    #[test]
    fn parse_ax25_minimal_frame() {
        let frame = build_ax25_frame("APRS", "SP2SJG", b"!5213.78N/02100.73E-Test");
        let parsed = parse_ax25(&frame).unwrap();
        assert_eq!(parsed.src.call, "SP2SJG");
        assert_eq!(parsed.dest.call, "APRS");
        assert!(parsed.digis.is_empty());
        assert_eq!(parsed.info, b"!5213.78N/02100.73E-Test");
    }

    #[test]
    fn parse_ax25_too_short_returns_none() {
        assert!(parse_ax25(&[0u8; 10]).is_none());
    }

    // ======================================================================
    // APRS position parsing
    // ======================================================================

    #[test]
    fn parse_aprs_lat_north() {
        let lat = parse_aprs_lat(b"5213.78N").unwrap();
        assert!((lat - 52.229667).abs() < 0.001);
    }

    #[test]
    fn parse_aprs_lat_south() {
        let lat = parse_aprs_lat(b"3352.13S").unwrap();
        assert!(lat < 0.0);
        assert!((lat + 33.868833).abs() < 0.001);
    }

    #[test]
    fn parse_aprs_lon_east() {
        let lon = parse_aprs_lon(b"02100.73E").unwrap();
        assert!((lon - 21.012167).abs() < 0.001);
    }

    #[test]
    fn parse_aprs_lon_west() {
        let lon = parse_aprs_lon(b"08737.79W").unwrap();
        assert!(lon < 0.0);
    }

    #[test]
    fn parse_aprs_position_uncompressed() {
        let info = b"!5213.78N/02100.73E-Test";
        let (lat, lon, sym_table, sym_code) = parse_aprs_position(info).unwrap();
        assert!((lat - 52.229667).abs() < 0.001);
        assert!((lon - 21.012167).abs() < 0.001);
        assert_eq!(sym_table, '/');
        assert_eq!(sym_code, '-');
    }

    #[test]
    fn parse_aprs_position_with_timestamp() {
        // '@' type requires 7-byte timestamp before position
        let info = b"@092345z5213.78N/02100.73E-Test";
        let (lat, lon, _, _) = parse_aprs_position(info).unwrap();
        assert!((lat - 52.229667).abs() < 0.001);
        assert!((lon - 21.012167).abs() < 0.001);
    }

    #[test]
    fn parse_aprs_compressed_position() {
        // Compressed format: symbol_table + 4 lat chars + 4 lon chars + symbol_code + ...
        // Encode lat=52.23, lon=21.01
        let lat_val = ((90.0_f64 - 52.23) * 380926.0).round() as u32;
        let lon_val = ((21.01_f64 + 180.0) * 190463.0).round() as u32;
        let mut pos = vec![b'/']; // symbol table
        for i in (0..4).rev() {
            pos.push(((lat_val / 91u32.pow(i)) % 91 + 33) as u8);
        }
        for i in (0..4).rev() {
            pos.push(((lon_val / 91u32.pow(i)) % 91 + 33) as u8);
        }
        pos.push(b'-'); // symbol code

        let result = parse_aprs_compressed(&pos);
        assert!(result.is_some());
        let (lat, lon, sym_table, sym_code) = result.unwrap();
        assert!((lat - 52.23).abs() < 0.01);
        assert!((lon - 21.01).abs() < 0.01);
        assert_eq!(sym_table, '/');
        assert_eq!(sym_code, '-');
    }

    #[test]
    fn parse_aprs_position_empty_returns_none() {
        assert!(parse_aprs_position(b"").is_none());
    }

    // ======================================================================
    // APRS packet type detection
    // ======================================================================

    #[test]
    fn aprs_packet_type_detection() {
        let frame = build_ax25_frame("APRS", "N0CALL", b"!5213.78N/02100.73E-");
        let ax25 = parse_ax25(&frame).unwrap();
        let pkt = parse_aprs(&ax25);
        assert_eq!(pkt.packet_type, "Position");
        assert_eq!(pkt.src_call, "N0CALL");
    }

    #[test]
    fn aprs_message_type() {
        let frame = build_ax25_frame("APRS", "N0CALL", b":BLN1     :Test bulletin");
        let ax25 = parse_ax25(&frame).unwrap();
        let pkt = parse_aprs(&ax25);
        assert_eq!(pkt.packet_type, "Message");
    }

    #[test]
    fn aprs_status_type() {
        let frame = build_ax25_frame("APRS", "N0CALL", b">On the air");
        let ax25 = parse_ax25(&frame).unwrap();
        let pkt = parse_aprs(&ax25);
        assert_eq!(pkt.packet_type, "Status");
    }

    #[test]
    fn aprs_mic_e_type() {
        let frame = build_ax25_frame("APRS", "N0CALL", b"`test mic-e");
        let ax25 = parse_ax25(&frame).unwrap();
        let pkt = parse_aprs(&ax25);
        assert_eq!(pkt.packet_type, "Mic-E");
    }

    // ======================================================================
    // format_call
    // ======================================================================

    #[test]
    fn format_call_no_ssid() {
        let addr = Ax25Address {
            call: "N0CALL".to_string(),
            ssid: 0,
            last: true,
        };
        assert_eq!(format_call(&addr), "N0CALL");
    }

    #[test]
    fn format_call_with_ssid() {
        let addr = Ax25Address {
            call: "SP2SJG".to_string(),
            ssid: 15,
            last: true,
        };
        assert_eq!(format_call(&addr), "SP2SJG-15");
    }

    // ======================================================================
    // HDLC bits_to_bytes
    // ======================================================================

    #[test]
    fn bits_to_bytes_too_short_returns_none() {
        let demod = Demodulator::new(48000, 1200.0, 1200.0, 2200.0, 1.0);
        // Less than 17 bytes worth of bits
        let mut d = demod;
        d.frame_bits = vec![0; 8 * 10]; // only 10 bytes
        assert!(d.bits_to_bytes().is_none());
    }

    #[test]
    fn bits_to_bytes_valid_frame() {
        let payload = b"Hello, AX.25 World!";
        let fcs = crc16ccitt(payload);
        // Convert payload + FCS to LSB-first bit stream
        let mut bits = Vec::new();
        for &byte in payload.iter() {
            for j in 0..8 {
                bits.push((byte >> j) & 1);
            }
        }
        bits.push((fcs as u8) & 1);
        for j in 1..8 {
            bits.push(((fcs as u8) >> j) & 1);
        }
        let fcs_hi = (fcs >> 8) as u8;
        for j in 0..8 {
            bits.push((fcs_hi >> j) & 1);
        }

        let mut demod = Demodulator::new(48000, 1200.0, 1200.0, 2200.0, 1.0);
        demod.frame_bits = bits;
        let frame = demod.bits_to_bytes().unwrap();
        assert!(frame.crc_ok);
        assert_eq!(frame.payload, payload);
    }

    // ======================================================================
    // Demodulator smoke test
    // ======================================================================

    #[test]
    fn demodulator_silence_produces_no_frames() {
        let mut decoder = AprsDecoder::new(48000);
        let silence = vec![0.0f32; 48000]; // 1 second of silence
        let packets = decoder.process_samples(&silence);
        assert!(packets.is_empty());
    }

    #[test]
    fn decoder_reset_clears_state() {
        let mut decoder = AprsDecoder::new(48000);
        let noise: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        decoder.process_samples(&noise);
        decoder.reset();
        // After reset, internal state should be clean
        for demod in &decoder.demodulators {
            assert_eq!(demod.mark_phase, 0.0);
            assert_eq!(demod.space_phase, 0.0);
            assert!(!demod.in_frame);
            assert!(demod.frame_bits.is_empty());
            assert!(demod.frames.is_empty());
        }
    }
}
