// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Configuration file support for trx-bin.
//!
//! Supports loading configuration from TOML files with the following search order:
//! 1. Path specified via `--config` CLI argument
//! 2. `./trx-rs.toml` (current directory)
//! 3. `~/.config/trx-rs/config.toml` (XDG config)
//! 4. `/etc/trx-rs/config.toml` (system-wide)
//!
//! CLI arguments override config file values.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use trx_core::rig::state::RigMode;

/// Top-level configuration structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// General settings
    pub general: GeneralConfig,
    /// Rig backend configuration
    pub rig: RigConfig,
    /// Frontend configurations
    pub frontends: FrontendsConfig,
    /// Polling and retry behavior
    pub behavior: BehaviorConfig,
}

/// General application settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Callsign or owner label to display in frontends
    pub callsign: Option<String>,
    /// Log level (trace, debug, info, warn, error)
    pub log_level: Option<String>,
}

/// Rig backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RigConfig {
    /// Rig model (e.g., "ft817", "ic7300")
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

/// Frontend configurations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FrontendsConfig {
    /// HTTP frontend settings
    pub http: HttpFrontendConfig,
    /// rigctl frontend settings
    pub rigctl: RigctlFrontendConfig,
    /// JSON TCP frontend settings
    pub http_json: HttpJsonFrontendConfig,
    /// Qt/QML frontend settings
    pub qt: QtFrontendConfig,
}

/// HTTP frontend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpFrontendConfig {
    /// Whether HTTP frontend is enabled
    pub enabled: bool,
    /// Listen address
    pub listen: IpAddr,
    /// Listen port
    pub port: u16,
}

impl Default for HttpFrontendConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: IpAddr::from([127, 0, 0, 1]),
            port: 8080,
        }
    }
}

/// rigctl frontend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RigctlFrontendConfig {
    /// Whether rigctl frontend is enabled
    pub enabled: bool,
    /// Listen address
    pub listen: IpAddr,
    /// Listen port
    pub port: u16,
}

/// JSON TCP frontend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpJsonFrontendConfig {
    /// Whether JSON TCP frontend is enabled
    pub enabled: bool,
    /// Listen address
    pub listen: IpAddr,
    /// Listen port (0 = ephemeral)
    pub port: u16,
    /// Authorization settings
    pub auth: HttpJsonAuthConfig,
}

/// Qt/QML frontend configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QtFrontendConfig {
    /// Whether Qt frontend is enabled
    pub enabled: bool,
    /// Remote connection settings
    pub remote: QtRemoteConfig,
}

/// Authorization settings for JSON TCP frontend.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpJsonAuthConfig {
    /// Accepted bearer tokens.
    pub tokens: Vec<String>,
}

/// Remote connection settings for Qt frontend.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QtRemoteConfig {
    /// Enable remote mode (no local rig task).
    pub enabled: bool,
    /// Remote URL (host:port or tcp://host:port).
    pub url: Option<String>,
    /// Remote auth settings.
    pub auth: QtRemoteAuthConfig,
}

/// Authentication settings for Qt remote mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QtRemoteAuthConfig {
    /// Bearer token to send with JSON commands.
    pub token: Option<String>,
}

impl Default for HttpJsonFrontendConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            auth: HttpJsonAuthConfig::default(),
        }
    }
}

impl Default for RigctlFrontendConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: IpAddr::from([127, 0, 0, 1]),
            port: 4532,
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

impl Config {
    /// Load configuration from a specific file path.
    pub fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::ReadError(path.to_path_buf(), e.to_string()))?;

        toml::from_str(&contents)
            .map_err(|e| ConfigError::ParseError(path.to_path_buf(), e.to_string()))
    }

    /// Load configuration from the default search paths.
    /// Returns default config if no config file is found.
    pub fn load_from_default_paths() -> Result<(Self, Option<PathBuf>), ConfigError> {
        let search_paths = Self::default_search_paths();

        for path in search_paths {
            if path.exists() {
                let config = Self::load_from_file(&path)?;
                return Ok((config, Some(path)));
            }
        }

        Ok((Self::default(), None))
    }

    /// Get the default search paths for config files.
    pub fn default_search_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // Current directory
        paths.push(PathBuf::from("trx-rs.toml"));

        // XDG config directory
        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("trx-rs").join("config.toml"));
        }

        // System-wide config
        paths.push(PathBuf::from("/etc/trx-rs/config.toml"));

        paths
    }

    /// Generate an example configuration as a TOML string.
    pub fn example_toml() -> String {
        let example = Config {
            general: GeneralConfig {
                callsign: Some("N0CALL".to_string()),
                log_level: Some("info".to_string()),
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
            frontends: FrontendsConfig {
                http: HttpFrontendConfig {
                    enabled: true,
                    listen: IpAddr::from([127, 0, 0, 1]),
                    port: 8080,
                },
                rigctl: RigctlFrontendConfig {
                    enabled: true,
                    listen: IpAddr::from([127, 0, 0, 1]),
                    port: 4532,
                },
                http_json: HttpJsonFrontendConfig::default(),
                qt: QtFrontendConfig::default(),
            },
            behavior: BehaviorConfig::default(),
        };

        toml::to_string_pretty(&example).unwrap_or_default()
    }
}

/// Errors that can occur when loading configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// Failed to read the config file
    ReadError(PathBuf, String),
    /// Failed to parse the config file
    ParseError(PathBuf, String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadError(path, err) => {
                write!(
                    f,
                    "failed to read config file '{}': {}",
                    path.display(),
                    err
                )
            }
            Self::ParseError(path, err) => {
                write!(
                    f,
                    "failed to parse config file '{}': {}",
                    path.display(),
                    err
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.frontends.http.enabled);
        assert!(!config.frontends.rigctl.enabled);
        assert_eq!(config.frontends.http.port, 8080);
        assert_eq!(config.frontends.rigctl.port, 4532);
        assert_eq!(config.rig.initial_freq_hz, 144_300_000);
        assert_eq!(config.rig.initial_mode, RigMode::USB);
        assert!(config.frontends.http_json.enabled);
        assert_eq!(config.frontends.http_json.port, 0);
        assert!(!config.frontends.qt.enabled);
        assert!(!config.frontends.qt.remote.enabled);
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

        let config: Config = toml::from_str(toml_str).unwrap();
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

[frontends.http]
enabled = true
listen = "0.0.0.0"
port = 8080

[frontends.rigctl]
enabled = true
listen = "127.0.0.1"
port = 4532

[frontends.http_json]
enabled = true
listen = "127.0.0.1"
port = 9000
auth.tokens = ["demo-token"]

[frontends.qt]
enabled = true
remote.enabled = true
remote.url = "127.0.0.1:9000"
remote.auth.token = "demo-token"

[behavior]
poll_interval_ms = 1000
poll_interval_tx_ms = 200
max_retries = 5
retry_base_delay_ms = 50
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.general.callsign, Some("W1AW".to_string()));
        assert_eq!(config.general.log_level, Some("debug".to_string()));
        assert_eq!(config.rig.initial_freq_hz, 7_100_000);
        assert_eq!(config.rig.initial_mode, RigMode::LSB);
        assert!(config.frontends.http.enabled);
        assert!(config.frontends.rigctl.enabled);
        assert_eq!(config.behavior.poll_interval_ms, 1000);
        assert_eq!(config.behavior.max_retries, 5);
    }

    #[test]
    fn test_example_toml_parses() {
        let example = Config::example_toml();
        let _config: Config = toml::from_str(&example).unwrap();
    }
}
