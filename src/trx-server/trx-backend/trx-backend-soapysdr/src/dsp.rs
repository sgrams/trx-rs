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
use trx_core::rig::state::{RdsData, RigMode};

use crate::demod::{DcBlocker, Demodulator, SoftAgc, WfmStereoDecoder};

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
        let mut h_buf: Vec<FftComplex<f32>> =
            coeffs.iter().map(|&c| FftComplex::new(c, 0.0)).collect();
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
// Per-mode post-processing helpers (DC block + AGC)
// ---------------------------------------------------------------------------

/// Build the AGC for a given mode.
///
/// AM AGC must be far slower than audio modulation.  With a 50 Hz bass
/// component the modulation period is 20 ms; an attack faster than that
/// causes the AGC to follow the audio envelope and distort (pumping).
/// 500 ms / 5 s only reacts to slow carrier-amplitude fading, not audio.
///
/// CW uses a fast attack/release to follow individual dots and dashes.
/// All other modes use 5 ms / 500 ms, suitable for SSB voice and FM.
fn agc_for_mode(mode: &RigMode, audio_sample_rate: u32) -> SoftAgc {
    let sr = audio_sample_rate.max(1) as f32;
    match mode {
        RigMode::CW | RigMode::CWR => SoftAgc::new(sr, 1.0, 50.0, 0.5, 30.0),
        RigMode::AM => SoftAgc::new(sr, 500.0, 5_000.0, 0.5, 30.0),
        _ => SoftAgc::new(sr, 5.0, 500.0, 0.5, 30.0),
    }
}

