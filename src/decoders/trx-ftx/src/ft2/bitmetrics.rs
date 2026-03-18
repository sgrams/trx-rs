// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Per-symbol FFT and multi-scale bit metrics extraction.
//!
//! Takes the downsampled complex signal, computes per-symbol FFTs to extract
//! complex tone amplitudes, and generates bit metrics at three scales:
//! 1-symbol, 2-symbol, and 4-symbol coherent integration.

use num_complex::Complex32;
use rustfft::FftPlanner;

use crate::constants::{FT4_COSTAS_PATTERN, FT4_GRAY_MAP};

use super::{FT2_FRAME_SYMBOLS, FT2_NSS};

/// Extract bit metrics from the downsampled signal region.
///
/// Returns a 2D array of shape `[2 * FT2_FRAME_SYMBOLS][3]` where:
/// - Index 0: 1-symbol scale metric
/// - Index 1: 2-symbol scale metric
/// - Index 2: 4-symbol scale metric
///
/// Returns `None` if the sync quality is too poor (fewer than 4 of 16
/// Costas sync tones decoded correctly).
pub fn extract_bitmetrics_raw(
    signal: &[Complex32],
) -> Option<Vec<[f32; 3]>> {
    let n_metrics = 2 * FT2_FRAME_SYMBOLS;
    let mut bitmetrics = vec![[0.0f32; 3]; n_metrics];

    // Per-symbol FFT to extract complex tone amplitudes
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FT2_NSS);
    let fft_scratch_len = fft.get_inplace_scratch_len();
    let mut scratch = vec![Complex32::new(0.0, 0.0); fft_scratch_len];

    // Complex symbols for each of the 4 tones at each frame symbol
    let mut symbols = vec![[Complex32::new(0.0, 0.0); 4]; FT2_FRAME_SYMBOLS];
    // Magnitude for each tone at each symbol
    let mut s4 = vec![[0.0f32; 4]; FT2_FRAME_SYMBOLS];

    for sym in 0..FT2_FRAME_SYMBOLS {
        let offset = sym * FT2_NSS;
        let mut csymb: Vec<Complex32> = (0..FT2_NSS)
            .map(|i| {
                if offset + i < signal.len() {
                    signal[offset + i]
                } else {
                    Complex32::new(0.0, 0.0)
                }
            })
            .collect();

        fft.process_with_scratch(&mut csymb, &mut scratch);

        for tone in 0..4 {
            if tone < csymb.len() {
                symbols[sym][tone] = csymb[tone];
                s4[sym][tone] = csymb[tone].norm();
            }
        }
    }

    // Sync quality check: verify Costas patterns are detectable
    let mut sync_ok = 0;
    for group in 0..4 {
        let base = group * 33;
        for i in 0..4 {
            if base + i >= FT2_FRAME_SYMBOLS {
                continue;
            }
            let mut best = 0;
            for tone in 1..4 {
                if s4[base + i][tone] > s4[base + i][best] {
                    best = tone;
                }
            }
            if best == FT4_COSTAS_PATTERN[group][i] as usize {
                sync_ok += 1;
            }
        }
    }

    if sync_ok < 4 {
        return None;
    }

    // Precompute one_mask: for each integer 0..255 and bit position 0..7,
    // whether that bit is set.
    let one_mask: Vec<[u8; 8]> = (0..256u16)
        .map(|i| {
            let mut m = [0u8; 8];
            for j in 0..8 {
                m[j] = if (i & (1 << j)) != 0 { 1 } else { 0 };
            }
            m
        })
        .collect();

    // Compute metrics at three scales
    let mut metric1 = vec![0.0f32; n_metrics];
    let mut metric2 = vec![0.0f32; n_metrics];
    let mut metric4 = vec![0.0f32; n_metrics];

    for nseq in 0..3 {
        let nsym = match nseq {
            0 => 1,
            1 => 2,
            _ => 4,
        };
        let nt = 1 << (2 * nsym); // number of tone sequences to enumerate

        let mut ks = 0;
        while ks + nsym <= FT2_FRAME_SYMBOLS {
            // Compute coherent magnitude for each possible tone sequence
            let mut s2 = vec![0.0f32; nt];
            for i in 0..nt {
                let i1 = i / 64;
                let i2 = (i & 63) / 16;
                let i3 = (i & 15) / 4;
                let i4 = i & 3;

                let sum = match nsym {
                    1 => symbols[ks][FT4_GRAY_MAP[i4] as usize],
                    2 => {
                        symbols[ks][FT4_GRAY_MAP[i3] as usize]
                            + symbols[ks + 1][FT4_GRAY_MAP[i4] as usize]
                    }
                    4 => {
                        symbols[ks][FT4_GRAY_MAP[i1] as usize]
                            + symbols[ks + 1][FT4_GRAY_MAP[i2] as usize]
                            + symbols[ks + 2][FT4_GRAY_MAP[i3] as usize]
                            + symbols[ks + 3][FT4_GRAY_MAP[i4] as usize]
                    }
                    _ => Complex32::new(0.0, 0.0),
                };
                s2[i] = sum.norm();
            }

            // Extract bit metrics: for each bit position, find max coherent
            // magnitude with that bit set vs unset
            let ipt = 2 * ks;
            let ibmax: usize = match nsym {
                1 => 1,
                2 => 3,
                4 => 7,
                _ => 0,
            };

            for ib in 0..=ibmax {
                let mut max_one = f32::NEG_INFINITY;
                let mut max_zero = f32::NEG_INFINITY;

                for i in 0..nt {
                    if i < 256 {
                        if one_mask[i][ibmax - ib] != 0 {
                            if s2[i] > max_one {
                                max_one = s2[i];
                            }
                        } else if s2[i] > max_zero {
                            max_zero = s2[i];
                        }
                    }
                }

                let metric_idx = ipt + ib;
                if metric_idx >= n_metrics {
                    continue;
                }

                match nseq {
                    0 => metric1[metric_idx] = max_one - max_zero,
                    1 => metric2[metric_idx] = max_one - max_zero,
                    _ => metric4[metric_idx] = max_one - max_zero,
                }
            }

            ks += nsym;
        }
    }

    // Patch boundary metrics where multi-symbol integration overruns
    if n_metrics >= 206 {
        metric2[204] = metric1[204];
        metric2[205] = metric1[205];
        metric4[200] = metric2[200];
        metric4[201] = metric2[201];
        metric4[202] = metric2[202];
        metric4[203] = metric2[203];
        metric4[204] = metric1[204];
        metric4[205] = metric1[205];
    }

    // Normalize each metric scale independently
    normalize_metric(&mut metric1);
    normalize_metric(&mut metric2);
    normalize_metric(&mut metric4);

    // Pack into output
    for i in 0..n_metrics {
        bitmetrics[i][0] = metric1[i];
        bitmetrics[i][1] = metric2[i];
        bitmetrics[i][2] = metric4[i];
    }

    Some(bitmetrics)
}

