// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! CRC-16 for VDES link-layer frames.
//!
//! ITU-R M.2092-1 uses the same CRC-16-CCITT polynomial (0x1021) as AIS,
//! applied over the decoded information bits (excluding FEC tail).  The CRC
//! is transmitted MSB-first in the encoded frame.

/// Pre-computed CRC-16-CCITT lookup table (normal / MSB-first form,
/// polynomial 0x1021).
const CRC16_CCITT_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = (i as u16) << 8;
        let mut j = 0;
        while j < 8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Compute CRC-16-CCITT over a byte slice (MSB-first, init 0xFFFF).
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc = (crc << 8) ^ CRC16_CCITT_TABLE[((crc >> 8) ^ b as u16) as usize];
    }
    crc ^ 0xFFFF
}

/// Compute CRC-16-CCITT over a bit slice (MSB-first packing).
///
/// Packs the bit slice into bytes (zero-padding the last byte if needed),
/// then runs the CRC over the packed data.
pub fn crc16_ccitt_bits(bits: &[u8]) -> u16 {
    let bytes = pack_bits_to_bytes(bits);
    crc16_ccitt(&bytes)
}

/// Check CRC-16-CCITT on a decoded bit-stream.
///
/// The last 16 bits of `bits` are the transmitted CRC.  Returns `true` if
/// the CRC computed over the preceding bits matches the received CRC.
pub fn check_crc16(bits: &[u8]) -> bool {
    if bits.len() < 16 {
        return false;
    }
    let payload_bits = &bits[..bits.len() - 16];
    let crc_bits = &bits[bits.len() - 16..];

    let computed = crc16_ccitt_bits(payload_bits);
    let received = bits_to_u16(crc_bits);

    computed == received
}

/// Extract the 16-bit CRC value from a bit slice.
fn bits_to_u16(bits: &[u8]) -> u16 {
    let mut value = 0u16;
    for &bit in bits.iter().take(16) {
        value = (value << 1) | u16::from(bit & 1);
    }
    value
}

/// Pack a bit slice into bytes (MSB-first, zero-pad last byte).
fn pack_bits_to_bytes(bits: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(bits.len().div_ceil(8));
    for chunk in bits.chunks(8) {
        let mut byte = 0u8;
        for (i, &bit) in chunk.iter().enumerate() {
            byte |= (bit & 1) << (7 - i);
        }
        bytes.push(byte);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_known_vector() {
        // CRC-16-CCITT (init=0xFFFF, poly=0x1021, xorout=0xFFFF) of "123456789"
        let data = b"123456789";
        let crc = crc16_ccitt(data);
        assert_eq!(crc, 0xD64E, "CRC-16-CCITT of '123456789'");
    }

    #[test]
    fn crc16_bits_matches_bytes() {
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        let crc_bytes = crc16_ccitt(&data);

        let bits: Vec<u8> = data
            .iter()
            .flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1))
            .collect();
        let crc_bits = crc16_ccitt_bits(&bits);
        assert_eq!(crc_bytes, crc_bits);
    }

    #[test]
    fn check_crc16_valid() {
        let payload = [0x01, 0x02, 0x03, 0x04];
        let crc = crc16_ccitt(&payload);
        let mut bits: Vec<u8> = payload
            .iter()
            .flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1))
            .collect();
        for i in (0..16).rev() {
            bits.push(((crc >> i) & 1) as u8);
        }
        assert!(check_crc16(&bits));
    }

    #[test]
    fn check_crc16_invalid() {
        let payload = [0x01, 0x02, 0x03, 0x04];
        let mut bits: Vec<u8> = payload
            .iter()
            .flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1))
            .collect();
        // Append wrong CRC
        for _ in 0..16 {
            bits.push(0);
        }
        assert!(!check_crc16(&bits));
    }

    #[test]
    fn pack_bits_round_trips() {
        let original = [0xAB, 0xCD];
        let bits: Vec<u8> = original
            .iter()
            .flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1))
            .collect();
        let packed = pack_bits_to_bytes(&bits);
        assert_eq!(packed, original);
    }
}
