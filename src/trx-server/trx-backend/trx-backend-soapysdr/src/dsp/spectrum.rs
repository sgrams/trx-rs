// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

use num_complex::Complex;
use rustfft::num_complex::Complex as FftComplex;
use rustfft::FftPlanner;

/// Number of FFT bins for the spectrum display.
pub(super) const SPECTRUM_FFT_SIZE: usize = 1024;

/// Update the spectrum buffer every this many IQ blocks (~10 Hz at 1.92 MHz / 4096 block).
pub(super) const SPECTRUM_UPDATE_BLOCKS: usize = 4;

pub(super) struct SpectrumSnapshotter {
    hann_window: Vec<f32>,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    counter: usize,
}

impl SpectrumSnapshotter {
    pub(super) fn new() -> Self {
        let hann_window: Vec<f32> = (0..SPECTRUM_FFT_SIZE)
            .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / (SPECTRUM_FFT_SIZE - 1) as f32).cos()))
            .collect();

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(SPECTRUM_FFT_SIZE);

        Self {
            hann_window,
            fft,
            counter: 0,
        }
    }

    pub(super) fn update(
        &mut self,
        samples: &[Complex<f32>],
        spectrum_buf: &Arc<Mutex<Option<Vec<f32>>>>,
    ) {
        self.counter += 1;
        if self.counter < SPECTRUM_UPDATE_BLOCKS {
            return;
        }
        self.counter = 0;

        let take = samples.len().min(SPECTRUM_FFT_SIZE);
        let mut buf: Vec<FftComplex<f32>> = samples[..take]
            .iter()
            .enumerate()
            .map(|(i, sample)| {
                FftComplex::new(
                    sample.re * self.hann_window[i],
                    sample.im * self.hann_window[i],
                )
            })
            .collect();
        buf.resize(SPECTRUM_FFT_SIZE, FftComplex::new(0.0, 0.0));
        self.fft.process(&mut buf);

        let half = SPECTRUM_FFT_SIZE / 2;
        let bins: Vec<f32> = buf[half..]
            .iter()
            .chain(buf[..half].iter())
            .map(|value| {
                let mag =
                    (value.re * value.re + value.im * value.im).sqrt() / SPECTRUM_FFT_SIZE as f32;
                20.0 * mag.max(1e-10_f32).log10()
            })
            .collect();

        if let Ok(mut guard) = spectrum_buf.lock() {
            *guard = Some(bins);
        }
    }
}
