// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Candidate search, sync scoring, and likelihood extraction for FTx decoding.
//!
//! Ports `decode.c` from ft8_lib.

use num_complex::Complex32;

use crate::constants::*;
use crate::monitor::{Waterfall, WfElem};
use crate::protocol::*;

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

fn wf_elem_to_complex(elem: WfElem) -> Complex32 {
    let amplitude = 10.0_f32.powf(elem.mag / 20.0);
    Complex32::from_polar(amplitude, elem.phase)
}

fn get_cand_offset(wf: &Waterfall, cand: &Candidate) -> usize {
    let offset = cand.time_offset as isize;
    let offset = offset * wf.time_osr as isize + cand.time_sub as isize;
    let offset = offset * wf.freq_osr as isize + cand.freq_sub as isize;
    let offset = offset * wf.num_bins as isize + cand.freq_offset as isize;
    offset.max(0) as usize
}

fn wf_mag_at(wf: &Waterfall, base: usize, idx: isize) -> &WfElem {
    let i = (base as isize + idx).max(0) as usize;
    if i < wf.mag.len() {
        &wf.mag[i]
    } else {
        &WfElem {
            mag: -120.0,
            phase: 0.0,
        }
    }
}

// Leaked reference for out-of-bounds default
static DEFAULT_WF_ELEM: WfElem = WfElem {
    mag: -120.0,
    phase: 0.0,
};

fn wf_mag_safe(wf: &Waterfall, idx: usize) -> &WfElem {
    if idx < wf.mag.len() {
        &wf.mag[idx]
    } else {
        &DEFAULT_WF_ELEM
    }
}

