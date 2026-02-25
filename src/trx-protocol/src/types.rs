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
    GetRigs,
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

/// Envelope for client commands with optional authentication token and rig routing.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClientEnvelope {
    pub token: Option<String>,
    /// Target rig ID. When absent, the first/default rig is used (backward compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    #[serde(flatten)]
    pub cmd: ClientCommand,
}

/// One entry in the GetRigs response: a rig's ID and its current snapshot.
#[derive(Debug, Serialize, Deserialize)]
pub struct RigEntry {
    pub rig_id: String,
    pub state: RigSnapshot,
}

/// Response sent to network clients over TCP.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClientResponse {
    pub success: bool,
    /// The rig this response pertains to. Set by the listener from MR-06 onward.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    pub state: Option<RigSnapshot>,
    /// Populated only for GetRigs responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rigs: Option<Vec<RigEntry>>,
    pub error: Option<String>,
}
