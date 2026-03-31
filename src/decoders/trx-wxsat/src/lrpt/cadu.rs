// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! CCSDS CADU (Channel Access Data Unit) frame synchronisation and extraction.
//!
//! Meteor-M LRPT uses CCSDS-compatible framing:
//! - Attached Sync Marker (ASM): `0x1ACFFC1D` (32 bits)
//! - CADU length: 1024 bytes (8192 bits) including ASM
//! - Rate 1/2 convolutional coding (Viterbi decoded upstream)
//! - Reed-Solomon (255, 223) error correction
//!
//! The framer correlates against the ASM pattern to find frame boundaries,
//! then extracts fixed-length CADUs.

/// CCSDS Attached Sync Marker for Meteor-M LRPT.
const ASM: [u8; 4] = [0x1A, 0xCF, 0xFC, 0x1D];

/// Total CADU length in bytes (including 4-byte ASM).
pub const CADU_LEN: usize = 1024;

/// CADU payload length (excluding ASM).
pub const CADU_PAYLOAD_LEN: usize = CADU_LEN - 4;

/// Generate the CCSDS pseudo-random derandomization sequence.
///
/// Polynomial: x^8 + x^7 + x^5 + x^3 + 1, initial state 0xFF.
/// The sequence is XOR'd with CADU bytes after the ASM to undo the
/// on-board randomization applied before transmission.
fn ccsds_derandomize(data: &mut [u8]) {
    let mut sr: u8 = 0xFF;
    for byte in data.iter_mut() {
        *byte ^= sr;
        for _ in 0..8 {
            let feedback = ((sr >> 7) ^ (sr >> 5) ^ (sr >> 3) ^ sr) & 1;
            sr = (sr << 1) | feedback;
        }
    }
}

/// A complete CADU frame (1024 bytes including ASM).
#[derive(Clone)]
pub struct Cadu {
    pub data: Vec<u8>,
}

impl Cadu {
    /// VCDU header: spacecraft ID (10 bits starting at byte 4).
    pub fn spacecraft_id(&self) -> u16 {
        if self.data.len() < 6 {
            return 0;
        }
        ((self.data[4] as u16) << 2) | ((self.data[5] as u16) >> 6)
    }

    /// VCDU header: virtual channel ID (6 bits).
    pub fn vcid(&self) -> u8 {
        if self.data.len() < 6 {
            return 0;
        }
        self.data[5] & 0x3F
    }

    /// VCDU counter (24 bits, bytes 6-8).
    pub fn vcdu_counter(&self) -> u32 {
        if self.data.len() < 9 {
            return 0;
        }
        ((self.data[6] as u32) << 16) | ((self.data[7] as u32) << 8) | (self.data[8] as u32)
    }

    /// MPDU payload region (after VCDU primary header).
    pub fn mpdu_payload(&self) -> &[u8] {
        if self.data.len() < 16 {
            return &[];
        }
        // VCDU primary header = 6 bytes, MPDU header pointer = 2 bytes
        // Payload starts at offset 4 (ASM) + 6 (VCDU hdr) + 2 (MPDU ptr) = 12
        &self.data[12..]
    }
}

/// Accumulates soft symbols, performs Viterbi-like hard decisions, and
/// searches for ASM to extract complete CADUs.
pub struct CaduFramer {
    /// Bit accumulation buffer.
    bit_buf: Vec<u8>,
    /// Byte accumulation buffer for frame extraction.
    byte_buf: Vec<u8>,
    /// Whether we are locked to a frame boundary.
    locked: bool,
    /// Bytes remaining in the current frame.
    remaining: usize,
}

impl Default for CaduFramer {
    fn default() -> Self {
        Self::new()
    }
}

impl CaduFramer {
    pub fn new() -> Self {
        Self {
            bit_buf: Vec::new(),
            byte_buf: Vec::new(),
            locked: false,
            remaining: 0,
        }
    }

