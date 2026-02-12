// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use serde::{Deserialize, Serialize};

use crate::radio::freq::Freq;
use crate::rig::{RigControl, RigInfo, RigRxStatus, RigStatus, RigStatusProvider, RigTxStatus};

/// Simple transceiver state representation held by the rig task.
#[derive(Debug, Clone, Serialize)]
pub struct RigState {
    #[serde(skip_deserializing)]
    pub rig_info: Option<RigInfo>,
    pub status: RigStatus,
    pub initialized: bool,
    #[serde(skip_serializing, skip_deserializing)]
    pub control: RigControl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_callsign: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_longitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pskreporter_status: Option<String>,
    #[serde(default)]
    pub aprs_decode_enabled: bool,
    #[serde(default)]
    pub cw_decode_enabled: bool,
    #[serde(default)]
    pub ft8_decode_enabled: bool,
    #[serde(default)]
    pub wspr_decode_enabled: bool,
    #[serde(default)]
    pub cw_auto: bool,
    #[serde(default)]
    pub cw_wpm: u32,
    #[serde(default)]
    pub cw_tone_hz: u32,
    #[serde(default, skip_serializing)]
    pub aprs_decode_reset_seq: u64,
    #[serde(default, skip_serializing)]
    pub cw_decode_reset_seq: u64,
    #[serde(default, skip_serializing)]
    pub ft8_decode_reset_seq: u64,
    #[serde(default, skip_serializing)]
    pub wspr_decode_reset_seq: u64,
}

/// Mode supported by the rig.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RigMode {
    LSB,
    USB,
    CW,
    CWR,
    AM,
    WFM,
    FM,
    DIG,
    PKT,
    Other(String),
}

impl Default for RigStatus {
    fn default() -> Self {
        Self {
            freq: Freq { hz: 144_300_000 }, // 2m calling frequency
            mode: RigMode::USB,
            tx_en: false,
            vfo: None,
            tx: Some(RigTxStatus {
                power: None,
                limit: None,
                swr: None,
                alc: None,
            }),
            rx: Some(RigRxStatus { sig: None }),
            lock: Some(false),
        }
    }
}

impl Default for RigControl {
    fn default() -> Self {
        Self {
            rpt_offset_hz: None,
            ctcss_hz: None,
            dcs_code: None,
            lock: Some(false),
            clar_hz: None,
            clar_on: None,
            enabled: Some(false),
        }
    }
}

impl RigStatusProvider for RigState {
    fn status(&self) -> RigStatus {
        self.status.clone()
    }
}

impl RigState {
    /// Create uninitialized state with common defaults (client-side).
    pub fn new_uninitialized() -> Self {
        Self {
            rig_info: None,
            status: RigStatus::default(),
            initialized: false,
            control: RigControl::default(),
            server_callsign: None,
            server_version: None,
            server_latitude: None,
            server_longitude: None,
            pskreporter_status: None,
            aprs_decode_enabled: false,
            cw_decode_enabled: false,
            ft8_decode_enabled: false,
            wspr_decode_enabled: false,
            cw_auto: true,
            cw_wpm: 15,
            cw_tone_hz: 700,
            aprs_decode_reset_seq: 0,
            cw_decode_reset_seq: 0,
            ft8_decode_reset_seq: 0,
            wspr_decode_reset_seq: 0,
        }
    }

    /// Create state with server metadata and initial freq/mode (server-side).
    pub fn new_with_metadata(
        callsign: Option<String>,
        version: Option<String>,
        latitude: Option<f64>,
        longitude: Option<f64>,
        initial_freq_hz: u64,
        initial_mode: RigMode,
    ) -> Self {
        let mut state = Self::new_uninitialized();
        state.server_callsign = callsign;
        state.server_version = version;
        state.server_latitude = latitude;
        state.server_longitude = longitude;
        state.status.freq = Freq {
            hz: initial_freq_hz,
        };
        state.status.mode = initial_mode;
        state
    }

