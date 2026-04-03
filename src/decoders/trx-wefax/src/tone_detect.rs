// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! APT tone detector for WEFAX start/stop signals.
//!
//! Detects three APT signals by counting black↔white transitions in the
//! **demodulated luminance** stream (0.0–1.0):
//! - 300 transitions/s: Start signal for IOC 576
//! - 675 transitions/s: Start signal for IOC 288
//! - 450 transitions/s: Stop signal (end of transmission)
//!
//! This matches the fldigi approach: the APT "tones" are not audio-frequency
//! tones but transition rates in the demodulated FM output.

/// Detected APT tone type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AptTone {
    /// Start tone for IOC 576 (300 transitions/s).
    Start576,
    /// Start tone for IOC 288 (675 transitions/s).
    Start288,
    /// Stop tone (450 transitions/s).
    Stop,
}

impl AptTone {
    /// Return the IOC value associated with this tone, if it's a start tone.
    pub fn ioc(self) -> Option<u16> {
        match self {
            AptTone::Start576 => Some(576),
            AptTone::Start288 => Some(288),
            AptTone::Stop => None,
        }
    }
}

/// Result from the tone detector for a single analysis window.
#[derive(Debug, Clone)]
pub struct ToneDetectResult {
    /// Which tone was detected, if any.
    pub tone: Option<AptTone>,
    /// Duration in seconds the tone has been sustained.
    pub sustained_s: f32,
}

/// Luminance threshold above which a sample is considered "high" (white).
const HIGH_THRESHOLD: f32 = 0.84;
/// Luminance threshold below which a sample is considered "low" (black).
const LOW_THRESHOLD: f32 = 0.16;

/// Frequency tolerance for matching APT frequencies (Hz).
const FREQ_TOLERANCE: u32 = 10;

/// APT transition-counting detector operating on demodulated luminance.
///
/// Counts low→high transitions in half-second windows and compares the
/// resulting frequency against the three APT target frequencies.
pub struct ToneDetector {
    sample_rate: u32,
    /// Analysis window size in samples (~0.5 s).
    window_size: usize,
    /// Number of samples accumulated in the current window.
    sample_count: usize,
    /// Whether the signal is currently in the "high" state.
    is_high: bool,
    /// Number of low→high transitions in the current window.
    transitions: u32,
    /// Currently sustained tone and duration counter.
    current_tone: Option<AptTone>,
    sustained_windows: u32,
    /// Minimum number of consecutive matching windows before confirming.
    min_sustain_windows: u32,
}

impl ToneDetector {
    pub fn new(sample_rate: u32) -> Self {
        let window_size = (sample_rate / 2) as usize; // ~0.5 s window
        let min_sustain_s = 1.5;
        let window_duration_s = window_size as f32 / sample_rate as f32;
        let min_sustain_windows = (min_sustain_s / window_duration_s).ceil() as u32;

        Self {
            sample_rate,
            window_size,
            sample_count: 0,
            is_high: false,
            transitions: 0,
            current_tone: None,
            sustained_windows: 0,
            min_sustain_windows,
        }
    }

    /// Feed **demodulated luminance** samples (0.0 = black, 1.0 = white).
    ///
    /// Returns detection results at the end of each analysis window.
    pub fn process(&mut self, luminance: &[f32]) -> Vec<ToneDetectResult> {
        let mut results = Vec::new();
        for &s in luminance {
            // Track low→high transitions with hysteresis.
            if s > HIGH_THRESHOLD && !self.is_high {
                self.is_high = true;
                self.transitions += 1;
            } else if s < LOW_THRESHOLD && self.is_high {
                self.is_high = false;
            }

            self.sample_count += 1;

            if self.sample_count >= self.window_size {
                results.push(self.analyze_window());
                self.sample_count = 0;
                self.transitions = 0;
            }
        }
        results
    }

    /// Check if a tone has been confirmed (sustained for the minimum duration).
    pub fn confirmed_tone(&self) -> Option<AptTone> {
        if self.sustained_windows >= self.min_sustain_windows {
            self.current_tone
        } else {
            None
        }
    }

    pub fn reset(&mut self) {
        self.sample_count = 0;
        self.transitions = 0;
        self.is_high = false;
        self.current_tone = None;
        self.sustained_windows = 0;
    }

