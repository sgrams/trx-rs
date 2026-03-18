// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Frequency-domain downsampling via IFFT.
//!
//! Given the full-rate raw audio, this module computes a single forward FFT of
//! the entire buffer, then for each candidate frequency extracts a narrow band
//! around that frequency, applies a spectral window, and inverse-FFTs to produce
//! a complex baseband signal at a reduced sample rate (12000/NDOWN = 1333.3 Hz).

use num_complex::Complex32;
use rustfft::FftPlanner;

use super::{FT2_NDOWN, FT2_SYMBOL_PERIOD_F};

/// Reusable scratch buffers for frequency-domain downsampling.
pub struct DownsampleWorkspace {
    band: Vec<Complex32>,
    ifft_scratch: Vec<Complex32>,
}

impl DownsampleWorkspace {
    fn new(nfft2: usize, ifft_scratch_len: usize) -> Self {
        Self {
            band: vec![Complex32::new(0.0, 0.0); nfft2],
            ifft_scratch: vec![Complex32::new(0.0, 0.0); ifft_scratch_len],
        }
    }

    fn prepare(&mut self, nfft2: usize, ifft_scratch_len: usize) {
        if self.band.len() != nfft2 {
            self.band.resize(nfft2, Complex32::new(0.0, 0.0));
        } else {
            self.band.fill(Complex32::new(0.0, 0.0));
        }

        if self.ifft_scratch.len() != ifft_scratch_len {
            self.ifft_scratch
                .resize(ifft_scratch_len, Complex32::new(0.0, 0.0));
        }
    }
}

/// Downsample context holding precomputed FFT data and spectral window.
pub struct DownsampleContext {
    /// Number of raw samples.
    nraw: usize,
    /// Length of the downsampled FFT (nraw / NDOWN).
    nfft2: usize,
    /// Frequency resolution of the raw FFT (Hz per bin).
    df: f32,
    /// Spectral extraction window (length nfft2).
    window: Vec<f32>,
    /// Full spectrum of the raw audio (nraw/2 + 1 complex bins).
    spectrum: Vec<Complex32>,
    /// IFFT plan for the downsampled length.
    ifft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    /// Scratch length required by the IFFT plan.
    ifft_scratch_len: usize,
}

impl DownsampleContext {
    /// Initialize the downsample context by computing the forward FFT of
    /// the raw audio and preparing the spectral window.
    ///
    /// Returns `None` if the raw audio is too short or allocation fails.
    pub fn new(raw_audio: &[f32], sample_rate: f32) -> Option<Self> {
        let nraw = raw_audio.len();
        if nraw == 0 {
            return None;
        }
        let nfft2 = nraw / FT2_NDOWN;
        if nfft2 == 0 {
            return None;
        }

        let df = sample_rate / nraw as f32;

        // Build spectral extraction window
        let mut window = build_spectral_window(nfft2, df);
        let inv_nfft2 = 1.0 / nfft2 as f32;
        for coeff in &mut window {
            *coeff *= inv_nfft2;
        }

        // Forward real FFT of raw audio
        let mut real_planner = realfft::RealFftPlanner::<f32>::new();
        let fft = real_planner.plan_fft_forward(nraw);
        let mut input = fft.make_input_vec();
        let mut output = fft.make_output_vec();
        let mut scratch = fft.make_scratch_vec();

        input.copy_from_slice(raw_audio);
        fft.process_with_scratch(&mut input, &mut output, &mut scratch)
            .ok()?;

        let spectrum = output;

        // IFFT plan for downsampled length
        let mut planner = FftPlanner::<f32>::new();
        let ifft = planner.plan_fft_inverse(nfft2);
        let ifft_scratch_len = ifft.get_inplace_scratch_len();

        Some(Self {
            nraw,
            nfft2,
            df,
            window,
            spectrum,
            ifft,
            ifft_scratch_len,
        })
    }

    /// Number of downsampled output samples.
    pub fn nfft2(&self) -> usize {
        self.nfft2
    }

    /// Create reusable buffers for repeated downsampling with this context.
    pub fn workspace(&self) -> DownsampleWorkspace {
        DownsampleWorkspace::new(self.nfft2, self.ifft_scratch_len)
    }

    /// Downsample the raw audio around `freq_hz`, writing complex baseband
    /// samples into `out`. Returns the number of samples produced.
    pub fn downsample(&self, freq_hz: f32, out: &mut [Complex32]) -> usize {
        let mut workspace = self.workspace();
        self.downsample_with_workspace(freq_hz, out, &mut workspace)
    }

