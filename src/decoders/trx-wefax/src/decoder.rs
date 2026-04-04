// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Top-level WEFAX decoder state machine.
//!
//! Drives the DSP pipeline: resampler → FM discriminator → tone detector →
//! phasing → line slicer → image assembler.

use std::path::PathBuf;

use base64::Engine;
use trx_core::decode::{WefaxMessage, WefaxProgress};

use tracing::{debug, trace};

use crate::config::WefaxConfig;
use crate::demod::FmDiscriminator;
use crate::image::ImageAssembler;
use crate::line_slicer::LineSlicer;
use crate::phase::PhasingDetector;
use crate::resampler::{Resampler, INTERNAL_RATE};
use crate::tone_detect::{AptTone, ToneDetector};

/// Progress events are emitted every this many lines.
const PROGRESS_INTERVAL: u32 = 5;

/// Minimum luminance standard deviation to consider a window as containing
/// active WEFAX signal (image data has varied luminance; silence/noise is flat).
const SIGNAL_DETECT_MIN_STDDEV: f32 = 0.08;

/// Number of consecutive active-signal windows needed to auto-start receiving.
/// At 0.5 s per window this is ~3 seconds.
const SIGNAL_DETECT_WINDOWS: u32 = 6;

/// Pearson correlation below which a new scan line is considered uncorrelated
/// with its predecessor — i.e. the slicer is looking at noise, not imagery.
/// Real WEFAX content typically shows r > 0.5 between adjacent lines.
const LINE_CORR_NOISE_THRESHOLD: f32 = 0.2;

/// Number of consecutive uncorrelated scan lines that trigger auto-finalize
/// while receiving. At 120 LPM this is 15 s; at 60 LPM it's 30 s. Modelled on
/// fldigi's line-to-line correlation check for automatic stop.
const LINE_CORR_NOISE_LINES: u32 = 30;

/// Minimum image height (lines) to save. Anything shorter is assumed to be a
/// false-positive auto-start (variance detector tripping on tones, noise
/// bursts, or phasing leakage) and discarded silently. A real WEFAX chart is
/// at least several hundred lines long.
const MIN_IMAGE_LINES: u32 = 100;

/// WEFAX decoder output event.
#[derive(Debug)]
pub enum WefaxEvent {
    /// A progress update with line data for live rendering.
    Progress(WefaxProgress, Vec<u8>),
    /// A completed image.
    Complete(WefaxMessage),
}

/// Internal decoder state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Listening for APT start tone.
    Idle,
    /// Start tone detected; waiting for phasing signal.
    StartDetected { ioc: u16 },
    /// Receiving phasing lines; aligning line-start phase.
    Phasing { ioc: u16, lpm: u16 },
    /// Actively decoding image lines.
    Receiving { ioc: u16, lpm: u16 },
    /// Stop tone detected; finalising image.
    Stopping { ioc: u16, lpm: u16 },
}

/// Top-level WEFAX decoder.
pub struct WefaxDecoder {
    config: WefaxConfig,
    state: State,
    resampler: Resampler,
    demodulator: FmDiscriminator,
    tone_detector: ToneDetector,
    phasing: Option<PhasingDetector>,
    slicer: Option<LineSlicer>,
    image: Option<ImageAssembler>,
    /// Total sample counter for timestamps.
    sample_count: u64,
    /// Timestamp (ms since epoch) when reception started.
    reception_start_ms: Option<i64>,
    /// Whether the initial "Idle" state event has been emitted.
    sent_idle_event: bool,
    /// Counts consecutive half-second windows where the luminance variance is
    /// high enough to indicate an active WEFAX transmission.  Used to auto-start
    /// receiving when tuning in mid-image (same idea as fldigi's "strong image
    /// signal" detection in `fax_signal`).
    signal_detect_count: u32,
    /// Accumulator for computing luminance variance within the current window.
    signal_detect_buf: Vec<f32>,
    /// Counts consecutive scan lines whose correlation with the previous
    /// line falls below `LINE_CORR_NOISE_THRESHOLD`. When it reaches
    /// `LINE_CORR_NOISE_LINES` the decoder auto-finalizes the in-progress
    /// image (carrier dropped / tx ended without an APT stop tone).
    low_corr_lines: u32,
    /// Current rig dial frequency in Hz (for image filenames).
    freq_hz: u64,
    /// Current rig mode name (for image filenames).
    mode: String,
}

