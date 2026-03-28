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
    #[serde(rename = "wxsat_image")]
    WxsatImage(WxsatImage),
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
            Self::WxsatImage(m) => m.rig_id = Some(id),
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
            Self::WxsatImage(m) => m.rig_id.as_deref(),
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

/// A completed weather satellite APT image, saved to disk as a JPEG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WxsatImage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    /// UTC timestamp (milliseconds since epoch) of pass start (first decoded line).
    pub pass_start_ms: i64,
    /// UTC timestamp (milliseconds since epoch) when the image was finalised.
    pub pass_end_ms: i64,
    /// Number of decoded image lines.
    pub line_count: u32,
    /// Absolute filesystem path to the saved JPEG file.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<i64>,
    /// Identified satellite (e.g. "NOAA-15", "NOAA-18", "NOAA-19").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satellite: Option<String>,
    /// Sensor channel name for sub-channel A (e.g. "1-VIS", "2-NIR", "4-TIR").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_a: Option<String>,
    /// Sensor channel name for sub-channel B.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_b: Option<String>,
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
