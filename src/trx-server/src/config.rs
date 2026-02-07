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
}

/// General application settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Callsign or owner label
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
            enabled: false,
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

impl ServerConfig {
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
        paths.push(PathBuf::from("trx-server.toml"));

        // XDG config directory
        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("trx-rs").join("server.toml"));
        }

        // System-wide config
        paths.push(PathBuf::from("/etc/trx-rs/server.toml"));

        paths
    }

    /// Generate an example configuration as a TOML string.
    pub fn example_toml() -> String {
        let example = ServerConfig {
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
            behavior: BehaviorConfig::default(),
            listen: ListenConfig::default(),
            audio: AudioConfig::default(),
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
        let config = ServerConfig::default();
        assert_eq!(config.rig.initial_freq_hz, 144_300_000);
        assert_eq!(config.rig.initial_mode, RigMode::USB);
        assert_eq!(config.behavior.poll_interval_ms, 500);
        assert_eq!(config.behavior.max_retries, 3);
        assert!(config.listen.enabled);
        assert_eq!(config.listen.port, 4532);
        assert!(config.listen.auth.tokens.is_empty());
        assert!(!config.audio.enabled);
        assert_eq!(config.audio.port, 4533);
        assert_eq!(config.audio.sample_rate, 48000);
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
}
