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
    pub server_build_date: Option<String>,
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
    /// Filter state for backends that support runtime filter adjustment.
    /// Skipped in serde; flows into RigSnapshot via snapshot().
    #[serde(skip)]
    pub filter: Option<RigFilterState>,
    /// Latest spectrum frame from SDR backends.
    /// Skipped in serde (not part of persistent state); flows into RigSnapshot on demand.
    #[serde(skip)]
    pub spectrum: Option<SpectrumData>,
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
            server_build_date: None,
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
            filter: None,
            spectrum: None,
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
        build_date: Option<String>,
        latitude: Option<f64>,
        longitude: Option<f64>,
        initial_freq_hz: u64,
        initial_mode: RigMode,
    ) -> Self {
        let mut state = Self::new_uninitialized();
        state.server_callsign = callsign;
        state.server_version = version;
        state.server_build_date = build_date;
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
            server_build_date: snapshot.server_build_date,
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
            filter: snapshot.filter,
            spectrum: None, // spectrum flows through /api/spectrum, not persistent state
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
            server_build_date: self.server_build_date.clone(),
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
            filter: self.filter.clone(),
            spectrum: self.spectrum.clone(),
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

/// Current filter/DSP state for backends that support runtime filter adjustment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigFilterState {
    pub bandwidth_hz: u32,
    pub fir_taps: u32,
    pub cw_center_hz: u32,
    #[serde(default = "default_wfm_deemphasis_us")]
    pub wfm_deemphasis_us: u32,
}

fn default_wfm_deemphasis_us() -> u32 {
    75
}

/// Spectrum data from SDR backends (FFT magnitude over the full capture bandwidth).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectrumData {
    /// FFT magnitude bins in dBFS, FFT-shifted so DC (centre frequency) is at index N/2.
    pub bins: Vec<f32>,
    /// Centre frequency of the SDR capture in Hz.
    pub center_hz: u64,
    /// SDR capture sample rate in Hz; the displayed span is ±sample_rate/2.
    pub sample_rate: u32,
    /// Decoded Radio Data System state, when available for WFM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rds: Option<RdsData>,
}

/// Live RDS metadata decoded from a WFM broadcast.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RdsData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_type_name_long: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic_program: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic_announcement: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stereo: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artificial_head: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compressed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_pty: Option<bool>,
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
    pub server_build_date: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<RigFilterState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spectrum: Option<SpectrumData>,
}
