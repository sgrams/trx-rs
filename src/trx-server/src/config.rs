// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Configuration file support for trx-server.
//!
//! Supports loading configuration from TOML files with the following search order:
//! 1. Path specified via `--config` CLI argument
//! 2. `./trx-server.toml` (current directory)
//! 3. `~/.config/trx-rs/server.toml` (XDG config)
//! 4. `/etc/trx-rs/server.toml` (system-wide)

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trx_app::{ConfigError, ConfigFile};

use trx_core::rig::state::RigMode;

/// Top-level server configuration structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// General settings
    pub general: GeneralConfig,
    /// Rig backend configuration
    pub rig: RigConfig,
    /// Polling and retry behavior
    pub behavior: BehaviorConfig,
    /// TCP listener configuration
    pub listen: ListenConfig,
    /// Audio streaming configuration
    pub audio: AudioConfig,
    /// PSK Reporter uplink configuration
    pub pskreporter: PskReporterConfig,
}

/// General application settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Callsign or owner label
    pub callsign: Option<String>,
    /// Log level (trace, debug, info, warn, error)
    pub log_level: Option<String>,
    /// Receiver latitude (decimal degrees, WGS84)
    pub latitude: Option<f64>,
    /// Receiver longitude (decimal degrees, WGS84)
    pub longitude: Option<f64>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            callsign: Some("N0CALL".to_string()),
            log_level: None,
            latitude: None,
            longitude: None,
        }
    }
}

/// Rig backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RigConfig {
    /// Rig model (e.g., "ft817", "ft450d", "ic7300")
    pub model: Option<String>,
    /// Initial frequency (Hz) for the rig state before first CAT read
    pub initial_freq_hz: u64,
    /// Initial mode for the rig state before first CAT read
    pub initial_mode: RigMode,
    /// Access method configuration
    pub access: AccessConfig,
}

/// Access method configuration for reaching the rig.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessConfig {
    /// Access type: "serial" or "tcp"
    #[serde(rename = "type")]
    pub access_type: Option<String>,
    /// Serial port path (for serial access)
    pub port: Option<String>,
    /// Baud rate (for serial access)
    pub baud: Option<u32>,
    /// Host address (for TCP access)
    pub host: Option<String>,
    /// TCP port (for TCP access)
    pub tcp_port: Option<u16>,
}

impl Default for RigConfig {
    fn default() -> Self {
        Self {
            model: None,
            initial_freq_hz: 144_300_000,
            initial_mode: RigMode::USB,
            access: AccessConfig::default(),
        }
    }
}

/// Behavior configuration for polling and retries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviorConfig {
    /// Polling interval in milliseconds when idle
    pub poll_interval_ms: u64,
    /// Polling interval in milliseconds when transmitting
    pub poll_interval_tx_ms: u64,
    /// Maximum retry attempts for transient errors
    pub max_retries: u32,
    /// Base delay for exponential backoff in milliseconds
    pub retry_base_delay_ms: u64,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 500,
            poll_interval_tx_ms: 100,
            max_retries: 3,
            retry_base_delay_ms: 100,
        }
    }
}

/// TCP listener configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ListenConfig {
    /// Whether the listener is enabled
    pub enabled: bool,
    /// IP address to listen on
    pub listen: IpAddr,
    /// TCP port to listen on
    pub port: u16,
    /// Authentication configuration
    pub auth: AuthConfig,
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: 4532,
            auth: AuthConfig::default(),
        }
    }
}

/// Authentication configuration for the TCP listener.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Valid authentication tokens (empty = no auth required)
    pub tokens: Vec<String>,
}

/// Audio streaming configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Whether audio streaming is enabled
    pub enabled: bool,
    /// IP address to listen on for audio connections
    pub listen: IpAddr,
    /// TCP port for audio connections
    pub port: u16,
    /// Whether RX audio capture is enabled
    pub rx_enabled: bool,
    /// Whether TX audio playback is enabled
    pub tx_enabled: bool,
    /// Audio input device name (None = system default)
    pub device: Option<String>,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of audio channels
    pub channels: u8,
    /// Opus frame duration in milliseconds
    pub frame_duration_ms: u16,
    /// Opus bitrate in bits per second
    pub bitrate_bps: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: 4533,
            rx_enabled: true,
            tx_enabled: true,
            device: None,
            sample_rate: 48000,
            channels: 1,
            frame_duration_ms: 20,
            bitrate_bps: 24000,
        }
    }
}

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

