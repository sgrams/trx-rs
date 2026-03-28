// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Meteor-M LRPT (Low Rate Picture Transmission) satellite image decoder.
//!
//! Decodes the LRPT digital signal broadcast by Meteor-M N2-3 (137.900 MHz)
//! and Meteor-M N2-4 (137.100 MHz) using QPSK modulation at 72 kbps.
//!
//! # Signal chain
//!
//! The input is baseband IQ or FM-demodulated soft symbols:
//! 1. QPSK demodulation with Costas loop carrier recovery.
//! 2. Symbol timing recovery (Gardner algorithm).
//! 3. CCSDS frame synchronisation (ASM = 0x1ACFFC1D).
//! 4. Viterbi decoding (rate 1/2 convolutional code).
//! 5. CADU deframing -> VCDU -> MPDU -> APID extraction.
//! 6. MCU (Minimum Coded Unit) JPEG decompression per channel.
//!
//! Active APIDs for Meteor-M imagery:
//!   - APID 64: channel 1 (visible, 0.5-0.7 um)
//!   - APID 65: channel 2 (visible/NIR, 0.7-1.1 um)
//!   - APID 66: channel 3 (near-IR, 1.6-1.8 um)
//!   - APID 67: channel 4 (mid-IR, 3.5-4.1 um)
//!   - APID 68: channel 5 (thermal IR, 10.5-11.5 um)
//!   - APID 69: channel 6 (thermal IR, 11.5-12.5 um)
//!
//! Call [`LrptDecoder::process_samples`] with each audio/baseband batch,
//! then [`LrptDecoder::finalize`] when the pass ends.

pub mod cadu;
pub mod demod;
pub mod mcu;

/// Identified Meteor satellite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeteorSatellite {
    MeteorM2_3,
    MeteorM2_4,
    Unknown,
}

impl std::fmt::Display for MeteorSatellite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeteorSatellite::MeteorM2_3 => write!(f, "Meteor-M N2-3"),
            MeteorSatellite::MeteorM2_4 => write!(f, "Meteor-M N2-4"),
            MeteorSatellite::Unknown => write!(f, "Meteor-M (unknown)"),
        }
    }
}

/// Completed LRPT image returned by [`LrptDecoder::finalize`].
pub struct LrptImage {
    /// PNG-encoded image bytes.
    pub png: Vec<u8>,
    /// Number of decoded MCU rows.
    pub mcu_count: u32,
    /// Identified satellite, if determinable.
    pub satellite: Option<MeteorSatellite>,
    /// Comma-separated APID channels present (e.g. "64,65,66").
    pub channels: Option<String>,
}

/// Top-level Meteor-M LRPT decoder.
///
/// Feed baseband samples with [`process_samples`] and call [`finalize`] at
/// pass end to retrieve the assembled image.
pub struct LrptDecoder {
    demod: demod::QpskDemod,
    framer: cadu::CaduFramer,
    channels: mcu::ChannelAssembler,
    first_mcu_ms: Option<i64>,
}

impl LrptDecoder {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            demod: demod::QpskDemod::new(sample_rate),
            framer: cadu::CaduFramer::new(),
            channels: mcu::ChannelAssembler::new(),
            first_mcu_ms: None,
        }
    }

    /// Process a batch of baseband samples.
    ///
    /// Returns the number of new MCU rows decoded in this batch.
    pub fn process_samples(&mut self, samples: &[f32]) -> u32 {
        let before = self.channels.mcu_count();

        // Demodulate to soft symbols
        let symbols = self.demod.push(samples);

        // Frame sync and CADU extraction
        let cadus = self.framer.push(&symbols);

        // Decode MCUs from each CADU
        for cadu in &cadus {
            self.channels.process_cadu(cadu);
        }

        let after = self.channels.mcu_count();
        let new_mcus = after - before;

        if new_mcus > 0 && self.first_mcu_ms.is_none() {
            self.first_mcu_ms = Some(crate::now_ms());
        }

        new_mcus
    }

    /// Total number of MCU rows decoded so far.
    pub fn mcu_count(&self) -> u32 {
        self.channels.mcu_count()
    }

    /// Encode all accumulated channel data as a PNG image.
    ///
    /// Returns `None` if no MCU rows have been decoded.
    pub fn finalize(&self) -> Option<LrptImage> {
        let png = self.channels.encode_png()?;
        let active_apids = self.channels.active_apids();
        let channels_str = if active_apids.is_empty() {
            None
        } else {
            Some(
                active_apids
                    .iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            )
        };

        Some(LrptImage {
            png,
            mcu_count: self.channels.mcu_count(),
            satellite: self.channels.identify_satellite(),
            channels: channels_str,
        })
    }

    /// Clear all state; ready to decode a fresh pass.
    pub fn reset(&mut self) {
        self.demod.reset();
        self.framer.reset();
        self.channels.reset();
        self.first_mcu_ms = None;
    }
}
