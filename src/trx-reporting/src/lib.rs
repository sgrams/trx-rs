// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
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
    /// APRS-IS passcode. -1 = auto-compute from callsign.
    pub passcode: i32,
    /// IGate callsign. Overrides [general].callsign when set.
    pub callsign: Option<String>,
    /// Send periodic position beacons for this IGate station.
    /// Requires [general].latitude/longitude (or [aprsfi].latitude/longitude).
    pub beacon: bool,
    /// How often to send a position beacon, in seconds. Default: 1200 (20 min).
    pub beacon_interval_secs: u64,
    /// APRS symbol table identifier: "/" = primary, "\\" = alternate.
    pub beacon_symbol_table: char,
    /// APRS symbol code. E.g. '-' = house, '&' = diamond/gateway, 'I' = IGate.
    pub beacon_symbol_code: char,
    /// Beacon latitude override (decimal degrees). Falls back to [general].latitude.
    pub latitude: Option<f64>,
    /// Beacon longitude override (decimal degrees). Falls back to [general].longitude.
    pub longitude: Option<f64>,
}

impl Default for AprsFiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "rotate.aprs.net".to_string(),
            port: 14580,
            passcode: -1,
            callsign: None,
            beacon: false,
            beacon_interval_secs: 1200,
            beacon_symbol_table: '/',
            beacon_symbol_code: '-',
            latitude: None,
            longitude: None,
        }
    }
}