impl WefaxDecoder {
    pub fn new(input_sample_rate: u32, config: WefaxConfig) -> Self {
        Self {
            resampler: Resampler::new(input_sample_rate),
            demodulator: FmDiscriminator::new(
                INTERNAL_RATE,
                config.center_freq_hz,
                config.deviation_hz,
            ),
            tone_detector: ToneDetector::new(INTERNAL_RATE),
            config,
            state: State::Idle,
            phasing: None,
            slicer: None,
            image: None,
            sample_count: 0,
            reception_start_ms: None,
            sent_idle_event: false,
            signal_detect_count: 0,
            signal_detect_buf: Vec::with_capacity(INTERNAL_RATE as usize / 2),
            low_corr_lines: 0,
            freq_hz: 0,
            mode: String::new(),
        }
    }

    /// Process a block of PCM audio samples (mono, at the input sample rate).
    ///
    /// Returns any events generated during processing.
    pub fn process_samples(&mut self, samples: &[f32]) -> Vec<WefaxEvent> {
        self.sample_count += samples.len() as u64;
        let mut events = Vec::new();

        // Emit an initial "Idle" state event so the frontend knows the decoder is processing audio.
        if !self.sent_idle_event {
            self.sent_idle_event = true;
            let ioc = self.config.ioc.unwrap_or(576);
            let lpm = self.config.lpm.unwrap_or(120);
            events.push(self.state_event("Idle \u{2014} scanning", ioc, lpm));
        }

        // Step 1: Resample to internal rate.
        let resampled = self.resampler.process(samples);

        // Step 2: FM demodulate to get luminance values.
        let luminance = self.demodulator.process(&resampled);

        // Periodic luminance stats for diagnostics (every ~5 seconds at 11025 Hz).
        if self.sample_count % (INTERNAL_RATE as u64 * 5) < samples.len() as u64
            && !luminance.is_empty()
        {
            let min = luminance.iter().cloned().fold(f32::INFINITY, f32::min);
            let max = luminance.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let mean = luminance.iter().sum::<f32>() / luminance.len() as f32;
            trace!(
                min = format!("{:.3}", min),
                max = format!("{:.3}", max),
                mean = format!("{:.3}", mean),
                n = luminance.len(),
                state = ?self.state,
                "WEFAX luminance stats"
            );
        }

        // Step 3: Run APT detector on demodulated luminance (transition counting).
        let tone_results = self.tone_detector.process(&luminance);

        // Step 4: Process based on current state.
        match self.state.clone() {
            State::Idle => {
                // Look for APT start tone first.
                for result in &tone_results {
                    if let Some(tone) = result.tone {
                        match tone {
                            AptTone::Start576 => {
                                events.push(self.transition_to_start_detected(576));
                                break;
                            }
                            AptTone::Start288 => {
                                events.push(self.transition_to_start_detected(288));
                                break;
                            }
                            AptTone::Stop => {} // Ignore stop in idle.
                        }
                    }
                }

                // Fallback: detect active WEFAX signal by luminance variance.
                // Like fldigi's "strong image signal" detection — if we see
                // sustained modulated signal, auto-start receiving with defaults.
                if self.state == State::Idle {
                    self.signal_detect_buf.extend_from_slice(&luminance);
                    let window_size = INTERNAL_RATE as usize / 2;
                    while self.signal_detect_buf.len() >= window_size {
                        let window = &self.signal_detect_buf[..window_size];
                        let mean = window.iter().sum::<f32>() / window.len() as f32;
                        let variance = window
                            .iter()
                            .map(|&v| {
                                let d = v - mean;
                                d * d
                            })
                            .sum::<f32>()
                            / window.len() as f32;
                        let stddev = variance.sqrt();

                        if stddev > SIGNAL_DETECT_MIN_STDDEV {
                            self.signal_detect_count += 1;
                            trace!(
                                stddev = format!("{:.4}", stddev),
                                count = self.signal_detect_count,
                                "WEFAX signal detected"
                            );
                        } else {
                            self.signal_detect_count = 0;
                        }

                        if self.signal_detect_count >= SIGNAL_DETECT_WINDOWS {
                            let ioc = self.config.ioc.unwrap_or(576);
                            let lpm = self.config.lpm.unwrap_or(120);
                            debug!(ioc, lpm, "WEFAX: auto-start from signal detection");
                            self.reception_start_ms = Some(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as i64,
                            );
                            self.signal_detect_buf.clear();
                            events.push(self.transition_to_receiving(ioc, lpm, 0));
                            break;
                        }

                        self.signal_detect_buf.drain(..window_size);
                    }
                }
            }

            State::StartDetected { ioc } => {
                // Wait for tone to end (no more start tone detected), then
                // transition to phasing.
                let still_start = tone_results
                    .iter()
                    .any(|r| matches!(r.tone, Some(AptTone::Start576 | AptTone::Start288)));

                if !still_start {
                    events.push(self.transition_to_phasing(ioc));
                }
            }

            State::Phasing { ioc, lpm } => {
                // Check for stop tone (abort).
                if tone_results.iter().any(|r| r.tone == Some(AptTone::Stop)) {
                    self.transition_to_idle();
                    return events;
                }

                if let Some(ref mut phasing) = self.phasing {
                    if let Some(offset) = phasing.process(&luminance) {
                        events.push(self.transition_to_receiving(ioc, lpm, offset));
                    }
                }
            }

            State::Receiving { ioc, lpm } => {
                // Check for stop tone.
                if tone_results.iter().any(|r| r.tone == Some(AptTone::Stop)) {
                    self.state = State::Stopping { ioc, lpm };
                    events.extend(self.finalize_image(ioc, lpm));
                    self.transition_to_idle();
                    return events;
                }

                // Feed luminance to line slicer.
                let mut carrier_lost = false;
                if let Some(ref mut slicer) = self.slicer {
                    let new_lines = slicer.process(&luminance);
                    for line in new_lines {
                        if let Some(ref mut image) = self.image {
                            // Line-to-line correlation watchdog: real imagery
                            // has highly correlated adjacent lines; pure noise
                            // does not. If correlation stays low for
                            // `LINE_CORR_NOISE_LINES` consecutive lines, the
                            // carrier is gone and we finalize (fldigi-style).
                            if let Some(r) = image.correlation_with_last(&line) {
                                if r < LINE_CORR_NOISE_THRESHOLD {
                                    self.low_corr_lines += 1;
                                    trace!(
                                        r = format!("{:.3}", r),
                                        count = self.low_corr_lines,
                                        "WEFAX low line-correlation"
                                    );
                                } else {
                                    self.low_corr_lines = 0;
                                }
                            }
                            // Flat lines (correlation == None) don't advance
                            // the counter but also don't reset it — an image
                            // with a solid band surrounded by noise still
                            // trips the watchdog once the noise resumes.

                            image.push_line(line);
                            let count = image.line_count();

                            if self.low_corr_lines >= LINE_CORR_NOISE_LINES {
                                debug!(
                                    lines = count,
                                    "WEFAX: line correlation lost — auto-finalizing image"
                                );
                                carrier_lost = true;
                                break;
                            }

                            // Emit progress event.
                            if self.config.emit_progress && count % PROGRESS_INTERVAL == 0 {
                                let line_data =
                                    image.last_line().map(|l| l.to_vec()).unwrap_or_default();
                                let b64 =
                                    base64::engine::general_purpose::STANDARD.encode(&line_data);
                                events.push(WefaxEvent::Progress(
                                    WefaxProgress {
                                        rig_id: None,
                                        line_count: count,
                                        lpm,
                                        ioc,
                                        pixels_per_line: WefaxConfig::pixels_per_line(ioc),
                                        line_data: Some(b64),
                                        state: None,
                                    },
                                    line_data,
                                ));
                            }
                        }
                    }
                }

                if carrier_lost {
                    events.extend(self.finalize_image(ioc, lpm));
                    self.transition_to_idle();
                    return events;
                }
            }

            State::Stopping { .. } => {
                // Already handled, transition back to idle.
                self.transition_to_idle();
            }
        }

        events
    }

