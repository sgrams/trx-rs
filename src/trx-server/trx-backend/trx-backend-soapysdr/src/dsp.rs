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

use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use num_complex::Complex;
use tokio::sync::broadcast;
use tracing::warn;
use trx_core::rig::state::RigMode;

pub use self::channel::{ChannelDsp, VirtualSquelchConfig};
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

    /// Apply a new gain to a named gain element (e.g. "LNA"). Default
    /// implementation is a no-op for sources that do not support named gains.
    fn set_named_gain(&mut self, _name: &str, _gain_db: f64) -> Result<(), String> {
        Ok(())
    }

    /// Read the current value of a named gain element. Returns `None` for
    /// sources that do not support named gain elements.
    fn read_named_gain(&self, _name: &str) -> Option<f64> {
        None
    }

    /// Enable or disable hardware automatic gain control.  Default
    /// implementation is a no-op for sources that do not support AGC.
    fn set_gain_mode(&mut self, _automatic: bool) -> Result<(), String> {
        Ok(())
    }

    /// Gives a source-specific implementation a chance to recover from a
    /// read error (for example, by rearming a hardware stream after overflow).
    /// Returns `true` when an active recovery action was attempted.
    fn handle_read_error(&mut self, _err: &str, _streak: u32) -> Result<bool, String> {
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
    pub iq_senders: Vec<broadcast::Sender<Vec<Complex<f32>>>>,
    /// All DSP channel slots, including fixed (primary, AIS) and dynamic
    /// (user virtual) channels.  Shared with the IQ read thread via RwLock.
    /// Virtual channels are appended beyond the fixed slots.
    pub channel_dsps: Arc<RwLock<Vec<Arc<Mutex<ChannelDsp>>>>>,
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
    /// Write `Some(gain_db)` here to adjust the LNA gain element.
    /// The IQ read loop picks it up on the next iteration.
    pub lna_gain_cmd: Arc<std::sync::Mutex<Option<f64>>>,
    /// Write `Some(enabled)` here to switch hardware AGC on or off.
    /// The IQ read loop picks it up on the next iteration.
    pub agc_cmd: Arc<std::sync::Mutex<Option<bool>>>,
    /// Current hardware center frequency in Hz, kept in sync by `SoapySdrRig`.
    /// Read by `SdrVirtualChannelManager` to validate and compute IF offsets.
    pub shared_center_hz: Arc<AtomicI64>,
    // Parameters cached here so `add_virtual_channel` can construct new
    // `ChannelDsp` instances without needing to be passed them again.
    audio_sample_rate: u32,
    audio_channels: usize,
    frame_duration_ms: u16,
    wfm_deemphasis_us: u32,
    wfm_stereo: bool,
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
        squelch_cfg: VirtualSquelchConfig,
        channels: &[(f64, RigMode, u32)],
    ) -> Self {
        const IQ_BROADCAST_CAPACITY: usize = 64;
        let (iq_tx, _iq_rx) = broadcast::channel::<Vec<Complex<f32>>>(IQ_BROADCAST_CAPACITY);

        const PCM_BROADCAST_CAPACITY: usize = 32;

        let mut pcm_senders = Vec::with_capacity(channels.len());
        let mut iq_senders = Vec::with_capacity(channels.len());
        let mut channel_dsps_vec: Vec<Arc<Mutex<ChannelDsp>>> = Vec::with_capacity(channels.len());

        for (channel_idx, &(channel_if_hz, ref mode, audio_bandwidth_hz)) in
            channels.iter().enumerate()
        {
            let (pcm_tx, _pcm_rx) = broadcast::channel::<Vec<f32>>(PCM_BROADCAST_CAPACITY);
            let (iq_tx, _iq_rx) = broadcast::channel::<Vec<Complex<f32>>>(IQ_BROADCAST_CAPACITY);
            let channel_squelch_cfg = if channel_idx == 0 {
                squelch_cfg
            } else {
                VirtualSquelchConfig::default()
            };
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
                false,
                channel_squelch_cfg,
                pcm_tx.clone(),
                iq_tx.clone(),
            );
            pcm_senders.push(pcm_tx);
            iq_senders.push(iq_tx);
            channel_dsps_vec.push(Arc::new(Mutex::new(dsp)));
        }

        let channel_dsps: Arc<RwLock<Vec<Arc<Mutex<ChannelDsp>>>>> =
            Arc::new(RwLock::new(channel_dsps_vec));
        let thread_dsps = channel_dsps.clone();
        let spectrum_buf: Arc<Mutex<Option<Vec<f32>>>> = Arc::new(Mutex::new(None));
        let thread_spectrum_buf = spectrum_buf.clone();
        let retune_cmd: Arc<std::sync::Mutex<Option<f64>>> = Arc::new(std::sync::Mutex::new(None));
        let thread_retune_cmd = retune_cmd.clone();
        let gain_cmd: Arc<std::sync::Mutex<Option<f64>>> = Arc::new(std::sync::Mutex::new(None));
        let thread_gain_cmd = gain_cmd.clone();
        let lna_gain_cmd: Arc<std::sync::Mutex<Option<f64>>> =
            Arc::new(std::sync::Mutex::new(None));
        let thread_lna_gain_cmd = lna_gain_cmd.clone();
        let agc_cmd: Arc<std::sync::Mutex<Option<bool>>> = Arc::new(std::sync::Mutex::new(None));
        let thread_agc_cmd = agc_cmd.clone();

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
                    thread_lna_gain_cmd,
                    thread_agc_cmd,
                );
            })
            .expect("failed to spawn sdr-iq-read thread");

        Self {
            pcm_senders,
            iq_senders,
            channel_dsps,
            spectrum_buf,
            sdr_sample_rate,
            retune_cmd,
            gain_cmd,
            lna_gain_cmd,
            agc_cmd,
            shared_center_hz: Arc::new(AtomicI64::new(0)),
            audio_sample_rate,
            audio_channels: output_channels,
            frame_duration_ms,
            wfm_deemphasis_us,
            wfm_stereo,
        }
    }

    /// Allocate a new virtual DSP channel.
    ///
    /// Returns the PCM and IQ broadcast senders so the caller can subscribe to
    /// audio frames and raw decimated IQ respectively.  The channel is appended
    /// beyond all fixed slots and is immediately visible to the IQ read thread.
    pub fn add_virtual_channel(
        &self,
        channel_if_hz: f64,
        mode: &RigMode,
        bandwidth_hz: u32,
    ) -> (broadcast::Sender<Vec<f32>>, broadcast::Sender<Vec<Complex<f32>>>) {
        const PCM_BROADCAST_CAPACITY: usize = 32;
        const IQ_BROADCAST_CAPACITY: usize = 64;
        let (pcm_tx, _) = broadcast::channel::<Vec<f32>>(PCM_BROADCAST_CAPACITY);
        let (iq_tx, _) = broadcast::channel::<Vec<Complex<f32>>>(IQ_BROADCAST_CAPACITY);
        let dsp = ChannelDsp::new(
            channel_if_hz,
            mode,
            self.sdr_sample_rate,
            self.audio_sample_rate,
            self.audio_channels,
            self.frame_duration_ms,
            bandwidth_hz,
            self.wfm_deemphasis_us,
            self.wfm_stereo,
            false,
            VirtualSquelchConfig::default(),
            pcm_tx.clone(),
            iq_tx.clone(),
        );
        self.channel_dsps
            .write()
            .expect("channel_dsps RwLock poisoned")
            .push(Arc::new(Mutex::new(dsp)));
        (pcm_tx, iq_tx)
    }

    /// Remove a DSP channel slot by its index in the full `channel_dsps` list.
    ///
    /// Returns `false` when the index is out of range.  Callers are responsible
    /// for not removing fixed slots (primary, AIS).
    pub fn remove_virtual_channel(&self, idx: usize) -> bool {
        let mut dsps = self
            .channel_dsps
            .write()
            .expect("channel_dsps RwLock poisoned");
        if idx >= dsps.len() {
            return false;
        }
        dsps.remove(idx);
        true
    }
}

