// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! MCU (Minimum Coded Unit) assembly and multi-channel image composition.
//!
//! Meteor-M LRPT imagery is transmitted as MCU blocks (8x8 pixel) across
//! multiple APIDs (Application Process Identifiers).  Each APID corresponds
//! to a different sensor channel:
//!
//!   - APID 64: channel 1 (visible, 0.5-0.7 um)
//!   - APID 65: channel 2 (visible/NIR, 0.7-1.1 um)
//!   - APID 66: channel 3 (near-IR, 1.6-1.8 um)
//!   - APID 67: channel 4 (mid-IR, 3.5-4.1 um)
//!   - APID 68: channel 5 (thermal IR, 10.5-11.5 um)
//!   - APID 69: channel 6 (thermal IR, 11.5-12.5 um)
//!
//! The standard colour composite uses APIDs 64 (R), 65 (G), 66 (B) or
//! APIDs 65 (R), 65 (G), 68 (B) depending on illumination.
//!
//! Each CCSDS packet carries compressed MCU data using a JPEG-like scheme:
//! Huffman-coded DCT coefficients with fixed quantization and Huffman tables.

use std::collections::BTreeMap;

use super::cadu::Cadu;
use super::MeteorSatellite;

/// Image width in pixels (Meteor-M MSU-MR swath: 196 MCU blocks * 8 px).
const LINE_WIDTH: u32 = 1568;

/// Number of 8x8 MCU blocks per image line.
const MCUS_PER_LINE: usize = (LINE_WIDTH / 8) as usize;

/// Known Meteor-M spacecraft IDs.
const SPACECRAFT_M2_3: u16 = 57; // Meteor-M N2-3
const SPACECRAFT_M2_4: u16 = 58; // Meteor-M N2-4

// ============================================================================
// Meteor-M LRPT JPEG quantization table
// ============================================================================

/// Standard quantization table for Meteor-M LRPT imagery.
/// Applied in zigzag order to dequantize DCT coefficients.
#[rustfmt::skip]
const QUANT_TABLE: [i32; 64] = [
    16, 11, 10, 16,  24,  40,  51,  61,
    12, 12, 14, 19,  26,  58,  60,  55,
    14, 13, 16, 24,  40,  57,  69,  56,
    14, 17, 22, 29,  51,  87,  80,  62,
    18, 22, 37, 56,  68, 109, 103,  77,
    24, 35, 55, 64,  81, 104, 113,  92,
    49, 64, 78, 87, 103, 121, 120, 101,
    72, 92, 95, 98, 112, 100, 103,  99,
];

