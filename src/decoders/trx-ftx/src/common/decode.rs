// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Candidate search, shared decode helpers, and dispatcher functions for FTx decoding.
//!
//! Ports `decode.c` from ft8_lib.

#[cfg(feature = "ft2")]
use num_complex::Complex32;

use super::constants::*;
use super::monitor::{Waterfall, WfElem};
use super::protocol::*;

/// Candidate position in time and frequency.
#[derive(Clone, Copy, Default)]
pub struct Candidate {
    pub score: i16,
    pub time_offset: i16,
    pub freq_offset: i16,
    pub time_sub: u8,
    pub freq_sub: u8,
}

/// Decode status information.
#[derive(Default)]
pub struct DecodeStatus {
    pub ldpc_errors: i32,
    pub crc_extracted: u16,
    pub crc_calculated: u16,
}

/// Message payload (77 bits packed into 10 bytes) with dedup hash.
#[derive(Clone, Default)]
pub struct FtxMessage {
    pub payload: [u8; FTX_PAYLOAD_LENGTH_BYTES],
    pub hash: u16,
}

#[cfg(feature = "ft2")]
pub(crate) fn wf_elem_to_complex(elem: WfElem) -> Complex32 {
    Complex32::new(elem.re, elem.im)
}

pub(crate) fn get_cand_offset(wf: &Waterfall, cand: &Candidate) -> usize {
    let offset = cand.time_offset as isize;
    let offset = offset * wf.time_osr as isize + cand.time_sub as isize;
    let offset = offset * wf.freq_osr as isize + cand.freq_sub as isize;
    let offset = offset * wf.num_bins as isize + cand.freq_offset as isize;
    offset.max(0) as usize
}

// Default element for out-of-bounds waterfall access
pub(crate) static DEFAULT_WF_ELEM: WfElem = WfElem {
    mag: -120.0,
    phase: 0.0,
    re: 0.0,
    im: 0.0,
};

pub(crate) fn wf_mag_safe(wf: &Waterfall, idx: usize) -> &WfElem {
    if idx < wf.mag.len() {
        &wf.mag[idx]
    } else {
        &DEFAULT_WF_ELEM
    }
}

/// Min-heap operations for candidate list.
fn heapify_down(heap: &mut [Candidate], size: usize) {
    let mut current = 0;
    loop {
        let left = 2 * current + 1;
        let right = left + 1;
        let mut smallest = current;
        if left < size && heap[left].score < heap[smallest].score {
            smallest = left;
        }
        if right < size && heap[right].score < heap[smallest].score {
            smallest = right;
        }
        if smallest == current {
            break;
        }
        heap.swap(current, smallest);
        current = smallest;
    }
}

fn heapify_up(heap: &mut [Candidate], size: usize) {
    let mut current = size - 1;
    while current > 0 {
        let parent = (current - 1) / 2;
        if heap[current].score >= heap[parent].score {
            break;
        }
        heap.swap(current, parent);
        current = parent;
    }
}