impl ServerConfig {
    pub fn validate(&self) -> Result<(), String> {
        validate_log_level(self.general.log_level.as_deref())?;
        validate_coordinates(self.general.latitude, self.general.longitude)?;

        if self.rig.initial_freq_hz == 0 {
            return Err("[rig].initial_freq_hz must be > 0".to_string());
        }

        validate_access(&self.rig.access)?;

        if self.behavior.poll_interval_ms == 0 {
            return Err("[behavior].poll_interval_ms must be > 0".to_string());
        }
        if self.behavior.poll_interval_tx_ms == 0 {
            return Err("[behavior].poll_interval_tx_ms must be > 0".to_string());
        }
        if self.behavior.max_retries == 0 {
            return Err("[behavior].max_retries must be > 0".to_string());
        }
        if self.behavior.retry_base_delay_ms == 0 {
            return Err("[behavior].retry_base_delay_ms must be > 0".to_string());
        }

        validate_tokens("[listen.auth].tokens", &self.listen.auth.tokens)?;
        if self.listen.enabled && self.listen.port == 0 {
            return Err("[listen].port must be > 0 when listener is enabled".to_string());
        }

        if self.audio.enabled {
            if self.audio.port == 0 {
                return Err("[audio].port must be > 0 when audio is enabled".to_string());
            }
            if !self.audio.rx_enabled && !self.audio.tx_enabled {
                return Err(
                    "[audio] enabled but both rx_enabled and tx_enabled are false".to_string(),
                );
            }
            if self.audio.sample_rate < 8_000 || self.audio.sample_rate > 192_000 {
                return Err("[audio].sample_rate must be in range 8000..=192000".to_string());
            }
            if !(1..=2).contains(&self.audio.channels) {
                return Err("[audio].channels must be 1 or 2".to_string());
            }
            match self.audio.frame_duration_ms {
                3 | 5 | 10 | 20 | 40 | 60 => {}
                _ => {
                    return Err(
                        "[audio].frame_duration_ms must be one of: 3, 5, 10, 20, 40, 60"
                            .to_string(),
                    )
                }
            }
            if self.audio.bitrate_bps == 0 {
                return Err("[audio].bitrate_bps must be > 0".to_string());
            }
        }

        if self.pskreporter.enabled {
            if self.pskreporter.host.trim().is_empty() {
                return Err("[pskreporter].host must not be empty".to_string());
            }
            if self.pskreporter.port == 0 {
                return Err("[pskreporter].port must be > 0".to_string());
            }
        }

        Ok(())
    }

    /// Load configuration from a specific file path.
    pub fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        <Self as ConfigFile>::load_from_file(path)
    }

    /// Load configuration from the default search paths.
    /// Returns default config if no config file is found.
    pub fn load_from_default_paths() -> Result<(Self, Option<PathBuf>), ConfigError> {
        <Self as ConfigFile>::load_from_default_paths()
    }

    /// Generate an example configuration as a TOML string.
    pub fn example_toml() -> String {
        let example = ServerConfig {
            general: GeneralConfig {
                callsign: Some("N0CALL".to_string()),
                log_level: Some("info".to_string()),
                latitude: None,
                longitude: None,
            },
            rig: RigConfig {
                model: Some("ft817".to_string()),
                initial_freq_hz: 144_300_000,
                initial_mode: RigMode::USB,
                access: AccessConfig {
                    access_type: Some("serial".to_string()),
                    port: Some("/dev/ttyUSB0".to_string()),
                    baud: Some(9600),
                    host: None,
                    tcp_port: None,
                },
            },
            behavior: BehaviorConfig::default(),
            listen: ListenConfig::default(),
            audio: AudioConfig::default(),
            pskreporter: PskReporterConfig::default(),
        };

        toml::to_string_pretty(&example).unwrap_or_default()
    }
}

fn validate_log_level(level: Option<&str>) -> Result<(), String> {
    if let Some(level) = level {
        match level {
            "trace" | "debug" | "info" | "warn" | "error" => {}
            _ => {
                return Err(format!(
                    "[general].log_level '{}' is invalid (expected one of: trace, debug, info, warn, error)",
                    level
                ))
            }
        }
    }
    Ok(())
}