/// JPEG zigzag scan order (maps zigzag index → row-major 8x8 index).
#[rustfmt::skip]
const ZIGZAG: [usize; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

// ============================================================================
// Huffman tables for Meteor-M LRPT (standard JPEG baseline tables)
// ============================================================================

/// DC Huffman table: (code_length, code_value) → category.
/// Standard JPEG luminance DC table.
struct HuffTable {
    /// For each bit length (1..=16), the codes and their symbol values.
    entries: Vec<(u8, u16, u8)>, // (bits, code, symbol)
}

impl HuffTable {
    fn dc_table() -> Self {
        // Standard JPEG luminance DC Huffman table
        // Category 0-11, code lengths from JPEG spec
        #[rustfmt::skip]
        let symbols_by_length: &[(u8, &[u8])] = &[
            (2, &[0, 1, 2, 3, 4, 5]),
            (3, &[6]),
            (4, &[7]),
            (5, &[8]),
            (6, &[9]),
            (7, &[10]),
            (8, &[11]),
        ];

        Self::build(symbols_by_length)
    }

    fn ac_table() -> Self {
        // Standard JPEG luminance AC Huffman table
        // Each symbol is (run_length << 4 | category)
        #[rustfmt::skip]
        let symbols_by_length: &[(u8, &[u8])] = &[
            (2,  &[0x01, 0x02]),
            (3,  &[0x03]),
            (4,  &[0x00, 0x04, 0x11]),
            (5,  &[0x05, 0x12, 0x21]),
            (6,  &[0x31, 0x41]),
            (7,  &[0x06, 0x13, 0x51, 0x61]),
            (8,  &[0x07, 0x22, 0x71]),
            (9,  &[0x14, 0x32, 0x81, 0x91, 0xA1]),
            (10, &[0x08, 0x23, 0x42, 0xB1, 0xC1]),
            (11, &[0x15, 0x52, 0xD1, 0xF0]),
            (12, &[0x24, 0x33, 0x62, 0x72]),
            (15, &[0x82]),
            (16, &[0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25,
                    0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36,
                    0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46,
                    0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56,
                    0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66,
                    0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76,
                    0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86,
                    0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95,
                    0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4,
                    0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3,
                    0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2,
                    0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA,
                    0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9,
                    0xDA, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7,
                    0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5,
                    0xF6, 0xF7, 0xF8, 0xF9, 0xFA]),
        ];

        Self::build(symbols_by_length)
    }

    fn build(symbols_by_length: &[(u8, &[u8])]) -> Self {
        let mut entries = Vec::new();
        let mut code: u16 = 0;

        // Sort by bit length to generate canonical Huffman codes
        let mut all: Vec<(u8, u8)> = Vec::new();
        for &(bits, syms) in symbols_by_length {
            for &sym in syms {
                all.push((bits, sym));
            }
        }
        all.sort_by_key(|&(bits, _)| bits);

        let mut prev_bits = 0u8;
        for &(bits, sym) in &all {
            if prev_bits > 0 {
                code = (code + 1) << (bits - prev_bits);
            }
            entries.push((bits, code, sym));
            prev_bits = bits;
        }

        Self { entries }
    }
}

// ============================================================================
// Bitstream reader
// ============================================================================

struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8, // 0-7, MSB first
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    fn read_bit(&mut self) -> Option<u8> {
        if self.byte_pos >= self.data.len() {
            return None;
        }
        let bit = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
        self.bit_pos += 1;
        if self.bit_pos >= 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
        Some(bit)
    }

    fn read_bits(&mut self, count: u8) -> Option<i32> {
        let mut val: i32 = 0;
        for _ in 0..count {
            val = (val << 1) | self.read_bit()? as i32;
        }
        Some(val)
    }

    fn decode_huffman(&mut self, table: &HuffTable) -> Option<u8> {
        let mut code: u16 = 0;
        let mut bits_read: u8 = 0;

        loop {
            let bit = self.read_bit()?;
            code = (code << 1) | bit as u16;
            bits_read += 1;

            for &(entry_bits, entry_code, symbol) in &table.entries {
                if entry_bits == bits_read && entry_code == code {
                    return Some(symbol);
                }
            }

            if bits_read >= 16 {
                return None;
            }
        }
    }

    fn has_remaining(&self) -> bool {
        self.byte_pos < self.data.len()
    }
}

/// Decode a signed value from category bits (JPEG magnitude encoding).
fn decode_magnitude(category: u8, bits: i32) -> i32 {
    if category == 0 {
        return 0;
    }
    // If MSB is 0, value is negative
    if bits < (1 << (category - 1)) {
        bits - (1 << category) + 1
    } else {
        bits
    }
}

// ============================================================================
// Inverse DCT (8x8)
// ============================================================================

/// Perform 8x8 inverse discrete cosine transform on dequantized coefficients.
fn idct_8x8(coeffs: &[i32; 64], output: &mut [u8; 64]) {
    // Use the standard IDCT formula with precomputed cosine values.
    // cos(pi * (2*x + 1) * u / 16) for x,u in 0..8
    let mut workspace = [0.0f64; 64];

    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0.0f64;
            for v in 0..8 {
                for u in 0..8 {
                    let cu = if u == 0 {
                        std::f64::consts::FRAC_1_SQRT_2
                    } else {
                        1.0
                    };
                    let cv = if v == 0 {
                        std::f64::consts::FRAC_1_SQRT_2
                    } else {
                        1.0
                    };
                    let coeff = coeffs[v * 8 + u] as f64;
                    let cos_x =
                        (std::f64::consts::PI * (2 * x + 1) as f64 * u as f64 / 16.0).cos();
                    let cos_y =
                        (std::f64::consts::PI * (2 * y + 1) as f64 * v as f64 / 16.0).cos();
                    sum += cu * cv * coeff * cos_x * cos_y;
                }
            }
            workspace[y * 8 + x] = sum / 4.0;
        }
    }

    // Level shift (+128) and clamp to [0, 255]
    for i in 0..64 {
        let val = (workspace[i] + 128.0).round();
        output[i] = val.clamp(0.0, 255.0) as u8;
    }
}

