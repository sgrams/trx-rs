// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! IQ DSP pipeline: IQ source abstraction, FIR low-pass filter,
//! per-channel mixer/decimator/demodulator, and frame accumulator.

use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

use num_complex::Complex;
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
// FIR low-pass filter
// ---------------------------------------------------------------------------

/// A simple windowed-sinc FIR low-pass filter.
///
/// Used for:
/// 1. Pre-decimation anti-aliasing (cutoff = audio_rate / 2 / sdr_rate)
/// 2. Post-demod audio BPF (cutoff = audio_bandwidth_hz / audio_rate)
pub struct FirFilter {
    coeffs: Vec<f32>,
    /// Ring buffer holding the last (coeffs.len() - 1) samples.
    state: Vec<f32>,
    pos: usize,
}

impl FirFilter {
    /// Build a windowed-sinc low-pass FIR.
    ///
    /// `cutoff_norm`: normalised cutoff frequency (0.0–0.5), i.e. `cutoff_hz / sample_rate`.
    /// `taps`: number of coefficients (odd recommended).
    pub fn new(cutoff_norm: f32, taps: usize) -> Self {
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
            // Hann window
            let window = if taps == 1 {
                1.0
            } else {
                0.5 * (1.0 - (2.0 * PI * i as f32 / m).cos())
            };
            coeffs.push(sinc * window);
        }

        // Normalise so sum of coefficients equals 1.0.
        let sum: f32 = coeffs.iter().sum();
        if sum.abs() > 1e-12 {
            let inv = 1.0 / sum;
            for c in &mut coeffs {
                *c *= inv;
            }
        }

        let state_len = taps.saturating_sub(1);
        Self {
            coeffs,
            state: vec![0.0; state_len],
            pos: 0,
        }
    }

    /// Filter a single real sample and return the filtered output.
    pub fn process(&mut self, sample: f32) -> f32 {
        let n = self.state.len();

        if n == 0 {
            // Single-tap: just scale by the one coefficient.
            return sample * self.coeffs[0];
        }

        // Write new sample into ring buffer.
        self.state[self.pos] = sample;
        self.pos = (self.pos + 1) % n;

        // Convolve: coeffs[0] applied to newest sample (before ring write),
        // then work through history.
        let mut acc = self.coeffs[0] * sample;
        for k in 1..self.coeffs.len() {
            // Index into ring buffer going backwards from pos.
            let idx = (self.pos + n - k) % n;
            acc += self.coeffs[k] * self.state[idx];
        }
        acc
    }
}

// ---------------------------------------------------------------------------
// Channel DSP context
// ---------------------------------------------------------------------------

/// Per-channel DSP state: mixer, FIR, decimator, demodulator, frame accumulator.
pub struct ChannelDsp {
    /// Frequency offset of this channel from the SDR centre (Hz).
    /// Already accounts for `center_offset_hz`.
    pub channel_if_hz: f64,
    /// Current demodulator (can be swapped via `set_mode`).
    pub demodulator: Demodulator,
    /// FIR anti-alias filter applied to I component before decimation.
    pub lpf_i: FirFilter,
    /// FIR anti-alias filter applied to Q component before decimation.
    pub lpf_q: FirFilter,
    /// Decimation factor: `sdr_sample_rate / audio_sample_rate`.
    pub decim_factor: usize,
    /// Accumulator for output PCM frames.
    pub frame_buf: Vec<f32>,
    /// Target frame size in samples (`audio_sample_rate * frame_duration_ms / 1000`).
    pub frame_size: usize,
    /// Sender for completed PCM frames.
    pub pcm_tx: broadcast::Sender<Vec<f32>>,
    /// Current oscillator phase (radians) for the complex mixer.
    pub mixer_phase: f64,
    /// Phase increment per IQ sample: `2π * channel_if_hz / sdr_sample_rate`.
    pub mixer_phase_inc: f64,
    /// Decimation counter: counts input samples, fires every `decim_factor` samples.
    decim_counter: usize,
}

impl ChannelDsp {
    /// Construct a new `ChannelDsp`.
    ///
    /// `channel_if_hz`: IF offset within the IQ band (Hz, signed).
    /// `mode`: initial demodulation mode.
    /// `sdr_sample_rate`: IQ capture rate (Hz).
    /// `audio_sample_rate`: output PCM rate (Hz).
    /// `frame_duration_ms`: output frame length (ms).
    /// `audio_bandwidth_hz`: one-sided audio BPF cutoff (Hz).
    /// `fir_taps`: number of FIR taps.
    /// `pcm_tx`: broadcast sender for completed PCM frames.
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

