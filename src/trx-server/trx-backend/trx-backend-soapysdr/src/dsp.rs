// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! IQ DSP pipeline: IQ source abstraction, FFT-based FIR low-pass filter,
//! per-channel mixer/decimator/demodulator, and frame accumulator.
//!
//! The FIR filter uses **overlap-save convolution** via `rustfft`, replacing
//! the previous per-sample ring-buffer approach.  For a block of M samples
//! and N taps, direct convolution costs O(N·M) multiply-adds, while the FFT
//! approach costs O(M log M) — a significant saving for the tap counts (64+)
//! and block sizes (4096) used here.

use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

use num_complex::Complex;
use rustfft::num_complex::Complex as FftComplex;
use rustfft::{Fft, FftPlanner};
use tokio::sync::broadcast;
use trx_core::rig::state::RigMode;

use crate::demod::Demodulator;

// ---------------------------------------------------------------------------
// IQ source abstraction
// ---------------------------------------------------------------------------

/// Abstraction over any IQ sample source (real SoapySDR device or mock).
pub trait IqSource: Send + 'static {
    /// Read the next block of IQ samples into `buf`.
    /// Returns the number of samples written, or an error string.
    fn read_into(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, String>;

    /// Returns `true` when `read_into` blocks until samples are ready
    /// (i.e. hardware-backed sources).  The read loop uses this to skip the
    /// extra throttle sleep that is only needed for non-blocking mock sources.
    fn is_blocking(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Mock IQ source
// ---------------------------------------------------------------------------

/// IQ source that produces silence (all zeros). Used when no SDR hardware is present.
pub struct MockIqSource;

impl IqSource for MockIqSource {
    fn read_into(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, String> {
        buf.fill(Complex::new(0.0, 0.0));
        Ok(buf.len())
    }
}

// ---------------------------------------------------------------------------
// Windowed-sinc coefficient generator (shared by FirFilter and BlockFirFilter)
// ---------------------------------------------------------------------------

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
        for c in &mut coeffs {
            *c *= inv;
        }
    }
    coeffs
}

// ---------------------------------------------------------------------------
// FIR low-pass filter (sample-by-sample, kept for unit tests)
// ---------------------------------------------------------------------------

/// A simple windowed-sinc FIR low-pass filter (sample-by-sample interface).
///
/// Used only in unit tests.  The DSP pipeline uses [`BlockFirFilter`] instead.
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

// ---------------------------------------------------------------------------
// Block FIR filter — overlap-save via rustfft
// ---------------------------------------------------------------------------

/// FFT-based overlap-save FIR low-pass filter (block interface).
///
/// For a block of M samples and N taps the direct cost is O(N·M); here it
/// is O(M log M) plus a single coefficient FFT computed once at construction.
///
/// The filter is initialised for a nominal block size of [`IQ_BLOCK_SIZE`].
/// Smaller blocks are handled correctly (they incur a small padding overhead).
pub struct BlockFirFilter {
    /// Frequency-domain filter coefficients (pre-computed, length `fft_size`).
    h_freq: Vec<FftComplex<f32>>,
    /// Overlap buffer: last `n_taps - 1` input samples (zero-initialised).
    overlap: Vec<f32>,
    n_taps: usize,
    fft_size: usize,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
}

impl BlockFirFilter {
    /// Create a new `BlockFirFilter`.
    ///
    /// `cutoff_norm`: normalised cutoff (0.0–0.5), i.e. `cutoff_hz / sample_rate`.
    /// `taps`: number of FIR taps.
    /// `block_size`: expected input block length (used to size the internal FFT).
    pub fn new(cutoff_norm: f32, taps: usize, block_size: usize) -> Self {
        let taps = taps.max(1);
        let coeffs = windowed_sinc_coeffs(cutoff_norm, taps);

        // Choose the smallest power-of-two FFT that fits the overlap-save frame.
        let fft_size = (block_size + taps - 1).next_power_of_two();

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_size);
        let ifft = planner.plan_fft_inverse(fft_size);

        // Pre-compute H(f) = FFT of zero-padded coefficients.
        let mut h_buf: Vec<FftComplex<f32>> = coeffs
            .iter()
            .map(|&c| FftComplex::new(c, 0.0))
            .collect();
        h_buf.resize(fft_size, FftComplex::new(0.0, 0.0));
        fft.process(&mut h_buf);

        Self {
            h_freq: h_buf,
            overlap: vec![0.0; taps.saturating_sub(1)],
            n_taps: taps,
            fft_size,
            fft,
            ifft,
        }
    }

    /// Filter a block of real samples and return filtered samples of the same length.
    ///
    /// Internally performs one forward FFT, a point-wise multiply with the
    /// pre-computed filter response, and one inverse FFT.
    pub fn filter_block(&mut self, input: &[f32]) -> Vec<f32> {
        let n_new = input.len();
        let n_overlap = self.n_taps.saturating_sub(1);

        // Build the time-domain frame: [overlap (N-1)] ++ [new input] ++ [zeros]
        let mut buf: Vec<FftComplex<f32>> = Vec::with_capacity(self.fft_size);
        for &s in &self.overlap {
            buf.push(FftComplex::new(s, 0.0));
        }
        for &s in input {
            buf.push(FftComplex::new(s, 0.0));
        }
        buf.resize(self.fft_size, FftComplex::new(0.0, 0.0));

        // Forward FFT.
        self.fft.process(&mut buf);

        // Point-wise multiply with H(f); fold in the IFFT normalisation here
        // to avoid a second pass.
        let scale = 1.0 / self.fft_size as f32;
        for (x, &h) in buf.iter_mut().zip(self.h_freq.iter()) {
            *x = FftComplex::new(
                (x.re * h.re - x.im * h.im) * scale,
                (x.re * h.im + x.im * h.re) * scale,
            );
        }

        // Inverse FFT.
        self.ifft.process(&mut buf);

        // Extract the valid output: discard the first n_overlap samples.
        let end = (n_overlap + n_new).min(buf.len());
        let output: Vec<f32> = buf[n_overlap..end].iter().map(|s| s.re).collect();

        // Update overlap with the tail of the current input.
        if n_overlap > 0 {
            let keep_old = n_overlap.saturating_sub(n_new);
            let mut new_overlap = Vec::with_capacity(n_overlap);
            if keep_old > 0 {
                let start = self.overlap.len() - keep_old;
                new_overlap.extend_from_slice(&self.overlap[start..]);
            }
            let new_start = n_new.saturating_sub(n_overlap);
            new_overlap.extend_from_slice(&input[new_start..]);
            self.overlap = new_overlap;
        }

        output
    }
}

