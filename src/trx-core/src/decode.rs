// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Shared types for server-side decoded messages (APRS, AIS, CW).

use serde::{Deserialize, Serialize};

/// A decoded message from the server-side decoders.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DecodedMessage {
    #[serde(rename = "ais")]
    Ais(AisMessage),
    #[serde(rename = "vdes")]
    Vdes(VdesMessage),
    #[serde(rename = "aprs")]
    Aprs(AprsPacket),
    #[serde(rename = "hf_aprs")]
    HfAprs(AprsPacket),
    #[serde(rename = "cw")]
    Cw(CwEvent),
    #[serde(rename = "ft8")]
    Ft8(Ft8Message),
    #[serde(rename = "ft4")]
    Ft4(Ft8Message),
    #[serde(rename = "ft2")]
    Ft2(Ft8Message),
    #[serde(rename = "wspr")]
    Wspr(WsprMessage),
    #[serde(rename = "lrpt_image")]
    LrptImage(LrptImage),
    #[serde(rename = "lrpt_progress")]
    LrptProgress(LrptProgress),
    #[serde(rename = "wefax")]
    Wefax(WefaxMessage),
    #[serde(rename = "wefax_progress")]
    WefaxProgress(WefaxProgress),
}

impl DecodedMessage {
    /// Attach a rig identifier to the inner message variant.
    pub fn set_rig_id(&mut self, id: String) {
        match self {
            Self::Ais(m) => m.rig_id = Some(id),
            Self::Vdes(m) => m.rig_id = Some(id),
            Self::Aprs(m) | Self::HfAprs(m) => m.rig_id = Some(id),
            Self::Cw(m) => m.rig_id = Some(id),
            Self::Ft8(m) | Self::Ft4(m) | Self::Ft2(m) => m.rig_id = Some(id),
            Self::Wspr(m) => m.rig_id = Some(id),
            Self::LrptImage(m) => m.rig_id = Some(id),
            Self::LrptProgress(m) => m.rig_id = Some(id),
            Self::Wefax(m) => m.rig_id = Some(id),
            Self::WefaxProgress(m) => m.rig_id = Some(id),
        }
    }

    /// Return the rig identifier from the inner message variant, if set.
    pub fn rig_id(&self) -> Option<&str> {
        match self {
            Self::Ais(m) => m.rig_id.as_deref(),
            Self::Vdes(m) => m.rig_id.as_deref(),
            Self::Aprs(m) | Self::HfAprs(m) => m.rig_id.as_deref(),
            Self::Cw(m) => m.rig_id.as_deref(),
            Self::Ft8(m) | Self::Ft4(m) | Self::Ft2(m) => m.rig_id.as_deref(),
            Self::Wspr(m) => m.rig_id.as_deref(),
            Self::LrptImage(m) => m.rig_id.as_deref(),
            Self::LrptProgress(m) => m.rig_id.as_deref(),
            Self::Wefax(m) => m.rig_id.as_deref(),
            Self::WefaxProgress(m) => m.rig_id.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AisMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<i64>,
    pub channel: String,
    pub message_type: u8,
    pub repeat: u8,
    pub mmsi: u32,
    pub crc_ok: bool,
    pub bit_len: usize,
    pub raw_bytes: Vec<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sog_knots: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cog_deg: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_deg: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nav_status: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vessel_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VdesMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<i64>,
    pub channel: String,
    pub message_type: u8,
    pub repeat: u8,
    pub mmsi: u32,
    pub crc_ok: bool,
    pub bit_len: usize,
    pub raw_bytes: Vec<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sog_knots: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cog_deg: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_deg: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nav_status: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vessel_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_count: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asm_identifier: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack_nack_mask: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_quality: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_id: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_errors: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_rotation: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fec_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AprsPacket {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<i64>,
    pub src_call: String,
    pub dest_call: String,
    pub path: String,
    pub info: String,
    pub info_bytes: Vec<u8>,
    pub packet_type: String,
    pub crc_ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CwEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    /// Decoded text fragment (one or more characters)
    pub text: String,
    /// Current detected WPM
    pub wpm: u32,
    /// Current detected tone frequency (Hz)
    pub tone_hz: u32,
    /// Whether a CW tone is currently detected
    pub signal_on: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ft8Message {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    /// UTC timestamp (milliseconds since epoch)
    pub ts_ms: i64,
    /// Approximate SNR (dB)
    pub snr_db: f32,
    /// Time offset within slot (seconds)
    pub dt_s: f32,
    /// Audio frequency (Hz)
    pub freq_hz: f32,
    /// Decoded message text
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsprMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    /// UTC timestamp (milliseconds since epoch)
    pub ts_ms: i64,
    /// Approximate SNR (dB)
    pub snr_db: f32,
    /// Time offset within slot (seconds)
    pub dt_s: f32,
    /// Audio frequency (Hz)
    pub freq_hz: f32,
    /// Decoded message text
    pub message: String,
}

/// Live LRPT decode progress update, sent periodically during active decoding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LrptProgress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    /// Number of MCU rows decoded so far in this pass.
    pub mcu_count: u32,
}

/// A completed Meteor-M LRPT satellite image, saved to disk as a PNG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LrptImage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    /// UTC timestamp (milliseconds since epoch) of pass start.
    pub pass_start_ms: i64,
    /// UTC timestamp (milliseconds since epoch) when the image was finalised.
    pub pass_end_ms: i64,
    /// Number of decoded MCU rows.
    pub mcu_count: u32,
    /// Absolute filesystem path to the saved image file.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<i64>,
    /// Identified satellite (e.g. "Meteor-M N2-3", "Meteor-M N2-4").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satellite: Option<String>,
    /// APID channels decoded (e.g. "64,65,66" for RGB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<String>,
    /// Geographic bounds `[south, west, north, east]` for map overlay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_bounds: Option<[f64; 4]>,
    /// Ground track points `[[lat, lon], ...]` from SGP4 propagation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground_track: Option<Vec<[f64; 2]>>,
}

/// A complete WEFAX image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WefaxMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<i64>,
    /// Number of image lines decoded.
    pub line_count: u32,
    /// Detected or configured LPM.
    pub lpm: u16,
    /// Detected or configured IOC.
    pub ioc: u16,
    /// Pixels per line (IOC × π, rounded).
    pub pixels_per_line: u16,
    /// Filesystem path to saved PNG (set on completion).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Base64-encoded PNG data for transfer to remote clients.
    /// Populated by the server when sending, stripped before storing in history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub png_data: Option<String>,
    /// True when image is complete (stop tone received).
    pub complete: bool,
}

/// Progress update emitted per-line during active WEFAX reception.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WefaxProgress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    /// Number of image lines decoded so far.
    pub line_count: u32,
    /// Detected or configured LPM.
    pub lpm: u16,
    /// Detected or configured IOC.
    pub ioc: u16,
    /// Pixels per line.
    pub pixels_per_line: u16,
    /// Base64-encoded greyscale line data (one row of pixels).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_data: Option<String>,
    /// Decoder state label (e.g. "APT Start 576", "Phasing", "Receiving").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}
