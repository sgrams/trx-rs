// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Shared LDPC encoding functions used by all FTx protocols.

use super::constants::FTX_LDPC_GENERATOR;
use super::protocol::{FTX_LDPC_K, FTX_LDPC_K_BYTES, FTX_LDPC_M, FTX_LDPC_N_BYTES};

/// Returns 1 if an odd number of bits are set in `x`, zero otherwise.
pub(crate) fn parity8(x: u8) -> u8 {
    let x = x ^ (x >> 4);
    let x = x ^ (x >> 2);
    let x = x ^ (x >> 1);
    x & 1
}

/// Encode via LDPC a 91-bit message and return a 174-bit codeword.
///
/// The generator matrix has dimensions (83, 91).
/// The code is a (174, 91) regular LDPC code with column weight 3.
///
/// `message` must be at least `FTX_LDPC_K_BYTES` (12) bytes.
/// `codeword` must be at least `FTX_LDPC_N_BYTES` (22) bytes.
pub(crate) fn encode174(message: &[u8], codeword: &mut [u8]) {
    // Fill the codeword with message and zeros
    for j in 0..FTX_LDPC_N_BYTES {
        codeword[j] = if j < FTX_LDPC_K_BYTES { message[j] } else { 0 };
    }

    // Compute the byte index and bit mask for the first checksum bit
    let mut col_mask: u8 = 0x80u8 >> (FTX_LDPC_K % 8);
    let mut col_idx: usize = FTX_LDPC_K_BYTES - 1;

    // Compute the LDPC checksum bits and store them in codeword
    for i in 0..FTX_LDPC_M {
        let mut nsum: u8 = 0;
        for j in 0..FTX_LDPC_K_BYTES {
            nsum ^= parity8(message[j] & FTX_LDPC_GENERATOR[i][j]);
        }

        if !nsum.is_multiple_of(2) {
            codeword[col_idx] |= col_mask;
        }

        col_mask >>= 1;
        if col_mask == 0 {
            col_mask = 0x80u8;
            col_idx += 1;
        }
    }
}

/// Encode a packed 91-bit message into a 174-bit codeword (bit array).
///
/// Each element of the returned array is 0 or 1.
/// Uses the same (174, 91) LDPC generator as `encode174`.
#[allow(dead_code)]
pub(crate) fn encode174_to_bits(a91: &[u8; FTX_LDPC_K_BYTES]) -> [u8; super::protocol::FTX_LDPC_N] {
    use super::protocol::FTX_LDPC_N;
    let mut codeword_packed = [0u8; FTX_LDPC_N_BYTES];
    encode174(a91, &mut codeword_packed);

    let mut bits = [0u8; FTX_LDPC_N];
    for i in 0..FTX_LDPC_N {
        bits[i] = (codeword_packed[i / 8] >> (7 - (i % 8))) & 0x01;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity8_basic() {
        assert_eq!(parity8(0x00), 0); // 0 bits set
        assert_eq!(parity8(0x01), 1); // 1 bit set
        assert_eq!(parity8(0x03), 0); // 2 bits set
        assert_eq!(parity8(0x07), 1); // 3 bits set
        assert_eq!(parity8(0xFF), 0); // 8 bits set
        assert_eq!(parity8(0xFE), 1); // 7 bits set
        assert_eq!(parity8(0x80), 1); // 1 bit set
        assert_eq!(parity8(0xA5), 0); // 4 bits set (10100101)
    }

    #[test]
    fn encode174_systematic() {
        // The first K_BYTES of the codeword should match the message
        let message = [0u8; FTX_LDPC_K_BYTES];
        let mut codeword = [0u8; FTX_LDPC_N_BYTES];
        encode174(&message, &mut codeword);

        // All-zero message should produce all-zero codeword
        for byte in &codeword {
            assert_eq!(*byte, 0);
        }
    }

    #[test]
    fn encode174_preserves_message() {
        // The codeword should start with the message bytes (systematic code).
        // Byte 11 shares bits between the last 3 message bits and the first
        // parity bits, so only check bytes 0..10 for exact match.
        let message: [u8; FTX_LDPC_K_BYTES] = [
            0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x22, 0x33, 0x40,
        ];
        let mut codeword = [0u8; FTX_LDPC_N_BYTES];
        encode174(&message, &mut codeword);

        // First 11 bytes are pure message data
        for j in 0..(FTX_LDPC_K_BYTES - 1) {
            assert_eq!(codeword[j], message[j]);
        }
        // Byte 11: top 3 bits are message, lower 5 bits may have parity
        assert_eq!(codeword[11] & 0xE0, message[11] & 0xE0);
    }

    #[test]
    fn encode174_nonzero_parity() {
        // A non-zero message should produce non-zero parity bits
        let message: [u8; FTX_LDPC_K_BYTES] = [
            0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0xE0,
        ];
        let mut codeword = [0u8; FTX_LDPC_N_BYTES];
        encode174(&message, &mut codeword);

        // Parity portion should not be all zeros
        let parity_nonzero = codeword[FTX_LDPC_K_BYTES..FTX_LDPC_N_BYTES]
            .iter()
            .any(|&b| b != 0);
        assert!(
            parity_nonzero,
            "Parity bits should be non-zero for non-zero input"
        );
    }

    #[test]
    fn encode174_to_bits_all_zeros() {
        let a91 = [0u8; FTX_LDPC_K_BYTES];
        let cw = encode174_to_bits(&a91);
        for &b in &cw {
            assert_eq!(b, 0);
        }
    }
}
