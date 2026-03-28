// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! QPSK demodulator for Meteor-M LRPT.
//!
//! Meteor-M transmits LRPT at 72 kbps using offset-QPSK modulation on a
//! ~137 MHz carrier.  The symbol rate is 72000 symbols/sec.
//!
//! This module implements:
//! - Costas loop for carrier and phase recovery
//! - Gardner timing error detector for symbol synchronisation
//! - Soft-decision symbol output (±1.0 for I and Q)

use num_complex::Complex;

const SYMBOL_RATE: f64 = 72_000.0;

/// QPSK demodulator with carrier and timing recovery.
pub struct QpskDemod {
    /// Samples per symbol.
    sps: f64,
    /// NCO phase (radians).
    nco_phase: f64,
    /// NCO frequency offset estimate (radians/sample).
    nco_freq: f64,
    /// Costas loop bandwidth parameter.
    costas_alpha: f64,
    costas_beta: f64,
    /// Symbol timing accumulator (fractional sample position).
    timing_accum: f64,
    /// Gardner TED state.
    prev_sample: Complex<f32>,
    mid_sample: Complex<f32>,
    /// Soft symbol output buffer.
    out: Vec<f32>,
}

impl QpskDemod {
    pub fn new(sample_rate: u32) -> Self {
        let sps = sample_rate as f64 / SYMBOL_RATE;
        // Costas loop BW ~ 0.01 of symbol rate
        let bw = 0.01;
        let damp = 0.707;
        let alpha = 4.0 * damp * bw / (1.0 + 2.0 * damp * bw + bw * bw);
        let beta = 4.0 * bw * bw / (1.0 + 2.0 * damp * bw + bw * bw);

        Self {
            sps,
            nco_phase: 0.0,
            nco_freq: 0.0,
            costas_alpha: alpha,
            costas_beta: beta,
            timing_accum: 0.0,
            prev_sample: Complex::new(0.0, 0.0),
            mid_sample: Complex::new(0.0, 0.0),
            out: Vec::new(),
        }
    }

    /// Push raw baseband samples; returns soft symbol pairs (I, Q interleaved).
    pub fn push(&mut self, samples: &[f32]) -> Vec<f32> {
        self.out.clear();

        for &s in samples {
            // Mix with NCO to remove carrier offset
            let lo =
                Complex::new(self.nco_phase.cos() as f32, (-self.nco_phase.sin()) as f32);
            let mixed = Complex::new(s, 0.0) * lo;

            // Symbol timing via Gardner TED
            self.timing_accum += 1.0;

            if self.timing_accum >= self.sps {
                self.timing_accum -= self.sps;

                // Costas loop phase error (QPSK: sgn(I)*Q - sgn(Q)*I)
                let phase_err = mixed.re.signum() * mixed.im - mixed.im.signum() * mixed.re;

                // Update NCO
                self.nco_freq += self.costas_beta * phase_err as f64;
                self.nco_phase += self.costas_alpha * phase_err as f64;

                // Gardner TED for timing
                let ted_err = self.mid_sample.re * (self.prev_sample.re - mixed.re)
                    + self.mid_sample.im * (self.prev_sample.im - mixed.im);
                self.timing_accum += 0.5 * ted_err as f64;

                // Output soft symbols
                self.out.push(mixed.re);
                self.out.push(mixed.im);

                self.prev_sample = mixed;
            } else if (self.timing_accum - self.sps / 2.0).abs() < 0.5 {
                self.mid_sample = mixed;
            }

            // Advance NCO
            self.nco_phase += self.nco_freq;
            if self.nco_phase > std::f64::consts::TAU {
                self.nco_phase -= std::f64::consts::TAU;
            } else if self.nco_phase < 0.0 {
                self.nco_phase += std::f64::consts::TAU;
            }
        }

        std::mem::take(&mut self.out)
    }

    pub fn reset(&mut self) {
        self.nco_phase = 0.0;
        self.nco_freq = 0.0;
        self.timing_accum = 0.0;
        self.prev_sample = Complex::new(0.0, 0.0);
        self.mid_sample = Complex::new(0.0, 0.0);
        self.out.clear();
    }
}
