// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Transport DTOs for the JSON line protocol.

use serde::{Deserialize, Serialize};

use trx_core::rig::state::RigSnapshot;

/// Command received from network clients (JSON).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ClientCommand {
    GetState,
    SetFreq { freq_hz: u64 },
    SetMode { mode: String },
    SetPtt { ptt: bool },
    PowerOn,
    PowerOff,
    ToggleVfo,
    Lock,
    Unlock,
    GetTxLimit,
    SetTxLimit { limit: u8 },
    SetAprsDecodeEnabled { enabled: bool },
    SetCwDecodeEnabled { enabled: bool },
    SetCwAuto { enabled: bool },
    SetCwWpm { wpm: u32 },
    SetCwToneHz { tone_hz: u32 },
    SetFt8DecodeEnabled { enabled: bool },
    SetWsprDecodeEnabled { enabled: bool },
    ResetAprsDecoder,
    ResetCwDecoder,
    ResetFt8Decoder,
    ResetWsprDecoder,
}

/// Envelope for client commands with optional authentication token.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClientEnvelope {
    pub token: Option<String>,
    #[serde(flatten)]
    pub cmd: ClientCommand,
}

/// Response sent to network clients over TCP.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClientResponse {
    pub success: bool,
    pub state: Option<RigSnapshot>,
    pub error: Option<String>,
}