/// Normalize a metric array by dividing by its standard deviation.
fn normalize_metric(metric: &mut [f32]) {
    let count = metric.len();
    if count == 0 {
        return;
    }

    let mut sum = 0.0f32;
    let mut sum2 = 0.0f32;
    for &v in metric.iter() {
        sum += v;
        sum2 += v * v;
    }

    let mean = sum / count as f32;
    let variance = (sum2 / count as f32) - (mean * mean);
    let sigma = if variance > 0.0 {
        variance.sqrt()
    } else {
        (sum2 / count as f32).max(0.0).sqrt()
    };

    if sigma <= 1e-6 {
        return;
    }

    for v in metric.iter_mut() {
        *v /= sigma;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_metric_zeros() {
        let mut m = vec![0.0f32; 100];
        normalize_metric(&mut m);
        for &v in &m {
            assert_eq!(v, 0.0);
        }
    }

    #[test]
    fn normalize_metric_uniform() {
        let mut m = vec![1.0f32; 100];
        normalize_metric(&mut m);
        // All values are the same so variance is zero, sigma will be computed
        // from sum2/n which is 1.0, so sigma=1.0 and values remain 1.0
        for &v in &m {
            assert!((v - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn normalize_metric_nonzero() {
        let mut m: Vec<f32> = (0..100).map(|i| (i as f32 - 50.0) * 0.1).collect();
        normalize_metric(&mut m);
        // After normalization, standard deviation should be ~1.0
        let mean: f32 = m.iter().sum::<f32>() / m.len() as f32;
        let variance: f32 =
            m.iter().map(|&v| (v - mean) * (v - mean)).sum::<f32>() / m.len() as f32;
        assert!(
            (variance - 1.0).abs() < 0.1,
            "Normalized variance should be ~1.0, got {}",
            variance
        );
    }

    #[test]
    fn extract_bitmetrics_silent_signal() {
        let signal = vec![Complex32::new(0.0, 0.0); FT2_FRAME_SYMBOLS * FT2_NSS];
        // Silent signal: all tones have zero magnitude, so the "best tone"
        // defaults to tone 0 for every symbol. When tone 0 happens to match
        // the Costas pattern (which it does for some groups), sync_ok may
        // reach >= 4. So a silent signal can still pass the sync quality
        // gate — the important thing is it does not panic.
        let _result = extract_bitmetrics_raw(&signal);
    }

    #[test]
    fn frame_symbols_constant() {
        // FT2_NN=105, FT2_NR=2 => FT2_FRAME_SYMBOLS=103
        assert_eq!(FT2_FRAME_SYMBOLS, 103);
    }

    #[test]
    fn nss_constant() {
        // FT2_NSTEP=288, FT2_NDOWN=9 => FT2_NSS=32
        assert_eq!(FT2_NSS, 32);
    }
}