// ============================================================================
// MCU block decoder
// ============================================================================

/// Decode a single 8x8 MCU block from a bitstream.
///
/// Returns the decoded 64-pixel block and the updated DC prediction value.
fn decode_mcu_block(
    reader: &mut BitReader,
    dc_table: &HuffTable,
    ac_table: &HuffTable,
    prev_dc: i32,
) -> Option<([u8; 64], i32)> {
    let mut coeffs = [0i32; 64];

    // DC coefficient
    let dc_category = reader.decode_huffman(dc_table)?;
    let dc_bits = if dc_category > 0 {
        reader.read_bits(dc_category)?
    } else {
        0
    };
    let dc_diff = decode_magnitude(dc_category, dc_bits);
    let dc_val = prev_dc + dc_diff;
    coeffs[0] = dc_val;

    // AC coefficients (zigzag positions 1-63)
    let mut idx = 1;
    while idx < 64 {
        let symbol = reader.decode_huffman(ac_table)?;
        if symbol == 0x00 {
            // EOB — remaining coefficients are zero
            break;
        }
        let run = (symbol >> 4) as usize;
        let category = symbol & 0x0F;

        if symbol == 0xF0 {
            // ZRL — skip 16 zeros
            idx += 16;
            continue;
        }

        idx += run;
        if idx >= 64 {
            break;
        }

        let ac_bits = if category > 0 {
            reader.read_bits(category)?
        } else {
            0
        };
        coeffs[idx] = decode_magnitude(category, ac_bits);
        idx += 1;
    }

    // De-zigzag and dequantize
    let mut dequant = [0i32; 64];
    for i in 0..64 {
        dequant[ZIGZAG[i]] = coeffs[i] * QUANT_TABLE[i];
    }

    // Inverse DCT
    let mut pixels = [0u8; 64];
    idct_8x8(&dequant, &mut pixels);

    Some((pixels, dc_val))
}

// ============================================================================
// Channel buffer and assembler
// ============================================================================

/// Per-APID channel accumulator.
struct ChannelBuffer {
    /// Row-major pixel data (grayscale, 0-255).
    pixels: Vec<u8>,
    /// Number of complete image lines accumulated.
    lines: u32,
    /// Current MCU column position within the current MCU row.
    mcu_col: usize,
    /// Row buffer for the current MCU row (8 lines * LINE_WIDTH pixels).
    row_buf: Vec<u8>,
    /// DC prediction value for differential coding.
    prev_dc: i32,
}

impl ChannelBuffer {
    fn new() -> Self {
        Self {
            pixels: Vec::new(),
            lines: 0,
            mcu_col: 0,
            row_buf: vec![0u8; 8 * LINE_WIDTH as usize],
            prev_dc: 0,
        }
    }

    /// Write an 8x8 MCU block at the current column position.
    fn push_mcu_block(&mut self, block: &[u8; 64]) {
        let col = self.mcu_col;
        if col >= MCUS_PER_LINE {
            // Flush the current MCU row to pixels, start a new one
            self.flush_mcu_row();
        }

        let x_off = self.mcu_col * 8;
        for row in 0..8 {
            let dst_start = row * LINE_WIDTH as usize + x_off;
            let src_start = row * 8;
            if dst_start + 8 <= self.row_buf.len() {
                self.row_buf[dst_start..dst_start + 8]
                    .copy_from_slice(&block[src_start..src_start + 8]);
            }
        }
        self.mcu_col += 1;

        // If we've filled a complete MCU row, flush it
        if self.mcu_col >= MCUS_PER_LINE {
            self.flush_mcu_row();
        }
    }

