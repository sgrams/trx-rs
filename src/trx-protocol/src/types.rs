// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Transport DTOs for the JSON line protocol.

use serde::{Deserialize, Serialize};

use trx_core::rig::state::RigSnapshot;
use trx_core::WfmDenoiseLevel;

/// Command received from network clients (JSON).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ClientCommand {
    GetState,
    GetRigs,
    SetFreq { freq_hz: u64 },
    SetCenterFreq { freq_hz: u64 },
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
    SetHfAprsDecodeEnabled { enabled: bool },
    SetCwDecodeEnabled { enabled: bool },
    SetCwAuto { enabled: bool },
    SetCwWpm { wpm: u32 },
    SetCwToneHz { tone_hz: u32 },
    SetFt8DecodeEnabled { enabled: bool },
    SetFt4DecodeEnabled { enabled: bool },
    SetFt2DecodeEnabled { enabled: bool },
    SetWsprDecodeEnabled { enabled: bool },
    ResetAprsDecoder,
    ResetHfAprsDecoder,
    ResetCwDecoder,
    ResetFt8Decoder,
    ResetFt4Decoder,
    ResetFt2Decoder,
    ResetWsprDecoder,
    SetBandwidth { bandwidth_hz: u32 },
    SetSdrGain { gain_db: f64 },
    SetSdrLnaGain { gain_db: f64 },
    SetSdrAgc { enabled: bool },
    SetSdrSquelch { enabled: bool, threshold_db: f64 },
    SetSdrNoiseBlanker { enabled: bool, threshold: f64 },
    SetWfmDeemphasis { deemphasis_us: u32 },
    SetWfmStereo { enabled: bool },
    SetWfmDenoise { level: WfmDenoiseLevel },
    GetSpectrum,
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
    /// Display name for the rig (long name from config, or rig_id if not set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub state: RigSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_port: Option<u16>,
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
