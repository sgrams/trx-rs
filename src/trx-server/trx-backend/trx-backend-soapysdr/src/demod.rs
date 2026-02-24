// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;
use trx_core::rig::state::RigMode;

/// Selects the demodulation algorithm for a channel.
#[derive(Debug, Clone, PartialEq)]
pub enum Demodulator {
    /// Upper sideband SSB: take the real part of baseband IQ.
    Usb,
    /// Lower sideband SSB: negate imaginary part before taking real part.
    Lsb,
    /// AM envelope detector: magnitude of IQ, DC-removed.
    Am,
    /// Narrow-band FM: instantaneous frequency via quadrature (arg of s[n]*conj(s[n-1])).
    Fm,
    /// Wide-band FM: same algorithm as FM, wider pre-filter (handled upstream in DSP).
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
            RigMode::DIG | RigMode::PKT => Self::Passthrough,
            RigMode::Other(_) => Self::Usb,
        }
    }

    /// Demodulate a block of baseband IQ samples.
    ///
    /// `samples`: complex f32 IQ already centred at 0 Hz.
    /// Returns real f32 audio samples, same length as input.
    pub fn demodulate(&self, samples: &[Complex<f32>]) -> Vec<f32> {
        match self {
            Self::Usb | Self::Passthrough => demod_usb(samples),
            Self::Lsb => demod_lsb(samples),
            Self::Am => demod_am(samples),
            Self::Fm | Self::Wfm => demod_fm(samples),
            Self::Cw => demod_cw(samples),
        }
    }
}

// ---------------------------------------------------------------------------
// USB / Passthrough
// ---------------------------------------------------------------------------

/// USB demodulator: take the real part of each IQ sample.
fn demod_usb(samples: &[Complex<f32>]) -> Vec<f32> {
    samples.iter().map(|s| s.re).collect()
}

// ---------------------------------------------------------------------------
// LSB
// ---------------------------------------------------------------------------

/// LSB demodulator: LSB mixing is handled upstream by negating `channel_if_hz`;
/// the demodulator itself is identical to USB — just take `.re`.
fn demod_lsb(samples: &[Complex<f32>]) -> Vec<f32> {
    samples.iter().map(|s| s.re).collect()
}

// ---------------------------------------------------------------------------
// AM
// ---------------------------------------------------------------------------

/// AM envelope detector: magnitude of IQ, DC-removed, peak-normalised to ≤ 1.0.
fn demod_am(samples: &[Complex<f32>]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    // Compute envelope (magnitude).
    let mag: Vec<f32> = samples
        .iter()
        .map(|s| (s.re * s.re + s.im * s.im).sqrt())
        .collect();

    // Remove DC offset.
    let mean = mag.iter().copied().sum::<f32>() / mag.len() as f32;
    let mut output: Vec<f32> = mag.iter().map(|&m| m - mean).collect();

    // Normalise peak to ≤ 1.0 (only if max > 1.0, to avoid amplifying noise).
    let max_abs = output
        .iter()
        .copied()
        .map(f32::abs)
        .fold(0.0_f32, f32::max);
    if max_abs > 1.0 {
        let inv = 1.0 / max_abs;
        for sample in &mut output {
            *sample *= inv;
        }
    }

    output
}

// ---------------------------------------------------------------------------
// FM / WFM
// ---------------------------------------------------------------------------

/// FM quadrature discriminator: instantaneous frequency via arg(s[n] * conj(s[n-1])).
/// Output is in radians/sample, scaled by 1/π to normalise to [-1, 1].
fn demod_fm(samples: &[Complex<f32>]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    let inv_pi = std::f32::consts::FRAC_1_PI;
    let mut output = Vec::with_capacity(samples.len());
    output.push(0.0_f32);

    for i in 1..samples.len() {
        let product = samples[i] * samples[i - 1].conj();
        let angle = product.im.atan2(product.re);
        output.push(angle * inv_pi);
    }

    output
}

// ---------------------------------------------------------------------------
// CW
// ---------------------------------------------------------------------------

/// CW envelope detector: magnitude of IQ, peak-normalised to ≤ 1.0.
/// Narrow BPF is applied upstream.
fn demod_cw(samples: &[Complex<f32>]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    let mut output: Vec<f32> = samples
        .iter()
        .map(|s| (s.re * s.re + s.im * s.im).sqrt())
        .collect();

    // Normalise peak to ≤ 1.0.
    let max_abs = output.iter().copied().fold(0.0_f32, f32::max);
    if max_abs > 1.0 {
        let inv = 1.0 / max_abs;
        for sample in &mut output {
            *sample *= inv;
        }
    }

    output
}