    fn flush_mcu_row(&mut self) {
        if self.mcu_col == 0 {
            return;
        }
        self.pixels.extend_from_slice(&self.row_buf);
        self.lines += 8;
        self.row_buf.fill(0);
        self.mcu_col = 0;
    }

    /// Push raw pixel data as a fallback (one LINE_WIDTH row at a time).
    fn push_raw_row(&mut self, data: &[u8]) {
        self.pixels.extend_from_slice(data);
        self.lines = (self.pixels.len() / LINE_WIDTH as usize) as u32;
    }
}

/// Assembles decoded MCU blocks from multiple APIDs into a composite image.
pub struct ChannelAssembler {
    /// Per-APID buffers.
    channels: BTreeMap<u16, ChannelBuffer>,
    /// Total MCU rows across all channels.
    total_mcu_count: u32,
    /// Spacecraft ID seen in CADUs (for satellite identification).
    spacecraft_id: Option<u16>,
    /// Huffman tables (built once).
    dc_table: HuffTable,
    ac_table: HuffTable,
    /// Partial CCSDS packet reassembly buffer, keyed by APID.
    packet_buf: BTreeMap<u16, Vec<u8>>,
}

impl Default for ChannelAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelAssembler {
    pub fn new() -> Self {
        Self {
            channels: BTreeMap::new(),
            total_mcu_count: 0,
            spacecraft_id: None,
            dc_table: HuffTable::dc_table(),
            ac_table: HuffTable::ac_table(),
            packet_buf: BTreeMap::new(),
        }
    }

    /// Process a single CADU, extracting MCU data for each APID found.
    pub fn process_cadu(&mut self, cadu: &Cadu) {
        // Record spacecraft ID
        let scid = cadu.spacecraft_id();
        if scid > 0 {
            self.spacecraft_id = Some(scid);
        }

        let vcid = cadu.vcid();
        let payload = cadu.mpdu_payload();

        // Virtual channels 0-5 carry APID 64-69 imagery
        if vcid > 5 || payload.is_empty() {
            return;
        }

        let apid = 64 + vcid as u16;

        // Parse first header pointer from MPDU header.
        // The 2 bytes before the payload in the CADU (at offset 10-11 after ASM)
        // contain the first header pointer. If 0x07FF, no packet starts here.
        let fhp = if cadu.data.len() >= 12 {
            ((cadu.data[10] as u16 & 0x07) << 8) | cadu.data[11] as u16
        } else {
            0x07FF
        };

        if fhp == 0x07FF {
            // No new packet starts in this MPDU — append to ongoing packet
            self.packet_buf
                .entry(apid)
                .or_default()
                .extend_from_slice(payload);
        } else {
            let fhp = fhp as usize;

            // Complete the previous packet with data before the pointer
            if fhp > 0 && fhp <= payload.len() {
                if let Some(buf) = self.packet_buf.get_mut(&apid) {
                    buf.extend_from_slice(&payload[..fhp]);
                    let packet_data = std::mem::take(buf);
                    self.decode_packet(apid, &packet_data);
                }
            }

            // Start new packet from the first header pointer
            if fhp < payload.len() {
                let buf = self.packet_buf.entry(apid).or_default();
                buf.clear();
                buf.extend_from_slice(&payload[fhp..]);
            }
        }
    }

    /// Attempt to decode MCU blocks from a reassembled CCSDS packet.
    fn decode_packet(&mut self, apid: u16, data: &[u8]) {
        // CCSDS source packet: 6-byte primary header + data zone
        if data.len() < 10 {
            return;
        }

        // Skip 6-byte CCSDS primary header + 4 bytes of secondary header
        // to reach the compressed MCU data
        let mcu_data = &data[10..];
        if mcu_data.is_empty() {
            return;
        }

        let buf = self.channels.entry(apid).or_insert_with(ChannelBuffer::new);

        // Try JPEG MCU decompression
        let mut reader = BitReader::new(mcu_data);
        let mut blocks_decoded = 0u32;

        while reader.has_remaining() {
            match decode_mcu_block(&mut reader, &self.dc_table, &self.ac_table, buf.prev_dc) {
                Some((block, new_dc)) => {
                    buf.prev_dc = new_dc;
                    buf.push_mcu_block(&block);
                    blocks_decoded += 1;
                }
                None => break,
            }
        }

        if blocks_decoded > 0 {
            self.total_mcu_count += blocks_decoded;
        } else if mcu_data.len() >= LINE_WIDTH as usize {
            // Fallback: if JPEG decode fails entirely, try as raw data
            let usable = mcu_data.len().min(LINE_WIDTH as usize);
            let mut row = vec![0u8; LINE_WIDTH as usize];
            row[..usable].copy_from_slice(&mcu_data[..usable]);
            buf.push_raw_row(&row);
            self.total_mcu_count += 1;
        }
    }

