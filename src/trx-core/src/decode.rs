// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Shared types for server-side decoded messages (APRS, CW).

use serde::{Deserialize, Serialize};

/// A decoded message from the server-side decoders.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DecodedMessage {
    #[serde(rename = "aprs")]
    Aprs(AprsPacket),
    #[serde(rename = "cw")]
    Cw(CwEvent),
    #[serde(rename = "ft8")]
    Ft8(Ft8Message),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AprsPacket {
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
