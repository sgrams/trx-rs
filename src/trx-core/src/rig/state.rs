// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use serde::{Deserialize, Serialize};

use crate::rig::{RigControl, RigInfo, RigStatus, RigStatusProvider};

/// Simple transceiver state representation held by the rig task.
#[derive(Debug, Clone, Serialize)]
pub struct RigState {
    #[serde(skip_deserializing)]
    pub rig_info: Option<RigInfo>,
    pub status: RigStatus,
    #[serde(skip_serializing, skip_deserializing)]
    pub control: RigControl,
}

/// Mode supported by the rig.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl RigStatusProvider for RigState {
    fn status(&self) -> RigStatus {
        self.status.clone()
    }
}

impl RigState {
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
        })
    }
}

/// Read-only projection of state shared with clients.
#[derive(Debug, Clone, Serialize)]
pub struct RigSnapshot {
    pub info: RigInfo,
    pub status: RigStatus,
    pub band: Option<String>,
    pub enabled: Option<bool>,
}
