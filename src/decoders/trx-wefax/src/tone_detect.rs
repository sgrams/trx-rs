// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Goertzel-based APT tone detector for WEFAX start/stop signals.
//!
//! Detects three tones:
//! - 300 Hz: Start tone for IOC 576
//! - 675 Hz: Start tone for IOC 288
//! - 450 Hz: Stop tone (end of transmission)
//!
//! Uses the same Goertzel pattern as `trx-cw`.

/// Detected APT tone type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AptTone {
    /// Start tone for IOC 576 (300 Hz).
    Start576,
    /// Start tone for IOC 288 (675 Hz).
    Start288,
    /// Stop tone (450 Hz).
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

/// Goertzel tone detector for APT start/stop signals.
pub struct ToneDetector {
    sample_rate: f32,
    /// Goertzel analysis window size in samples (~200 ms).
    window_size: usize,
    /// Accumulated samples for the current window.
    buffer: Vec<f32>,
    /// Goertzel coefficients for each target frequency.
    coeffs: [GoertzelCoeff; 3],
    /// Currently sustained tone and duration counter.
    current_tone: Option<AptTone>,
    sustained_windows: u32,
    /// Minimum sustained detection time in windows before confirming.
    min_sustain_windows: u32,
    /// SNR threshold for tone detection (energy ratio vs broadband).
    snr_threshold: f32,
}

struct GoertzelCoeff {
    tone: AptTone,
    coeff: f32, // 2 * cos(2π * freq / sample_rate * N) — but we use the standard form
    #[allow(dead_code)]
    freq: f32,
}

impl ToneDetector {
    pub fn new(sample_rate: u32) -> Self {
        let window_size = (sample_rate as f32 * 0.2) as usize; // ~200 ms
        let min_sustain_s = 1.5;
        let window_duration_s = window_size as f32 / sample_rate as f32;
        let min_sustain_windows = (min_sustain_s / window_duration_s).ceil() as u32;

        let coeffs = [
            GoertzelCoeff::new(AptTone::Start576, 300.0, sample_rate, window_size),
            GoertzelCoeff::new(AptTone::Start288, 675.0, sample_rate, window_size),
            GoertzelCoeff::new(AptTone::Stop, 450.0, sample_rate, window_size),
        ];

        Self {
            sample_rate: sample_rate as f32,
            window_size,
            buffer: Vec::with_capacity(window_size),
            coeffs,
            current_tone: None,
            sustained_windows: 0,
            min_sustain_windows,
            snr_threshold: 10.0, // tone must be 10× broadband energy
        }
    }

    /// Feed audio samples (luminance values from FM discriminator are NOT
    /// suitable; feed the raw resampled audio before demodulation).
    pub fn process(&mut self, samples: &[f32]) -> Vec<ToneDetectResult> {
        let mut results = Vec::new();
        for &s in samples {
            self.buffer.push(s);
            if self.buffer.len() >= self.window_size {
                results.push(self.analyze_window());
                self.buffer.clear();
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
        self.buffer.clear();
        self.current_tone = None;
        self.sustained_windows = 0;
    }

    fn analyze_window(&mut self) -> ToneDetectResult {
        let samples = &self.buffer;

        // Compute broadband energy (RMS²).
        let broadband: f32 = samples.iter().map(|&s| s * s).sum::<f32>() / samples.len() as f32;

        // Find the strongest tone above the SNR threshold.
        let mut best: Option<(AptTone, f32)> = None;
        for gc in &self.coeffs {
            let energy = goertzel_energy(samples, gc.coeff);
            let normalized = energy / samples.len() as f32;
            if broadband > 1e-12 && normalized / broadband > self.snr_threshold
                && best.is_none_or(|(_, e)| normalized > e) {
                    best = Some((gc.tone, normalized));
                }
        }

        let detected = best.map(|(tone, _)| tone);

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
                / self.sample_rate,
        }
    }
}

impl GoertzelCoeff {
    fn new(tone: AptTone, freq: f32, sample_rate: u32, window_size: usize) -> Self {
        let k = (freq * window_size as f32 / sample_rate as f32).round();
        let coeff = 2.0 * (2.0 * std::f32::consts::PI * k / window_size as f32).cos();
        Self { tone, coeff, freq }
    }
}

/// Standard Goertzel algorithm returning magnitude² at the target bin.
fn goertzel_energy(samples: &[f32], coeff: f32) -> f32 {
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;

    for &x in samples {
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }

    // Magnitude² = s1² + s2² - coeff·s1·s2
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn generate_tone(freq: f32, sample_rate: u32, duration_s: f32) -> Vec<f32> {
        let n = (sample_rate as f32 * duration_s) as usize;
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f32 / sample_rate as f32).sin())
            .collect()
    }

    #[test]
    fn detect_start_576_tone() {
        let sr = 11025;
        let mut det = ToneDetector::new(sr);
        let tone = generate_tone(300.0, sr, 3.0); // 3 seconds of 300 Hz
        let results = det.process(&tone);
        let confirmed = results.iter().any(|r| r.tone == Some(AptTone::Start576));
        assert!(confirmed, "should detect 300 Hz start tone for IOC 576");
    }

    #[test]
    fn detect_start_288_tone() {
        let sr = 11025;
        let mut det = ToneDetector::new(sr);
        let tone = generate_tone(675.0, sr, 3.0);
        let results = det.process(&tone);
        let confirmed = results.iter().any(|r| r.tone == Some(AptTone::Start288));
        assert!(confirmed, "should detect 675 Hz start tone for IOC 288");
    }

    #[test]
    fn detect_stop_tone() {
        let sr = 11025;
        let mut det = ToneDetector::new(sr);
        let tone = generate_tone(450.0, sr, 3.0);
        let results = det.process(&tone);
        let confirmed = results.iter().any(|r| r.tone == Some(AptTone::Stop));
        assert!(confirmed, "should detect 450 Hz stop tone");
    }

    #[test]
    fn no_false_detect_on_silence() {
        let sr = 11025;
        let mut det = ToneDetector::new(sr);
        let silence = vec![0.0f32; sr as usize * 3];
        let results = det.process(&silence);
        assert!(
            results.iter().all(|r| r.tone.is_none()),
            "should not detect any tone in silence"
        );
    }
}
