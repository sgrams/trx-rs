// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::num_complex::Complex as FftComplex;
use rustfft::{Fft, FftPlanner};

fn windowed_sinc_coeffs(cutoff_norm: f32, taps: usize) -> Vec<f32> {
    assert!(taps >= 1, "FIR filter must have at least 1 tap");
    let m = (taps - 1) as f32;
    let mut coeffs = Vec::with_capacity(taps);
    for i in 0..taps {
        let x = i as f32 - m / 2.0;
        let sinc = if x == 0.0 {
            2.0 * cutoff_norm
        } else {
            (2.0 * PI * cutoff_norm * x).sin() / (PI * x)
        };
        let window = if taps == 1 {
            1.0
        } else {
            0.5 * (1.0 - (2.0 * PI * i as f32 / m).cos())
        };
        coeffs.push(sinc * window);
    }
    let sum: f32 = coeffs.iter().sum();
    if sum.abs() > 1e-12 {
        let inv = 1.0 / sum;
        for coeff in &mut coeffs {
            *coeff *= inv;
        }
    }
    coeffs
}

/// A simple windowed-sinc FIR low-pass filter (sample-by-sample interface).
///
/// Used only in unit tests. The DSP pipeline uses [`BlockFirFilter`] instead.
pub struct FirFilter {
    coeffs: Vec<f32>,
    state: Vec<f32>,
    pos: usize,
}

impl FirFilter {
    pub fn new(cutoff_norm: f32, taps: usize) -> Self {
        let coeffs = windowed_sinc_coeffs(cutoff_norm, taps);
        let state_len = taps.saturating_sub(1);
        Self {
            coeffs,
            state: vec![0.0; state_len],
            pos: 0,
        }
    }

    pub fn process(&mut self, sample: f32) -> f32 {
        let n = self.state.len();
        if n == 0 {
            return sample * self.coeffs[0];
        }
        self.state[self.pos] = sample;
        self.pos = (self.pos + 1) % n;
        let mut acc = self.coeffs[0] * sample;
        for k in 1..self.coeffs.len() {
            let idx = (self.pos + n - k) % n;
            acc += self.coeffs[k] * self.state[idx];
        }
        acc
    }
}

/// FFT-based overlap-save FIR low-pass filter (block interface).
pub struct BlockFirFilter {
    h_freq: Vec<FftComplex<f32>>,
    overlap: Vec<f32>,
    n_taps: usize,
    fft_size: usize,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    scratch_freq: Vec<FftComplex<f32>>,
}

pub struct BlockFirFilterPair {
    h_freq: Vec<FftComplex<f32>>,
    overlap: Vec<FftComplex<f32>>,
    n_taps: usize,
    fft_size: usize,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    scratch_freq: Vec<FftComplex<f32>>,
}

type FirKernel = (
    Vec<FftComplex<f32>>,
    usize,
    Arc<dyn Fft<f32>>,
    Arc<dyn Fft<f32>>,
);

fn build_fir_kernel(cutoff_norm: f32, taps: usize, block_size: usize) -> FirKernel {
    let coeffs = windowed_sinc_coeffs(cutoff_norm, taps);
    let fft_size = (block_size + taps - 1).next_power_of_two();

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_size);
    let ifft = planner.plan_fft_inverse(fft_size);

    let mut h_buf: Vec<FftComplex<f32>> = coeffs
        .iter()
        .map(|&coeff| FftComplex::new(coeff, 0.0))
        .collect();
    fft.process({
        h_buf.resize(fft_size, FftComplex::new(0.0, 0.0));
        &mut h_buf
    });

    (h_buf, fft_size, fft, ifft)
}

fn mul_freq_domain_scalar(buf: &mut [FftComplex<f32>], h_freq: &[FftComplex<f32>], scale: f32) {
    for (x, &h) in buf.iter_mut().zip(h_freq.iter()) {
        *x = FftComplex::new(
            (x.re * h.re - x.im * h.im) * scale,
            (x.re * h.im + x.im * h.re) * scale,
        );
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn mul_freq_domain_avx2(
    buf: &mut [FftComplex<f32>],
    h_freq: &[FftComplex<f32>],
    scale: f32,
) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let len = buf.len().min(h_freq.len());
    let mut i = 0usize;
    let scale_v = _mm256_set1_ps(scale);
    while i + 4 <= len {
        let x_ptr = buf.as_mut_ptr().add(i) as *mut f32;
        let h_ptr = h_freq.as_ptr().add(i) as *const f32;
        let x_v = _mm256_loadu_ps(x_ptr);
        let h_v = _mm256_loadu_ps(h_ptr);
        let h_re = _mm256_moveldup_ps(h_v);
        let h_im = _mm256_movehdup_ps(h_v);
        let x_swapped = _mm256_permute_ps(x_v, 0xB1);
        let prod = _mm256_addsub_ps(_mm256_mul_ps(x_v, h_re), _mm256_mul_ps(x_swapped, h_im));
        let out = _mm256_mul_ps(prod, scale_v);
        _mm256_storeu_ps(x_ptr, out);
        i += 4;
    }

    mul_freq_domain_scalar(&mut buf[i..len], &h_freq[i..len], scale);
}

fn mul_freq_domain(buf: &mut [FftComplex<f32>], h_freq: &[FftComplex<f32>], scale: f32) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            unsafe {
                mul_freq_domain_avx2(buf, h_freq, scale);
            }
            return;
        }
    }

    mul_freq_domain_scalar(buf, h_freq, scale);
}

