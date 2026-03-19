// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! FT8-specific sync scoring, likelihood extraction, and tone encoding.

use crate::common::constants::*;
use crate::common::crc::ftx_add_crc;
use crate::common::decode::{get_cand_offset, wf_mag_safe, Candidate};
use crate::common::encode::encode174;
use crate::common::monitor::Waterfall;
use crate::common::protocol::*;

/// Compute FT8 sync score for a candidate.
pub(crate) fn ft8_sync_score(wf: &Waterfall, cand: &Candidate) -> i32 {
    let base = get_cand_offset(wf, cand);
    let mut score: i32 = 0;
    let mut num_average: i32 = 0;

    for m in 0..FT8_NUM_SYNC {
        for (k, &sm_val) in FT8_COSTAS_PATTERN.iter().enumerate().take(FT8_LENGTH_SYNC) {
            let block = FT8_SYNC_OFFSET * m + k;
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
            if sm < 7 {
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
            if k + 1 < FT8_LENGTH_SYNC && block_abs + 1 < wf.num_blocks as i32 {
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

/// Extract log-likelihood ratios for FT8 symbols.
pub(crate) fn ft8_extract_likelihood(
    wf: &Waterfall,
    cand: &Candidate,
    log174: &mut [f32; FTX_LDPC_N],
) {
    let base = get_cand_offset(wf, cand);

    for k in 0..FT8_ND {
        let sym_idx = k + if k < 29 { 7 } else { 14 };
        let bit_idx = 3 * k;
        let block = cand.time_offset as i32 + sym_idx as i32;

        if block < 0 || block >= wf.num_blocks as i32 {
            log174[bit_idx] = 0.0;
            log174[bit_idx + 1] = 0.0;
            log174[bit_idx + 2] = 0.0;
        } else {
            let p_offset = base + sym_idx * wf.block_stride;
            ft8_extract_symbol(wf, p_offset, &mut log174[bit_idx..bit_idx + 3]);
        }
    }
}

fn ft8_extract_symbol(wf: &Waterfall, offset: usize, logl: &mut [f32]) {
    let mut s2 = [0.0f32; 8];
    for j in 0..8 {
        s2[j] = wf_mag_safe(wf, offset + FT8_GRAY_MAP[j] as usize).mag;
    }
    logl[0] = max4(s2[4], s2[5], s2[6], s2[7]) - max4(s2[0], s2[1], s2[2], s2[3]);
    logl[1] = max4(s2[2], s2[3], s2[6], s2[7]) - max4(s2[0], s2[1], s2[4], s2[5]);
    logl[2] = max4(s2[1], s2[3], s2[5], s2[7]) - max4(s2[0], s2[2], s2[4], s2[6]);
}

fn max4(a: f32, b: f32, c: f32, d: f32) -> f32 {
    a.max(b).max(c.max(d))
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ft8_encode_deterministic() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x10];
        let mut tones1 = [0u8; FT8_NN];
        let mut tones2 = [0u8; FT8_NN];
        ft8_encode(&payload, &mut tones1);
        ft8_encode(&payload, &mut tones2);
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