/// Find candidate signals in the waterfall. Returns sorted candidates (best first).
pub fn ftx_find_candidates(
    wf: &Waterfall,
    max_candidates: usize,
    min_score: i32,
) -> Vec<Candidate> {
    #[cfg(feature = "ft2")]
    let is_ft2 = wf.protocol == FtxProtocol::Ft2;
    #[cfg(not(feature = "ft2"))]
    let is_ft2 = false;
    let num_tones = if wf.protocol.uses_ft4_layout() { 4 } else { 8 };

    let (time_offset_min, time_offset_max) = if is_ft2 {
        #[cfg(feature = "ft2")]
        {
            let max = (wf.num_blocks as i32 - FT2_NN as i32 + 2).max(-1);
            (-2i16, max as i16)
        }
        #[cfg(not(feature = "ft2"))]
        unreachable!()
    } else if wf.protocol == FtxProtocol::Ft4 {
        let max = (wf.num_blocks as i32 - FT4_NN as i32 + 34).max(-33);
        (-34i16, max as i16)
    } else {
        (-10i16, 20i16)
    };

    let mut heap = vec![Candidate::default(); max_candidates];
    let mut heap_size = 0;

    for time_sub in 0..wf.time_osr as u8 {
        for freq_sub in 0..wf.freq_osr as u8 {
            let mut time_offset = time_offset_min;
            while time_offset < time_offset_max {
                let mut freq_offset: i16 = 0;
                while (freq_offset as usize + num_tones - 1) < wf.num_bins {
                    let cand = Candidate {
                        score: 0,
                        time_offset,
                        freq_offset,
                        time_sub,
                        freq_sub,
                    };

                    let score = if is_ft2 {
                        #[cfg(feature = "ft2")]
                        {
                            crate::ft2::ft2_sync_score(wf, &cand)
                        }
                        #[cfg(not(feature = "ft2"))]
                        unreachable!()
                    } else if wf.protocol.uses_ft4_layout() {
                        crate::ft4::ft4_sync_score(wf, &cand)
                    } else {
                        crate::ft8::ft8_sync_score(wf, &cand)
                    };

                    if score >= min_score {
                        if heap_size == max_candidates && score > heap[0].score as i32 {
                            heap_size -= 1;
                            heap[0] = heap[heap_size];
                            heapify_down(&mut heap, heap_size);
                        }
                        if heap_size < max_candidates {
                            heap[heap_size] = Candidate {
                                score: score as i16,
                                time_offset,
                                freq_offset,
                                time_sub,
                                freq_sub,
                            };
                            heap_size += 1;
                            heapify_up(&mut heap, heap_size);
                        }
                    }

                    freq_offset += 1;
                }
                time_offset += 1;
            }
        }
    }

    // Sort by descending score (heap sort)
    let mut len_unsorted = heap_size;
    while len_unsorted > 1 {
        heap.swap(0, len_unsorted - 1);
        len_unsorted -= 1;
        heapify_down(&mut heap, len_unsorted);
    }

    heap.truncate(heap_size);
    heap
}

/// Verify CRC of a 174-bit plaintext and build an FtxMessage.
///
/// `plain174`: decoded LDPC codeword (174 bits, each 0 or 1).
/// `uses_xor`: true for FT4/FT2 (apply XOR sequence), false for FT8.
///
/// Returns `None` if CRC check fails.
pub(crate) fn verify_crc_and_build_message(
    plain174: &[u8; FTX_LDPC_N],
    uses_xor: bool,
) -> Option<FtxMessage> {
    let mut a91 = [0u8; FTX_LDPC_K_BYTES];
    pack_bits(plain174, FTX_LDPC_K, &mut a91);

    let a91_orig = a91;
    let crc_extracted = super::crc::ftx_extract_crc(&a91);
    a91[9] &= 0xF8;
    a91[10] = 0x00;
    let crc_calculated = super::crc::ftx_compute_crc(&a91, 96 - 14);

    if crc_extracted != crc_calculated {
        return None;
    }

    let a91 = a91_orig;

    let mut message = FtxMessage {
        hash: crc_calculated,
        payload: [0; FTX_PAYLOAD_LENGTH_BYTES],
    };

    if uses_xor {
        for i in 0..10 {
            message.payload[i] = a91[i] ^ FT4_XOR_SEQUENCE[i];
        }
    } else {
        message.payload[..10].copy_from_slice(&a91[..10]);
    }

    Some(message)
}

/// Normalize log-likelihoods.
fn ftx_normalize_logl(log174: &mut [f32; FTX_LDPC_N]) {
    let mut sum = 0.0f32;
    let mut sum2 = 0.0f32;
    for &v in log174.iter() {
        sum += v;
        sum2 += v * v;
    }
    let inv_n = 1.0 / FTX_LDPC_N as f32;
    let variance = (sum2 - sum * sum * inv_n) * inv_n;
    if variance > 0.0 {
        let norm_factor = (24.0 / variance).sqrt();
        for v in log174.iter_mut() {
            *v *= norm_factor;
        }
    }
}