        // Normalised cutoff for the anti-alias LPF: audio_bandwidth_hz / sdr_sample_rate.
        // This keeps only the audio-bandwidth-wide slice of the IQ after mixing.
        let cutoff_norm = if sdr_sample_rate == 0 {
            0.1
        } else {
            (audio_bandwidth_hz as f32 / 2.0) / sdr_sample_rate as f32
        };
        let cutoff_norm = cutoff_norm.min(0.499); // clamp below Nyquist

        let taps = fir_taps.max(1);

        let mixer_phase_inc = if sdr_sample_rate == 0 {
            0.0
        } else {
            2.0 * std::f64::consts::PI * channel_if_hz / sdr_sample_rate as f64
        };

        Self {
            channel_if_hz,
            demodulator: Demodulator::for_mode(mode),
            lpf_i: FirFilter::new(cutoff_norm, taps),
            lpf_q: FirFilter::new(cutoff_norm, taps),
            decim_factor,
            frame_buf: Vec::with_capacity(frame_size * 2),
            frame_size,
            pcm_tx,
            mixer_phase: 0.0,
            mixer_phase_inc,
            decim_counter: 0,
        }
    }

    /// Update the demodulator to match a new mode.
    pub fn set_mode(&mut self, mode: &RigMode) {
        self.demodulator = Demodulator::for_mode(mode);
    }

    /// Process a block of raw IQ samples through the full DSP chain.
    ///
    /// 1. Mix each sample to baseband using the channel oscillator.
    /// 2. Apply LPF to both I and Q.
    /// 3. Decimate by `decim_factor`.
    /// 4. Accumulate decimated samples; when enough are collected, demodulate.
    /// 5. Append demodulated audio to `frame_buf`.
    /// 6. Emit complete frames via `pcm_tx`.
    pub fn process_block(&mut self, block: &[Complex<f32>]) {
        // Pre-allocate a local decimated IQ accumulation buffer.
        let capacity = block.len() / self.decim_factor + 1;
        let mut local_dec: Vec<Complex<f32>> = Vec::with_capacity(capacity);

        for &sample in block {
            // --- 1. Mix to baseband ---
            let (sin, cos) = self.mixer_phase.sin_cos();
            // exp(-j * phase) = cos(phase) - j*sin(phase)
            let lo = Complex::new(cos as f32, -(sin as f32));
            let mixed = sample * lo;

            // Advance mixer phase, wrap at 2π.
            self.mixer_phase += self.mixer_phase_inc;
            if self.mixer_phase >= std::f64::consts::TAU {
                self.mixer_phase -= std::f64::consts::TAU;
            } else if self.mixer_phase < -std::f64::consts::TAU {
                self.mixer_phase += std::f64::consts::TAU;
            }

            // --- 2. Apply LPF to both I and Q ---
            let filtered_i = self.lpf_i.process(mixed.re);
            let filtered_q = self.lpf_q.process(mixed.im);
            let filtered = Complex::new(filtered_i, filtered_q);

            // --- 3. Decimate ---
            self.decim_counter += 1;
            if self.decim_counter >= self.decim_factor {
                self.decim_counter = 0;
                local_dec.push(filtered);
            }
        }

        if local_dec.is_empty() {
            return;
        }

        // --- 4. Demodulate the decimated block ---
        let audio = self.demodulator.demodulate(&local_dec);

        // --- 5. Append to frame buffer ---
        self.frame_buf.extend_from_slice(&audio);

        // --- 6. Emit complete frames ---
        while self.frame_buf.len() >= self.frame_size {
            let frame: Vec<f32> = self.frame_buf.drain(..self.frame_size).collect();
            // Ignore send errors (no active receivers is fine).
            let _ = self.pcm_tx.send(frame);
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level pipeline struct
// ---------------------------------------------------------------------------

/// Handle to the running SDR DSP pipeline.
pub struct SdrPipeline {
    /// One PCM sender per channel (index matches `channels` order in `start`).
    pub pcm_senders: Vec<broadcast::Sender<Vec<f32>>>,
    /// Shared references to per-channel DSP state (for runtime mode updates).
    pub channel_dsps: Vec<Arc<Mutex<ChannelDsp>>>,
}

impl SdrPipeline {
    /// Build and start the IQ DSP pipeline.
    ///
    /// Spawns one OS thread that reads IQ samples from `source` and drives
    /// all per-channel DSP chains.
    ///
    /// # Parameters
    /// - `source`: IQ sample source (ownership transferred to read thread).
    /// - `sdr_sample_rate`: IQ capture rate (Hz).
    /// - `audio_sample_rate`: output PCM rate (Hz).
    /// - `frame_duration_ms`: output frame length (ms).
    /// - `channels`: slice of `(channel_if_hz, initial_mode, audio_bandwidth_hz, fir_taps)`.
    ///
    /// # Returns
    /// A `SdrPipeline` handle with PCM senders and shared channel DSP references.
    pub fn start(
        source: Box<dyn IqSource>,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        frame_duration_ms: u16,
        channels: &[(f64, RigMode, u32, usize)],
    ) -> Self {
        // Broadcast channel capacity: 64 IQ blocks.
        const IQ_BROADCAST_CAPACITY: usize = 64;
        let (iq_tx, _iq_rx) = broadcast::channel::<Vec<Complex<f32>>>(IQ_BROADCAST_CAPACITY);

        // PCM broadcast capacity: enough for several frames of latency.
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

        // Clone the Arc references for the read thread.
        let thread_dsps: Vec<Arc<Mutex<ChannelDsp>>> = channel_dsps.clone();

        // Spawn the IQ read thread.
        std::thread::Builder::new()
            .name("sdr-iq-read".to_string())
            .spawn(move || {
                iq_read_loop(source, sdr_sample_rate, thread_dsps, iq_tx);
            })
            .expect("failed to spawn sdr-iq-read thread");

        Self {
            pcm_senders,
            channel_dsps,
        }
    }
}

// ---------------------------------------------------------------------------
// IQ read loop
// ---------------------------------------------------------------------------

/// Block size for IQ reads.
const IQ_BLOCK_SIZE: usize = 4096;

/// The main IQ read loop. Runs on a dedicated OS thread.
///
/// Reads IQ blocks from `source` and dispatches them to all channel DSP
/// instances. Also publishes raw IQ blocks on `iq_tx` for any downstream
/// subscribers.
fn iq_read_loop(
    mut source: Box<dyn IqSource>,
    sdr_sample_rate: u32,
    channel_dsps: Vec<Arc<Mutex<ChannelDsp>>>,
    iq_tx: broadcast::Sender<Vec<Complex<f32>>>,
) {
    let mut block = vec![Complex::new(0.0_f32, 0.0_f32); IQ_BLOCK_SIZE];
    // Estimate time per block: IQ_BLOCK_SIZE samples at sdr_sample_rate Hz.
    // This is used to throttle MockIqSource to avoid busy-looping.
    let block_duration_ms = if sdr_sample_rate > 0 {
        (IQ_BLOCK_SIZE as f64 / sdr_sample_rate as f64 * 1000.0) as u64
    } else {
        1
    };

    loop {
        let n = match source.read_into(&mut block) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("IQ source read error: {}; retrying", e);
                // Brief back-off to avoid spinning on persistent errors.
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
        };

        if n == 0 {
            // Source returned zero samples — treat as transient, back off.
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }

        let samples = &block[..n];

        // Publish raw IQ to broadcast (best-effort; ignore lagged errors).
        let _ = iq_tx.send(samples.to_vec());

        // Drive per-channel DSP chains.
        for dsp_arc in &channel_dsps {
            match dsp_arc.lock() {
                Ok(mut dsp) => dsp.process_block(samples),
                Err(e) => {
                    tracing::error!("channel DSP mutex poisoned: {}", e);
                }
            }
        }

        // Throttle to avoid busy-looping when using MockIqSource.
        // Real hardware would naturally have this timing;
        // the mock needs artificial throttling.
        std::thread::sleep(std::time::Duration::from_millis(block_duration_ms));
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
        // Run enough samples for the filter to settle.
        for _ in 0..100 {
            out = fir.process(1.0);
        }
        // After settling, DC output should be close to 1.0.
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
        // With one tap, the coefficient sum is normalised to 1.0, so output ≈ 0.5.
        assert!((out - 0.5).abs() < 1e-5, "single-tap output: {}", out);
    }

    #[test]
    fn channel_dsp_processes_silence() {
        let (pcm_tx, _pcm_rx) = broadcast::channel::<Vec<f32>>(8);
        let mut dsp = ChannelDsp::new(
            0.0, // channel_if_hz
            &RigMode::USB,
            48_000, // sdr_sample_rate
            8_000,  // audio_sample_rate  (decim = 6)
            20,     // frame_duration_ms → frame_size = 160
            3000,   // audio_bandwidth_hz
            31,     // fir_taps
            pcm_tx,
        );

        // Feed 4096 zero samples — should not panic.
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