// ---------------------------------------------------------------------------
// Channel DSP context
// ---------------------------------------------------------------------------

/// Per-channel DSP state: mixer, FFT-FIR, decimator, demodulator, frame accumulator.
pub struct ChannelDsp {
    /// Frequency offset of this channel from the SDR centre (Hz).
    pub channel_if_hz: f64,
    /// Current demodulator (can be swapped via `set_mode`).
    pub demodulator: Demodulator,
    /// FFT-based FIR low-pass filter applied to I component before decimation.
    lpf_i: BlockFirFilter,
    /// FFT-based FIR low-pass filter applied to Q component before decimation.
    lpf_q: BlockFirFilter,
    /// Decimation factor: `sdr_sample_rate / audio_sample_rate`.
    pub decim_factor: usize,
    /// Accumulator for output PCM frames.
    pub frame_buf: Vec<f32>,
    /// Target frame size in samples.
    pub frame_size: usize,
    /// Sender for completed PCM frames.
    pub pcm_tx: broadcast::Sender<Vec<f32>>,
    /// Current oscillator phase (radians).
    pub mixer_phase: f64,
    /// Phase increment per IQ sample.
    pub mixer_phase_inc: f64,
    /// Decimation counter.
    decim_counter: usize,
}

impl ChannelDsp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel_if_hz: f64,
        mode: &RigMode,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        frame_duration_ms: u16,
        audio_bandwidth_hz: u32,
        fir_taps: usize,
        pcm_tx: broadcast::Sender<Vec<f32>>,
    ) -> Self {
        let decim_factor = if audio_sample_rate == 0 || sdr_sample_rate == 0 {
            1
        } else {
            (sdr_sample_rate / audio_sample_rate).max(1) as usize
        };

        let frame_size = if audio_sample_rate == 0 || frame_duration_ms == 0 {
            960
        } else {
            (audio_sample_rate as usize * frame_duration_ms as usize) / 1000
        };

        let cutoff_norm = if sdr_sample_rate == 0 {
            0.1
        } else {
            (audio_bandwidth_hz as f32 / 2.0) / sdr_sample_rate as f32
        }
        .min(0.499);

        let taps = fir_taps.max(1);

        let mixer_phase_inc = if sdr_sample_rate == 0 {
            0.0
        } else {
            2.0 * std::f64::consts::PI * channel_if_hz / sdr_sample_rate as f64
        };

        Self {
            channel_if_hz,
            demodulator: Demodulator::for_mode(mode),
            lpf_i: BlockFirFilter::new(cutoff_norm, taps, IQ_BLOCK_SIZE),
            lpf_q: BlockFirFilter::new(cutoff_norm, taps, IQ_BLOCK_SIZE),
            decim_factor,
            frame_buf: Vec::with_capacity(frame_size * 2),
            frame_size,
            pcm_tx,
            mixer_phase: 0.0,
            mixer_phase_inc,
            decim_counter: 0,
        }
    }

    pub fn set_mode(&mut self, mode: &RigMode) {
        self.demodulator = Demodulator::for_mode(mode);
    }

    /// Process a block of raw IQ samples through the full DSP chain.
    ///
    /// 1. **Batch mixer**: compute the full LO signal for the block at once,
    ///    then multiply element-wise.  The loop body has no cross-iteration
    ///    data dependency so the compiler can auto-vectorise it.
    /// 2. **FFT FIR**: apply the overlap-save FIR to I and Q in one FFT pair
    ///    each instead of N multiplies per sample.
    /// 3. Decimate.
    /// 4. Demodulate.
    /// 5. Emit complete PCM frames.
    pub fn process_block(&mut self, block: &[Complex<f32>]) {
        let n = block.len();
        if n == 0 {
            return;
        }

        // --- 1. Batch mixer -------------------------------------------------
        // Pre-compute I and Q mixer outputs for the whole block.
        // Each iteration is independent → the compiler can vectorise.
        let mut mixed_i = vec![0.0_f32; n];
        let mut mixed_q = vec![0.0_f32; n];

        let phase_start = self.mixer_phase;
        let phase_inc = self.mixer_phase_inc;
        for (idx, &sample) in block.iter().enumerate() {
            let phase = phase_start + idx as f64 * phase_inc;
            let (sin, cos) = phase.sin_cos();
            let lo_re = cos as f32;
            let lo_im = -(sin as f32);
            // mixed = sample * exp(-j*phase) = sample * (cos - j*sin)
            mixed_i[idx] = sample.re * lo_re - sample.im * lo_im;
            mixed_q[idx] = sample.re * lo_im + sample.im * lo_re;
        }
        // Advance phase with wrap to avoid precision loss.
        self.mixer_phase =
            (phase_start + n as f64 * phase_inc).rem_euclid(std::f64::consts::TAU);

        // --- 2. FFT FIR (overlap-save) --------------------------------------
        let filtered_i = self.lpf_i.filter_block(&mixed_i);
        let filtered_q = self.lpf_q.filter_block(&mixed_q);

        // --- 3. Decimate ----------------------------------------------------
        let capacity = n / self.decim_factor + 1;
        let mut decimated: Vec<Complex<f32>> = Vec::with_capacity(capacity);
        for i in 0..n {
            self.decim_counter += 1;
            if self.decim_counter >= self.decim_factor {
                self.decim_counter = 0;
                let fi = filtered_i.get(i).copied().unwrap_or(0.0);
                let fq = filtered_q.get(i).copied().unwrap_or(0.0);
                decimated.push(Complex::new(fi, fq));
            }
        }

        if decimated.is_empty() {
            return;
        }

        // --- 4. Demodulate --------------------------------------------------
        let audio = self.demodulator.demodulate(&decimated);

        // --- 5. Emit complete PCM frames ------------------------------------
        self.frame_buf.extend_from_slice(&audio);
        while self.frame_buf.len() >= self.frame_size {
            let frame: Vec<f32> = self.frame_buf.drain(..self.frame_size).collect();
            let _ = self.pcm_tx.send(frame);
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level pipeline struct
// ---------------------------------------------------------------------------

pub struct SdrPipeline {
    pub pcm_senders: Vec<broadcast::Sender<Vec<f32>>>,
    pub channel_dsps: Vec<Arc<Mutex<ChannelDsp>>>,
    /// Latest FFT magnitude bins (dBFS, FFT-shifted), updated ~10 Hz.
    pub spectrum_buf: Arc<Mutex<Option<Vec<f32>>>>,
    /// SDR capture sample rate, needed by `SoapySdrRig::get_spectrum`.
    pub sdr_sample_rate: u32,
}

impl SdrPipeline {
    pub fn start(
        source: Box<dyn IqSource>,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        frame_duration_ms: u16,
        channels: &[(f64, RigMode, u32, usize)],
    ) -> Self {
        const IQ_BROADCAST_CAPACITY: usize = 64;
        let (iq_tx, _iq_rx) = broadcast::channel::<Vec<Complex<f32>>>(IQ_BROADCAST_CAPACITY);

        const PCM_BROADCAST_CAPACITY: usize = 32;

        let mut pcm_senders = Vec::with_capacity(channels.len());
        let mut channel_dsps: Vec<Arc<Mutex<ChannelDsp>>> = Vec::with_capacity(channels.len());

        for &(channel_if_hz, ref mode, audio_bandwidth_hz, fir_taps) in channels {
            let (pcm_tx, _pcm_rx) = broadcast::channel::<Vec<f32>>(PCM_BROADCAST_CAPACITY);
            let dsp = ChannelDsp::new(
                channel_if_hz,
                mode,
                sdr_sample_rate,
                audio_sample_rate,
                frame_duration_ms,
                audio_bandwidth_hz,
                fir_taps,
                pcm_tx.clone(),
            );
            pcm_senders.push(pcm_tx);
            channel_dsps.push(Arc::new(Mutex::new(dsp)));
        }

        let thread_dsps: Vec<Arc<Mutex<ChannelDsp>>> = channel_dsps.clone();
        let spectrum_buf: Arc<Mutex<Option<Vec<f32>>>> = Arc::new(Mutex::new(None));
        let thread_spectrum_buf = spectrum_buf.clone();

        std::thread::Builder::new()
            .name("sdr-iq-read".to_string())
            .spawn(move || {
                iq_read_loop(source, sdr_sample_rate, thread_dsps, iq_tx, thread_spectrum_buf);
            })
            .expect("failed to spawn sdr-iq-read thread");

        Self {
            pcm_senders,
            channel_dsps,
            spectrum_buf,
            sdr_sample_rate,
        }
    }
}

// ---------------------------------------------------------------------------
// IQ read loop
// ---------------------------------------------------------------------------

pub const IQ_BLOCK_SIZE: usize = 4096;

/// Number of FFT bins for the spectrum display.
const SPECTRUM_FFT_SIZE: usize = 1024;

/// Update the spectrum buffer every this many IQ blocks (~10 Hz at 1.92 MHz / 4096 block).
const SPECTRUM_UPDATE_BLOCKS: usize = 4;

fn iq_read_loop(
    mut source: Box<dyn IqSource>,
    sdr_sample_rate: u32,
    channel_dsps: Vec<Arc<Mutex<ChannelDsp>>>,
    iq_tx: broadcast::Sender<Vec<Complex<f32>>>,
    spectrum_buf: Arc<Mutex<Option<Vec<f32>>>>,
) {
    let mut block = vec![Complex::new(0.0_f32, 0.0_f32); IQ_BLOCK_SIZE];
    let block_duration_ms = if sdr_sample_rate > 0 {
        (IQ_BLOCK_SIZE as f64 / sdr_sample_rate as f64 * 1000.0) as u64
    } else {
        1
    };
    let throttle = !source.is_blocking();

    // Pre-compute Hann window coefficients.
    let hann_window: Vec<f32> = (0..SPECTRUM_FFT_SIZE)
        .map(|i| {
            0.5 * (1.0 - (2.0 * PI * i as f32 / (SPECTRUM_FFT_SIZE - 1) as f32).cos())
        })
        .collect();

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(SPECTRUM_FFT_SIZE);
    let mut spectrum_counter: usize = 0;

    loop {
        let n = match source.read_into(&mut block) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("IQ source read error: {}; retrying", e);
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
        };

        if n == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }

        let samples = &block[..n];

        let _ = iq_tx.send(samples.to_vec());

        for dsp_arc in &channel_dsps {
            match dsp_arc.lock() {
                Ok(mut dsp) => dsp.process_block(samples),
                Err(e) => {
                    tracing::error!("channel DSP mutex poisoned: {}", e);
                }
            }
        }

        // Periodically compute and store a spectrum snapshot.
        spectrum_counter += 1;
        if spectrum_counter >= SPECTRUM_UPDATE_BLOCKS {
            spectrum_counter = 0;
            let take = n.min(SPECTRUM_FFT_SIZE);
            let mut buf: Vec<FftComplex<f32>> = samples[..take]
                .iter()
                .enumerate()
                .map(|(i, s)| FftComplex::new(s.re * hann_window[i], s.im * hann_window[i]))
                .collect();
            buf.resize(SPECTRUM_FFT_SIZE, FftComplex::new(0.0, 0.0));
            fft.process(&mut buf);

            // FFT-shift: rearrange so negative frequencies come first (DC in centre).
            let half = SPECTRUM_FFT_SIZE / 2;
            let bins: Vec<f32> = buf[half..]
                .iter()
                .chain(buf[..half].iter())
                .map(|c| {
                    let mag = (c.re * c.re + c.im * c.im).sqrt() / SPECTRUM_FFT_SIZE as f32;
                    20.0 * (mag.max(1e-10_f32)).log10()
                })
                .collect();

            if let Ok(mut guard) = spectrum_buf.lock() {
                *guard = Some(bins);
            }
        }

        if throttle {
            std::thread::sleep(std::time::Duration::from_millis(block_duration_ms));
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use trx_core::rig::state::RigMode;

    #[test]
    fn mock_iq_source_fills_zeros() {
        let mut src = MockIqSource;
        let mut buf = vec![Complex::new(1.0_f32, 1.0_f32); 64];
        let n = src.read_into(&mut buf).unwrap();
        assert_eq!(n, 64);
        for s in &buf {
            assert_eq!(s.re, 0.0);
            assert_eq!(s.im, 0.0);
        }
    }

    #[test]
    fn fir_filter_dc_passthrough() {
        // A DC signal (constant 1.0) should pass through the LPF.
        let mut fir = FirFilter::new(0.1, 31);
        let mut out = 0.0_f32;
        for _ in 0..100 {
            out = fir.process(1.0);
        }
        assert!(
            (out - 1.0).abs() < 0.05,
            "DC passthrough failed: got {}",
            out
        );
    }

    #[test]
    fn fir_filter_single_tap() {
        let mut fir = FirFilter::new(0.25, 1);
        let out = fir.process(0.5);
        assert!((out - 0.5).abs() < 1e-5, "single-tap output: {}", out);
    }

    #[test]
    fn block_fir_dc_passthrough() {
        let mut fir = BlockFirFilter::new(0.1, 31, 256);
        // Feed several blocks of DC signal; output should settle near 1.0.
        let input = vec![1.0_f32; 256];
        let mut last = vec![];
        for _ in 0..8 {
            last = fir.filter_block(&input);
        }
        let mean: f32 = last.iter().copied().sum::<f32>() / last.len() as f32;
        assert!(
            (mean - 1.0).abs() < 0.05,
            "BlockFirFilter DC passthrough failed: mean={}",
            mean
        );
    }

    #[test]
    fn block_fir_same_length_output() {
        let mut fir = BlockFirFilter::new(0.2, 64, 128);
        let input = vec![0.5_f32; 128];
        let out = fir.filter_block(&input);
        assert_eq!(out.len(), 128, "output length should equal input length");
    }

    #[test]
    fn channel_dsp_processes_silence() {
        let (pcm_tx, _pcm_rx) = broadcast::channel::<Vec<f32>>(8);
        let mut dsp = ChannelDsp::new(
            0.0,
            &RigMode::USB,
            48_000,
            8_000,
            20,
            3000,
            31,
            pcm_tx,
        );
        let block = vec![Complex::new(0.0_f32, 0.0_f32); 4096];
        dsp.process_block(&block);
    }

    #[test]
    fn channel_dsp_set_mode() {
        let (pcm_tx, _) = broadcast::channel::<Vec<f32>>(8);
        let mut dsp = ChannelDsp::new(0.0, &RigMode::USB, 48_000, 8_000, 20, 3000, 31, pcm_tx);
        assert_eq!(dsp.demodulator, Demodulator::Usb);
        dsp.set_mode(&RigMode::FM);
        assert_eq!(dsp.demodulator, Demodulator::Fm);
    }

    #[test]
    fn pipeline_starts_with_mock_source() {
        let pipeline = SdrPipeline::start(
            Box::new(MockIqSource),
            1_920_000,
            48_000,
            20,
            &[(200_000.0, RigMode::USB, 3000, 64)],
        );
        assert_eq!(pipeline.pcm_senders.len(), 1);
        assert_eq!(pipeline.channel_dsps.len(), 1);
    }

    #[test]
    fn pipeline_empty_channels() {
        let pipeline = SdrPipeline::start(Box::new(MockIqSource), 1_920_000, 48_000, 20, &[]);
        assert_eq!(pipeline.pcm_senders.len(), 0);
        assert_eq!(pipeline.channel_dsps.len(), 0);
    }
}
