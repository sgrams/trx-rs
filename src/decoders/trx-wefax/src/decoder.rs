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

use crate::config::WefaxConfig;
use crate::demod::FmDiscriminator;
use crate::image::ImageAssembler;
use crate::line_slicer::LineSlicer;
use crate::phase::PhasingDetector;
use crate::resampler::{Resampler, INTERNAL_RATE};
use crate::tone_detect::{AptTone, ToneDetector};

/// Progress events are emitted every this many lines.
const PROGRESS_INTERVAL: u32 = 5;

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
    /// Fallback phasing detector that runs in Idle state to catch ongoing
    /// transmissions when the APT start tone was missed.
    idle_phasing: Option<PhasingDetector>,
    slicer: Option<LineSlicer>,
    image: Option<ImageAssembler>,
    /// Total sample counter for timestamps.
    sample_count: u64,
    /// Timestamp (ms since epoch) when reception started.
    reception_start_ms: Option<i64>,
}

impl WefaxDecoder {
    pub fn new(input_sample_rate: u32, config: WefaxConfig) -> Self {
        let default_lpm = config.lpm.unwrap_or(120);
        Self {
            resampler: Resampler::new(input_sample_rate),
            demodulator: FmDiscriminator::new(
                INTERNAL_RATE,
                config.center_freq_hz,
                config.deviation_hz,
            ),
            tone_detector: ToneDetector::new(INTERNAL_RATE),
            idle_phasing: Some(PhasingDetector::new(default_lpm, INTERNAL_RATE)),
            config,
            state: State::Idle,
            phasing: None,
            slicer: None,
            image: None,
            sample_count: 0,
            reception_start_ms: None,
        }
    }

    /// Process a block of PCM audio samples (mono, at the input sample rate).
    ///
    /// Returns any events generated during processing.
    pub fn process_samples(&mut self, samples: &[f32]) -> Vec<WefaxEvent> {
        self.sample_count += samples.len() as u64;
        let mut events = Vec::new();

        // Step 1: Resample to internal rate.
        let resampled = self.resampler.process(samples);

        // Step 2: FM demodulate to get luminance values.
        let luminance = self.demodulator.process(&resampled);

        // Step 3: Run APT detector on demodulated luminance (transition counting).
        let tone_results = self.tone_detector.process(&luminance);

        // Step 4: Process based on current state.
        match self.state.clone() {
            State::Idle => {
                // Look for APT start tone first.
                let mut got_start = false;
                for result in &tone_results {
                    if let Some(tone) = result.tone {
                        match tone {
                            AptTone::Start576 => {
                                self.idle_phasing = None;
                                self.transition_to_start_detected(576);
                                got_start = true;
                                break;
                            }
                            AptTone::Start288 => {
                                self.idle_phasing = None;
                                self.transition_to_start_detected(288);
                                got_start = true;
                                break;
                            }
                            AptTone::Stop => {} // Ignore stop in idle.
                        }
                    }
                }

                // Fallback: try phasing detection on luminance to catch
                // ongoing transmissions where the start tone was missed.
                if !got_start {
                    if let Some(ref mut idle_ph) = self.idle_phasing {
                        if let Some(offset) = idle_ph.process(&luminance) {
                            let ioc = self.config.ioc.unwrap_or(576);
                            let lpm = self.config.lpm.unwrap_or(120);
                            self.reception_start_ms = Some(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as i64,
                            );
                            self.idle_phasing = None;
                            self.transition_to_receiving(ioc, lpm, offset);
                        }
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
                    self.transition_to_phasing(ioc);
                }
            }

            State::Phasing { ioc, lpm } => {
                // Check for stop tone (abort).
                if tone_results
                    .iter()
                    .any(|r| r.tone == Some(AptTone::Stop))
                {
                    self.transition_to_idle();
                    return events;
                }

                if let Some(ref mut phasing) = self.phasing {
                    if let Some(offset) = phasing.process(&luminance) {
                        self.transition_to_receiving(ioc, lpm, offset);
                    }
                }
            }

            State::Receiving { ioc, lpm } => {
                // Check for stop tone.
                if tone_results
                    .iter()
                    .any(|r| r.tone == Some(AptTone::Stop))
                {
                    self.state = State::Stopping { ioc, lpm };
                    events.extend(self.finalize_image(ioc, lpm));
                    self.transition_to_idle();
                    return events;
                }

                // Feed luminance to line slicer.
                if let Some(ref mut slicer) = self.slicer {
                    let new_lines = slicer.process(&luminance);
                    for line in new_lines {
                        if let Some(ref mut image) = self.image {
                            image.push_line(line);
                            let count = image.line_count();

                            // Emit progress event.
                            if self.config.emit_progress && count % PROGRESS_INTERVAL == 0 {
                                let line_data = image
                                    .last_line()
                                    .map(|l| l.to_vec())
                                    .unwrap_or_default();
                                let b64 = base64::engine::general_purpose::STANDARD
                                    .encode(&line_data);
                                events.push(WefaxEvent::Progress(
                                    WefaxProgress {
                                        rig_id: None,
                                        line_count: count,
                                        lpm,
                                        ioc,
                                        pixels_per_line: WefaxConfig::pixels_per_line(ioc),
                                        line_data: Some(b64),
                                    },
                                    line_data,
                                ));
                            }
                        }
                    }
                }
            }

            State::Stopping { .. } => {
                // Already handled, transition back to idle.
                self.transition_to_idle();
            }
        }

        events
    }

    /// Reset the decoder, discarding any in-progress image.
    pub fn reset(&mut self) {
        let default_lpm = self.config.lpm.unwrap_or(120);
        self.state = State::Idle;
        self.resampler.reset();
        self.demodulator.reset();
        self.tone_detector.reset();
        self.phasing = None;
        self.idle_phasing = Some(PhasingDetector::new(default_lpm, INTERNAL_RATE));
        self.slicer = None;
        self.image = None;
        self.sample_count = 0;
        self.reception_start_ms = None;
    }

    /// Check if the decoder is currently receiving an image.
    pub fn is_receiving(&self) -> bool {
        matches!(
            self.state,
            State::Phasing { .. } | State::Receiving { .. }
        )
    }

    fn transition_to_start_detected(&mut self, ioc: u16) {
        let ioc = self.config.ioc.unwrap_or(ioc);
        self.state = State::StartDetected { ioc };
        self.reception_start_ms = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        );
    }