    /// Push soft symbols (interleaved I/Q) and extract any complete CADUs.
    ///
    /// Soft symbols are hard-decided (threshold at 0.0) and packed into bytes.
    pub fn push(&mut self, symbols: &[f32]) -> Vec<Cadu> {
        // Hard-decide symbols to bits
        for &sym in symbols {
            self.bit_buf.push(if sym >= 0.0 { 1 } else { 0 });
        }

        // Pack bits into bytes
        while self.bit_buf.len() >= 8 {
            let byte = (self.bit_buf[0] << 7)
                | (self.bit_buf[1] << 6)
                | (self.bit_buf[2] << 5)
                | (self.bit_buf[3] << 4)
                | (self.bit_buf[4] << 3)
                | (self.bit_buf[5] << 2)
                | (self.bit_buf[6] << 1)
                | self.bit_buf[7];
            self.byte_buf.push(byte);
            self.bit_buf.drain(..8);
        }

        let mut cadus = Vec::new();
        self.extract_frames(&mut cadus);
        cadus
    }

    fn extract_frames(&mut self, cadus: &mut Vec<Cadu>) {
        loop {
            if self.locked {
                if self.byte_buf.len() >= self.remaining {
                    // Collect the rest of the frame
                    let frame_bytes: Vec<u8> = self.byte_buf.drain(..self.remaining).collect();
                    // Prepend ASM to make a complete CADU
                    let mut data = ASM.to_vec();
                    data.extend_from_slice(&frame_bytes);
                    if data.len() == CADU_LEN {
                        // Derandomize payload (everything after 4-byte ASM)
                        ccsds_derandomize(&mut data[4..]);
                        cadus.push(Cadu { data });
                    }
                    self.locked = false;
                    continue;
                }
                break;
            }

            // Search for ASM in the byte buffer
            if let Some(pos) = find_asm(&self.byte_buf) {
                // Discard bytes before ASM
                self.byte_buf.drain(..pos);
                // Skip the 4 ASM bytes
                if self.byte_buf.len() >= 4 {
                    self.byte_buf.drain(..4);
                    self.locked = true;
                    self.remaining = CADU_LEN - 4; // payload bytes needed
                    continue;
                }
                break;
            }

            // No ASM found; keep last 3 bytes (partial ASM might straddle boundary)
            if self.byte_buf.len() > 3 {
                let keep = self.byte_buf.len().saturating_sub(3);
                self.byte_buf.drain(..keep);
            }
            break;
        }
    }

    pub fn reset(&mut self) {
        self.bit_buf.clear();
        self.byte_buf.clear();
        self.locked = false;
        self.remaining = 0;
    }
}

/// Find the ASM pattern in a byte buffer; returns the offset if found.
fn find_asm(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    (0..=(buf.len() - 4)).find(|&i| {
        buf[i] == ASM[0] && buf[i + 1] == ASM[1] && buf[i + 2] == ASM[2] && buf[i + 3] == ASM[3]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_asm() {
        let buf = [0x00, 0x1A, 0xCF, 0xFC, 0x1D, 0x00];
        assert_eq!(find_asm(&buf), Some(1));
    }

    #[test]
    fn test_find_asm_at_start() {
        let buf = [0x1A, 0xCF, 0xFC, 0x1D, 0x00];
        assert_eq!(find_asm(&buf), Some(0));
    }

    #[test]
    fn test_find_asm_not_found() {
        let buf = [0x00, 0x01, 0x02, 0x03, 0x04];
        assert_eq!(find_asm(&buf), None);
    }

    #[test]
    fn test_derandomize_roundtrip() {
        let original = vec![0xAB; CADU_PAYLOAD_LEN];
        let mut data = original.clone();
        // Randomize
        ccsds_derandomize(&mut data);
        // Should differ from original
        assert_ne!(data, original);
        // Derandomize again (same sequence) should restore
        ccsds_derandomize(&mut data);
        assert_eq!(data, original);
    }

    #[test]
    fn test_cadu_spacecraft_id() {
        let mut data = vec![0u8; CADU_LEN];
        // ASM
        data[0..4].copy_from_slice(&ASM);
        // Spacecraft ID = 0x0C3 (195) in bits [4*8..4*8+10]
        // byte 4 = 0x30 (top 8 bits: 00110000), byte 5 bits 7-6 = 11
        data[4] = 0x30;
        data[5] = 0xC0;
        let cadu = Cadu { data };
        assert_eq!(cadu.spacecraft_id(), 0xC3);
    }
}
