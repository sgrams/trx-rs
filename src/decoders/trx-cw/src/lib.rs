// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Goertzel-based CW (Morse code) decoder.
//!
//! Ported from the browser-side JavaScript implementation.

use trx_core::decode::CwEvent;

// ITU Morse code lookup
fn morse_lookup(code: &str) -> Option<char> {
    match code {
        ".-" => Some('A'),
        "-..." => Some('B'),
        "-.-." => Some('C'),
        "-.." => Some('D'),
        "." => Some('E'),
        "..-." => Some('F'),
        "--." => Some('G'),
        "...." => Some('H'),
        ".." => Some('I'),
        ".---" => Some('J'),
        "-.-" => Some('K'),
        ".-.." => Some('L'),
        "--" => Some('M'),
        "-." => Some('N'),
        "---" => Some('O'),
        ".--." => Some('P'),
        "--.-" => Some('Q'),
        ".-." => Some('R'),
        "..." => Some('S'),
        "-" => Some('T'),
        "..-" => Some('U'),
        "...-" => Some('V'),
        ".--" => Some('W'),
        "-..-" => Some('X'),
        "-.--" => Some('Y'),
        "--.." => Some('Z'),
        "-----" => Some('0'),
        ".----" => Some('1'),
        "..---" => Some('2'),
        "...--" => Some('3'),
        "....-" => Some('4'),
        "....." => Some('5'),
        "-...." => Some('6'),
        "--..." => Some('7'),
        "---.." => Some('8'),
        "----." => Some('9'),
        ".-.-.-" => Some('.'),
        "--..--" => Some(','),
        "..--.." => Some('?'),
        ".----." => Some('\''),
        "-.-.--" => Some('!'),
        "-..-." => Some('/'),
        "-.--." => Some('('),
        "-.--.-" => Some(')'),
        ".-..." => Some('&'),
        "---..." => Some(':'),
        "-.-.-." => Some(';'),
        "-...-" => Some('='),
        ".-.-." => Some('+'),
        "-....-" => Some('-'),
        "..--.-" => Some('_'),
        ".-..-." => Some('"'),
        "...-..-" => Some('$'),
        ".--.-." => Some('@'),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Goertzel detector
// ---------------------------------------------------------------------------

fn goertzel_energy(buf: &[f32], coeff: f32) -> f32 {
    let mut s1: f32 = 0.0;
    let mut s2: f32 = 0.0;
    for &sample in buf {
        let s0 = coeff * s1 - s2 + sample;
        s2 = s1;
        s1 = s0;
    }
    let n2 = (buf.len() * buf.len()) as f32;
    (s1 * s1 + s2 * s2 - coeff * s1 * s2) / n2
}

// ---------------------------------------------------------------------------
// Tone scan bins
// ---------------------------------------------------------------------------

const TONE_SET_LOW: u32 = 100;
const TONE_SET_HIGH: u32 = 10_000;
const TONE_SCAN_LOW: u32 = 300;
const TONE_SCAN_HIGH: u32 = 1200;
const TONE_SCAN_STEP: u32 = 25;
const TONE_STABLE_NEEDED: u32 = 3;
const THRESHOLD: f32 = 0.05;

fn tone_high_for_sample_rate(sample_rate: u32, low: u32, high: u32) -> u32 {
    let nyquist = sample_rate / 2;
    if nyquist <= low + 1 {
        low
    } else {
        high.min(nyquist - 1)
    }
}

struct ToneScanBin {
    freq: u32,
    coeff: f32,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub struct CwDecoder {
    sample_rate: u32,
    window_size: usize,
    sample_buf: Vec<f32>,
    sample_idx: usize,

    // Goertzel parameters
    tone_freq: u32,
    coeff: f32,

    // Tone state
    tone_on: bool,
    tone_on_at: f64,
    tone_off_at: f64,
    current_symbol: String,
    sample_counter: u64,

    // WPM
    wpm: u32,

    // Auto control
    auto_tone: bool,
    auto_wpm: bool,

    // Auto tone detection
    tone_scan_bins: Vec<ToneScanBin>,
    tone_stable_bin: i32,
    tone_stable_count: u32,

    // Auto WPM detection
    on_durations: Vec<f64>,

    // Results
    events: Vec<CwEvent>,
}

impl CwDecoder {
    pub fn new(sample_rate: u32) -> Self {
        let window_ms = 10;
        let window_size = (sample_rate as usize * window_ms) / 1000;
        let default_tone = 700u32;
        let k = (default_tone as f32 * window_size as f32 / sample_rate as f32).round();
        let omega = (2.0 * std::f32::consts::PI * k) / window_size as f32;
        let coeff = 2.0 * omega.cos();

        // Build scan bins
        let mut tone_scan_bins = Vec::new();
        let mut f = TONE_SCAN_LOW;
        let scan_high = tone_high_for_sample_rate(sample_rate, TONE_SCAN_LOW, TONE_SCAN_HIGH);
        while f <= scan_high {
            let bk = (f as f32 * window_size as f32 / sample_rate as f32).round();
            let b_omega = (2.0 * std::f32::consts::PI * bk) / window_size as f32;
            tone_scan_bins.push(ToneScanBin {
                freq: f,
                coeff: 2.0 * b_omega.cos(),
            });
            f += TONE_SCAN_STEP;
        }

        Self {
            sample_rate,
            window_size,
            sample_buf: vec![0.0f32; window_size],
            sample_idx: 0,
            tone_freq: default_tone,
            coeff,
            tone_on: false,
            tone_on_at: 0.0,
            tone_off_at: 0.0,
            current_symbol: String::new(),
            sample_counter: 0,
            wpm: 15,
            auto_tone: true,
            auto_wpm: true,
            tone_scan_bins,
            tone_stable_bin: -1,
            tone_stable_count: 0,
            on_durations: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn set_auto(&mut self, enabled: bool) {
        self.auto_tone = enabled;
        self.auto_wpm = enabled;
    }

    pub fn set_wpm(&mut self, wpm: u32) {
        self.wpm = wpm.clamp(5, 40);
    }

    pub fn set_tone_hz(&mut self, tone_hz: u32) {
        let tone_hz = tone_hz.clamp(
            TONE_SET_LOW,
            tone_high_for_sample_rate(self.sample_rate, TONE_SET_LOW, TONE_SET_HIGH),
        );
        self.recompute_goertzel(tone_hz);
    }

    fn recompute_goertzel(&mut self, new_freq: u32) {
        self.tone_freq = new_freq;
        let k = (new_freq as f32 * self.window_size as f32 / self.sample_rate as f32).round();
        let omega = (2.0 * std::f32::consts::PI * k) / self.window_size as f32;
        self.coeff = 2.0 * omega.cos();
    }

    fn unit_ms(&self) -> f64 {
        1200.0 / self.wpm as f64
    }

    fn now_ms(&self) -> f64 {
        self.sample_counter as f64 * 1000.0 / self.sample_rate as f64
    }

    fn goertzel_detect(&self) -> bool {
        let tone_energy = goertzel_energy(&self.sample_buf, self.coeff);
        let mut total_energy: f32 = 0.0;
        for &s in &self.sample_buf {
            total_energy += s * s;
        }
        let avg_energy = total_energy / self.sample_buf.len() as f32;
        if avg_energy < 1e-10 {
            return false;
        }
        (tone_energy / avg_energy) > THRESHOLD
    }

    fn auto_detect_tone(&mut self) {
        let mut total_energy: f32 = 0.0;
        for &s in &self.sample_buf {
            total_energy += s * s;
        }
        let avg_energy = total_energy / self.sample_buf.len() as f32;
        if avg_energy < 1e-10 {
            return;
        }

        let mut best_idx: i32 = -1;
        let mut best_ratio: f32 = 0.0;
        for (i, bin) in self.tone_scan_bins.iter().enumerate() {
            let e = goertzel_energy(&self.sample_buf, bin.coeff);
            let ratio = e / avg_energy;
            if ratio > best_ratio {
                best_ratio = ratio;
                best_idx = i as i32;
            }
        }

        if best_ratio < THRESHOLD || best_idx < 0 {
            self.tone_stable_count = 0;
            self.tone_stable_bin = -1;
            return;
        }

        if self.tone_stable_bin >= 0 && (best_idx - self.tone_stable_bin).unsigned_abs() <= 1 {
            self.tone_stable_count += 1;
        } else {
            self.tone_stable_bin = best_idx;
            self.tone_stable_count = 1;
        }

        if self.tone_stable_count >= TONE_STABLE_NEEDED {
            let detected_freq = self.tone_scan_bins[self.tone_stable_bin as usize].freq;
            if (detected_freq as i32 - self.tone_freq as i32).unsigned_abs() > TONE_SCAN_STEP {
                self.recompute_goertzel(detected_freq);
            }
        }
    }

    fn auto_detect_wpm(&mut self) {
        if self.on_durations.len() < 8 {
            return;
        }

        let mut sorted: Vec<f64> = self.on_durations.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let mut best_boundary = 1usize;
        let mut best_score = f64::INFINITY;
        for i in 1..sorted.len() {
            let c1 = &sorted[..i];
            let c2 = &sorted[i..];
            let mean1: f64 = c1.iter().sum::<f64>() / c1.len() as f64;
            let mean2: f64 = c2.iter().sum::<f64>() / c2.len() as f64;
            let mut score: f64 = 0.0;
            for &v in c1 {
                score += (v - mean1) * (v - mean1);
            }
            for &v in c2 {
                score += (v - mean2) * (v - mean2);
            }
            if score < best_score {
                best_score = score;
                best_boundary = i;
            }
        }

        let dit_cluster = &sorted[..best_boundary];
        if dit_cluster.is_empty() {
            return;
        }
        let dit_ms = dit_cluster[dit_cluster.len() / 2];
        if dit_ms < 10.0 {
            return;
        }

        let new_wpm = (1200.0 / dit_ms).round() as u32;
        let new_wpm = new_wpm.clamp(5, 40);
        if new_wpm != self.wpm {
            self.wpm = new_wpm;
        }
    }

    fn process_window(&mut self) {
        if self.auto_tone {
            self.auto_detect_tone();
        }

        let detected = self.goertzel_detect();
        let now = self.now_ms();

        // Emit signal state event on transitions
        if detected && !self.tone_on {
            // Tone just turned on
            self.tone_on = true;
            let off_duration = now - self.tone_off_at;
            if self.tone_off_at > 0.0 {
                let u = self.unit_ms();
                if off_duration > u * 5.0 {
                    // Word gap
                    if !self.current_symbol.is_empty() {
                        let ch = morse_lookup(&self.current_symbol).unwrap_or('?');
                        self.emit_event(&ch.to_string());
                        self.current_symbol.clear();
                    }
                    self.emit_event(" ");
                } else if off_duration > u * 2.0 {
                    // Character gap
                    if !self.current_symbol.is_empty() {
                        let ch = morse_lookup(&self.current_symbol).unwrap_or('?');
                        self.emit_event(&ch.to_string());
                        self.current_symbol.clear();
                    }
                }
            }
            self.tone_on_at = now;
            self.emit_event("");
        } else if !detected && self.tone_on {
            // Tone just turned off
            self.tone_on = false;
            let on_duration = now - self.tone_on_at;
            let u = self.unit_ms();
            if on_duration > u * 1.5 {
                self.current_symbol.push('-');
            } else {
                self.current_symbol.push('.');
            }
            self.tone_off_at = now;

            if self.auto_wpm {
                // Collect for auto WPM
                self.on_durations.push(on_duration);
                if self.on_durations.len() > 30 {
                    self.on_durations.drain(..self.on_durations.len() - 30);
                }
                self.auto_detect_wpm();
            }

            self.emit_event("");
        }

        // Flush pending character after long silence
        if !self.tone_on && !self.current_symbol.is_empty() && self.tone_off_at > 0.0 {
            let silence = now - self.tone_off_at;
            if silence > self.unit_ms() * 5.0 {
                let ch = morse_lookup(&self.current_symbol).unwrap_or('?');
                self.emit_event(&ch.to_string());
                self.current_symbol.clear();
            }
        }
    }

    fn emit_event(&mut self, text: &str) {
        self.events.push(CwEvent {
            rig_id: None,
            text: text.to_string(),
            wpm: self.wpm,
            tone_hz: self.tone_freq,
            signal_on: self.tone_on,
        });
    }

    pub fn process_samples(&mut self, samples: &[f32]) -> Vec<CwEvent> {
        for &s in samples {
            self.sample_buf[self.sample_idx] = s;
            self.sample_idx += 1;
            self.sample_counter += 1;
            if self.sample_idx >= self.window_size {
                self.process_window();
                self.sample_idx = 0;
            }
        }
        std::mem::take(&mut self.events)
    }

    pub fn reset(&mut self) {
        let tone = self.tone_freq;
        let wpm = self.wpm;
        let auto_tone = self.auto_tone;
        let auto_wpm = self.auto_wpm;
        self.sample_buf.fill(0.0);
        self.sample_idx = 0;
        self.tone_on = false;
        self.tone_on_at = 0.0;
        self.tone_off_at = 0.0;
        self.current_symbol.clear();
        self.sample_counter = 0;
        self.wpm = wpm;
        self.tone_freq = tone;
        self.auto_tone = auto_tone;
        self.auto_wpm = auto_wpm;
        self.recompute_goertzel(tone);
        self.tone_stable_bin = -1;
        self.tone_stable_count = 0;
        self.on_durations.clear();
        self.events.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::CwDecoder;

    fn tone_samples(sample_rate: u32, freq_hz: f32, ms: u32) -> Vec<f32> {
        let len = (sample_rate as usize * ms as usize) / 1000;
        let step = 2.0 * std::f32::consts::PI * freq_hz / sample_rate as f32;
        (0..len).map(|i| (i as f32 * step).sin() * 0.8).collect()
    }

    fn silence_samples(sample_rate: u32, ms: u32) -> Vec<f32> {
        vec![0.0; (sample_rate as usize * ms as usize) / 1000]
    }

    #[test]
    fn emits_signal_transition_events() {
        let sample_rate = 48_000;
        let mut decoder = CwDecoder::new(sample_rate);
        decoder.set_auto(false);
        decoder.set_wpm(15);
        decoder.set_tone_hz(700);

        let mut input = tone_samples(sample_rate, 700.0, 100);
        input.extend(silence_samples(sample_rate, 500));

        let events = decoder.process_samples(&input);

        assert!(events
            .iter()
            .any(|evt| evt.text.is_empty() && evt.signal_on));
        assert!(events
            .iter()
            .any(|evt| evt.text.is_empty() && !evt.signal_on));
    }

    #[test]
    fn decodes_single_e_from_synthetic_tone() {
        let sample_rate = 48_000;
        let mut decoder = CwDecoder::new(sample_rate);
        decoder.set_auto(false);
        decoder.set_wpm(15);
        decoder.set_tone_hz(700);

        let mut input = tone_samples(sample_rate, 700.0, 100);
        input.extend(silence_samples(sample_rate, 500));

        let events = decoder.process_samples(&input);
        let text: String = events
            .iter()
            .filter(|evt| !evt.text.is_empty())
            .map(|evt| evt.text.as_str())
            .collect();

        assert_eq!(text, "E");
    }
}