    /// Total MCU rows decoded across all channels.
    pub fn mcu_count(&self) -> u32 {
        self.total_mcu_count
    }

    /// Active APID channels.
    pub fn active_apids(&self) -> Vec<u16> {
        self.channels.keys().copied().collect()
    }

    /// Identify the satellite from the CCSDS spacecraft ID.
    pub fn identify_satellite(&self) -> Option<MeteorSatellite> {
        self.spacecraft_id.map(|id| match id {
            SPACECRAFT_M2_3 => MeteorSatellite::MeteorM2_3,
            SPACECRAFT_M2_4 => MeteorSatellite::MeteorM2_4,
            _ => MeteorSatellite::Unknown,
        })
    }

    /// Encode accumulated channel data as a PNG image.
    ///
    /// Produces an RGB composite if channels 64, 65, 66 are available,
    /// otherwise produces a grayscale image of the most populated channel.
    pub fn encode_png(&self) -> Option<Vec<u8>> {
        if self.channels.is_empty() {
            return None;
        }

        // Flush any partial MCU rows by computing effective heights
        let max_lines = self
            .channels
            .values()
            .map(|ch| {
                let extra = if ch.mcu_col > 0 { 8 } else { 0 };
                ch.lines + extra
            })
            .max()
            .unwrap_or(0);

        if max_lines == 0 {
            return None;
        }

        let width = LINE_WIDTH;
        let height = max_lines;
        let npix = (width * height) as usize;

        // Helper to get pixel data including unflushed MCU row
        let get_pixels = |ch: &ChannelBuffer| -> Vec<u8> {
            let mut px = ch.pixels.clone();
            if ch.mcu_col > 0 {
                px.extend_from_slice(&ch.row_buf);
            }
            px
        };

        // Try RGB composite (APIDs 64=R, 65=G, 66=B)
        let ch_r = self.channels.get(&64);
        let ch_g = self.channels.get(&65);
        let ch_b = self.channels.get(&66);

        if ch_r.is_some() || ch_g.is_some() || ch_b.is_some() {
            let px_r = ch_r.map(get_pixels);
            let px_g = ch_g.map(get_pixels);
            let px_b = ch_b.map(get_pixels);

            let mut rgb_pixels: Vec<u8> = Vec::with_capacity(npix * 3);
            for i in 0..npix {
                let r = px_r.as_ref().and_then(|p| p.get(i).copied()).unwrap_or(0);
                let g = px_g.as_ref().and_then(|p| p.get(i).copied()).unwrap_or(0);
                let b = px_b.as_ref().and_then(|p| p.get(i).copied()).unwrap_or(0);
                rgb_pixels.push(r);
                rgb_pixels.push(g);
                rgb_pixels.push(b);
            }
            crate::image_enc::encode_rgb_png(width, height, rgb_pixels)
        } else {
            // Fallback: grayscale from the first available channel
            let first_ch = self.channels.values().next()?;
            let px = get_pixels(first_ch);
            let mut gray_pixels: Vec<u8> = Vec::with_capacity(npix);
            for i in 0..npix {
                gray_pixels.push(px.get(i).copied().unwrap_or(0));
            }
            crate::image_enc::encode_grayscale_png(width, height, gray_pixels)
        }
    }

