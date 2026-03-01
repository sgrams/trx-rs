// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

mod am;
mod fm;
mod math;
mod math_arm;
mod math_x86;
mod ssb;
mod wfm;

use num_complex::Complex;
use trx_core::rig::state::RigMode;

pub use self::wfm::WfmStereoDecoder;

/// Shared DC blocker used by narrowband and WFM audio paths.
#[derive(Debug, Clone)]
pub(crate) struct DcBlocker {
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlocker {
    pub(crate) fn new(r: f32) -> Self {
        Self {
            r: r.clamp(0.9, 0.9999),
            x1: 0.0,
            y1: 0.0,
        }
    }

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }

    pub(crate) fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

/// Soft AGC with a fast-attack / slow-release envelope follower.
///
/// Tracks the signal envelope and adjusts gain so the output level converges
/// toward `target`. Gain decreases quickly when the signal is louder than
/// the target and recovers slowly during quieter periods.
#[derive(Debug, Clone)]
pub(crate) struct SoftAgc {
    gain: f32,
    envelope: f32,
    attack_coeff: f32,
    release_coeff: f32,
    target: f32,
    max_gain: f32,
}

impl SoftAgc {
    /// Create a new `SoftAgc`.
    pub(crate) fn new(
        sample_rate: f32,
        attack_ms: f32,
        release_ms: f32,
        target: f32,
        max_gain_db: f32,
    ) -> Self {
        let sr = sample_rate.max(1.0);
        let attack_coeff = 1.0 - (-1.0 / (attack_ms * 1e-3 * sr)).exp();
        let release_coeff = 1.0 - (-1.0 / (release_ms * 1e-3 * sr)).exp();
        Self {
            gain: 1.0,
            envelope: 0.0,
            attack_coeff,
            release_coeff,
            target: target.max(0.01),
            max_gain: 10.0_f32.powf(max_gain_db / 20.0),
        }
    }

    fn update_gain(&mut self, level: f32) -> f32 {
        let env_coeff = if level > self.envelope {
            self.attack_coeff
        } else {
            self.release_coeff
        };
        self.envelope += env_coeff * (level - self.envelope);

        if self.envelope > 1e-6 {
            let desired = (self.target / self.envelope).min(self.max_gain);
            let gain_coeff = if desired < self.gain {
                self.attack_coeff
            } else {
                self.release_coeff
            };
            self.gain += gain_coeff * (desired - self.gain);
        }

        self.gain
    }

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        let gain = self.update_gain(x.abs());
        (x * gain).clamp(-1.0, 1.0)
    }

    #[allow(dead_code)]
    pub(crate) fn process_pair(&mut self, left: f32, right: f32) -> (f32, f32) {
        let gain = self.update_gain(left.abs().max(right.abs()));
        (
            (left * gain).clamp(-1.0, 1.0),
            (right * gain).clamp(-1.0, 1.0),
        )
    }

    pub(crate) fn process_complex(&mut self, x: Complex<f32>) -> Complex<f32> {
        let gain = self.update_gain((x.re * x.re + x.im * x.im).sqrt());
        let mut y = x * gain;
        let mag = (y.re * y.re + y.im * y.im).sqrt();
        if mag > 1.0 {
            y /= mag;
        }
        y
    }
}

/// Selects the demodulation algorithm for a channel.
#[derive(Debug, Clone, PartialEq)]
pub enum Demodulator {
    /// Upper sideband SSB: take the real part of baseband IQ.
    Usb,
    /// Lower sideband SSB: negate imaginary part before taking real part.
    Lsb,
    /// AM envelope detector: magnitude of IQ, DC-removed.
    Am,
    /// Narrow-band FM: instantaneous frequency via quadrature.
    Fm,
    /// Wide-band FM: same quadrature discriminator, wider filtering upstream.
    Wfm,
    /// CW: magnitude of IQ after narrow BPF (BPF applied upstream), envelope.
    Cw,
    /// Pass-through (DIG, PKT): same as USB.
    Passthrough,
}

impl Demodulator {
    /// Construct the appropriate demodulator for a [`RigMode`].
    pub fn for_mode(mode: &RigMode) -> Self {
        match mode {
            RigMode::USB => Self::Usb,
            RigMode::LSB => Self::Lsb,
            RigMode::AM => Self::Am,
            RigMode::FM => Self::Fm,
            RigMode::WFM => Self::Wfm,
            RigMode::CW | RigMode::CWR => Self::Cw,
            RigMode::DIG => Self::Passthrough,
            // VHF/UHF packet radio (APRS, AX.25) is FM-encoded AFSK.
            RigMode::PKT => Self::Fm,
            RigMode::Other(_) => Self::Usb,
        }
    }

    /// Demodulate a block of baseband IQ samples.
    pub fn demodulate(&self, samples: &[Complex<f32>]) -> Vec<f32> {
        match self {
            Self::Usb | Self::Passthrough => ssb::demod_usb(samples),
            Self::Lsb => ssb::demod_lsb(samples),
            Self::Am => am::demod_am(samples),
            Self::Fm | Self::Wfm => fm::demod_fm(samples),
            Self::Cw => ssb::demod_cw(samples),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_demodulator_for_mode_mapping() {
        assert_eq!(Demodulator::for_mode(&RigMode::USB), Demodulator::Usb);
        assert_eq!(Demodulator::for_mode(&RigMode::LSB), Demodulator::Lsb);
        assert_eq!(Demodulator::for_mode(&RigMode::AM), Demodulator::Am);
        assert_eq!(Demodulator::for_mode(&RigMode::FM), Demodulator::Fm);
        assert_eq!(Demodulator::for_mode(&RigMode::WFM), Demodulator::Wfm);
        assert_eq!(Demodulator::for_mode(&RigMode::CW), Demodulator::Cw);
        assert_eq!(Demodulator::for_mode(&RigMode::CWR), Demodulator::Cw);
        assert_eq!(
            Demodulator::for_mode(&RigMode::DIG),
            Demodulator::Passthrough
        );
        assert_eq!(Demodulator::for_mode(&RigMode::PKT), Demodulator::Fm);
    }

    #[test]
    fn test_empty_input() {
        let empty = Vec::new();
        let demodulators = [
            Demodulator::Usb,
            Demodulator::Lsb,
            Demodulator::Am,
            Demodulator::Fm,
            Demodulator::Wfm,
            Demodulator::Cw,
            Demodulator::Passthrough,
        ];
        for demod in &demodulators {
            assert!(
                demod.demodulate(&empty).is_empty(),
                "{demod:?} should return empty Vec for empty input",
            );
        }
    }
}
