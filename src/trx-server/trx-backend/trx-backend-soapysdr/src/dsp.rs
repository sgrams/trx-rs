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

mod channel;
mod filter;
mod spectrum;

use std::sync::{Arc, Mutex};

use num_complex::Complex;
use tokio::sync::broadcast;
use trx_core::rig::state::RigMode;

pub use self::channel::ChannelDsp;
pub use self::filter::{BlockFirFilter, BlockFirFilterPair, FirFilter};
use self::spectrum::SpectrumSnapshotter;

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

    /// Retune the hardware center frequency.  Default implementation is a
    /// no-op (used by `MockIqSource`).
    fn set_center_freq(&mut self, _hz: f64) -> Result<(), String> {
        Ok(())
    }

    /// Apply a new hardware receive gain in dB. Default implementation is a
    /// no-op for non-hardware sources.
    fn set_gain(&mut self, _gain_db: f64) -> Result<(), String> {
        Ok(())
    }

    /// Gives a source-specific implementation a chance to recover from a
    /// read error (for example, by rearming a hardware stream after overflow).
    /// Returns `true` when an active recovery action was attempted.
    fn handle_read_error(&mut self, _err: &str) -> Result<bool, String> {
        Ok(false)
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
// Top-level pipeline struct
// ---------------------------------------------------------------------------

pub struct SdrPipeline {
    pub pcm_senders: Vec<broadcast::Sender<Vec<f32>>>,
    pub channel_dsps: Vec<Arc<Mutex<ChannelDsp>>>,
    /// Latest FFT magnitude bins (dBFS, FFT-shifted), updated ~10 Hz.
    pub spectrum_buf: Arc<Mutex<Option<Vec<f32>>>>,
    /// SDR capture sample rate, needed by `SoapySdrRig::get_spectrum`.
    pub sdr_sample_rate: u32,
    /// Write `Some(hz)` here to retune the hardware center frequency.
    /// The IQ read loop picks it up on the next iteration.
    pub retune_cmd: Arc<std::sync::Mutex<Option<f64>>>,
    /// Write `Some(gain_db)` here to adjust the hardware RX gain.
    /// The IQ read loop picks it up on the next iteration.
    pub gain_cmd: Arc<std::sync::Mutex<Option<f64>>>,
}

impl SdrPipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        source: Box<dyn IqSource>,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        output_channels: usize,
        frame_duration_ms: u16,
        wfm_deemphasis_us: u32,
        wfm_stereo: bool,
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
                output_channels,
                frame_duration_ms,
                audio_bandwidth_hz,
                wfm_deemphasis_us,
                wfm_stereo,
                fir_taps,
                pcm_tx.clone(),
            );
            pcm_senders.push(pcm_tx);
            channel_dsps.push(Arc::new(Mutex::new(dsp)));
        }

        let thread_dsps: Vec<Arc<Mutex<ChannelDsp>>> = channel_dsps.clone();
        let spectrum_buf: Arc<Mutex<Option<Vec<f32>>>> = Arc::new(Mutex::new(None));
        let thread_spectrum_buf = spectrum_buf.clone();
        let retune_cmd: Arc<std::sync::Mutex<Option<f64>>> = Arc::new(std::sync::Mutex::new(None));
        let thread_retune_cmd = retune_cmd.clone();
        let gain_cmd: Arc<std::sync::Mutex<Option<f64>>> = Arc::new(std::sync::Mutex::new(None));
        let thread_gain_cmd = gain_cmd.clone();

        std::thread::Builder::new()
            .name("sdr-iq-read".to_string())
            .spawn(move || {
                iq_read_loop(
                    source,
                    sdr_sample_rate,
                    thread_dsps,
                    iq_tx,
                    thread_spectrum_buf,
                    thread_retune_cmd,
                    thread_gain_cmd,
                );
            })
            .expect("failed to spawn sdr-iq-read thread");

        Self {
            pcm_senders,
            channel_dsps,
            spectrum_buf,
            sdr_sample_rate,
            retune_cmd,
            gain_cmd,
        }
    }
}

