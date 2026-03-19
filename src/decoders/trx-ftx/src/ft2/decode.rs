// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! FT2-specific waterfall sync scoring and likelihood extraction.

use num_complex::Complex32;

use crate::common::constants::*;
use crate::common::decode::{get_cand_offset, wf_elem_to_complex, wf_mag_safe, Candidate};
use crate::common::monitor::Waterfall;
use crate::common::protocol::*;

/// Compute FT2 sync score for a candidate (coherent multi-tone).
pub(crate) fn ft2_sync_score(wf: &Waterfall, cand: &Candidate) -> i32 {
    let base = get_cand_offset(wf, cand);
    let mut score_f: f32 = 0.0;
    let mut groups = 0;

    for (m, costas_group) in FT4_COSTAS_PATTERN.iter().enumerate().take(FT2_NUM_SYNC) {
        let mut sum = Complex32::new(0.0, 0.0);
        let mut complete = true;
        for (k, &costas_tone) in costas_group.iter().enumerate().take(FT2_LENGTH_SYNC) {
            let block = 1 + FT2_SYNC_OFFSET * m + k;
            let block_abs = cand.time_offset as i32 + block as i32;
            if block_abs < 0 || block_abs >= wf.num_blocks as i32 {
                complete = false;
                break;
            }
            let sym_offset = base + block * wf.block_stride;
            let tone = costas_tone as usize;
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

/// Extract log-likelihood ratios for FT2 symbols (multi-scale coherent).
pub(crate) fn ft2_extract_likelihood(
    wf: &Waterfall,
    cand: &Candidate,
    log174: &mut [f32; FTX_LDPC_N],
) {
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
        for (tone, symbol_row) in symbols.iter_mut().enumerate().take(4) {
            let elem = *wf_mag_safe(wf, sym_offset + tone);
            symbol_row[frame_sym] = wf_elem_to_complex(elem);
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