/// Build the DC blocker for a given mode, or `None` if not applicable.
///
/// WFM is excluded because it has its own internal DC blockers per channel.
/// All other modes use r = 0.9999 (corner ≈ 0.76 Hz @ 48 kHz), which strips
/// only true carrier DC without affecting any audible bass content.
fn dc_for_mode(mode: &RigMode) -> Option<DcBlocker> {
    match mode {
        RigMode::WFM => None,
        _ => Some(DcBlocker::new(0.9999)),
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
    /// Current rig mode so the decimation pipeline can be rebuilt.
    mode: RigMode,
    /// FFT-based FIR low-pass filter applied to I component before decimation.
    lpf_i: BlockFirFilter,
    /// FFT-based FIR low-pass filter applied to Q component before decimation.
    lpf_q: BlockFirFilter,
    /// SDR capture sample rate — kept for filter rebuilds.
    sdr_sample_rate: u32,
    /// Output audio sample rate.
    audio_sample_rate: u32,
    /// Requested audio bandwidth.
    audio_bandwidth_hz: u32,
    /// FIR tap count used when rebuilding filters.
    fir_taps: usize,
    /// WFM deemphasis time constant in microseconds.
    wfm_deemphasis_us: u32,
    /// Whether WFM stereo decoding is enabled.
    wfm_stereo: bool,
    /// Whether multiband stereo denoising is enabled for WFM.
    wfm_denoise: bool,
    /// Decimation factor: `sdr_sample_rate / audio_sample_rate`.
    pub decim_factor: usize,
    /// Number of PCM channels emitted in each frame.
    output_channels: usize,
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
    /// Decimation counter (used only for the WFM path).
    decim_counter: usize,
    /// Fractional-phase resampler for non-WFM paths.
    /// Advances by `resample_phase_inc` per IQ sample and emits an output
    /// sample whenever it crosses 1.0, giving a long-term output rate of
    /// exactly `audio_sample_rate` regardless of SDR rate rounding.
    resample_phase: f64,
    /// `audio_sample_rate / sdr_sample_rate`  (updated in `rebuild_filters`).
    resample_phase_inc: f64,
    /// Dedicated WFM decoder that preserves the FM composite baseband.
    wfm_decoder: Option<WfmStereoDecoder>,
    /// Soft AGC applied to all demodulated audio for consistent cross-mode levels.
    audio_agc: SoftAgc,
    /// DC blocker for modes whose demodulator output can carry a DC offset
    /// (USB/LSB/AM/FM/DIG).  None for CW and WFM.
    audio_dc: Option<DcBlocker>,
}

impl ChannelDsp {
    pub fn set_channel_if_hz(&mut self, channel_if_hz: f64) {
        self.channel_if_hz = channel_if_hz;
        self.mixer_phase_inc = if self.sdr_sample_rate == 0 {
            0.0
        } else {
            2.0 * std::f64::consts::PI * channel_if_hz / self.sdr_sample_rate as f64
        };
    }

    fn pipeline_rates(
        mode: &RigMode,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        audio_bandwidth_hz: u32,
    ) -> (usize, u32) {
        if sdr_sample_rate == 0 {
            return (1, audio_sample_rate.max(1));
        }

        let target_rate = if *mode == RigMode::WFM {
            audio_bandwidth_hz.max(audio_sample_rate.saturating_mul(4)).max(228_000)
        } else {
            audio_sample_rate.max(1)
        };
        let decim_factor = (sdr_sample_rate / target_rate.max(1)).max(1) as usize;
        let channel_sample_rate = (sdr_sample_rate / decim_factor as u32).max(1);
        (decim_factor, channel_sample_rate)
    }

    fn rebuild_filters(&mut self, reset_wfm_decoder: bool) {
        let (next_decim_factor, channel_sample_rate) = Self::pipeline_rates(
            &self.mode,
            self.sdr_sample_rate,
            self.audio_sample_rate,
            self.audio_bandwidth_hz,
        );
        // For WFM, widen the IQ filter enough to pass the RDS subcarrier at
        // 57 kHz (requires cutoff ≥ 65 kHz → bandwidth ≥ 130 kHz).
        let iq_filter_bw = if self.mode == RigMode::WFM {
            self.audio_bandwidth_hz.max(130_000)
        } else {
            self.audio_bandwidth_hz
        };
        let cutoff_hz = iq_filter_bw.min(channel_sample_rate.saturating_sub(1)) as f32 / 2.0;
        let cutoff_norm = if self.sdr_sample_rate == 0 {
            0.1
        } else {
            (cutoff_hz / self.sdr_sample_rate as f32).min(0.499)
        };
        self.lpf_i = BlockFirFilter::new(cutoff_norm, self.fir_taps, IQ_BLOCK_SIZE);
        self.lpf_q = BlockFirFilter::new(cutoff_norm, self.fir_taps, IQ_BLOCK_SIZE);
        let rate_changed = self.decim_factor != next_decim_factor;
        self.decim_factor = next_decim_factor;
        self.decim_counter = 0;
        self.resample_phase = 0.0;
        self.resample_phase_inc = if self.sdr_sample_rate == 0 {
            1.0
        } else {
            self.audio_sample_rate as f64 / self.sdr_sample_rate as f64
        };
        if self.mode == RigMode::WFM {
            if reset_wfm_decoder || rate_changed || self.wfm_decoder.is_none() {
                self.wfm_decoder = Some(WfmStereoDecoder::new(
                    channel_sample_rate,
                    self.audio_sample_rate,
                    self.output_channels,
                    self.wfm_stereo,
                    self.wfm_deemphasis_us,
                    self.wfm_denoise,
                ));
            }
        } else {
            self.wfm_decoder = None;
        }
        self.audio_agc = agc_for_mode(&self.mode, self.audio_sample_rate);
        self.audio_dc = dc_for_mode(&self.mode);
        self.frame_buf.clear();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel_if_hz: f64,
        mode: &RigMode,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        output_channels: usize,
        frame_duration_ms: u16,
        audio_bandwidth_hz: u32,
        wfm_deemphasis_us: u32,
        wfm_stereo: bool,
        wfm_denoise: bool,
        fir_taps: usize,
        pcm_tx: broadcast::Sender<Vec<f32>>,
    ) -> Self {
        let output_channels = output_channels.max(1);
        let frame_size = if audio_sample_rate == 0 || frame_duration_ms == 0 {
            960 * output_channels
        } else {
            (audio_sample_rate as usize * frame_duration_ms as usize * output_channels) / 1000
        };

        let taps = fir_taps.max(1);
        let (decim_factor, channel_sample_rate) =
            Self::pipeline_rates(mode, sdr_sample_rate, audio_sample_rate, audio_bandwidth_hz);
        // For WFM, widen the IQ filter enough to pass the RDS subcarrier at
        // 57 kHz (requires cutoff ≥ 65 kHz → bandwidth ≥ 130 kHz).
        let iq_filter_bw = if *mode == RigMode::WFM {
            audio_bandwidth_hz.max(130_000)
        } else {
            audio_bandwidth_hz
        };
        let cutoff_hz = iq_filter_bw.min(channel_sample_rate.saturating_sub(1)) as f32 / 2.0;
        let cutoff_norm = if sdr_sample_rate == 0 {
            0.1
        } else {
            (cutoff_hz / sdr_sample_rate as f32).min(0.499)
        };

        let mixer_phase_inc = if sdr_sample_rate == 0 {
            0.0
        } else {
            2.0 * std::f64::consts::PI * channel_if_hz / sdr_sample_rate as f64
        };

        Self {
            channel_if_hz,
            demodulator: Demodulator::for_mode(mode),
            mode: mode.clone(),
            lpf_i: BlockFirFilter::new(cutoff_norm, taps, IQ_BLOCK_SIZE),
            lpf_q: BlockFirFilter::new(cutoff_norm, taps, IQ_BLOCK_SIZE),
            sdr_sample_rate,
            audio_sample_rate,
            audio_bandwidth_hz,
            fir_taps: taps,
            wfm_deemphasis_us,
            wfm_stereo,
            wfm_denoise,
            decim_factor,
            output_channels,
            frame_buf: Vec::with_capacity(frame_size + output_channels),
            frame_size,
            pcm_tx,
            mixer_phase: 0.0,
            mixer_phase_inc,
            decim_counter: 0,
            resample_phase: 0.0,
            resample_phase_inc: if sdr_sample_rate == 0 {
                1.0
            } else {
                audio_sample_rate as f64 / sdr_sample_rate as f64
            },
            wfm_decoder: if *mode == RigMode::WFM {
                Some(WfmStereoDecoder::new(
                    channel_sample_rate,
                    audio_sample_rate,
                    output_channels,
                    wfm_stereo,
                    wfm_deemphasis_us,
                    wfm_denoise,
                ))
            } else {
                None
            },
            audio_agc: agc_for_mode(mode, audio_sample_rate),
            audio_dc: dc_for_mode(mode),
        }
    }

    pub fn set_mode(&mut self, mode: &RigMode) {
        self.mode = mode.clone();
        self.audio_bandwidth_hz = default_bandwidth_for_mode(mode);
        self.demodulator = Demodulator::for_mode(mode);
        self.rebuild_filters(true);
    }
}

/// Returns the appropriate channel filter bandwidth for a given mode.
fn default_bandwidth_for_mode(mode: &RigMode) -> u32 {
    match mode {
        RigMode::LSB | RigMode::USB | RigMode::PKT | RigMode::DIG => 3_000,
        RigMode::CW | RigMode::CWR => 500,
        RigMode::AM => 12_000,
        RigMode::FM => 12_500,
        RigMode::WFM => 180_000,
        RigMode::Other(_) => 3_000,
    }
}

impl ChannelDsp {

    /// Rebuild the FIR low-pass filters with new bandwidth and tap count.
    ///
    /// Changes take effect on the next call to `process_block`.
    pub fn set_filter(&mut self, bandwidth_hz: u32, taps: usize) {
        self.audio_bandwidth_hz = bandwidth_hz;
        self.fir_taps = taps.max(1);
        self.rebuild_filters(false);
    }

    pub fn set_wfm_deemphasis(&mut self, deemphasis_us: u32) {
        self.wfm_deemphasis_us = deemphasis_us;
        self.rebuild_filters(true);
    }

    pub fn set_wfm_stereo(&mut self, enabled: bool) {
        self.wfm_stereo = enabled;
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.set_stereo_enabled(enabled);
        }
    }

    pub fn set_wfm_denoise(&mut self, enabled: bool) {
        self.wfm_denoise = enabled;
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.set_denoise_enabled(enabled);
        }
    }

    pub fn rds_data(&self) -> Option<RdsData> {
        self.wfm_decoder.as_ref().and_then(WfmStereoDecoder::rds_data)
    }

    pub fn reset_rds(&mut self) {
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.reset_rds();
        }
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
        self.mixer_phase = (phase_start + n as f64 * phase_inc).rem_euclid(std::f64::consts::TAU);

        // --- 2. FFT FIR (overlap-save) --------------------------------------
        let filtered_i = self.lpf_i.filter_block(&mixed_i);
        let filtered_q = self.lpf_q.filter_block(&mixed_q);

        // --- 3. Decimate / resample -----------------------------------------
        let capacity = n / self.decim_factor + 1;
        let mut decimated: Vec<Complex<f32>> = Vec::with_capacity(capacity);
        if self.wfm_decoder.is_some() {
            // WFM: integer decimation preserves the FM composite signal at the
            // rate expected by WfmStereoDecoder.  Final rate correction is done
            // inside WfmStereoDecoder via its own fractional-phase accumulator.
            for i in 0..n {
                self.decim_counter += 1;
                if self.decim_counter >= self.decim_factor {
                    self.decim_counter = 0;
                    let fi = filtered_i.get(i).copied().unwrap_or(0.0);
                    let fq = filtered_q.get(i).copied().unwrap_or(0.0);
                    decimated.push(Complex::new(fi, fq));
                }
            }
        } else {
            // Non-WFM: fractional-phase resampler so the long-term output rate
            // equals exactly `audio_sample_rate` regardless of SDR rate rounding.
            for i in 0..n {
                self.resample_phase += self.resample_phase_inc;
                if self.resample_phase >= 1.0 {
                    self.resample_phase -= 1.0;
                    let fi = filtered_i.get(i).copied().unwrap_or(0.0);
                    let fq = filtered_q.get(i).copied().unwrap_or(0.0);
                    decimated.push(Complex::new(fi, fq));
                }
            }
        }

        if decimated.is_empty() {
            return;
        }

        // --- 4. Demodulate + post-process -----------------------------------
        // WFM: full composite decoder (handles its own DC blocks + deemphasis).
        // All other modes: stateless demodulator → DC blocker (where enabled) → AGC.
        // AGC is applied to WFM output too so all modes share the same target level.
        let audio = if let Some(decoder) = self.wfm_decoder.as_mut() {
            let mut out = decoder.process_iq(&decimated);
            for s in &mut out {
                *s = self.audio_agc.process(*s);
            }
            out
        } else {
            let mut raw = self.demodulator.demodulate(&decimated);
            for s in &mut raw {
                if let Some(dc) = &mut self.audio_dc {
                    *s = dc.process(*s);
                }
                *s = self.audio_agc.process(*s);
            }
            raw
        };

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
    /// Write `Some(hz)` here to retune the hardware center frequency.
    /// The IQ read loop picks it up on the next iteration.
    pub retune_cmd: Arc<std::sync::Mutex<Option<f64>>>,
}

