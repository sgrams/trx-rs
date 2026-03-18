// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use crate::protocol::{FT8_CRC_POLYNOMIAL, FT8_CRC_WIDTH};

const TOPBIT: u16 = 1 << (FT8_CRC_WIDTH - 1);

/// Compute 14-bit CRC for a sequence of given number of bits.
/// `message` is a byte sequence (MSB first), `num_bits` is the number of bits.
pub fn ftx_compute_crc(message: &[u8], num_bits: usize) -> u16 {
    let mut remainder: u16 = 0;
    let mut idx_byte: usize = 0;

    for idx_bit in 0..num_bits {
        if idx_bit % 8 == 0 {
            remainder ^= (message[idx_byte] as u16) << (FT8_CRC_WIDTH - 8);
            idx_byte += 1;
        }

        if remainder & TOPBIT != 0 {
            remainder = (remainder << 1) ^ FT8_CRC_POLYNOMIAL;
        } else {
            remainder <<= 1;
        }
    }

    remainder & ((TOPBIT << 1) - 1)
}

/// Extract the FT8/FT4 CRC from a packed 91-bit message.
pub fn ftx_extract_crc(a91: &[u8]) -> u16 {
    ((a91[9] as u16 & 0x07) << 11) | ((a91[10] as u16) << 3) | ((a91[11] as u16) >> 5)
}

/// Add FT8/FT4 CRC to a packed message.
/// `payload` contains 77 bits of payload data, `a91` receives 91 bits (payload + CRC).
pub fn ftx_add_crc(payload: &[u8], a91: &mut [u8]) {
    // Copy 77 bits of payload data
    for i in 0..10 {
        a91[i] = payload[i];
    }

    // Clear 3 bits after the payload to make 82 bits
    a91[9] &= 0xF8;
    a91[10] = 0;

    // Calculate CRC of 82 bits (77 + 5 zeros)
    let checksum = ftx_compute_crc(a91, 96 - 14);

    // Store the CRC at the end of 77 bit message
    a91[9] |= (checksum >> 11) as u8;
    a91[10] = (checksum >> 3) as u8;
    a91[11] = (checksum << 5) as u8;
}

/// Check CRC of a packed 91-bit message. Returns true if valid.
pub fn ftx_check_crc(a91: &[u8; 12]) -> bool {
    let crc_extracted = ftx_extract_crc(a91);
    let mut temp = *a91;
    temp[9] &= 0xF8;
    temp[10] = 0x00;
    let crc_calculated = ftx_compute_crc(&temp, 96 - 14);
    crc_extracted == crc_calculated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_round_trip() {
        let payload: [u8; 10] = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x20];
        let mut a91 = [0u8; 12];
        ftx_add_crc(&payload, &mut a91);
        let crc = ftx_extract_crc(&a91);
        // Verify CRC matches what we computed
        let mut check = a91;
        check[9] &= 0xF8;
        check[10] = 0x00;
        assert_eq!(crc, ftx_compute_crc(&check, 96 - 14));
    }

    #[test]
    fn crc_check() {
        let payload: [u8; 10] = [0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0x0A, 0xB0];
        let mut a91 = [0u8; 12];
        ftx_add_crc(&payload, &mut a91);
        assert!(ftx_check_crc(&a91));
        // Corrupt a bit
        a91[0] ^= 0x01;
        assert!(!ftx_check_crc(&a91));
    }
}
