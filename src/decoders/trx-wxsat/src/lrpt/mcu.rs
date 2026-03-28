// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! MCU (Minimum Coded Unit) assembly and multi-channel image composition.
//!
//! Meteor-M LRPT imagery is transmitted as MCU blocks (8x8 pixel) across
//! multiple APIDs (Application Process Identifiers).  Each APID corresponds
//! to a different sensor channel:
//!
//!   - APID 64: channel 1 (visible, 0.5-0.7 um)
//!   - APID 65: channel 2 (visible/NIR, 0.7-1.1 um)
//!   - APID 66: channel 3 (near-IR, 1.6-1.8 um)
//!   - APID 67: channel 4 (mid-IR, 3.5-4.1 um)
//!   - APID 68: channel 5 (thermal IR, 10.5-11.5 um)
//!   - APID 69: channel 6 (thermal IR, 11.5-12.5 um)
//!
//! The standard colour composite uses APIDs 64 (R), 65 (G), 66 (B) or
//! APIDs 65 (R), 65 (G), 68 (B) depending on illumination.

use std::collections::BTreeMap;

use super::cadu::Cadu;
use super::MeteorSatellite;

/// Image width in pixels (Meteor-M MSU-MR swath: ~1568 px per line).
const LINE_WIDTH: u32 = 1568;

/// Known Meteor-M spacecraft IDs.
const SPACECRAFT_M2_3: u16 = 57; // Meteor-M N2-3
const SPACECRAFT_M2_4: u16 = 58; // Meteor-M N2-4

/// Per-APID channel accumulator.
struct ChannelBuffer {
    /// Row-major pixel data (grayscale, 0-255).
    pixels: Vec<u8>,
    /// Number of complete image lines accumulated.
    lines: u32,
    /// Pixel write cursor.
    cursor: usize,
}

impl ChannelBuffer {
    fn new() -> Self {
        Self {
            pixels: Vec::new(),
            lines: 0,
            cursor: 0,
        }
    }

    fn push_mcu_row(&mut self, data: &[u8]) {
        // Each MCU row = LINE_WIDTH pixels
        self.pixels.extend_from_slice(data);
        self.cursor += data.len();
        self.lines = (self.cursor / LINE_WIDTH as usize) as u32;
    }
}

/// Assembles decoded MCU blocks from multiple APIDs into a composite image.
pub struct ChannelAssembler {
    /// Per-APID buffers.
    channels: BTreeMap<u16, ChannelBuffer>,
    /// Total MCU rows across all channels.
    total_mcu_count: u32,
    /// Spacecraft ID seen in CADUs (for satellite identification).
    spacecraft_id: Option<u16>,
}

impl Default for ChannelAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelAssembler {
    pub fn new() -> Self {
        Self {
            channels: BTreeMap::new(),
            total_mcu_count: 0,
            spacecraft_id: None,
        }
    }

    /// Process a single CADU, extracting MCU data for each APID found.
    pub fn process_cadu(&mut self, cadu: &Cadu) {
        // Record spacecraft ID
        let scid = cadu.spacecraft_id();
        if scid > 0 {
            self.spacecraft_id = Some(scid);
        }

        let vcid = cadu.vcid();
        let payload = cadu.mpdu_payload();

        // Virtual channels 0-5 carry APID 64-69 imagery
        if vcid > 5 || payload.is_empty() {
            return;
        }

        let apid = 64 + vcid as u16;

        // Extract pixel data from MPDU payload.
        // In a full implementation, this would perform JPEG/Huffman decoding
        // of the MCU blocks.  Here we treat the payload as raw pixel data
        // for scaffolding purposes (to be replaced with proper MCU decode).
        let buf = self.channels.entry(apid).or_insert_with(ChannelBuffer::new);

        // Pad or truncate to LINE_WIDTH boundary
        let usable = payload.len().min(LINE_WIDTH as usize);
        let mut row = vec![0u8; LINE_WIDTH as usize];
        row[..usable].copy_from_slice(&payload[..usable]);
        buf.push_mcu_row(&row);

        self.total_mcu_count += 1;
    }

