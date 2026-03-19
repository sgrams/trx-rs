// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
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

const N_METRICS: usize = 2 * FT2_FRAME_SYMBOLS;

/// Reusable FFT plans and scratch buffers for bit-metric extraction.
pub struct BitMetricsWorkspace {
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    scratch: Vec<Complex32>,
    symbols: [[Complex32; 4]; FT2_FRAME_SYMBOLS],
    s4: [[f32; 4]; FT2_FRAME_SYMBOLS],
    metric1: [f32; N_METRICS],
    metric2: [f32; N_METRICS],
    metric4: [f32; N_METRICS],
    bitmetrics: [[f32; 3]; N_METRICS],
    csymb: [Complex32; FT2_NSS],
}

impl BitMetricsWorkspace {
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FT2_NSS);
        let scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];

        Self {
            fft,
            scratch,
            symbols: [[Complex32::new(0.0, 0.0); 4]; FT2_FRAME_SYMBOLS],
            s4: [[0.0; 4]; FT2_FRAME_SYMBOLS],
            metric1: [0.0; N_METRICS],
            metric2: [0.0; N_METRICS],
            metric4: [0.0; N_METRICS],
            bitmetrics: [[0.0; 3]; N_METRICS],
            csymb: [Complex32::new(0.0, 0.0); FT2_NSS],
        }
    }

    /// Extract bit metrics into a reusable internal buffer.
    pub fn extract<'a>(&'a mut self, signal: &[Complex32]) -> Option<&'a [[f32; 3]]> {
        self.metric1.fill(0.0);
        self.metric2.fill(0.0);
        self.metric4.fill(0.0);

        for sym in 0..FT2_FRAME_SYMBOLS {
            let offset = sym * FT2_NSS;
            if offset + FT2_NSS <= signal.len() {
                self.csymb
                    .copy_from_slice(&signal[offset..(offset + FT2_NSS)]);
            } else {
                self.csymb.fill(Complex32::new(0.0, 0.0));
                let remaining = signal.len().saturating_sub(offset);
                self.csymb[..remaining].copy_from_slice(&signal[offset..(offset + remaining)]);
            }

            self.fft
                .process_with_scratch(&mut self.csymb, &mut self.scratch);

            for tone in 0..4 {
                let symbol = self.csymb[tone];
                self.symbols[sym][tone] = symbol;
                self.s4[sym][tone] = symbol.norm();
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
                    if self.s4[base + i][tone] > self.s4[base + i][best] {
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

        for nseq in 0..3 {
            let (nsym, metric): (usize, &mut [f32; N_METRICS]) = match nseq {
                0 => (1, &mut self.metric1),
                1 => (2, &mut self.metric2),
                _ => (4, &mut self.metric4),
            };
            let nt = 1usize << (2 * nsym);
            let ibmax = match nsym {
                1 => 1,
                2 => 3,
                4 => 7,
                _ => 0,
            };

            let mut ks = 0;
            while ks + nsym <= FT2_FRAME_SYMBOLS {
                let mut max_one = [f32::NEG_INFINITY; 8];
                let mut max_zero = [f32::NEG_INFINITY; 8];

                for i in 0..nt {
                    let sum = match nsym {
                        1 => self.symbols[ks][FT4_GRAY_MAP[i & 0x03] as usize],
                        2 => {
                            self.symbols[ks][FT4_GRAY_MAP[(i >> 2) & 0x03] as usize]
                                + self.symbols[ks + 1][FT4_GRAY_MAP[i & 0x03] as usize]
                        }
                        4 => {
                            self.symbols[ks][FT4_GRAY_MAP[(i >> 6) & 0x03] as usize]
                                + self.symbols[ks + 1][FT4_GRAY_MAP[(i >> 4) & 0x03] as usize]
                                + self.symbols[ks + 2][FT4_GRAY_MAP[(i >> 2) & 0x03] as usize]
                                + self.symbols[ks + 3][FT4_GRAY_MAP[i & 0x03] as usize]
                        }
                        _ => Complex32::new(0.0, 0.0),
                    };
                    let coherent = sum.norm_sqr();

                    for ib in 0..=ibmax {
                        if ((i >> (ibmax - ib)) & 1) != 0 {
                            max_one[ib] = max_one[ib].max(coherent);
                        } else {
                            max_zero[ib] = max_zero[ib].max(coherent);
                        }
                    }
                }

                let ipt = 2 * ks;
                for ib in 0..=ibmax {
                    let metric_idx = ipt + ib;
                    if metric_idx < N_METRICS {
                        metric[metric_idx] = max_one[ib] - max_zero[ib];
                    }
                }

                ks += nsym;
            }
        }

        // Patch boundary metrics where multi-symbol integration overruns
        self.metric2[204] = self.metric1[204];
        self.metric2[205] = self.metric1[205];
        self.metric4[200] = self.metric2[200];
        self.metric4[201] = self.metric2[201];
        self.metric4[202] = self.metric2[202];
        self.metric4[203] = self.metric2[203];
        self.metric4[204] = self.metric1[204];
        self.metric4[205] = self.metric1[205];

        normalize_metric(&mut self.metric1);
        normalize_metric(&mut self.metric2);
        normalize_metric(&mut self.metric4);

        for i in 0..N_METRICS {
            self.bitmetrics[i][0] = self.metric1[i];
            self.bitmetrics[i][1] = self.metric2[i];
            self.bitmetrics[i][2] = self.metric4[i];
        }

        Some(&self.bitmetrics)
    }
}

impl Default for BitMetricsWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract bit metrics from the downsampled signal region.
///
/// Returns a 2D array of shape `[2 * FT2_FRAME_SYMBOLS][3]` where:
/// - Index 0: 1-symbol scale metric
/// - Index 1: 2-symbol scale metric
/// - Index 2: 4-symbol scale metric
///
/// Returns `None` if the sync quality is too poor (fewer than 4 of 16
/// Costas sync tones decoded correctly).
pub fn extract_bitmetrics_raw(signal: &[Complex32]) -> Option<Vec<[f32; 3]>> {
    let mut workspace = BitMetricsWorkspace::new();
    workspace
        .extract(signal)
        .map(|bitmetrics| bitmetrics.to_vec())
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