/// Pack bits into bytes (MSB first).
pub fn pack_bits(bit_array: &[u8], num_bits: usize, packed: &mut [u8]) {
    let num_bytes = num_bits.div_ceil(8);
    for b in packed[..num_bytes].iter_mut() {
        *b = 0;
    }
    let mut mask: u8 = 0x80;
    let mut byte_idx = 0;
    for &bit in bit_array.iter().take(num_bits) {
        if bit != 0 {
            packed[byte_idx] |= mask;
        }
        mask >>= 1;
        if mask == 0 {
            mask = 0x80;
            byte_idx += 1;
        }
    }
}

/// Attempt to decode a candidate. Returns decoded message or None.
pub fn ftx_decode_candidate(
    wf: &Waterfall,
    cand: &Candidate,
    max_iterations: usize,
) -> Option<FtxMessage> {
    let mut log174 = [0.0f32; FTX_LDPC_N];

    #[cfg(feature = "ft2")]
    if wf.protocol == FtxProtocol::Ft2 {
        crate::ft2::ft2_extract_likelihood(wf, cand, &mut log174);
    } else if wf.protocol.uses_ft4_layout() {
        crate::ft4::ft4_extract_likelihood(wf, cand, &mut log174);
    } else {
        crate::ft8::ft8_extract_likelihood(wf, cand, &mut log174);
    }
    #[cfg(not(feature = "ft2"))]
    if wf.protocol.uses_ft4_layout() {
        crate::ft4::ft4_extract_likelihood(wf, cand, &mut log174);
    } else {
        crate::ft8::ft8_extract_likelihood(wf, cand, &mut log174);
    }

    ftx_normalize_logl(&mut log174);

    let mut plain174 = [0u8; FTX_LDPC_N];
    let errors = super::ldpc::bp_decode(&log174, max_iterations, &mut plain174);
    if errors > 0 {
        return None;
    }

    verify_crc_and_build_message(&plain174, wf.protocol.uses_ft4_layout())
}

/// Compute post-decode SNR.
pub fn ftx_post_decode_snr(wf: &Waterfall, cand: &Candidate, message: &FtxMessage) -> f32 {
    let is_ft4 = wf.protocol.uses_ft4_layout();
    let nn = if is_ft4 { FT4_NN } else { FT8_NN };
    let num_tones = if is_ft4 { 4 } else { 8 };

    let mut tones = [0u8; FT4_NN]; // FT4_NN >= FT8_NN
    if is_ft4 {
        crate::ft4::ft4_encode(&message.payload, &mut tones);
    } else {
        crate::ft8::ft8_encode(&message.payload, &mut tones);
    }

    let base = get_cand_offset(wf, cand);
    let mut sum_snr = 0.0f32;
    let mut n_valid = 0;

    for (sym, &tone) in tones.iter().enumerate().take(nn) {
        let block_abs = cand.time_offset as i32 + sym as i32;
        if block_abs < 0 || block_abs >= wf.num_blocks as i32 {
            continue;
        }

        let p_offset = base + sym * wf.block_stride;
        let sig_db = wf_mag_safe(wf, p_offset + tone as usize).mag;

        let mut noise_min = 0.0f32;
        let mut found_noise = false;
        for t in 0..num_tones {
            if t == tone as usize {
                continue;
            }
            let db = wf_mag_safe(wf, p_offset + t).mag;
            if !found_noise || db < noise_min {
                noise_min = db;
                found_noise = true;
            }
        }

        if found_noise {
            sum_snr += sig_db - noise_min;
            n_valid += 1;
        }
    }

    if n_valid == 0 {
        return cand.score as f32 * 0.5 - 29.0;
    }

    let symbol_period = wf.protocol.symbol_period();
    let bw_correction = 10.0 * (2500.0 * symbol_period * wf.freq_osr as f32).log10();
    sum_snr / n_valid as f32 - bw_correction
}