    /// Total MCU rows decoded across all channels.
    pub fn mcu_count(&self) -> u32 {
        self.total_mcu_count
    }

    /// Active APID channels.
    pub fn active_apids(&self) -> Vec<u16> {
        self.channels.keys().copied().collect()
    }

    /// Identify the satellite from the CCSDS spacecraft ID.
    pub fn identify_satellite(&self) -> Option<MeteorSatellite> {
        self.spacecraft_id.map(|id| match id {
            SPACECRAFT_M2_3 => MeteorSatellite::MeteorM2_3,
            SPACECRAFT_M2_4 => MeteorSatellite::MeteorM2_4,
            _ => MeteorSatellite::Unknown,
        })
    }

    /// Encode accumulated channel data as a PNG image.
    ///
    /// Produces an RGB composite if channels 64, 65, 66 are available,
    /// otherwise produces a grayscale image of the most populated channel.
    pub fn encode_png(&self) -> Option<Vec<u8>> {
        if self.channels.is_empty() {
            return None;
        }

        // Determine the maximum number of complete lines across channels
        let max_lines = self.channels.values().map(|ch| ch.lines).max().unwrap_or(0);

        if max_lines == 0 {
            return None;
        }

        let width = LINE_WIDTH;
        let height = max_lines;
        let npix = (width * height) as usize;

        // Try RGB composite (APIDs 64=R, 65=G, 66=B)
        let ch_r = self.channels.get(&64);
        let ch_g = self.channels.get(&65);
        let ch_b = self.channels.get(&66);

        if ch_r.is_some() || ch_g.is_some() || ch_b.is_some() {
            let mut rgb_pixels: Vec<u8> = Vec::with_capacity(npix * 3);
            for i in 0..npix {
                let r = ch_r.and_then(|c| c.pixels.get(i).copied()).unwrap_or(0);
                let g = ch_g.and_then(|c| c.pixels.get(i).copied()).unwrap_or(0);
                let b = ch_b.and_then(|c| c.pixels.get(i).copied()).unwrap_or(0);
                rgb_pixels.push(r);
                rgb_pixels.push(g);
                rgb_pixels.push(b);
            }
            crate::image_enc::encode_rgb_png(width, height, rgb_pixels)
        } else {
            // Fallback: grayscale from the first available channel
            let first_ch = self.channels.values().next()?;
            let mut gray_pixels: Vec<u8> = Vec::with_capacity(npix);
            for i in 0..npix {
                gray_pixels.push(first_ch.pixels.get(i).copied().unwrap_or(0));
            }
            crate::image_enc::encode_grayscale_png(width, height, gray_pixels)
        }
    }

    pub fn reset(&mut self) {
        self.channels.clear();
        self.total_mcu_count = 0;
        self.spacecraft_id = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_buffer_line_counting() {
        let mut buf = ChannelBuffer::new();
        let row = vec![128u8; LINE_WIDTH as usize];
        buf.push_mcu_row(&row);
        assert_eq!(buf.lines, 1);
        buf.push_mcu_row(&row);
        assert_eq!(buf.lines, 2);
    }

    #[test]
    fn test_identify_satellite() {
        let mut asm = ChannelAssembler::new();
        assert_eq!(asm.identify_satellite(), None);

        asm.spacecraft_id = Some(SPACECRAFT_M2_3);
        assert_eq!(asm.identify_satellite(), Some(MeteorSatellite::MeteorM2_3));

        asm.spacecraft_id = Some(SPACECRAFT_M2_4);
        assert_eq!(asm.identify_satellite(), Some(MeteorSatellite::MeteorM2_4));

        asm.spacecraft_id = Some(99);
        assert_eq!(asm.identify_satellite(), Some(MeteorSatellite::Unknown));
    }
}