// ---------------------------------------------------------------------------
// IQ read loop
// ---------------------------------------------------------------------------

pub const IQ_BLOCK_SIZE: usize = 4096;

fn iq_read_loop(
    mut source: Box<dyn IqSource>,
    sdr_sample_rate: u32,
    channel_dsps: Arc<RwLock<Vec<Arc<Mutex<ChannelDsp>>>>>,
    iq_tx: broadcast::Sender<Vec<Complex<f32>>>,
    spectrum_buf: Arc<Mutex<Option<Vec<f32>>>>,
    retune_cmd: Arc<std::sync::Mutex<Option<f64>>>,
    gain_cmd: Arc<std::sync::Mutex<Option<f64>>>,
    lna_gain_cmd: Arc<std::sync::Mutex<Option<f64>>>,
    agc_cmd: Arc<std::sync::Mutex<Option<bool>>>,
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
    let mut zero_read_streak: u32 = 0;
    let mut overflow_log_window_start: Option<Instant> = None;
    let mut overflow_log_suppressed: u32 = 0;
    loop {
        // Apply any pending hardware retune before the next read.
        if let Ok(mut cmd) = retune_cmd.try_lock() {
            if let Some(hz) = cmd.take() {
                if let Err(e) = source.set_center_freq(hz) {
                    warn!("set_center_freq failed ({}): {}", hz, e);
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
        if let Ok(mut cmd) = lna_gain_cmd.try_lock() {
            if let Some(gain_db) = cmd.take() {
                if let Err(e) = source.set_named_gain("LNA", gain_db) {
                    tracing::warn!("SDR LNA gain change to {:.1} dB failed: {}", gain_db, e);
                } else {
                    tracing::info!("SDR LNA gain updated to {:.1} dB", gain_db);
                }
            }
        }
        if let Ok(mut cmd) = agc_cmd.try_lock() {
            if let Some(enabled) = cmd.take() {
                if let Err(e) = source.set_gain_mode(enabled) {
                    tracing::warn!("SDR AGC mode change to {} failed: {}", enabled, e);
                } else {
                    tracing::info!("SDR AGC mode set to {}", enabled);
                }
            }
        }

        let n = match source.read_into(&mut block) {
            Ok(n) => {
                read_error_streak = 0;
                if n > 0 {
                    zero_read_streak = 0;
                }
                n
            }
            Err(e) => {
                read_error_streak = read_error_streak.saturating_add(1);
                let err_lc = e.to_ascii_lowercase();
                let is_overflow = err_lc.contains("overflow") || err_lc.contains("overrun");
                let restarted = read_error_streak >= 3;
                let recovered = match source.handle_read_error(&e, read_error_streak) {
                    Ok(result) => result,
                    Err(recovery_err) => {
                        tracing::warn!(
                            "IQ source recovery after read error failed: {}",
                            recovery_err
                        );
                        false
                    }
                };
                // After a stream restart, reset the streak so we don't
                // immediately trigger another restart on the first transient
                // failure during hardware stabilisation.
                if restarted && recovered {
                    read_error_streak = 1;
                }
                if is_overflow {
                    let now = Instant::now();
                    if overflow_log_window_start
                        .map(|ts| now.duration_since(ts) >= Duration::from_secs(3))
                        .unwrap_or(true)
                    {
                        let suppressed = overflow_log_suppressed;
                        overflow_log_window_start = Some(now);
                        overflow_log_suppressed = 0;
                        tracing::warn!(
                            "IQ source overflow: {}; retrying (streak={}, recovered={}, suppressed={})",
                            e,
                            read_error_streak,
                            recovered,
                            suppressed
                        );
                    } else {
                        overflow_log_suppressed = overflow_log_suppressed.saturating_add(1);
                        tracing::debug!(
                            "IQ source overflow suppressed: {} (streak={}, recovered={})",
                            e,
                            read_error_streak,
                            recovered
                        );
                    }
                } else {
                    tracing::warn!(
                        "IQ source read error: {}; retrying (streak={}, recovered={})",
                        e,
                        read_error_streak,
                        recovered
                    );
                }
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
            zero_read_streak = zero_read_streak.saturating_add(1);
            let base_sleep_ms = block_duration_ms.max(2);
            let sleep_ms = (base_sleep_ms as u128)
                .saturating_mul(1u128 << zero_read_streak.saturating_sub(1).min(4))
                .min(50) as u64;
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            continue;
        }

        let samples = &block[..n];

        // Avoid per-block allocation/copy when there are no IQ subscribers.
        if iq_tx.receiver_count() > 0 {
            let _ = iq_tx.send(samples.to_vec());
        }

        // Hold a read lock only for the duration of this block's DSP pass.
        // Write lock (add/remove channel) waits at most one block (~2 ms).
        {
            let dsps = channel_dsps
                .read()
                .expect("channel_dsps RwLock poisoned");
            for dsp_arc in dsps.iter() {
                match dsp_arc.lock() {
                    Ok(mut dsp) => dsp.process_block(samples),
                    Err(e) => {
                        tracing::error!("channel DSP mutex poisoned: {}", e);
                    }
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
            VirtualSquelchConfig::default(),
            &[(200_000.0, RigMode::USB, 3000)],
        );
        assert_eq!(pipeline.pcm_senders.len(), 1);
        assert_eq!(pipeline.channel_dsps.read().unwrap().len(), 1);
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
            VirtualSquelchConfig::default(),
            &[],
        );
        assert_eq!(pipeline.pcm_senders.len(), 0);
        assert_eq!(pipeline.channel_dsps.read().unwrap().len(), 0);
    }
}
