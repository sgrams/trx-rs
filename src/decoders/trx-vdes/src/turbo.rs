// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Turbo FEC decoder for VDES TER-MCS-1 (100 kHz channel).
//!
//! ITU-R M.2092-1 specifies a turbo code consisting of two 8-state Recursive
//! Systematic Convolutional (RSC) encoders with feedback polynomial 013 (octal)
//! and feedforward polynomial 015 (octal), connected through a Quadratic
//! Permutation Polynomial (QPP) interleaver.
//!
//! The encoder produces systematic bits plus two parity streams which are
//! punctured to achieve rate 1/2.  This module implements:
//!
//! - QPP interleaver generation
//! - BCJR (MAP) component decoder with log-domain arithmetic
//! - Iterative turbo decoding with configurable iteration count
//! - Puncture pattern handling for rate 1/2

/// Number of turbo decoder iterations.
const TURBO_ITERATIONS: usize = 8;

/// RSC constraint length K=4 → 8 states.
const NUM_STATES: usize = 8;

/// Tail bits per constituent encoder (K-1 = 3).
const TAIL_BITS: usize = 3;

/// RSC feedback polynomial (octal 013 = binary 001011 → decimal 11).
/// g_fb(D) = 1 + D + D^3
const FB_POLY: u8 = 0o13; // 0b001_011

/// RSC feedforward polynomial (octal 015 = binary 001101 → decimal 13).
/// g_ff(D) = 1 + D^2 + D^3
const FF_POLY: u8 = 0o15; // 0b001_101

/// Log-likelihood ratio type (soft bit representation).
type Llr = f32;

/// Large magnitude used as "infinity" in log-domain computations.
const LLR_INF: Llr = 1.0e6;

/// QPP interleaver: π(i) = (f1 * i + f2 * i^2) mod K
///
/// ITU-R M.2092-1 Table A2-5 defines QPP parameters for various block sizes.
/// This function returns the interleaver permutation vector for a given
/// information block size.
pub fn qpp_interleaver(block_size: usize) -> Vec<usize> {
    let (f1, f2) = qpp_parameters(block_size);
    let mut perm = Vec::with_capacity(block_size);
    for i in 0..block_size {
        let idx = ((f1 as u64 * i as u64 + f2 as u64 * (i as u64 * i as u64)) % block_size as u64)
            as usize;
        perm.push(idx);
    }
    perm
}

/// QPP parameter lookup for VDE-TER block sizes.
///
/// Parameters (f1, f2) are chosen per ITU-R M.2092-1 Table A2-5 so that
/// the permutation polynomial generates a valid interleaver (all indices
/// are unique).  For block sizes not in the table, we use a best-effort
/// selection.
fn qpp_parameters(block_size: usize) -> (usize, usize) {
    match block_size {
        // TER-MCS-1.100: 936 info bits (1872 coded / 2 = 936)
        936 => (11, 156),
        // TER-MCS-1.50: 468 info bits
        468 => (11, 156),
        // TER-MCS-2.100: higher MCS, 1872 info bits
        1872 => (11, 156),
        // TER-MCS-3.100: 2808 info bits
        2808 => (11, 156),
        // Generic fallback: search for valid QPP parameters.
        _ => find_qpp_params(block_size),
    }
}

/// Search for valid QPP parameters for a given block size.
///
/// Tests (f1, f2) pairs to find one that produces a valid permutation
/// (all indices unique).
fn find_qpp_params(block_size: usize) -> (usize, usize) {
    if block_size <= 1 {
        return (1, 0);
    }
    // Try even f2 values with various f1
    for f2 in (2..block_size).step_by(2) {
        for f1 in 1..block_size {
            if is_valid_qpp(block_size, f1, f2) {
                return (f1, f2);
            }
        }
    }
    // Last resort: simple coprime interleaver (f2=0)
    let f1 = find_coprime(block_size);
    (f1, 0)
}