// ---------------------------------------------------------------------------
// IQ read loop
// ---------------------------------------------------------------------------

pub const IQ_BLOCK_SIZE: usize = 4096;

fn iq_read_loop(
    mut source: Box<dyn IqSource>,
    sdr_sample_rate: u32,
    channel_dsps: Vec<Arc<Mutex<ChannelDsp>>>,
    iq_tx: broadcast::Sender<Vec<Complex<f32>>>,
    spectrum_buf: Arc<Mutex<Option<Vec<f32>>>>,
    retune_cmd: Arc<std::sync::Mutex<Option<f64>>>,
    gain_cmd: Arc<std::sync::Mutex<Option<f64>>>,
) {
    let mut block = vec![Complex::new(0.0_f32, 0.0_f32); IQ_BLOCK_SIZE];
    let block_duration_ms = if sdr_sample_rate > 0 {
        (IQ_BLOCK_SIZE as f64 / sdr_sample_rate as f64 * 1000.0) as u64
    } else {
        1
    };
    let throttle = !source.is_blocking();

    let mut spectrum = SpectrumSnapshotter::new();
    let mut read_error_streak: u32 = 0;

    loop {
        // Apply any pending hardware retune before the next read.
        if let Ok(mut cmd) = retune_cmd.try_lock() {
            if let Some(hz) = cmd.take() {
                if let Err(e) = source.set_center_freq(hz) {
                    tracing::warn!("SDR retune to {:.0} Hz failed: {}", hz, e);
                } else {
                    tracing::info!("SDR retuned to {:.0} Hz", hz);
                }
            }
        }
        if let Ok(mut cmd) = gain_cmd.try_lock() {
            if let Some(gain_db) = cmd.take() {
                if let Err(e) = source.set_gain(gain_db) {
                    tracing::warn!("SDR gain change to {:.1} dB failed: {}", gain_db, e);
                } else {
                    tracing::info!("SDR gain updated to {:.1} dB", gain_db);
                }
            }
        }

        let n = match source.read_into(&mut block) {
            Ok(n) => {
                read_error_streak = 0;
                n
            }
            Err(e) => {
                read_error_streak = read_error_streak.saturating_add(1);
                let recovered = match source.handle_read_error(&e) {
                    Ok(result) => result,
                    Err(recovery_err) => {
                        tracing::warn!(
                            "IQ source recovery after read error failed: {}",
                            recovery_err
                        );
                        false
                    }
                };
                tracing::warn!(
                    "IQ source read error: {}; retrying (streak={}, recovered={})",
                    e,
                    read_error_streak,
                    recovered
                );
                let base_sleep_ms = if recovered {
                    block_duration_ms.max(20)
                } else {
                    10
                };
                let sleep_ms = (base_sleep_ms as u128)
                    .saturating_mul(1u128 << read_error_streak.saturating_sub(1).min(4))
                    .min(250) as u64;
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
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

        spectrum.update(samples, &spectrum_buf);

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
    fn pipeline_starts_with_mock_source() {
        let pipeline = SdrPipeline::start(
            Box::new(MockIqSource),
            1_920_000,
            48_000,
            1,
            20,
            75,
            true,
            &[(200_000.0, RigMode::USB, 3000, 64)],
        );
        assert_eq!(pipeline.pcm_senders.len(), 1);
        assert_eq!(pipeline.channel_dsps.len(), 1);
    }

    #[test]
    fn pipeline_empty_channels() {
        let pipeline = SdrPipeline::start(
            Box::new(MockIqSource),
            1_920_000,
            48_000,
            1,
            20,
            75,
            true,
            &[],
        );
        assert_eq!(pipeline.pcm_senders.len(), 0);
        assert_eq!(pipeline.channel_dsps.len(), 0);
    }
}
