// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use serde::{Deserialize, Serialize};

use crate::rig::state::RigSnapshot;

/// Command received from network clients (JSON).
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ClientCommand {
    GetState,
    SetFreq { freq_hz: u64 },
    SetMode { mode: String },
    SetPtt { ptt: bool },
    PowerOn,
    PowerOff,
    ToggleVfo,
    GetTxLimit,
    SetTxLimit { limit: u8 },
}

/// Response sent to network clients over TCP.
#[derive(Debug, Serialize)]
pub struct ClientResponse {
    pub success: bool,
    pub state: Option<RigSnapshot>,
    pub error: Option<String>,
}