    fn analyze_window(&mut self) -> ToneDetectResult {
        // Compute transition frequency: transitions per second.
        let freq =
            self.transitions * self.sample_rate / self.sample_count.max(1) as u32;

        let detected = classify_freq(freq);

        // Update sustained detection tracking.
        if detected == self.current_tone && detected.is_some() {
            self.sustained_windows += 1;
        } else {
            self.current_tone = detected;
            self.sustained_windows = if detected.is_some() { 1 } else { 0 };
        }

        ToneDetectResult {
            tone: self.confirmed_tone(),
            sustained_s: self.sustained_windows as f32 * self.window_size as f32
                / self.sample_rate as f32,
        }
    }
}

/// Classify a measured transition frequency into an APT tone.
fn classify_freq(freq: u32) -> Option<AptTone> {
    if freq.abs_diff(300) <= FREQ_TOLERANCE {
        Some(AptTone::Start576)
    } else if freq.abs_diff(675) <= FREQ_TOLERANCE {
        Some(AptTone::Start288)
    } else if freq.abs_diff(450) <= FREQ_TOLERANCE {
        Some(AptTone::Stop)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Generate a luminance signal that alternates between black and white
    /// at the given transition frequency (transitions per second).
    fn generate_apt_signal(trans_freq: f32, sample_rate: u32, duration_s: f32) -> Vec<f32> {
        let n = (sample_rate as f32 * duration_s) as usize;
        (0..n)
            .map(|i| {
                // Square wave at trans_freq Hz: above 0 → white, below 0 → black.
                let phase = (2.0 * PI * trans_freq * i as f32 / sample_rate as f32).sin();
                if phase >= 0.0 { 1.0 } else { 0.0 }
            })
            .collect()
    }

    #[test]
    fn detect_start_576_tone() {
        let sr = 11025;
        let mut det = ToneDetector::new(sr);
        let signal = generate_apt_signal(300.0, sr, 3.0);
        let results = det.process(&signal);
        let confirmed = results.iter().any(|r| r.tone == Some(AptTone::Start576));
        assert!(confirmed, "should detect 300 Hz APT start for IOC 576");
    }

    #[test]
    fn detect_start_288_tone() {
        let sr = 11025;
        let mut det = ToneDetector::new(sr);
        let signal = generate_apt_signal(675.0, sr, 3.0);
        let results = det.process(&signal);
        let confirmed = results.iter().any(|r| r.tone == Some(AptTone::Start288));
        assert!(confirmed, "should detect 675 Hz APT start for IOC 288");
    }

    #[test]
    fn detect_stop_tone() {
        let sr = 11025;
        let mut det = ToneDetector::new(sr);
        let signal = generate_apt_signal(450.0, sr, 3.0);
        let results = det.process(&signal);
        let confirmed = results.iter().any(|r| r.tone == Some(AptTone::Stop));
        assert!(confirmed, "should detect 450 Hz APT stop tone");
    }

    #[test]
    fn no_false_detect_on_silence() {
        let sr = 11025;
        let mut det = ToneDetector::new(sr);
        let silence = vec![0.5f32; sr as usize * 3]; // mid-grey, no transitions
        let results = det.process(&silence);
        assert!(
            results.iter().all(|r| r.tone.is_none()),
            "should not detect any tone on constant signal"
        );
    }

    #[test]
    fn no_false_detect_on_image_data() {
        let sr = 11025;
        let mut det = ToneDetector::new(sr);
        // Simulate random-ish image data (varying luminance, no consistent frequency).
        let n = sr as usize * 3;
        let signal: Vec<f32> = (0..n)
            .map(|i| {
                // Mix of frequencies that don't match any APT tone.
                let t = i as f32 / sr as f32;
                (0.5 + 0.3 * (2.0 * PI * 137.0 * t).sin()
                    + 0.2 * (2.0 * PI * 523.0 * t).sin())
                .clamp(0.0, 1.0)
            })
            .collect();
        let results = det.process(&signal);
        assert!(
            results.iter().all(|r| r.tone.is_none()),
            "should not detect APT tone in random image data"
        );
    }
}
