// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use crate::constants::{
    FT4_COSTAS_PATTERN, FT4_GRAY_MAP, FT4_XOR_SEQUENCE, FT8_COSTAS_PATTERN, FT8_GRAY_MAP,
    FTX_LDPC_GENERATOR,
};
use crate::crc::ftx_add_crc;
use crate::protocol::{FT4_NN, FT8_NN, FTX_LDPC_K, FTX_LDPC_K_BYTES, FTX_LDPC_M, FTX_LDPC_N_BYTES};

/// Returns 1 if an odd number of bits are set in `x`, zero otherwise.
fn parity8(x: u8) -> u8 {
    let x = x ^ (x >> 4);
    let x = x ^ (x >> 2);
    let x = x ^ (x >> 1);
    x % 2
}

/// Encode via LDPC a 91-bit message and return a 174-bit codeword.
///
/// The generator matrix has dimensions (83, 91).
/// The code is a (174, 91) regular LDPC code with column weight 3.
///
/// `message` must be at least `FTX_LDPC_K_BYTES` (12) bytes.
/// `codeword` must be at least `FTX_LDPC_N_BYTES` (22) bytes.
fn encode174(message: &[u8], codeword: &mut [u8]) {
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

/// Generate FT8 tone sequence from payload data.
///
/// `payload` is a 10-byte array containing 77 bits of payload data.
/// `tones` is an array of `FT8_NN` (79) bytes to store the generated tones (encoded as 0..7).
///
/// Message structure: S7 D29 S7 D29 S7
pub fn ft8_encode(payload: &[u8], tones: &mut [u8]) {
    let mut a91 = [0u8; FTX_LDPC_K_BYTES];

    // Compute and add CRC at the end of the message
    ftx_add_crc(payload, &mut a91);

    let mut codeword = [0u8; FTX_LDPC_N_BYTES];
    encode174(&a91, &mut codeword);

    let mut mask: u8 = 0x80;
    let mut i_byte: usize = 0;

    for i_tone in 0..FT8_NN {
        if i_tone < 7 {
            tones[i_tone] = FT8_COSTAS_PATTERN[i_tone];
        } else if (36..43).contains(&i_tone) {
            tones[i_tone] = FT8_COSTAS_PATTERN[i_tone - 36];
        } else if (72..79).contains(&i_tone) {
            tones[i_tone] = FT8_COSTAS_PATTERN[i_tone - 72];
        } else {
            // Extract 3 bits from codeword
            let mut bits3: u8 = 0;

            if codeword[i_byte] & mask != 0 {
                bits3 |= 4;
            }
            mask >>= 1;
            if mask == 0 {
                mask = 0x80;
                i_byte += 1;
            }

            if codeword[i_byte] & mask != 0 {
                bits3 |= 2;
            }
            mask >>= 1;
            if mask == 0 {
                mask = 0x80;
                i_byte += 1;
            }

            if codeword[i_byte] & mask != 0 {
                bits3 |= 1;
            }
            mask >>= 1;
            if mask == 0 {
                mask = 0x80;
                i_byte += 1;
            }

            tones[i_tone] = FT8_GRAY_MAP[bits3 as usize];
        }
    }
}

/// Generate FT4 tone sequence from payload data.
///
/// `payload` is a 10-byte array containing 77 bits of payload data.
/// `tones` is an array of `FT4_NN` (105) bytes to store the generated tones (encoded as 0..3).
///
/// The payload is XOR'd with `FT4_XOR_SEQUENCE` before CRC computation to avoid
/// transmitting long runs of zeros when sending CQ messages.
///
/// Message structure: R S4_1 D29 S4_2 D29 S4_3 D29 S4_4 R
pub fn ft4_encode(payload: &[u8], tones: &mut [u8]) {
    let mut payload_xor = [0u8; 10];

    // XOR payload with pseudorandom sequence
    for i in 0..10 {
        payload_xor[i] = payload[i] ^ FT4_XOR_SEQUENCE[i];
    }

    let mut a91 = [0u8; FTX_LDPC_K_BYTES];

    // Compute and add CRC at the end of the message
    ftx_add_crc(&payload_xor, &mut a91);

    let mut codeword = [0u8; FTX_LDPC_N_BYTES];
    encode174(&a91, &mut codeword);

    let mut mask: u8 = 0x80;
    let mut i_byte: usize = 0;

    for i_tone in 0..FT4_NN {
        if i_tone == 0 || i_tone == 104 {
            tones[i_tone] = 0; // R (ramp) symbol
        } else if (1..5).contains(&i_tone) {
            tones[i_tone] = FT4_COSTAS_PATTERN[0][i_tone - 1];
        } else if (34..38).contains(&i_tone) {
            tones[i_tone] = FT4_COSTAS_PATTERN[1][i_tone - 34];
        } else if (67..71).contains(&i_tone) {
            tones[i_tone] = FT4_COSTAS_PATTERN[2][i_tone - 67];
        } else if (100..104).contains(&i_tone) {
            tones[i_tone] = FT4_COSTAS_PATTERN[3][i_tone - 100];
        } else {
            // Extract 2 bits from codeword
            let mut bits2: u8 = 0;

            if codeword[i_byte] & mask != 0 {
                bits2 |= 2;
            }
            mask >>= 1;
            if mask == 0 {
                mask = 0x80;
                i_byte += 1;
            }

            if codeword[i_byte] & mask != 0 {
                bits2 |= 1;
            }
            mask >>= 1;
            if mask == 0 {
                mask = 0x80;
                i_byte += 1;
            }

            tones[i_tone] = FT4_GRAY_MAP[bits2 as usize];
        }
    }
}

/// Generate FT2 tone sequence from payload data.
///
/// FT2 uses the FT4 framing with a doubled symbol rate.
///
/// `payload` is a 10-byte array containing 77 bits of payload data.
/// `tones` is an array of `FT4_NN` (105) bytes to store the generated tones (encoded as 0..3).
pub fn ft2_encode(payload: &[u8], tones: &mut [u8]) {
    ft4_encode(payload, tones);
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
    fn ft8_encode_length() {
        let payload = [0u8; 10];
        let mut tones = [0u8; FT8_NN];
        ft8_encode(&payload, &mut tones);
        assert_eq!(tones.len(), 79);
    }

    #[test]
    fn ft8_encode_costas_sync() {
        let payload = [0u8; 10];
        let mut tones = [0u8; FT8_NN];
        ft8_encode(&payload, &mut tones);

        // Verify the three Costas sync patterns at positions 0..7, 36..43, 72..79
        for i in 0..7 {
            assert_eq!(tones[i], FT8_COSTAS_PATTERN[i], "Costas S1 mismatch at {i}");
            assert_eq!(
                tones[36 + i],
                FT8_COSTAS_PATTERN[i],
                "Costas S2 mismatch at {}",
                36 + i
            );
            assert_eq!(
                tones[72 + i],
                FT8_COSTAS_PATTERN[i],
                "Costas S3 mismatch at {}",
                72 + i
            );
        }
    }

    #[test]
    fn ft8_encode_tones_in_range() {
        let payload = [0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0x0A, 0xB0];
        let mut tones = [0u8; FT8_NN];
        ft8_encode(&payload, &mut tones);

        for (i, &t) in tones.iter().enumerate() {
            assert!(t < 8, "FT8 tone at position {i} out of range: {t}");
        }
    }

    #[test]
    fn ft4_encode_length() {
        let payload = [0u8; 10];
        let mut tones = [0u8; FT4_NN];
        ft4_encode(&payload, &mut tones);
        assert_eq!(tones.len(), 105);
    }

    #[test]
    fn ft4_encode_ramp_symbols() {
        let payload = [0u8; 10];
        let mut tones = [0u8; FT4_NN];
        ft4_encode(&payload, &mut tones);

        assert_eq!(tones[0], 0, "First ramp symbol should be 0");
        assert_eq!(tones[104], 0, "Last ramp symbol should be 0");
    }

    #[test]
    fn ft4_encode_costas_sync() {
        let payload = [0u8; 10];
        let mut tones = [0u8; FT4_NN];
        ft4_encode(&payload, &mut tones);

        // Verify four Costas sync groups
        for i in 0..4 {
            assert_eq!(tones[1 + i], FT4_COSTAS_PATTERN[0][i], "S4_1 at {i}");
        }
        for i in 0..4 {
            assert_eq!(tones[34 + i], FT4_COSTAS_PATTERN[1][i], "S4_2 at {i}");
        }
        for i in 0..4 {
            assert_eq!(tones[67 + i], FT4_COSTAS_PATTERN[2][i], "S4_3 at {i}");
        }
        for i in 0..4 {
            assert_eq!(tones[100 + i], FT4_COSTAS_PATTERN[3][i], "S4_4 at {i}");
        }
    }

    #[test]
    fn ft4_encode_tones_in_range() {
        let payload = [0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0x0A, 0xB0];
        let mut tones = [0u8; FT4_NN];
        ft4_encode(&payload, &mut tones);

        for (i, &t) in tones.iter().enumerate() {
            assert!(t < 4, "FT4 tone at position {i} out of range: {t}");
        }
    }

    #[test]
    fn ft2_encode_matches_ft4() {
        let payload = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x20];
        let mut tones_ft4 = [0u8; FT4_NN];
        let mut tones_ft2 = [0u8; FT4_NN];
        ft4_encode(&payload, &mut tones_ft4);
        ft2_encode(&payload, &mut tones_ft2);
        assert_eq!(tones_ft4, tones_ft2);
    }

    #[test]
    fn ft8_encode_deterministic() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x10];
        let mut tones1 = [0u8; FT8_NN];
        let mut tones2 = [0u8; FT8_NN];
        ft8_encode(&payload, &mut tones1);
        ft8_encode(&payload, &mut tones2);
        assert_eq!(tones1, tones2);
    }

    #[test]
    fn ft4_encode_deterministic() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x10];
        let mut tones1 = [0u8; FT4_NN];
        let mut tones2 = [0u8; FT4_NN];
        ft4_encode(&payload, &mut tones1);
        ft4_encode(&payload, &mut tones2);
        assert_eq!(tones1, tones2);
    }

    #[test]
    fn ft8_encode_different_payloads_differ() {
        let payload1 = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let payload2 = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xF0];
        let mut tones1 = [0u8; FT8_NN];
        let mut tones2 = [0u8; FT8_NN];
        ft8_encode(&payload1, &mut tones1);
        ft8_encode(&payload2, &mut tones2);
        // Data tones should differ (sync tones are the same)
        assert_ne!(tones1, tones2);
    }
}