    /// Convert snapshot to full state (remote client).
    pub fn from_snapshot(snapshot: RigSnapshot) -> Self {
        let lock = snapshot.status.lock;
        Self {
            rig_info: Some(snapshot.info),
            status: snapshot.status,
            initialized: snapshot.initialized,
            control: RigControl {
                rpt_offset_hz: None,
                ctcss_hz: None,
                dcs_code: None,
                lock,
                clar_hz: None,
                clar_on: None,
                enabled: snapshot.enabled,
            },
            server_callsign: snapshot.server_callsign,
            server_version: snapshot.server_version,
            server_latitude: snapshot.server_latitude,
            server_longitude: snapshot.server_longitude,
            pskreporter_status: snapshot.pskreporter_status,
            aprs_decode_enabled: snapshot.aprs_decode_enabled,
            cw_decode_enabled: snapshot.cw_decode_enabled,
            cw_auto: snapshot.cw_auto,
            cw_wpm: snapshot.cw_wpm,
            cw_tone_hz: snapshot.cw_tone_hz,
            ft8_decode_enabled: snapshot.ft8_decode_enabled,
            wspr_decode_enabled: snapshot.wspr_decode_enabled,
            aprs_decode_reset_seq: 0,
            cw_decode_reset_seq: 0,
            ft8_decode_reset_seq: 0,
            wspr_decode_reset_seq: 0,
        }
    }

    pub fn band_name(&self) -> Option<String> {
        self.rig_info.as_ref().and_then(|info| {
            self.status
                .freq
                .band_name(&info.capabilities.supported_bands)
        })
    }

    /// Produce an immutable snapshot suitable for sharing with clients.
    pub fn snapshot(&self) -> Option<RigSnapshot> {
        let info = self.rig_info.clone()?;
        Some(RigSnapshot {
            info,
            status: self.status.clone(),
            band: self.band_name(),
            enabled: self.control.enabled,
            initialized: self.initialized,
            server_callsign: self.server_callsign.clone(),
            server_version: self.server_version.clone(),
            server_latitude: self.server_latitude,
            server_longitude: self.server_longitude,
            pskreporter_status: self.pskreporter_status.clone(),
            aprs_decode_enabled: self.aprs_decode_enabled,
            cw_decode_enabled: self.cw_decode_enabled,
            cw_auto: self.cw_auto,
            cw_wpm: self.cw_wpm,
            cw_tone_hz: self.cw_tone_hz,
            ft8_decode_enabled: self.ft8_decode_enabled,
            wspr_decode_enabled: self.wspr_decode_enabled,
        })
    }

    /// Apply a frequency change into the state.
    pub fn apply_freq(&mut self, freq: crate::radio::freq::Freq) {
        self.status.freq = freq;
    }

    /// Apply a mode change into the state.
    pub fn apply_mode(&mut self, mode: RigMode) {
        self.status.mode = mode;
    }

    /// Apply a PTT change, resetting meters on TX off.
    pub fn apply_ptt(&mut self, ptt: bool) {
        self.status.tx_en = ptt;
        self.status.lock = self.control.lock;
        if !ptt {
            if let Some(tx) = self.status.tx.as_mut() {
                tx.power = Some(0);
                tx.swr = Some(0.0);
            }
        }
    }
}

/// Read-only projection of state shared with clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigSnapshot {
    pub info: RigInfo,
    pub status: RigStatus,
    pub band: Option<String>,
    pub enabled: Option<bool>,
    pub initialized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_callsign: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_longitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pskreporter_status: Option<String>,
    #[serde(default)]
    pub aprs_decode_enabled: bool,
    #[serde(default)]
    pub cw_decode_enabled: bool,
    #[serde(default)]
    pub ft8_decode_enabled: bool,
    #[serde(default)]
    pub wspr_decode_enabled: bool,
    #[serde(default)]
    pub cw_auto: bool,
    #[serde(default)]
    pub cw_wpm: u32,
    #[serde(default)]
    pub cw_tone_hz: u32,
}