    /// Reset the decoder.  Saves the in-progress image (if any) before
    /// returning to Idle.  Returns any completion events produced.
    pub fn reset(&mut self) -> Vec<WefaxEvent> {
        let events = match self.state {
            State::Receiving { ioc, lpm } | State::Phasing { ioc, lpm } => {
                self.finalize_image(ioc, lpm)
            }
            _ => Vec::new(),
        };
        self.state = State::Idle;
        self.resampler.reset();
        self.demodulator.reset();
        self.tone_detector.reset();
        self.phasing = None;
        self.slicer = None;
        self.image = None;
        self.sample_count = 0;
        self.reception_start_ms = None;
        self.sent_idle_event = false;
        self.signal_detect_count = 0;
        self.signal_detect_buf.clear();
        self.low_corr_lines = 0;
        events
    }

    /// Update the current rig tuning (used for image filenames).
    pub fn set_tuning(&mut self, freq_hz: u64, mode: &str) {
        self.freq_hz = freq_hz;
        self.mode = mode.to_string();
    }

    /// Check if the decoder is currently receiving an image.
    pub fn is_receiving(&self) -> bool {
        matches!(self.state, State::Phasing { .. } | State::Receiving { .. })
    }

    fn state_event(&self, label: &str, ioc: u16, lpm: u16) -> WefaxEvent {
        WefaxEvent::Progress(
            WefaxProgress {
                rig_id: None,
                line_count: 0,
                lpm,
                ioc,
                pixels_per_line: WefaxConfig::pixels_per_line(ioc),
                line_data: None,
                state: Some(label.to_string()),
            },
            Vec::new(),
        )
    }