impl BlockFirFilter {
    pub fn new(cutoff_norm: f32, taps: usize, block_size: usize) -> Self {
        let taps = taps.max(1);
        let (h_buf, fft_size, fft, ifft) = build_fir_kernel(cutoff_norm, taps, block_size);

        Self {
            h_freq: h_buf,
            overlap: vec![0.0; taps.saturating_sub(1)],
            n_taps: taps,
            fft_size,
            fft,
            ifft,
            scratch_freq: vec![FftComplex::new(0.0, 0.0); fft_size],
        }
    }

    pub fn filter_block_into(&mut self, input: &[f32], output: &mut Vec<f32>) {
        let n_new = input.len();
        let n_overlap = self.n_taps.saturating_sub(1);

        let buf = &mut self.scratch_freq;
        buf.clear();
        buf.reserve(self.fft_size.saturating_sub(buf.capacity()));
        for &sample in &self.overlap {
            buf.push(FftComplex::new(sample, 0.0));
        }
        for &sample in input {
            buf.push(FftComplex::new(sample, 0.0));
        }
        buf.resize(self.fft_size, FftComplex::new(0.0, 0.0));

        self.fft.process(buf);
        mul_freq_domain(buf, &self.h_freq, 1.0 / self.fft_size as f32);
        self.ifft.process(buf);

        let end = (n_overlap + n_new).min(buf.len());
        output.clear();
        output.reserve(n_new.saturating_sub(output.capacity()));
        output.extend(buf[n_overlap..end].iter().map(|sample| sample.re));

        if n_overlap > 0 {
            if n_new >= n_overlap {
                let new_start = n_new - n_overlap;
                self.overlap.copy_from_slice(&input[new_start..]);
            } else {
                let keep_old = n_overlap - n_new;
                self.overlap.copy_within(n_new..n_overlap, 0);
                self.overlap[keep_old..].copy_from_slice(input);
            }
        }
    }

    pub fn filter_block(&mut self, input: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len());
        self.filter_block_into(input, &mut output);
        output
    }
}

impl BlockFirFilterPair {
    pub fn new(cutoff_norm: f32, taps: usize, block_size: usize) -> Self {
        let taps = taps.max(1);
        let (h_buf, fft_size, fft, ifft) = build_fir_kernel(cutoff_norm, taps, block_size);
        Self {
            h_freq: h_buf,
            overlap: vec![FftComplex::new(0.0, 0.0); taps.saturating_sub(1)],
            n_taps: taps,
            fft_size,
            fft,
            ifft,
            scratch_freq: vec![FftComplex::new(0.0, 0.0); fft_size],
        }
    }

    pub fn filter_block_into(
        &mut self,
        input_i: &[f32],
        input_q: &[f32],
        output_i: &mut Vec<f32>,
        output_q: &mut Vec<f32>,
    ) {
        let n_new = input_i.len().min(input_q.len());
        let n_overlap = self.n_taps.saturating_sub(1);

        let buf = &mut self.scratch_freq;
        buf.clear();
        buf.reserve(self.fft_size.saturating_sub(buf.capacity()));
        buf.extend(self.overlap.iter().copied());
        for idx in 0..n_new {
            buf.push(FftComplex::new(input_i[idx], input_q[idx]));
        }
        buf.resize(self.fft_size, FftComplex::new(0.0, 0.0));

        self.fft.process(buf);
        mul_freq_domain(buf, &self.h_freq, 1.0 / self.fft_size as f32);
        self.ifft.process(buf);

        let end = (n_overlap + n_new).min(buf.len());
        output_i.clear();
        output_q.clear();
        output_i.reserve(n_new.saturating_sub(output_i.capacity()));
        output_q.reserve(n_new.saturating_sub(output_q.capacity()));
        for sample in &buf[n_overlap..end] {
            output_i.push(sample.re);
            output_q.push(sample.im);
        }

        if n_overlap > 0 {
            if n_new >= n_overlap {
                let new_start = n_new - n_overlap;
                for (dst, idx) in self.overlap.iter_mut().zip(new_start..n_new) {
                    *dst = FftComplex::new(input_i[idx], input_q[idx]);
                }
            } else {
                let keep_old = n_overlap - n_new;
                self.overlap.copy_within(n_new..n_overlap, 0);
                for (dst, idx) in self.overlap[keep_old..].iter_mut().zip(0..n_new) {
                    *dst = FftComplex::new(input_i[idx], input_q[idx]);
                }
            }
        }
    }
}
