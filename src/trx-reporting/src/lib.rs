// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Reporting uplink tasks: PSK Reporter and APRS-IS IGate.

pub mod aprsfi;
pub mod pskreporter;

use serde::{Deserialize, Serialize};

/// PSK Reporter uplink configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PskReporterConfig {
    /// Whether PSK Reporter uplink is enabled
    pub enabled: bool,
    /// PSK Reporter host
    pub host: String,
    /// PSK Reporter UDP port
    pub port: u16,
    /// Receiver locator (Maidenhead, 4 or 6 chars). If omitted, derived from
    /// [general].latitude/[general].longitude when available.
    pub receiver_locator: Option<String>,
}

impl Default for PskReporterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "report.pskreporter.info".to_string(),
            port: 4739,
            receiver_locator: None,
        }
    }
}

/// APRS-IS IGate uplink configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AprsFiConfig {
    /// Whether APRS-IS IGate uplink is enabled
    pub enabled: bool,
    /// APRS-IS server hostname
    pub host: String,
    /// APRS-IS server port
    pub port: u16,
    /// APRS-IS passcode. -1 = auto-compute from [general].callsign.
    pub passcode: i32,
}

impl Default for AprsFiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "rotate.aprs.net".to_string(),
            port: 14580,
            passcode: -1,
        }
    }
}
