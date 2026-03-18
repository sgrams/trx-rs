// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Pure Rust LDPC decoder for FTx protocols.
//!
//! This is a port of the sum-product and belief-propagation LDPC decoders
//! from ft8_lib's `ldpc.c`. Given a 174-bit codeword as an array of
//! log-likelihood ratios (log(P(x=0)/P(x=1))), returns a corrected 174-bit
//! codeword. The last 87 bits are the systematic plain-text.

use crate::constants::{FTX_LDPC_MN, FTX_LDPC_NM, FTX_LDPC_NUM_ROWS};
use crate::protocol::{FTX_LDPC_M, FTX_LDPC_N};

/// Fast rational approximation of `tanh(x)`, clamped at +/-4.97.
fn fast_tanh(x: f32) -> f32 {
    if x < -4.97f32 {
        return -1.0f32;
    }
    if x > 4.97f32 {
        return 1.0f32;
    }
    let x2 = x * x;
    let a = x * (945.0f32 + x2 * (105.0f32 + x2));
    let b = 945.0f32 + x2 * (420.0f32 + x2 * 15.0f32);
    a / b
}

/// Fast rational approximation of `atanh(x)`.
fn fast_atanh(x: f32) -> f32 {
    let x2 = x * x;
    let a = x * (945.0f32 + x2 * (-735.0f32 + x2 * 64.0f32));
    let b = 945.0f32 + x2 * (-1050.0f32 + x2 * 225.0f32);
    a / b
}

/// Count the number of LDPC parity errors in a 174-bit codeword.
///
/// Returns 0 if all parity checks pass (valid codeword).
pub fn ldpc_check(codeword: &[u8; FTX_LDPC_N]) -> i32 {
    let mut errors = 0i32;
    for m in 0..FTX_LDPC_M {
        let mut x: u8 = 0;
        let num_rows = FTX_LDPC_NUM_ROWS[m] as usize;
        for i in 0..num_rows {
            x ^= codeword[FTX_LDPC_NM[m][i] as usize - 1];
        }
        if x != 0 {
            errors += 1;
        }
    }
    errors
}

/// Sum-product LDPC decoder.
///
/// `codeword` contains 174 log-likelihood ratios (modified in place during
/// decoding). `plain` receives the decoded 174-bit hard decisions (0 or 1).
/// `max_iters` controls how many iterations to attempt.
///
/// Returns the number of remaining parity errors (0 = success).
pub fn ldpc_decode(
    codeword: &mut [f32; FTX_LDPC_N],
    max_iters: usize,
    plain: &mut [u8; FTX_LDPC_N],
) -> i32 {
    // Allocate m[][] and e[][] on the heap (~60 kB each) to avoid stack overflow.
    let mut m_matrix: Vec<Vec<f32>> =
        vec![vec![0.0f32; FTX_LDPC_N]; FTX_LDPC_M];
    let mut e_matrix: Vec<Vec<f32>> =
        vec![vec![0.0f32; FTX_LDPC_N]; FTX_LDPC_M];

    // Initialize m[][] with the channel LLRs.
    for j in 0..FTX_LDPC_M {
        for i in 0..FTX_LDPC_N {
            m_matrix[j][i] = codeword[i];
        }
    }

    let mut min_errors = FTX_LDPC_M as i32;

    for _iter in 0..max_iters {
        // Update e[][] from m[][]
        for j in 0..FTX_LDPC_M {
            let num_rows = FTX_LDPC_NUM_ROWS[j] as usize;
            for ii1 in 0..num_rows {
                let i1 = FTX_LDPC_NM[j][ii1] as usize - 1;
                let mut a = 1.0f32;
                for ii2 in 0..num_rows {
                    let i2 = FTX_LDPC_NM[j][ii2] as usize - 1;
                    if i2 != i1 {
                        a *= fast_tanh(-m_matrix[j][i2] / 2.0f32);
                    }
                }
                e_matrix[j][i1] = -2.0f32 * fast_atanh(a);
            }
        }

        // Hard decisions
        for i in 0..FTX_LDPC_N {
            let mut l = codeword[i];
            for j in 0..3 {
                l += e_matrix[FTX_LDPC_MN[i][j] as usize - 1][i];
            }
            plain[i] = if l > 0.0 { 1 } else { 0 };
        }

        let errors = ldpc_check(plain);
        if errors < min_errors {
            min_errors = errors;
            if errors == 0 {
                break;
            }
        }

        // Update m[][] from e[][]
        for i in 0..FTX_LDPC_N {
            for ji1 in 0..3 {
                let j1 = FTX_LDPC_MN[i][ji1] as usize - 1;
                let mut l = codeword[i];
                for ji2 in 0..3 {
                    if ji1 != ji2 {
                        let j2 = FTX_LDPC_MN[i][ji2] as usize - 1;
                        l += e_matrix[j2][i];
                    }
                }
                m_matrix[j1][i] = l;
            }
        }
    }

    min_errors
}

