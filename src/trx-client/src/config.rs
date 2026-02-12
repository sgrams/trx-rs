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
use trx_app::{ConfigError, ConfigFile};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Callsign or owner label to display in frontends
    pub callsign: Option<String>,
    /// Log level (trace, debug, info, warn, error)
    pub log_level: Option<String>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            callsign: Some("N0CALL".to_string()),
            log_level: None,
        }
    }
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
    /// Audio streaming settings
    pub audio: AudioClientConfig,
}

/// Audio streaming client configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioClientConfig {
    /// Whether audio streaming is enabled
    pub enabled: bool,
    /// Audio TCP port on the remote server
    pub server_port: u16,
}

impl Default for AudioClientConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            server_port: 4533,
        }
    }
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

impl ClientConfig {
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
                audio: AudioClientConfig::default(),
            },
        };

        toml::to_string_pretty(&example).unwrap_or_default()
    }
}

impl ConfigFile for ClientConfig {
    fn config_filename() -> &'static str {
        "client.toml"
    }

    fn default_search_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        paths.push(PathBuf::from("trx-client.toml"));
        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("trx-rs").join("client.toml"));
        }
        paths.push(PathBuf::from("/etc/trx-rs/client.toml"));
        paths
    }
}

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
        assert!(config.remote.url.is_none());
        assert_eq!(config.remote.poll_interval_ms, 750);
        assert!(config.frontends.audio.enabled);
        assert_eq!(config.frontends.audio.server_port, 4533);
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

"#;

        let config: ClientConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.general.callsign, Some("W1AW".to_string()));
        assert_eq!(config.remote.url, Some("192.168.1.100:9000".to_string()));
        assert_eq!(config.remote.auth.token, Some("my-token".to_string()));
        assert_eq!(config.remote.poll_interval_ms, 500);
        assert!(config.frontends.http.enabled);
    }

    #[test]
    fn test_example_toml_parses() {
        let example = ClientConfig::example_toml();
        let _config: ClientConfig = toml::from_str(&example).unwrap();
    }
}
