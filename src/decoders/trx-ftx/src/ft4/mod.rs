// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! FT4-specific sync scoring, likelihood extraction, and tone encoding.

use crate::common::constants::*;
use crate::common::crc::ftx_add_crc;
use crate::common::decode::{get_cand_offset, wf_mag_safe, Candidate};
use crate::common::encode::encode174;
use crate::common::monitor::Waterfall;
use crate::common::protocol::*;

/// Compute FT4 sync score for a candidate.
pub(crate) fn ft4_sync_score(wf: &Waterfall, cand: &Candidate) -> i32 {
    let base = get_cand_offset(wf, cand);
    let mut score: i32 = 0;
    let mut num_average: i32 = 0;

    for (m, costas_group) in FT4_COSTAS_PATTERN.iter().enumerate().take(FT4_NUM_SYNC) {
        for (k, &sm_val) in costas_group.iter().enumerate().take(FT4_LENGTH_SYNC) {
            let block = 1 + FT4_SYNC_OFFSET * m + k;
            let block_abs = cand.time_offset as i32 + block as i32;
            if block_abs < 0 {
                continue;
            }
            if block_abs >= wf.num_blocks as i32 {
                break;
            }

            let p_offset = base + block * wf.block_stride;
            let sm = sm_val as usize;

            if sm > 0 {
                let a = wf_mag_safe(wf, p_offset + sm).mag_int();
                let b = wf_mag_safe(wf, p_offset + sm - 1).mag_int();
                score += a - b;
                num_average += 1;
            }
            if sm < 3 {
                let a = wf_mag_safe(wf, p_offset + sm).mag_int();
                let b = wf_mag_safe(wf, p_offset + sm + 1).mag_int();
                score += a - b;
                num_average += 1;
            }
            if k > 0 && block_abs > 0 {
                let a = wf_mag_safe(wf, p_offset + sm).mag_int();
                let b_idx = (p_offset + sm).wrapping_sub(wf.block_stride);
                let b = if b_idx < wf.mag.len() {
                    wf.mag[b_idx].mag_int()
                } else {
                    0
                };
                score += a - b;
                num_average += 1;
            }
            if k + 1 < FT4_LENGTH_SYNC && block_abs + 1 < wf.num_blocks as i32 {
                let a = wf_mag_safe(wf, p_offset + sm).mag_int();
                let b = wf_mag_safe(wf, p_offset + sm + wf.block_stride).mag_int();
                score += a - b;
                num_average += 1;
            }
        }
    }

    if num_average > 0 {
        score / num_average
    } else {
        0
    }
}

/// Extract log-likelihood ratios for FT4 symbols.
pub(crate) fn ft4_extract_likelihood(
    wf: &Waterfall,
    cand: &Candidate,
    log174: &mut [f32; FTX_LDPC_N],
) {
    let base = get_cand_offset(wf, cand);

    for k in 0..FT4_ND {
        let sym_idx = k + if k < 29 {
            5
        } else if k < 58 {
            9
        } else {
            13
        };
        let bit_idx = 2 * k;
        let block = cand.time_offset as i32 + sym_idx as i32;

        if block < 0 || block >= wf.num_blocks as i32 {
            log174[bit_idx] = 0.0;
            log174[bit_idx + 1] = 0.0;
        } else {
            let p_offset = base + sym_idx * wf.block_stride;
            ft4_extract_symbol(wf, p_offset, &mut log174[bit_idx..bit_idx + 2]);
        }
    }
}

fn ft4_extract_symbol(wf: &Waterfall, offset: usize, logl: &mut [f32]) {
    let mut s2 = [0.0f32; 4];
    for j in 0..4 {
        s2[j] = wf_mag_safe(wf, offset + FT4_GRAY_MAP[j] as usize).mag;
    }
    logl[0] = s2[2].max(s2[3]) - s2[0].max(s2[1]);
    logl[1] = s2[1].max(s2[3]) - s2[0].max(s2[2]);
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ft4_encode_deterministic() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x10];
        let mut tones1 = [0u8; FT4_NN];
        let mut tones2 = [0u8; FT4_NN];
        ft4_encode(&payload, &mut tones1);
        ft4_encode(&payload, &mut tones2);
        assert_eq!(tones1, tones2);
    }
}
