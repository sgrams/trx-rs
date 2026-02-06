// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Configuration file support for trx-client.
//!
//! Supports loading configuration from TOML files with the following search order:
//! 1. Path specified via `--config` CLI argument
//! 2. `./trx-client.toml` (current directory)
//! 3. `~/.config/trx-rs/client.toml` (XDG config)
//! 4. `/etc/trx-rs/client.toml` (system-wide)

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Top-level client configuration structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    /// General settings
    pub general: GeneralConfig,
    /// Remote connection settings
    pub remote: RemoteConfig,
    /// Frontend configurations
    pub frontends: FrontendsConfig,
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

/// Remote connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteConfig {
    /// Remote URL (host:port or tcp://host:port).
    pub url: Option<String>,
    /// Remote auth settings.
    pub auth: RemoteAuthConfig,
    /// Poll interval in milliseconds.
    pub poll_interval_ms: u64,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            url: None,
            auth: RemoteAuthConfig::default(),
            poll_interval_ms: 750,
        }
    }
}

/// Authentication settings for remote connection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteAuthConfig {
    /// Bearer token to send with JSON commands.
    pub token: Option<String>,
}

/// Frontend configurations (client — includes Qt).
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

impl Default for RigctlFrontendConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: IpAddr::from([127, 0, 0, 1]),
            port: 4532,
        }
    }
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

/// Authorization settings for JSON TCP frontend.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpJsonAuthConfig {
    /// Accepted bearer tokens.
    pub tokens: Vec<String>,
}

/// Qt/QML frontend configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QtFrontendConfig {
    /// Whether Qt frontend is enabled
    pub enabled: bool,
}

impl ClientConfig {
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
        paths.push(PathBuf::from("trx-client.toml"));

        // XDG config directory
        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("trx-rs").join("client.toml"));
        }

        // System-wide config
        paths.push(PathBuf::from("/etc/trx-rs/client.toml"));

        paths
    }

    /// Generate an example configuration as a TOML string.
    pub fn example_toml() -> String {
        let example = ClientConfig {
            general: GeneralConfig {
                callsign: Some("N0CALL".to_string()),
                log_level: Some("info".to_string()),
            },
            remote: RemoteConfig {
                url: Some("192.168.1.100:9000".to_string()),
                auth: RemoteAuthConfig {
                    token: Some("my-token".to_string()),
                },
                poll_interval_ms: 750,
            },
            frontends: FrontendsConfig {
                http: HttpFrontendConfig {
                    enabled: true,
                    listen: IpAddr::from([127, 0, 0, 1]),
                    port: 8080,
                },
                rigctl: RigctlFrontendConfig {
                    enabled: false,
                    listen: IpAddr::from([127, 0, 0, 1]),
                    port: 4532,
                },
                http_json: HttpJsonFrontendConfig::default(),
                qt: QtFrontendConfig { enabled: false },
            },
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
        let config = ClientConfig::default();
        assert!(config.frontends.http.enabled);
        assert!(!config.frontends.rigctl.enabled);
        assert_eq!(config.frontends.http.port, 8080);
        assert_eq!(config.frontends.rigctl.port, 4532);
        assert!(config.frontends.http_json.enabled);
        assert_eq!(config.frontends.http_json.port, 0);
        assert!(!config.frontends.qt.enabled);
        assert!(config.remote.url.is_none());
        assert_eq!(config.remote.poll_interval_ms, 750);
    }

    #[test]
    fn test_parse_client_toml() {
        let toml_str = r#"
[general]
callsign = "W1AW"

[remote]
url = "192.168.1.100:9000"
auth.token = "my-token"
poll_interval_ms = 500

[frontends.http]
enabled = true
listen = "127.0.0.1"
port = 8080

[frontends.qt]
enabled = true
"#;

        let config: ClientConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.general.callsign, Some("W1AW".to_string()));
        assert_eq!(config.remote.url, Some("192.168.1.100:9000".to_string()));
        assert_eq!(config.remote.auth.token, Some("my-token".to_string()));
        assert_eq!(config.remote.poll_interval_ms, 500);
        assert!(config.frontends.http.enabled);
        assert!(config.frontends.qt.enabled);
    }

    #[test]
    fn test_example_toml_parses() {
        let example = ClientConfig::example_toml();
        let _config: ClientConfig = toml::from_str(&example).unwrap();
    }
}