fn validate_coordinates(latitude: Option<f64>, longitude: Option<f64>) -> Result<(), String> {
    match (latitude, longitude) {
        (Some(lat), Some(lon)) => {
            if !(-90.0..=90.0).contains(&lat) {
                return Err("[general].latitude must be in range -90..=90".to_string());
            }
            if !(-180.0..=180.0).contains(&lon) {
                return Err("[general].longitude must be in range -180..=180".to_string());
            }
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(
            "[general].latitude and [general].longitude must be set together or both omitted"
                .to_string(),
        ),
    }
}

fn validate_access(access: &AccessConfig) -> Result<(), String> {
    let serial_fields_set = access.port.is_some() || access.baud.is_some();
    let tcp_fields_set = access.host.is_some() || access.tcp_port.is_some();

    if access.access_type.is_none() && !serial_fields_set && !tcp_fields_set {
        return Ok(());
    }

    match access.access_type.as_deref().unwrap_or("serial") {
        "serial" => {
            if access.port.as_deref().unwrap_or("").trim().is_empty() {
                return Err(
                    "[rig.access].port must be set for serial access ([rig.access].type='serial')"
                        .to_string(),
                );
            }
            if access.baud.unwrap_or(0) == 0 {
                return Err(
                    "[rig.access].baud must be > 0 for serial access ([rig.access].type='serial')"
                        .to_string(),
                );
            }
        }
        "tcp" => {
            if access.host.as_deref().unwrap_or("").trim().is_empty() {
                return Err(
                    "[rig.access].host must be set for tcp access ([rig.access].type='tcp')"
                        .to_string(),
                );
            }
            if access.tcp_port.unwrap_or(0) == 0 {
                return Err(
                    "[rig.access].tcp_port must be > 0 for tcp access ([rig.access].type='tcp')"
                        .to_string(),
                );
            }
        }
        other => {
            return Err(format!(
                "[rig.access].type '{}' is invalid (expected 'serial' or 'tcp')",
                other
            ))
        }
    }
    Ok(())
}

fn validate_tokens(path: &str, tokens: &[String]) -> Result<(), String> {
    if tokens.iter().any(|t| t.trim().is_empty()) {
        return Err(format!("{path} must not contain empty tokens"));
    }
    Ok(())
}

impl ConfigFile for ServerConfig {
    fn config_filename() -> &'static str {
        "server.toml"
    }

    fn default_search_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        paths.push(PathBuf::from("trx-server.toml"));
        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("trx-rs").join("server.toml"));
        }
        paths.push(PathBuf::from("/etc/trx-rs/server.toml"));
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.rig.initial_freq_hz, 144_300_000);
        assert_eq!(config.rig.initial_mode, RigMode::USB);
        assert_eq!(config.behavior.poll_interval_ms, 500);
        assert_eq!(config.behavior.max_retries, 3);
        assert!(config.listen.enabled);
        assert_eq!(config.listen.port, 4532);
        assert!(config.listen.auth.tokens.is_empty());
        assert!(config.audio.enabled);
        assert_eq!(config.audio.port, 4533);
        assert_eq!(config.audio.sample_rate, 48000);
        assert!(!config.pskreporter.enabled);
        assert_eq!(config.pskreporter.port, 4739);
    }

    #[test]
    fn test_parse_minimal_toml() {
        let toml_str = r#"
[rig]
model = "ft817"

[rig.access]
type = "serial"
port = "/dev/ttyUSB0"
baud = 9600
"#;

        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.rig.model, Some("ft817".to_string()));
        assert_eq!(config.rig.access.port, Some("/dev/ttyUSB0".to_string()));
        assert_eq!(config.rig.access.baud, Some(9600));
    }

    #[test]
    fn test_parse_full_toml() {
        let toml_str = r#"
[general]
callsign = "W1AW"
log_level = "debug"

[rig]
model = "ft817"
initial_freq_hz = 7100000
initial_mode = "LSB"

[rig.access]
type = "serial"
port = "/dev/ttyUSB0"
baud = 9600

[behavior]
poll_interval_ms = 1000
poll_interval_tx_ms = 200
max_retries = 5
retry_base_delay_ms = 50

[listen]
enabled = true
listen = "0.0.0.0"
port = 5000

[listen.auth]
tokens = ["secret123"]
"#;

        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.general.callsign, Some("W1AW".to_string()));
        assert_eq!(config.general.log_level, Some("debug".to_string()));
        assert_eq!(config.rig.initial_freq_hz, 7_100_000);
        assert_eq!(config.rig.initial_mode, RigMode::LSB);
        assert_eq!(config.behavior.poll_interval_ms, 1000);
        assert_eq!(config.behavior.max_retries, 5);
        assert!(config.listen.enabled);
        assert_eq!(
            config.listen.listen,
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0))
        );
        assert_eq!(config.listen.port, 5000);
        assert_eq!(config.listen.auth.tokens, vec!["secret123".to_string()]);
    }

    #[test]
    fn test_example_toml_parses() {
        let example = ServerConfig::example_toml();
        let _config: ServerConfig = toml::from_str(&example).unwrap();
    }

    #[test]
    fn test_validate_rejects_invalid_coordinates() {
        let mut config = ServerConfig::default();
        config.general.latitude = Some(120.0);
        config.general.longitude = Some(10.0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_invalid_audio_frame_duration() {
        let mut config = ServerConfig::default();
        config.rig.access.port = Some("/dev/ttyUSB0".to_string());
        config.rig.access.baud = Some(9600);
        config.audio.frame_duration_ms = 7;
        assert!(config.validate().is_err());
    }
}