    fn transition_to_start_detected(&mut self, ioc: u16) -> WefaxEvent {
        let ioc = self.config.ioc.unwrap_or(ioc);
        debug!(ioc, "WEFAX: APT start detected");
        self.state = State::StartDetected { ioc };
        self.reception_start_ms = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        );
        let lpm = self.config.lpm.unwrap_or(120);
        self.state_event(&format!("APT Start {}", ioc), ioc, lpm)
    }

    fn transition_to_phasing(&mut self, ioc: u16) -> WefaxEvent {
        let lpm = self.config.lpm.unwrap_or(120); // Default 120 LPM.
        debug!(ioc, lpm, "WEFAX: entering phasing");
        self.tone_detector.reset();
        self.phasing = Some(PhasingDetector::new(lpm, INTERNAL_RATE));
        self.demodulator.reset();
        self.state = State::Phasing { ioc, lpm };
        self.state_event("Phasing", ioc, lpm)
    }

    fn transition_to_receiving(&mut self, ioc: u16, lpm: u16, phase_offset: usize) -> WefaxEvent {
        debug!(ioc, lpm, phase_offset, "WEFAX: entering receiving");
        let ppl = WefaxConfig::pixels_per_line(ioc) as usize;
        self.slicer = Some(LineSlicer::new(lpm, ioc, INTERNAL_RATE, phase_offset));
        self.image = Some(ImageAssembler::new(ppl));
        self.tone_detector.reset();
        self.low_corr_lines = 0;
        self.state = State::Receiving { ioc, lpm };
        self.state_event("Receiving", ioc, lpm)
    }

    fn transition_to_idle(&mut self) {
        self.state = State::Idle;
        self.phasing = None;
        self.slicer = None;
        // image is kept until finalize_image is called or next reception starts.
        self.tone_detector.reset();
        self.signal_detect_count = 0;
        self.signal_detect_buf.clear();
        self.low_corr_lines = 0;
    }

    fn finalize_image(&mut self, ioc: u16, lpm: u16) -> Vec<WefaxEvent> {
        let mut events = Vec::new();

        if let Some(ref image) = self.image {
            let lines = image.line_count();
            if lines == 0 {
                return events;
            }
            if lines < MIN_IMAGE_LINES {
                debug!(
                    lines,
                    min = MIN_IMAGE_LINES,
                    "WEFAX: discarding short image (likely false auto-start)"
                );
                self.image = None;
                self.reception_start_ms = None;
                return events;
            }

            let ppl = WefaxConfig::pixels_per_line(ioc);
            let mut path_str = None;

            // Save PNG if output directory is configured.
            if let Some(ref dir) = self.config.output_dir {
                let output_path = PathBuf::from(dir);
                match image.save_png(&output_path, self.freq_hz, &self.mode) {
                    Ok(p) => {
                        path_str = Some(p.to_string_lossy().into_owned());
                    }
                    Err(e) => {
                        // Log the error but still emit the completion event.
                        eprintln!("WEFAX: failed to save PNG: {}", e);
                    }
                }
            }

            events.push(WefaxEvent::Complete(WefaxMessage {
                rig_id: None,
                ts_ms: self.reception_start_ms,
                line_count: image.line_count(),
                lpm,
                ioc,
                pixels_per_line: ppl,
                path: path_str,
                complete: true,
            }));
        }

        self.image = None;
        self.reception_start_ms = None;
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Generate an FM-modulated WEFAX APT start signal.
    ///
    /// The APT start signal alternates between black (1500 Hz) and white
    /// (2300 Hz) at the given transition rate, FM-modulated onto the 1900 Hz
    /// subcarrier.
    fn generate_apt_start(trans_freq: f32, sample_rate: u32, duration_s: f32) -> Vec<f32> {
        let n = (sample_rate as f32 * duration_s) as usize;
        let center = 1900.0f32;
        let deviation = 400.0f32;
        let mut phase = 0.0f64;
        (0..n)
            .map(|i| {
                // Square wave modulation at trans_freq.
                let t = i as f32 / sample_rate as f32;
                let mod_sign = if (2.0 * PI * trans_freq * t).sin() >= 0.0 {
                    1.0
                } else {
                    -1.0
                };
                let inst_freq = center + deviation * mod_sign;
                phase += 2.0 * std::f64::consts::PI * inst_freq as f64 / sample_rate as f64;
                phase.sin() as f32
            })
            .collect()
    }

    #[test]
    fn decoder_starts_idle() {
        let dec = WefaxDecoder::new(48000, WefaxConfig::default());
        assert_eq!(dec.state, State::Idle);
        assert!(!dec.is_receiving());
    }

    #[test]
    fn decoder_detects_start_tone() {
        let mut dec = WefaxDecoder::new(11025, WefaxConfig::default());
        // Feed 3 seconds of APT start signal (300 transitions/s, IOC 576)
        // at internal sample rate (bypass resampler).
        let signal = generate_apt_start(300.0, 11025, 3.0);
        dec.process_samples(&signal);
        assert!(
            matches!(
                dec.state,
                State::StartDetected { ioc: 576 } | State::Phasing { ioc: 576, .. }
            ),
            "state should be StartDetected or Phasing, got {:?}",
            dec.state
        );
    }

    #[test]
    fn decoder_reset_returns_to_idle() {
        let mut dec = WefaxDecoder::new(48000, WefaxConfig::default());
        dec.state = State::Receiving { ioc: 576, lpm: 120 };
        dec.reset();
        assert_eq!(dec.state, State::Idle);
    }
}