    pub fn reset(&mut self) {
        self.channels.clear();
        self.total_mcu_count = 0;
        self.spacecraft_id = None;
        self.packet_buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_buffer_line_counting() {
        let mut buf = ChannelBuffer::new();
        let row = vec![128u8; LINE_WIDTH as usize];
        buf.push_raw_row(&row);
        assert_eq!(buf.lines, 1);
        buf.push_raw_row(&row);
        assert_eq!(buf.lines, 2);
    }

    #[test]
    fn test_mcu_block_placement() {
        let mut buf = ChannelBuffer::new();
        let block = [200u8; 64];

        // Push one MCU block
        buf.push_mcu_block(&block);
        assert_eq!(buf.mcu_col, 1);
        assert_eq!(buf.lines, 0); // Not yet a full MCU row

        // The first 8 pixels of row 0 in row_buf should be 200
        assert_eq!(buf.row_buf[0], 200);
        assert_eq!(buf.row_buf[7], 200);
        // Pixel at column 8 should still be 0
        assert_eq!(buf.row_buf[8], 0);
    }

    #[test]
    fn test_mcu_row_flush() {
        let mut buf = ChannelBuffer::new();
        let block = [128u8; 64];

        // Fill a complete MCU row (196 blocks)
        for _ in 0..MCUS_PER_LINE {
            buf.push_mcu_block(&block);
        }

        // Should have flushed: 8 lines of LINE_WIDTH pixels
        assert_eq!(buf.lines, 8);
        assert_eq!(buf.pixels.len(), 8 * LINE_WIDTH as usize);
        assert_eq!(buf.mcu_col, 0);
    }

    #[test]
    fn test_identify_satellite() {
        let mut asm = ChannelAssembler::new();
        assert_eq!(asm.identify_satellite(), None);

        asm.spacecraft_id = Some(SPACECRAFT_M2_3);
        assert_eq!(
            asm.identify_satellite(),
            Some(MeteorSatellite::MeteorM2_3)
        );

        asm.spacecraft_id = Some(SPACECRAFT_M2_4);
        assert_eq!(
            asm.identify_satellite(),
            Some(MeteorSatellite::MeteorM2_4)
        );

        asm.spacecraft_id = Some(99);
        assert_eq!(asm.identify_satellite(), Some(MeteorSatellite::Unknown));
    }

    #[test]
    fn test_decode_magnitude() {
        assert_eq!(decode_magnitude(0, 0), 0);
        assert_eq!(decode_magnitude(1, 1), 1);
        assert_eq!(decode_magnitude(1, 0), -1);
        assert_eq!(decode_magnitude(2, 3), 3);
        assert_eq!(decode_magnitude(2, 2), 2);
        assert_eq!(decode_magnitude(2, 1), -2);
        assert_eq!(decode_magnitude(2, 0), -3);
    }

    #[test]
    fn test_idct_dc_only() {
        // A block with only a DC coefficient should produce a uniform block
        let mut coeffs = [0i32; 64];
        coeffs[0] = 100;
        let mut output = [0u8; 64];
        idct_8x8(&coeffs, &mut output);

        // All pixels should be close to 128 + 100/4 = 153 (DC is scaled by 1/4)
        // Actually DC: C(0)*C(0) * coeff * cos(0)*cos(0) / 4
        // = (1/√2)*(1/√2) * 100 * 1 * 1 / 4 = 100/8 = 12.5, + 128 = 140.5
        let expected = (100.0_f64 * 0.5 / 4.0 + 128.0).round() as u8;
        for &px in &output {
            assert!(
                (px as i32 - expected as i32).unsigned_abs() <= 1,
                "pixel {} != expected {}",
                px,
                expected
            );
        }
    }

    #[test]
    fn test_bitreader_basics() {
        let data = [0b10110100, 0b01100000];
        let mut reader = BitReader::new(&data);

        assert_eq!(reader.read_bit(), Some(1));
        assert_eq!(reader.read_bit(), Some(0));
        assert_eq!(reader.read_bit(), Some(1));
        assert_eq!(reader.read_bit(), Some(1));
        assert_eq!(reader.read_bits(4), Some(0b0100));
        assert_eq!(reader.read_bits(3), Some(0b011));
    }
}
