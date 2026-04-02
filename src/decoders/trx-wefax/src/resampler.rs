// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Polyphase rational resampler: 48000 Hz → 11025 Hz.
//!
//! Ratio: 11025/48000 = 441/1920 (after GCD reduction).
//! Uses a polyphase FIR filter bank to avoid computing the full upsampled
//! signal, consistent with `docs/Optimization-Guidelines.md`.

/// Internal processing sample rate.
pub const INTERNAL_RATE: u32 = 11025;

/// Default input sample rate.
pub const DEFAULT_INPUT_RATE: u32 = 48000;

/// Polyphase rational resampler.
pub struct Resampler {
    /// Interpolation factor (numerator of the ratio).
    up: usize,
    /// Decimation factor (denominator of the ratio).
    down: usize,
    /// Number of taps per polyphase sub-filter.
    taps_per_phase: usize,
    /// Polyphase filter bank: `up` sub-filters, each with `taps_per_phase` taps.
    bank: Vec<Vec<f32>>,
    /// Input history buffer for FIR convolution.
    history: Vec<f32>,
    /// Current phase accumulator (tracks position in the up-sampled domain).
    phase: usize,
}

impl Resampler {
    /// Create a resampler from `input_rate` to [`INTERNAL_RATE`].
    pub fn new(input_rate: u32) -> Self {
        let g = gcd(INTERNAL_RATE as usize, input_rate as usize);
        let up = INTERNAL_RATE as usize / g;
        let down = input_rate as usize / g;

        // Design a low-pass FIR prototype for the upsampled rate.
        // The upsampled rate is `input_rate * up`. The output is then
        // decimated by `down`. The anti-alias cutoff should be at
        // `min(input_rate, output_rate) / 2`, which in normalized terms
        // (relative to the upsampled rate) is `0.5 / max(up, down)`.
        // Use 0.45 instead of 0.5 for transition band headroom.
        let num_taps = up * 16 + 1; // ~16 taps per phase
        let cutoff = 0.5 / (up.max(down) as f64);
        let prototype = design_lowpass(num_taps, cutoff, up as f64);

        // Split prototype into polyphase bank.
        let taps_per_phase = prototype.len().div_ceil(up);
        let mut bank = vec![vec![0.0f32; taps_per_phase]; up];
        for (i, &coeff) in prototype.iter().enumerate() {
            let phase = i % up;
            let tap = i / up;
            bank[phase][tap] = coeff;
        }

        // Normalize: each output sample comes from one sub-filter convolved
        // with the input history. For unity DC gain, each sub-filter's sum
        // must equal 1.0.
        for sub in &mut bank {
            let sub_sum: f64 = sub.iter().map(|&c| c as f64).sum();
            if sub_sum.abs() > 1e-12 {
                let scale = (1.0 / sub_sum) as f32;
                for c in sub.iter_mut() {
                    *c *= scale;
                }
            }
        }

        let history = vec![0.0f32; taps_per_phase];

        Self {
            up,
            down,
            taps_per_phase,
            bank,
            history,
            phase: 0,
        }
    }

    /// Process a block of input samples, returning resampled output.
    #[allow(clippy::needless_range_loop)]
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len() * self.up / self.down + 2);

        for &sample in input {
            // Shift sample into history (newest at end).
            self.history.copy_within(1.., 0);
            self.history[self.taps_per_phase - 1] = sample;

            // Generate output samples for all phases that map to this input.
            while self.phase < self.up {
                let coeffs = &self.bank[self.phase];
                let mut acc = 0.0f32;
                for k in 0..self.taps_per_phase {
                    // History is stored newest-last, coefficients are indexed
                    // from newest to oldest (matching the polyphase decomposition).
                    acc += coeffs[k] * self.history[self.taps_per_phase - 1 - k];
                }
                output.push(acc);
                self.phase += self.down;
            }
            self.phase -= self.up;
        }

        output
    }

    /// Reset internal state (call on frequency change / decoder reset).
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.phase = 0;
    }
}

/// Design a windowed-sinc low-pass FIR filter.
#[allow(clippy::needless_range_loop)]
fn design_lowpass(num_taps: usize, cutoff: f64, gain: f64) -> Vec<f32> {
    let mut coeffs = vec![0.0f32; num_taps];
    let m = num_taps as f64 - 1.0;
    let mid = m / 2.0;

    for i in 0..num_taps {
        let n = i as f64 - mid;
        // Sinc function.
        let sinc = if n.abs() < 1e-12 {
            2.0 * std::f64::consts::PI * cutoff
        } else {
            (2.0 * std::f64::consts::PI * cutoff * n).sin() / n
        };
        // Blackman window.
        let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / m).cos()
            + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / m).cos();
        coeffs[i] = (sinc * w * gain) as f32;
    }

    coeffs
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_ratio_48k_to_11025() {
        let r = Resampler::new(48000);
        // Feed 48000 samples, should get ~11025 out.
        let input: Vec<f32> = vec![0.0; 48000];
        let output = r.clone_and_process(&input);
        // Allow ±2 samples tolerance for edge effects.
        assert!(
            (output.len() as i64 - 11025).unsigned_abs() <= 2,
            "expected ~11025 samples, got {}",
            output.len()
        );
    }

    #[test]
    fn resampler_dc_passthrough() {
        let mut r = Resampler::new(48000);
        // DC signal should pass through with unity gain (after settling).
        let input: Vec<f32> = vec![1.0; 4800];
        let output = r.process(&input);
        // Check last quarter of output is close to 1.0.
        let tail = &output[output.len() * 3 / 4..];
        let avg: f32 = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(
            (avg - 1.0).abs() < 0.02,
            "DC gain mismatch: avg = {}",
            avg
        );
    }

    impl Resampler {
        fn clone_and_process(&self, input: &[f32]) -> Vec<f32> {
            let mut r = Self {
                up: self.up,
                down: self.down,
                taps_per_phase: self.taps_per_phase,
                bank: self.bank.clone(),
                history: self.history.clone(),
                phase: self.phase,
            };
            r.process(input)
        }
    }
}