    /// Downsample the raw audio using reusable scratch buffers.
    pub fn downsample_with_workspace(
        &self,
        freq_hz: f32,
        out: &mut [Complex32],
        workspace: &mut DownsampleWorkspace,
    ) -> usize {
        if out.len() < self.nfft2 {
            return 0;
        }

        workspace.prepare(self.nfft2, self.ifft_scratch_len);
        let band = &mut workspace.band;
        let i0 = (freq_hz / self.df).round() as i32;
        let half_nraw = (self.nraw / 2) as i32;

        // DC bin
        if i0 >= 0 && i0 <= half_nraw && (i0 as usize) < self.spectrum.len() {
            band[0] = self.spectrum[i0 as usize];
        }

        // Positive and negative frequency bins
        for i in 1..=(self.nfft2 as i32 / 2) {
            let pos = i0 + i;
            if pos >= 0 && pos <= half_nraw && (pos as usize) < self.spectrum.len() {
                band[i as usize] = self.spectrum[pos as usize];
            }
            let neg = i0 - i;
            if neg >= 0 && neg <= half_nraw && (neg as usize) < self.spectrum.len() {
                band[(self.nfft2 as i32 - i) as usize] = self.spectrum[neg as usize];
            }
        }

        // Apply spectral window
        for i in 0..self.nfft2 {
            band[i] *= self.window[i];
        }

        // Inverse FFT (in-place)
        self.ifft
            .process_with_scratch(band, &mut workspace.ifft_scratch);

        out[..self.nfft2].copy_from_slice(band);
        self.nfft2
    }
}

/// Build the spectral window used during band extraction.
///
/// The window has a raised-cosine transition, a flat passband covering
/// the FT2 signal bandwidth (4 * baud), and is circularly shifted by
/// one baud rate worth of bins.
fn build_spectral_window(nfft2: usize, df: f32) -> Vec<f32> {
    let baud = 1.0 / FT2_SYMBOL_PERIOD_F;
    let iwt = ((0.5 * baud) / df) as usize;
    let iwf = ((4.0 * baud) / df) as usize;
    let iws = (baud / df) as usize;

    let mut window = vec![0.0f32; nfft2];

    if iwt == 0 {
        return window;
    }

    // Raised-cosine leading edge
    for i in 0..iwt.min(nfft2) {
        window[i] = 0.5 * (1.0 + (std::f32::consts::PI * (iwt - 1 - i) as f32 / iwt as f32).cos());
    }

    // Flat passband
    for i in iwt..(iwt + iwf).min(nfft2) {
        window[i] = 1.0;
    }

    // Raised-cosine trailing edge
    for i in (iwt + iwf)..(2 * iwt + iwf).min(nfft2) {
        window[i] =
            0.5 * (1.0 + (std::f32::consts::PI * (i - (iwt + iwf)) as f32 / iwt as f32).cos());
    }

    // Circular shift by iws bins
    if iws > 0 && iws < nfft2 {
        window.rotate_left(iws);
    }

    window
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectral_window_length() {
        let w = build_spectral_window(5000, 12000.0 / 45000.0);
        assert_eq!(w.len(), 5000);
    }

    #[test]
    fn spectral_window_nonnegative() {
        let w = build_spectral_window(5000, 12000.0 / 45000.0);
        for &v in &w {
            assert!(v >= 0.0, "Window value should be non-negative: {}", v);
        }
    }

    #[test]
    fn downsample_context_creation() {
        let raw = vec![0.0f32; 45000];
        let ctx = DownsampleContext::new(&raw, 12000.0);
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        assert_eq!(ctx.nfft2(), 45000 / 9);
    }

    #[test]
    fn downsample_produces_samples() {
        let raw = vec![0.0f32; 45000];
        let ctx = DownsampleContext::new(&raw, 12000.0).unwrap();
        let nfft2 = ctx.nfft2();
        let mut out = vec![Complex32::new(0.0, 0.0); nfft2];
        let n = ctx.downsample(1000.0, &mut out);
        assert_eq!(n, nfft2);
    }

    #[test]
    fn downsample_output_too_small() {
        let raw = vec![0.0f32; 45000];
        let ctx = DownsampleContext::new(&raw, 12000.0).unwrap();
        let mut out = vec![Complex32::new(0.0, 0.0); 10];
        let n = ctx.downsample(1000.0, &mut out);
        assert_eq!(n, 0);
    }

    #[test]
    fn empty_audio_returns_none() {
        let raw: Vec<f32> = Vec::new();
        assert!(DownsampleContext::new(&raw, 12000.0).is_none());
    }
}