/// Belief-propagation LDPC decoder.
///
/// `codeword` contains 174 log-likelihood ratios. `plain` receives the
/// decoded 174-bit hard decisions (0 or 1). `max_iters` controls how many
/// iterations to attempt.
///
/// Returns the number of remaining parity errors (0 = success).
pub fn bp_decode(
    codeword: &[f32; FTX_LDPC_N],
    max_iters: usize,
    plain: &mut [u8; FTX_LDPC_N],
) -> i32 {
    let mut tov = [[0.0f32; 3]; FTX_LDPC_N];
    let mut toc = [[0.0f32; 7]; FTX_LDPC_M];

    let mut min_errors = FTX_LDPC_M as i32;

    for _iter in 0..max_iters {
        // Hard decision guess (tov=0 in iter 0)
        let mut plain_sum = 0u32;
        for n in 0..FTX_LDPC_N {
            let sum = codeword[n] + tov[n][0] + tov[n][1] + tov[n][2];
            plain[n] = if sum > 0.0 { 1 } else { 0 };
            plain_sum += plain[n] as u32;
        }

        if plain_sum == 0 {
            // Message converged to all-zeros, which is prohibited.
            break;
        }

        let errors = ldpc_check(plain);
        if errors < min_errors {
            min_errors = errors;
            if errors == 0 {
                break;
            }
        }

        // Send messages from bits to check nodes
        for m in 0..FTX_LDPC_M {
            let num_rows = FTX_LDPC_NUM_ROWS[m] as usize;
            for n_idx in 0..num_rows {
                let n = FTX_LDPC_NM[m][n_idx] as usize - 1;
                let mut tnm = codeword[n];
                for m_idx in 0..3 {
                    if (FTX_LDPC_MN[n][m_idx] as usize - 1) != m {
                        tnm += tov[n][m_idx];
                    }
                }
                toc[m][n_idx] = fast_tanh(-tnm / 2.0);
            }
        }

        // Send messages from check nodes to variable nodes
        for n in 0..FTX_LDPC_N {
            for m_idx in 0..3 {
                let m = FTX_LDPC_MN[n][m_idx] as usize - 1;
                let num_rows = FTX_LDPC_NUM_ROWS[m] as usize;
                let mut tmn = 1.0f32;
                for n_idx in 0..num_rows {
                    if (FTX_LDPC_NM[m][n_idx] as usize - 1) != n {
                        tmn *= toc[m][n_idx];
                    }
                }
                tov[n][m_idx] = -2.0 * fast_atanh(tmn);
            }
        }
    }

    min_errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fast_tanh_clamp() {
        assert_eq!(fast_tanh(-5.0), -1.0);
        assert_eq!(fast_tanh(5.0), 1.0);
    }

    #[test]
    fn test_fast_tanh_zero() {
        assert!((fast_tanh(0.0)).abs() < 1e-6);
    }

    #[test]
    fn test_fast_tanh_approximation() {
        for &x in &[-3.0f32, -1.0, -0.5, 0.5, 1.0, 3.0] {
            let approx = fast_tanh(x);
            let exact = x.tanh();
            assert!(
                (approx - exact).abs() < 0.01,
                "fast_tanh({}) = {}, expected ~{}",
                x,
                approx,
                exact
            );
        }
    }

    #[test]
    fn test_fast_atanh_zero() {
        assert!((fast_atanh(0.0)).abs() < 1e-6);
    }

    #[test]
    fn test_fast_atanh_approximation() {
        for &x in &[-0.5f32, -0.25, 0.25, 0.5] {
            let approx = fast_atanh(x);
            let exact = x.atanh();
            assert!(
                (approx - exact).abs() < 0.05,
                "fast_atanh({}) = {}, expected ~{}",
                x,
                approx,
                exact
            );
        }
    }

    #[test]
    fn test_ldpc_check_all_zeros() {
        // All-zero codeword should pass all parity checks.
        let codeword = [0u8; FTX_LDPC_N];
        assert_eq!(ldpc_check(&codeword), 0);
    }

    #[test]
    fn test_ldpc_check_single_bit_error() {
        // Flipping one bit should cause parity errors.
        let mut codeword = [0u8; FTX_LDPC_N];
        codeword[0] = 1;
        assert!(ldpc_check(&codeword) > 0);
    }

    #[test]
    fn test_ldpc_decode_all_zeros() {
        // Negative LLRs → hard decision 0 for all bits.
        // The all-zeros codeword satisfies all LDPC parity checks.
        let mut codeword = [-10.0f32; FTX_LDPC_N];
        let mut plain = [0u8; FTX_LDPC_N];
        let errors = ldpc_decode(&mut codeword, 20, &mut plain);
        assert_eq!(errors, 0);
        assert!(plain.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_bp_decode_all_ones() {
        // Positive LLRs → hard decision 1 for all bits.
        // All-ones is not a valid codeword, so bp_decode should report errors.
        let codeword = [10.0f32; FTX_LDPC_N];
        let mut plain = [0u8; FTX_LDPC_N];
        let errors = bp_decode(&codeword, 20, &mut plain);
        assert!(errors > 0);
    }
}