fn is_valid_qpp(block_size: usize, f1: usize, f2: usize) -> bool {
    let mut seen = vec![false; block_size];
    for i in 0..block_size {
        let idx = ((f1 as u64 * i as u64 + f2 as u64 * (i as u64 * i as u64))
            % block_size as u64) as usize;
        if seen[idx] {
            return false;
        }
        seen[idx] = true;
    }
    true
}

/// Find a value coprime to n (for fallback interleaver).
fn find_coprime(n: usize) -> usize {
    if n <= 1 {
        return 1;
    }
    for candidate in (1..n).rev() {
        if gcd(candidate, n) == 1 {
            return candidate;
        }
    }
    1
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Depuncture rate-1/2 turbo-coded stream.
///
/// ITU-R M.2092-1 rate-1/2 puncture pattern for TER-MCS-1:
/// - Even positions: systematic + parity1 (encoder 1 output)
/// - Odd positions: systematic + parity2 (encoder 2 output)
///
/// The transmitted stream alternates: [sys, p1, sys, p2, sys, p1, sys, p2, ...]
///
/// Input: received LLRs (positive = likely 0, negative = likely 1)
/// Output: (systematic, parity1, parity2) LLR vectors
pub fn depuncture_rate_half(received_llrs: &[Llr], info_len: usize) -> (Vec<Llr>, Vec<Llr>, Vec<Llr>) {
    let mut systematic = vec![0.0; info_len];
    let mut parity1 = vec![0.0; info_len];
    let mut parity2 = vec![0.0; info_len];

    // Rate 1/2: for each info bit, we have 2 coded bits.
    // Puncture pattern: [sys_i, p1_i] for even i, [sys_i, p2_i] for odd i
    // This means parity1 is available for even indices, parity2 for odd.
    let mut rx_idx = 0;
    for k in 0..info_len {
        if rx_idx < received_llrs.len() {
            systematic[k] = received_llrs[rx_idx];
            rx_idx += 1;
        }
        if k % 2 == 0 {
            // Parity from encoder 1
            if rx_idx < received_llrs.len() {
                parity1[k] = received_llrs[rx_idx];
                rx_idx += 1;
            }
            // Parity2 is punctured (erasure = 0.0 LLR, no information)
        } else {
            // Parity from encoder 2
            if rx_idx < received_llrs.len() {
                parity2[k] = received_llrs[rx_idx];
                rx_idx += 1;
            }
            // Parity1 is punctured
        }
    }

    (systematic, parity1, parity2)
}

/// Convert hard bits (0/1) to LLRs.
///
/// Uses a fixed reliability magnitude.  0 → +RELIABILITY, 1 → -RELIABILITY.
pub fn hard_bits_to_llr(bits: &[u8]) -> Vec<Llr> {
    const RELIABILITY: Llr = 2.0;
    bits.iter()
        .map(|&b| if b == 0 { RELIABILITY } else { -RELIABILITY })
        .collect()
}

/// Main turbo decoder entry point.
///
/// Takes the received coded bits (hard decision), the information block
/// length, and returns decoded information bits + a confidence metric.
///
/// Returns `(decoded_bits, avg_reliability)` where avg_reliability is the
/// mean absolute LLR of the final decisions (higher = more confident).
pub fn turbo_decode(coded_bits: &[u8], info_len: usize) -> (Vec<u8>, f32) {
    let received_llrs = hard_bits_to_llr(coded_bits);
    turbo_decode_soft(&received_llrs, info_len)
}

/// Soft-input turbo decoder.
pub fn turbo_decode_soft(received_llrs: &[Llr], info_len: usize) -> (Vec<u8>, f32) {
    if info_len == 0 {
        return (Vec::new(), 0.0);
    }

    let interleaver = qpp_interleaver(info_len);
    debug_assert_eq!(
        interleaver.len(),
        info_len,
        "interleaver length must equal info_len"
    );
    let deinterleaver = invert_permutation(&interleaver);
    debug_assert_eq!(
        deinterleaver.len(),
        info_len,
        "deinterleaver length must equal info_len"
    );

    let (sys_llr, par1_llr, par2_llr) = depuncture_rate_half(received_llrs, info_len);

    // Interleaved systematic bits for decoder 2
    let sys_interleaved: Vec<Llr> = interleaver.iter().map(|&i| sys_llr[i]).collect();

    // Extrinsic information passed between decoders
    let mut extrinsic_1_to_2 = vec![0.0_f32; info_len];
    let mut extrinsic_2_to_1 = vec![0.0_f32; info_len];

    let mut final_llr = vec![0.0_f32; info_len];

    for _iter in 0..TURBO_ITERATIONS {
        // --- Decoder 1 (natural order) ---
        let apriori_1: Vec<Llr> = deinterleaver
            .iter()
            .map(|&i| extrinsic_2_to_1[i])
            .collect();
        let aposteriori_1 = bcjr_decode(&sys_llr, &par1_llr, &apriori_1);
        // Extrinsic = aposteriori - systematic - apriori
        for k in 0..info_len {
            extrinsic_1_to_2[k] = aposteriori_1[k] - sys_llr[k] - apriori_1[k];
        }

        // --- Decoder 2 (interleaved order) ---
        let apriori_2: Vec<Llr> = interleaver
            .iter()
            .map(|&i| extrinsic_1_to_2[i])
            .collect();
        let aposteriori_2 = bcjr_decode(&sys_interleaved, &par2_llr, &apriori_2);
        for k in 0..info_len {
            extrinsic_2_to_1[k] = aposteriori_2[k] - sys_interleaved[k] - apriori_2[k];
        }

        // Combine for final decision (deinterleave decoder 2 output)
        for k in 0..info_len {
            let deint_apost2 = aposteriori_2[deinterleaver[k]];
            final_llr[k] = sys_llr[k] + extrinsic_1_to_2[k] + deint_apost2
                - sys_llr[k]
                - extrinsic_1_to_2[k];
            // Simplified: final = systematic + extrinsic from both decoders
            final_llr[k] = sys_llr[k] + apriori_1[k] + (aposteriori_1[k] - sys_llr[k] - apriori_1[k]);
        }
    }

    // Final decision: combine all information
    for k in 0..info_len {
        let apriori_1: Llr = if let Some(&di) = deinterleaver.get(k) {
            extrinsic_2_to_1[di]
        } else {
            0.0
        };
        let aposteriori_1 = sys_llr[k] + apriori_1 + extrinsic_1_to_2[k];
        final_llr[k] = aposteriori_1;
    }

    let decoded: Vec<u8> = final_llr
        .iter()
        .map(|&llr| if llr >= 0.0 { 0 } else { 1 })
        .collect();

    let avg_reliability = if info_len > 0 {
        final_llr.iter().map(|l: &f32| l.abs()).sum::<f32>() / info_len as f32
    } else {
        0.0
    };

    (decoded, avg_reliability)
}

/// Invert a permutation vector.
fn invert_permutation(perm: &[usize]) -> Vec<usize> {
    let mut inv = vec![0usize; perm.len()];
    for (i, &p) in perm.iter().enumerate() {
        if p < inv.len() {
            inv[p] = i;
        }
    }
    inv
}

/// BCJR (MAP) decoder for a single RSC constituent encoder.
///
/// Inputs:
/// - `systematic`: channel LLRs for systematic bits
/// - `parity`: channel LLRs for parity bits
/// - `apriori`: a priori LLRs (extrinsic from other decoder)
///
/// Returns: a posteriori LLRs for each information bit.
#[allow(clippy::needless_range_loop)]
fn bcjr_decode(systematic: &[Llr], parity: &[Llr], apriori: &[Llr]) -> Vec<Llr> {
    let n = systematic.len();
    if n == 0 {
        return Vec::new();
    }

    let total_len = n + TAIL_BITS;

    // Extend parity for tail section
    let mut par_ext = vec![0.0_f32; total_len];
    par_ext[..parity.len().min(total_len)].copy_from_slice(&parity[..parity.len().min(total_len)]);

    // --- Forward recursion (alpha) ---
    // alpha[t][s] = log P(state_t = s, y_1..t)
    let mut alpha = vec![vec![-LLR_INF; NUM_STATES]; total_len + 1];
    alpha[0][0] = 0.0; // Start in state 0

    for t in 0..total_len {
        let sys_llr = if t < n {
            systematic[t] + apriori.get(t).copied().unwrap_or(0.0)
        } else {
            0.0 // Tail: force to zero state
        };

        for s in 0..NUM_STATES {
            if alpha[t][s] <= -LLR_INF + 1.0 {
                continue;
            }
            for input in 0..=1u8 {
                let (next_state, parity_bit) = rsc_transition(s, input);
                let sys_metric = if input == 0 {
                    sys_llr / 2.0
                } else {
                    -sys_llr / 2.0
                };
                let par_metric = if parity_bit == 0 {
                    par_ext[t] / 2.0
                } else {
                    -par_ext[t] / 2.0
                };
                let branch = sys_metric + par_metric;
                alpha[t + 1][next_state] = log_sum_exp(alpha[t + 1][next_state], alpha[t][s] + branch);
            }
        }
    }

    // --- Backward recursion (beta) ---
    let mut beta = vec![vec![-LLR_INF; NUM_STATES]; total_len + 1];
    beta[total_len][0] = 0.0; // End in state 0 (after tail)

    for t in (0..total_len).rev() {
        let sys_llr = if t < n {
            systematic[t] + apriori.get(t).copied().unwrap_or(0.0)
        } else {
            0.0
        };

        for s in 0..NUM_STATES {
            for input in 0..=1u8 {
                let (next_state, parity_bit) = rsc_transition(s, input);
                if beta[t + 1][next_state] <= -LLR_INF + 1.0 {
                    continue;
                }
                let sys_metric = if input == 0 {
                    sys_llr / 2.0
                } else {
                    -sys_llr / 2.0
                };
                let par_metric = if parity_bit == 0 {
                    par_ext[t] / 2.0
                } else {
                    -par_ext[t] / 2.0
                };
                let branch = sys_metric + par_metric;
                beta[t][s] = log_sum_exp(beta[t][s], beta[t + 1][next_state] + branch);
            }
        }
    }

    // --- LLR computation ---
    let mut output_llr = vec![0.0_f32; n];
    for t in 0..n {
        let sys_llr_t = systematic[t] + apriori.get(t).copied().unwrap_or(0.0);
        let mut prob_0 = -LLR_INF;
        let mut prob_1 = -LLR_INF;

        for s in 0..NUM_STATES {
            if alpha[t][s] <= -LLR_INF + 1.0 {
                continue;
            }
            for input in 0..=1u8 {
                let (next_state, parity_bit) = rsc_transition(s, input);
                if beta[t + 1][next_state] <= -LLR_INF + 1.0 {
                    continue;
                }
                let sys_metric = if input == 0 {
                    sys_llr_t / 2.0
                } else {
                    -sys_llr_t / 2.0
                };
                let par_metric = if parity_bit == 0 {
                    par_ext[t] / 2.0
                } else {
                    -par_ext[t] / 2.0
                };
                let gamma = sys_metric + par_metric;
                let metric = alpha[t][s] + gamma + beta[t + 1][next_state];

                if input == 0 {
                    prob_0 = log_sum_exp(prob_0, metric);
                } else {
                    prob_1 = log_sum_exp(prob_1, metric);
                }
            }
        }

        output_llr[t] = prob_0 - prob_1;
    }

    output_llr
}

/// RSC encoder state transition.
///
/// Given current state and input bit, returns (next_state, parity_output).
///
/// The RSC encoder uses:
/// - Feedback polynomial: g_fb = 1 + D + D^3 (octal 013)
/// - Feedforward polynomial: g_ff = 1 + D^2 + D^3 (octal 015)
///
/// State is the shift register content (3 bits for K=4).
fn rsc_transition(state: usize, input: u8) -> (usize, u8) {
    let s = state as u8;

    // Feedback: XOR of input with feedback taps
    let feedback = input ^ parity_of(s & (FB_POLY >> 1));

    // New state: shift in the feedback bit
    let next_state = (((s << 1) | feedback) & 0x07) as usize;

    // Parity output: feedforward taps applied to new register contents
    let reg_with_input = (feedback << 3) | s;
    let parity = parity_of(reg_with_input & FF_POLY);

    (next_state, parity)
}

/// Compute parity (XOR of all set bits) of a byte value.
fn parity_of(val: u8) -> u8 {
    (val.count_ones() as u8) & 1
}

/// Numerically stable log-sum-exp: log(exp(a) + exp(b)).
///
/// Uses the Jacobian logarithm approximation for speed, with a correction
/// table for improved accuracy.
fn log_sum_exp(a: Llr, b: Llr) -> Llr {
    if a <= -LLR_INF + 1.0 {
        return b;
    }
    if b <= -LLR_INF + 1.0 {
        return a;
    }
    let max = a.max(b);
    let diff = (a - b).abs();
    // Correction term: log(1 + exp(-|diff|))
    let correction = if diff > 5.0 {
        0.0
    } else {
        (1.0 + (-diff).exp()).ln()
    };
    max + correction
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qpp_interleaver_is_valid_permutation() {
        for &size in &[468, 936, 1872] {
            let perm = qpp_interleaver(size);
            assert_eq!(perm.len(), size);
            let mut seen = vec![false; size];
            for &idx in &perm {
                assert!(idx < size, "index {} out of range for size {}", idx, size);
                assert!(!seen[idx], "duplicate index {} for size {}", idx, size);
                seen[idx] = true;
            }
        }
    }

    #[test]
    fn rsc_transition_state_zero_input_zero() {
        let (next, par) = rsc_transition(0, 0);
        assert_eq!(next, 0);
        assert_eq!(par, 0);
    }

    #[test]
    fn rsc_transition_all_states_valid() {
        for state in 0..NUM_STATES {
            for input in 0..=1u8 {
                let (next, par) = rsc_transition(state, input);
                assert!(next < NUM_STATES);
                assert!(par <= 1);
            }
        }
    }

    #[test]
    fn turbo_decode_all_zeros() {
        let info_len = 40;
        // Encode all-zeros: systematic=0, parity=0 for both encoders
        let coded_len = info_len * 2;
        let coded_bits = vec![0u8; coded_len];
        let (decoded, reliability) = turbo_decode(&coded_bits, info_len);
        assert_eq!(decoded.len(), info_len);
        // All-zeros input should decode to all zeros
        assert!(decoded.iter().all(|&b| b == 0), "decoded: {:?}", decoded);
        assert!(reliability > 0.0);
    }

    #[test]
    fn turbo_decode_handles_empty() {
        let (decoded, reliability) = turbo_decode(&[], 0);
        assert!(decoded.is_empty());
        assert_eq!(reliability, 0.0);
    }

    #[test]
    fn log_sum_exp_correctness() {
        let a = 2.0f32;
        let b = 3.0f32;
        let expected = (a.exp() + b.exp()).ln();
        let result = log_sum_exp(a, b);
        assert!((result - expected).abs() < 0.01, "got {}, expected {}", result, expected);
    }

    #[test]
    fn invert_permutation_round_trips() {
        let perm = qpp_interleaver(40);
        let inv = invert_permutation(&perm);
        for (i, &p) in perm.iter().enumerate() {
            assert_eq!(inv[p], i);
        }
    }

    #[test]
    fn depuncture_produces_correct_lengths() {
        let info_len = 100;
        let coded = vec![0u8; info_len * 2];
        let llrs = hard_bits_to_llr(&coded);
        let (sys, p1, p2) = depuncture_rate_half(&llrs, info_len);
        assert_eq!(sys.len(), info_len);
        assert_eq!(p1.len(), info_len);
        assert_eq!(p2.len(), info_len);
    }
}