impl SdrPipeline {
    pub fn start(
        source: Box<dyn IqSource>,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        output_channels: usize,
        frame_duration_ms: u16,
        wfm_deemphasis_us: u32,
        wfm_stereo: bool,
        wfm_denoise: bool,
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
                wfm_denoise,
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
                );
            })
            .expect("failed to spawn sdr-iq-read thread");

        Self {
            pcm_senders,
            channel_dsps,
            spectrum_buf,
            sdr_sample_rate,
            retune_cmd,
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
    retune_cmd: Arc<std::sync::Mutex<Option<f64>>>,
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
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / (SPECTRUM_FFT_SIZE - 1) as f32).cos()))
        .collect();

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(SPECTRUM_FFT_SIZE);
    let mut spectrum_counter: usize = 0;
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
        let mut dsp =
            ChannelDsp::new(0.0, &RigMode::USB, 48_000, 8_000, 1, 20, 3000, 75, true, 31, pcm_tx);
        let block = vec![Complex::new(0.0_f32, 0.0_f32); 4096];
        dsp.process_block(&block);
    }

    #[test]
    fn channel_dsp_set_mode() {
        let (pcm_tx, _) = broadcast::channel::<Vec<f32>>(8);
        let mut dsp =
            ChannelDsp::new(0.0, &RigMode::USB, 48_000, 8_000, 1, 20, 3000, 75, true, 31, pcm_tx);
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
        let pipeline = SdrPipeline::start(Box::new(MockIqSource), 1_920_000, 48_000, 1, 20, 75, true, &[]);
        assert_eq!(pipeline.pcm_senders.len(), 0);
        assert_eq!(pipeline.channel_dsps.len(), 0);
    }
}