    fn transition_to_phasing(&mut self, ioc: u16) {
        let lpm = self.config.lpm.unwrap_or(120); // Default 120 LPM.
        self.tone_detector.reset();
        self.phasing = Some(PhasingDetector::new(lpm, INTERNAL_RATE));
        self.demodulator.reset();
        self.state = State::Phasing { ioc, lpm };
    }

    fn transition_to_receiving(&mut self, ioc: u16, lpm: u16, phase_offset: usize) {
        let ppl = WefaxConfig::pixels_per_line(ioc) as usize;
        self.slicer = Some(LineSlicer::new(lpm, ioc, INTERNAL_RATE, phase_offset));
        self.image = Some(ImageAssembler::new(ppl));
        self.tone_detector.reset();
        self.state = State::Receiving { ioc, lpm };
    }

    fn transition_to_idle(&mut self) {
        let default_lpm = self.config.lpm.unwrap_or(120);
        self.state = State::Idle;
        self.phasing = None;
        self.slicer = None;
        // image is kept until finalize_image is called or next reception starts.
        self.tone_detector.reset();
        self.idle_phasing = Some(PhasingDetector::new(default_lpm, INTERNAL_RATE));
    }

    fn finalize_image(&mut self, ioc: u16, lpm: u16) -> Vec<WefaxEvent> {
        let mut events = Vec::new();

        if let Some(ref image) = self.image {
            if image.line_count() == 0 {
                return events;
            }

            let ppl = WefaxConfig::pixels_per_line(ioc);
            let mut path_str = None;

            // Save PNG if output directory is configured.
            if let Some(ref dir) = self.config.output_dir {
                let output_path = PathBuf::from(dir);
                match image.save_png(&output_path, ioc, lpm) {
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
            matches!(dec.state, State::StartDetected { ioc: 576 } | State::Phasing { ioc: 576, .. }),
            "state should be StartDetected or Phasing, got {:?}",
            dec.state
        );
    }

    #[test]
    fn decoder_reset_returns_to_idle() {
        let mut dec = WefaxDecoder::new(48000, WefaxConfig::default());
        dec.state = State::Receiving {
            ioc: 576,
            lpm: 120,
        };
        dec.reset();
        assert_eq!(dec.state, State::Idle);
    }
}