fn ft8_sync_score(wf: &Waterfall, cand: &Candidate) -> i32 {
    let base = get_cand_offset(wf, cand);
    let mut score: i32 = 0;
    let mut num_average: i32 = 0;

    for m in 0..FT8_NUM_SYNC {
        for k in 0..FT8_LENGTH_SYNC {
            let block = FT8_SYNC_OFFSET * m + k;
            let block_abs = cand.time_offset as i32 + block as i32;
            if block_abs < 0 {
                continue;
            }
            if block_abs >= wf.num_blocks as i32 {
                break;
            }

            let p_offset = base + block * wf.block_stride;
            let sm = FT8_COSTAS_PATTERN[k] as usize;

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

fn ft4_sync_score(wf: &Waterfall, cand: &Candidate) -> i32 {
    let base = get_cand_offset(wf, cand);
    let mut score: i32 = 0;
    let mut num_average: i32 = 0;

    for m in 0..FT4_NUM_SYNC {
        for k in 0..FT4_LENGTH_SYNC {
            let block = 1 + FT4_SYNC_OFFSET * m + k;
            let block_abs = cand.time_offset as i32 + block as i32;
            if block_abs < 0 {
                continue;
            }
            if block_abs >= wf.num_blocks as i32 {
                break;
            }

            let p_offset = base + block * wf.block_stride;
            let sm = FT4_COSTAS_PATTERN[m][k] as usize;

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

fn ft2_sync_score(wf: &Waterfall, cand: &Candidate) -> i32 {
    let base = get_cand_offset(wf, cand);
    let mut score_f: f32 = 0.0;
    let mut groups = 0;

    for m in 0..FT2_NUM_SYNC {
        let mut sum = Complex32::new(0.0, 0.0);
        let mut complete = true;
        for k in 0..FT2_LENGTH_SYNC {
            let block = 1 + FT2_SYNC_OFFSET * m + k;
            let block_abs = cand.time_offset as i32 + block as i32;
            if block_abs < 0 || block_abs >= wf.num_blocks as i32 {
                complete = false;
                break;
            }
            let sym_offset = base + block * wf.block_stride;
            let tone = FT4_COSTAS_PATTERN[m][k] as usize;
            let elem = *wf_mag_safe(wf, sym_offset + tone);
            sum += wf_elem_to_complex(elem);
        }
        if !complete {
            continue;
        }
        score_f += sum.norm();
        groups += 1;
    }

    if groups == 0 {
        return 0;
    }
    (score_f / groups as f32 * 8.0).round() as i32
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
    let is_ft2 = wf.protocol == FtxProtocol::Ft2;
    let num_tones = if wf.protocol.uses_ft4_layout() { 4 } else { 8 };

    let (time_offset_min, time_offset_max) = if is_ft2 {
        let max = (wf.num_blocks as i32 - FT2_NN as i32 + 2).max(-1);
        (-2i16, max as i16)
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
                        ft2_sync_score(wf, &cand)
                    } else if wf.protocol.uses_ft4_layout() {
                        ft4_sync_score(wf, &cand)
                    } else {
                        ft8_sync_score(wf, &cand)
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

/// Extract log-likelihood ratios for FT8 symbols.
fn ft8_extract_likelihood(wf: &Waterfall, cand: &Candidate, log174: &mut [f32; FTX_LDPC_N]) {
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

/// Extract log-likelihood ratios for FT4 symbols.
fn ft4_extract_likelihood(wf: &Waterfall, cand: &Candidate, log174: &mut [f32; FTX_LDPC_N]) {
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

/// Extract log-likelihood ratios for FT2 symbols (multi-scale coherent).
fn ft2_extract_likelihood(wf: &Waterfall, cand: &Candidate, log174: &mut [f32; FTX_LDPC_N]) {
    let base = get_cand_offset(wf, cand);
    let frame_syms = FT2_NN - FT2_NR;

    // Collect complex symbols
    let mut symbols = [[Complex32::new(0.0, 0.0); 103]; 4]; // FT2_NN - FT2_NR = 103
    for frame_sym in 0..frame_syms {
        let sym_idx = frame_sym + 1; // skip ramp-up
        let block = cand.time_offset as i32 + sym_idx as i32;
        if block < 0 || block >= wf.num_blocks as i32 {
            continue;
        }
        let sym_offset = base + sym_idx * wf.block_stride;
        for tone in 0..4 {
            let elem = *wf_mag_safe(wf, sym_offset + tone);
            symbols[tone][frame_sym] = wf_elem_to_complex(elem);
        }
    }

    // Multi-scale metrics
    let mut metric1 = vec![0.0f32; 2 * frame_syms];
    let mut metric2 = vec![0.0f32; 2 * frame_syms];
    let mut metric4 = vec![0.0f32; 2 * frame_syms];

    for start in 0..frame_syms {
        ft2_extract_logl_seq(&symbols, start, 1, &mut metric1[2 * start..]);
    }
    let mut start = 0;
    while start + 1 < frame_syms {
        ft2_extract_logl_seq(&symbols, start, 2, &mut metric2[2 * start..]);
        start += 2;
    }
    start = 0;
    while start + 3 < frame_syms {
        ft2_extract_logl_seq(&symbols, start, 4, &mut metric4[2 * start..]);
        start += 4;
    }

    // Patch boundaries
    if 2 * frame_syms >= 206 {
        metric2[204] = metric1[204];
        metric2[205] = metric1[205];
        metric4[200] = metric2[200];
        metric4[201] = metric2[201];
        metric4[202] = metric2[202];
        metric4[203] = metric2[203];
        metric4[204] = metric1[204];
        metric4[205] = metric1[205];
    }

    // Map to 174 data bits, selecting max-magnitude metric
    for data_sym in 0..FT2_ND {
        let frame_sym = data_sym
            + if data_sym < 29 {
                4
            } else if data_sym < 58 {
                8
            } else {
                12
            };
        let src_bit = 2 * frame_sym;
        let dst_bit = 2 * data_sym;

        for b in 0..2 {
            let a = metric1[src_bit + b];
            let bv = metric2[src_bit + b];
            let c = metric4[src_bit + b];
            log174[dst_bit + b] = if a.abs() >= bv.abs() && a.abs() >= c.abs() {
                a
            } else if bv.abs() >= c.abs() {
                bv
            } else {
                c
            };
        }
    }
}

fn ft2_extract_logl_seq(
    symbols: &[[Complex32; 103]; 4],
    start_sym: usize,
    n_syms: usize,
    metrics: &mut [f32],
) {
    let n_bits = 2 * n_syms;
    let n_sequences = 1 << n_bits;

    for bit in 0..n_bits {
        let mut max_zero = f32::NEG_INFINITY;
        let mut max_one = f32::NEG_INFINITY;
        for seq in 0..n_sequences {
            let mut sum = Complex32::new(0.0, 0.0);
            for sym in 0..n_syms {
                let shift = 2 * (n_syms - sym - 1);
                let dibit = (seq >> shift) & 0x3;
                let tone = FT4_GRAY_MAP[dibit] as usize;
                if start_sym + sym < 103 {
                    sum += symbols[tone][start_sym + sym];
                }
            }
            let strength = sum.norm();
            let mask_bit = n_bits - bit - 1;
            if (seq >> mask_bit) & 1 != 0 {
                if strength > max_one {
                    max_one = strength;
                }
            } else if strength > max_zero {
                max_zero = strength;
            }
        }
        if bit < metrics.len() {
            metrics[bit] = max_one - max_zero;
        }
    }
}

/// Normalize LLR array by dividing by standard deviation, then optionally
/// scaling to a target variance.
///
/// If `target_variance` is `Some(v)`, the output has variance ≈ v.
/// If `None`, the output has unit variance (σ = 1).
pub(crate) fn normalize_llr(log174: &mut [f32; FTX_LDPC_N], target_variance: Option<f32>) {
    let mut sum = 0.0f32;
    let mut sum2 = 0.0f32;
    for &v in log174.iter() {
        sum += v;
        sum2 += v * v;
    }
    let inv_n = 1.0 / FTX_LDPC_N as f32;
    let variance = (sum2 - sum * sum * inv_n) * inv_n;
    if variance <= 1e-12 {
        return;
    }
    let scale = match target_variance {
        Some(tv) => (tv / variance).sqrt(),
        None => 1.0 / variance.sqrt(),
    };
    for v in log174.iter_mut() {
        *v *= scale;
    }
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
    let mut a91 = [0u8; crate::protocol::FTX_LDPC_K_BYTES];
    pack_bits(plain174, crate::protocol::FTX_LDPC_K, &mut a91);

    let crc_extracted = crate::crc::ftx_extract_crc(&a91);
    a91[9] &= 0xF8;
    a91[10] = 0x00;
    let crc_calculated = crate::crc::ftx_compute_crc(&a91, 96 - 14);

    if crc_extracted != crc_calculated {
        return None;
    }

    // Re-read a91 since we modified it for CRC check
    pack_bits(plain174, crate::protocol::FTX_LDPC_K, &mut a91);

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
    for i in 0..num_bits {
        if bit_array[i] != 0 {
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

    if wf.protocol == FtxProtocol::Ft2 {
        ft2_extract_likelihood(wf, cand, &mut log174);
    } else if wf.protocol.uses_ft4_layout() {
        ft4_extract_likelihood(wf, cand, &mut log174);
    } else {
        ft8_extract_likelihood(wf, cand, &mut log174);
    }

    ftx_normalize_logl(&mut log174);

    let mut plain174 = [0u8; FTX_LDPC_N];
    let errors = crate::ldpc::bp_decode(&log174, max_iterations, &mut plain174);
    if errors > 0 {
        return None;
    }

    verify_crc_and_build_message(&plain174, wf.protocol.uses_ft4_layout())
}

fn max4(a: f32, b: f32, c: f32, d: f32) -> f32 {
    a.max(b).max(c.max(d))
}

/// Compute post-decode SNR.
pub fn ftx_post_decode_snr(wf: &Waterfall, cand: &Candidate, message: &FtxMessage) -> f32 {
    let is_ft4 = wf.protocol.uses_ft4_layout();
    let nn = if is_ft4 { FT4_NN } else { FT8_NN };
    let num_tones = if is_ft4 { 4 } else { 8 };

    let mut tones = [0u8; FT4_NN]; // FT4_NN >= FT8_NN
    if is_ft4 {
        crate::encode::ft4_encode(&message.payload, &mut tones);
    } else {
        crate::encode::ft8_encode(&message.payload, &mut tones);
    }

    let base = get_cand_offset(wf, cand);
    let mut sum_snr = 0.0f32;
    let mut n_valid = 0;

    for sym in 0..nn {
        let block_abs = cand.time_offset as i32 + sym as i32;
        if block_abs < 0 || block_abs >= wf.num_blocks as i32 {
            continue;
        }

        let p_offset = base + sym * wf.block_stride;
        let sig_db = wf_mag_safe(wf, p_offset + tones[sym] as usize).mag;

        let mut noise_min = 0.0f32;
        let mut found_noise = false;
        for t in 0..num_tones {
            if t == tones[sym] as usize {
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
