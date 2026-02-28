// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Configuration file support for trx-client.
//!
//! Config is loaded from the `[trx-client]` section of `trx-rs.toml`.
//! Default search order:
//! 1. Path specified via `--config` CLI argument
//! 2. `./trx-rs.toml`
//! 3. `~/.config/trx-rs/trx-rs.toml`
//! 4. `/etc/trx-rs/trx-rs.toml`

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// Optional target rig ID on the remote multi-rig server.
    pub rig_id: Option<String>,
    /// Remote auth settings.
    pub auth: RemoteAuthConfig,
    /// Poll interval in milliseconds.
    pub poll_interval_ms: u64,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            url: None,
            rig_id: None,
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
    /// Optional per-rig audio port overrides for multi-rig servers.
    pub rig_ports: HashMap<String, u16>,
    /// Local audio bridge (virtual device integration)
    pub bridge: AudioBridgeConfig,
}

impl Default for AudioClientConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            server_port: 4531,
            rig_ports: HashMap::new(),
            bridge: AudioBridgeConfig::default(),
        }
    }
}

/// Local audio bridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioBridgeConfig {
    /// Enable local cpal bridge between remote stream and local audio devices.
    pub enabled: bool,
    /// Local output device for remote RX playback.
    pub rx_output_device: Option<String>,
    /// Local input device for TX uplink capture.
    pub tx_input_device: Option<String>,
    /// Opus bitrate in bits per second for TX uplink capture.
    pub bitrate_bps: u32,
    /// RX playback gain multiplier.
    pub rx_gain: f32,
    /// TX capture gain multiplier.
    pub tx_gain: f32,
}

impl Default for AudioBridgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rx_output_device: None,
            tx_input_device: None,
            bitrate_bps: 192000,
            rx_gain: 1.0,
            tx_gain: 1.0,
        }
    }
}

/// Cookie SameSite attribute options.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub enum CookieSameSite {
    /// Strict: cookie only sent in same-site context
    Strict,
    /// Lax: cookie sent with top-level navigation (default)
    #[default]
    Lax,
    /// None: cookie sent in all contexts (requires Secure=true)
    None,
}

impl AsRef<str> for CookieSameSite {
    fn as_ref(&self) -> &str {
        match self {
            Self::Strict => "Strict",
            Self::Lax => "Lax",
            Self::None => "None",
        }
    }
}

/// HTTP frontend authentication configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpAuthConfig {
    /// Enable HTTP frontend authentication
    pub enabled: bool,
    /// Passphrase for read-only access (rx role)
    pub rx_passphrase: Option<String>,
    /// Passphrase for full control access (control role)
    pub control_passphrase: Option<String>,
    /// Enforce TX/PTT access control (hide from unauthenticated/rx users)
    pub tx_access_control_enabled: bool,
    /// Session time-to-live in minutes
    pub session_ttl_min: u64,
    /// Set Secure flag on session cookie (required for HTTPS)
    pub cookie_secure: bool,
    /// SameSite attribute for session cookie
    pub cookie_same_site: CookieSameSite,
}

impl Default for HttpAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rx_passphrase: None,
            control_passphrase: None,
            tx_access_control_enabled: true,
            session_ttl_min: 480,
            cookie_secure: false,
            cookie_same_site: CookieSameSite::Lax,
        }
    }
}

impl HttpAuthConfig {
    /// Convert session TTL from minutes to Duration
    #[allow(dead_code)]
    pub fn session_ttl(&self) -> Duration {
        Duration::from_secs(self.session_ttl_min * 60)
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
    /// Default rig selected in the web UI on startup.
    pub default_rig_id: Option<String>,
    /// Whether to expose the RF Gain control in the web UI.
    pub show_sdr_gain_control: bool,
    /// Authentication settings
    pub auth: HttpAuthConfig,
}

impl Default for HttpFrontendConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: IpAddr::from([127, 0, 0, 1]),
            port: 8080,
            default_rig_id: None,
            show_sdr_gain_control: true,
            auth: HttpAuthConfig::default(),
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
    /// Legacy shared-listener port. Ignored; per-rig ports must be configured.
    pub port: u16,
    /// Per-rig rigctl listener ports.
    /// Maps rig ID -> local rigctl port. One rigctl listener is spawned per
    /// entry, each routing commands to its assigned rig.
    pub rig_ports: HashMap<String, u16>,
}

impl Default for RigctlFrontendConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: IpAddr::from([127, 0, 0, 1]),
            port: 4532,
            rig_ports: HashMap::new(),
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
    pub fn validate(&self) -> Result<(), String> {
        validate_log_level(self.general.log_level.as_deref())?;

        if self.remote.poll_interval_ms == 0 {
            return Err("[remote].poll_interval_ms must be > 0".to_string());
        }
        if let Some(url) = &self.remote.url {
            if url.trim().is_empty() {
                return Err("[remote].url must not be empty when set".to_string());
            }
        }
        if let Some(rig_id) = &self.remote.rig_id {
            if rig_id.trim().is_empty() {
                return Err("[remote].rig_id must not be empty when set".to_string());
            }
        }
        if let Some(token) = &self.remote.auth.token {
            if token.trim().is_empty() {
                return Err("[remote.auth].token must not be empty when set".to_string());
            }
        }

        if self.frontends.http.enabled && self.frontends.http.port == 0 {
            return Err("[frontends.http].port must be > 0 when enabled".to_string());
        }
        if let Some(rig_id) = &self.frontends.http.default_rig_id {
            if rig_id.trim().is_empty() {
                return Err("[frontends.http].default_rig_id must not be empty when set".to_string());
            }
        }
        if self.frontends.rigctl.enabled && self.frontends.rigctl.rig_ports.is_empty() {
            return Err(
                "[frontends.rigctl].rig_ports must contain at least one rig when enabled"
                    .to_string(),
            );
        }
        for (rig_id, port) in &self.frontends.rigctl.rig_ports {
            if rig_id.trim().is_empty() {
                return Err("[frontends.rigctl].rig_ports keys must not be empty".to_string());
            }
            if *port == 0 {
                return Err(format!(
                    "[frontends.rigctl].rig_ports[\"{}\"] must be > 0",
                    rig_id
                ));
            }
        }
        if self.frontends.audio.enabled && self.frontends.audio.server_port == 0 {
            return Err("[frontends.audio].server_port must be > 0 when enabled".to_string());
        }
        for (rig_id, port) in &self.frontends.audio.rig_ports {
            if rig_id.trim().is_empty() {
                return Err("[frontends.audio].rig_ports keys must not be empty".to_string());
            }
            if *port == 0 {
                return Err(format!(
                    "[frontends.audio].rig_ports[\"{}\"] must be > 0",
                    rig_id
                ));
            }
        }
        if !self.frontends.audio.bridge.rx_gain.is_finite()
            || self.frontends.audio.bridge.rx_gain < 0.0
        {
            return Err("[frontends.audio.bridge].rx_gain must be finite and >= 0".to_string());
        }
        if !self.frontends.audio.bridge.tx_gain.is_finite()
            || self.frontends.audio.bridge.tx_gain < 0.0
        {
            return Err("[frontends.audio.bridge].tx_gain must be finite and >= 0".to_string());
        }
        if self.frontends.audio.bridge.bitrate_bps == 0 {
            return Err("[frontends.audio.bridge].bitrate_bps must be > 0".to_string());
        }
        validate_tokens(
            "[frontends.http_json.auth].tokens",
            &self.frontends.http_json.auth.tokens,
        )?;

        validate_http_auth(&self.frontends.http.auth)?;

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

    /// Generate an example configuration wrapped under the `[trx-client]`
    /// section header, suitable for use in a combined `trx-rs.toml` file.
    pub fn example_combined_toml() -> String {
        #[derive(serde::Serialize)]
        struct Wrapper {
            #[serde(rename = "trx-client")]
            inner: ClientConfig,
        }
        let example = ClientConfig {
            general: GeneralConfig {
                callsign: Some("N0CALL".to_string()),
                log_level: Some("info".to_string()),
            },
            remote: RemoteConfig {
                url: Some("192.168.1.100:9000".to_string()),
                rig_id: Some("hf".to_string()),
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
                    default_rig_id: Some("hf".to_string()),
                    show_sdr_gain_control: true,
                    auth: HttpAuthConfig {
                        enabled: false,
                        rx_passphrase: Some("rx-passphrase-example".to_string()),
                        control_passphrase: Some("control-passphrase-example".to_string()),
                        tx_access_control_enabled: true,
                        session_ttl_min: 480,
                        cookie_secure: false,
                        cookie_same_site: CookieSameSite::Lax,
                    },
                },
                rigctl: RigctlFrontendConfig {
                    enabled: false,
                    listen: IpAddr::from([127, 0, 0, 1]),
                    port: 4532,
                    rig_ports: HashMap::new(),
                },
                http_json: HttpJsonFrontendConfig::default(),
                audio: AudioClientConfig::default(),
            },
        };
        toml::to_string_pretty(&Wrapper { inner: example }).unwrap_or_default()
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

fn validate_tokens(path: &str, tokens: &[String]) -> Result<(), String> {
    if tokens.iter().any(|t| t.trim().is_empty()) {
        return Err(format!("{path} must not contain empty tokens"));
    }
    Ok(())
}

fn validate_http_auth(auth: &HttpAuthConfig) -> Result<(), String> {
    if !auth.enabled {
        return Ok(());
    }

    // If enabled, require at least one passphrase
    if auth.rx_passphrase.is_none() && auth.control_passphrase.is_none() {
        return Err(
            "[frontends.http.auth] enabled=true requires at least one passphrase \
             (rx_passphrase and/or control_passphrase)"
                .to_string(),
        );
    }

    // Validate passphrases are not empty strings
    if let Some(rx) = &auth.rx_passphrase {
        if rx.trim().is_empty() {
            return Err("[frontends.http.auth].rx_passphrase must not be empty if set".to_string());
        }
    }
    if let Some(ctrl) = &auth.control_passphrase {
        if ctrl.trim().is_empty() {
            return Err(
                "[frontends.http.auth].control_passphrase must not be empty if set".to_string(),
            );
        }
    }

    // Session TTL must be > 0
    if auth.session_ttl_min == 0 {
        return Err("[frontends.http.auth].session_ttl_min must be > 0".to_string());
    }

    Ok(())
}

impl ConfigFile for ClientConfig {
    fn section_key() -> &'static str {
        "trx-client"
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
        assert_eq!(config.frontends.audio.server_port, 4531);
        assert!(config.frontends.audio.rig_ports.is_empty());
        assert!(!config.frontends.audio.bridge.enabled);
        assert_eq!(config.frontends.audio.bridge.rx_gain, 1.0);
        assert_eq!(config.frontends.audio.bridge.tx_gain, 1.0);
    }

    #[test]
    fn test_parse_client_toml() {
        let toml_str = r#"
[general]
callsign = "W1AW"

[remote]
url = "192.168.1.100:9000"
rig_id = "hf"
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
        assert_eq!(config.remote.rig_id, Some("hf".to_string()));
        assert_eq!(config.remote.auth.token, Some("my-token".to_string()));
        assert_eq!(config.remote.poll_interval_ms, 500);
        assert!(config.frontends.http.enabled);
    }

    #[test]
    fn test_example_combined_toml_parses() {
        let example = ClientConfig::example_combined_toml();
        let table: toml::Table = toml::from_str(&example).unwrap();
        let section = toml::to_string(table.get("trx-client").unwrap()).unwrap();
        let _config: ClientConfig = toml::from_str(&section).unwrap();
    }

    #[test]
    fn test_validate_rejects_zero_poll_interval() {
        let mut config = ClientConfig::default();
        config.remote.poll_interval_ms = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_empty_remote_rig_id() {
        let mut config = ClientConfig::default();
        config.remote.rig_id = Some("  ".to_string());
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_empty_http_json_token() {
        let mut config = ClientConfig::default();
        config.frontends.http_json.auth.tokens = vec!["".to_string()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_zero_audio_rig_port() {
        let mut config = ClientConfig::default();
        config
            .frontends
            .audio
            .rig_ports
            .insert("ft817".to_string(), 0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_http_auth_enabled_without_passphrases() {
        let mut config = ClientConfig::default();
        config.frontends.http.auth.enabled = true;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_accepts_http_auth_with_rx_passphrase() {
        let mut config = ClientConfig::default();
        config.frontends.http.auth.enabled = true;
        config.frontends.http.auth.rx_passphrase = Some("rx-secret".to_string());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_accepts_http_auth_with_control_passphrase() {
        let mut config = ClientConfig::default();
        config.frontends.http.auth.enabled = true;
        config.frontends.http.auth.control_passphrase = Some("control-secret".to_string());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_accepts_http_auth_with_both_passphrases() {
        let mut config = ClientConfig::default();
        config.frontends.http.auth.enabled = true;
        config.frontends.http.auth.rx_passphrase = Some("rx-secret".to_string());
        config.frontends.http.auth.control_passphrase = Some("control-secret".to_string());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_empty_rx_passphrase() {
        let mut config = ClientConfig::default();
        config.frontends.http.auth.enabled = true;
        config.frontends.http.auth.rx_passphrase = Some("".to_string());
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_zero_session_ttl() {
        let mut config = ClientConfig::default();
        config.frontends.http.auth.enabled = true;
        config.frontends.http.auth.rx_passphrase = Some("rx-secret".to_string());
        config.frontends.http.auth.session_ttl_min = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_auth_disabled_ignores_passphrases() {
        let mut config = ClientConfig::default();
        config.frontends.http.auth.enabled = false;
        config.frontends.http.auth.rx_passphrase = Some("".to_string());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_http_auth_config_default() {
        let auth = HttpAuthConfig::default();
        assert!(!auth.enabled);
        assert!(auth.rx_passphrase.is_none());
        assert!(auth.control_passphrase.is_none());
        assert!(auth.tx_access_control_enabled);
        assert_eq!(auth.session_ttl_min, 480);
        assert!(!auth.cookie_secure);
        assert!(matches!(auth.cookie_same_site, CookieSameSite::Lax));
    }

    #[test]
    fn test_http_auth_session_ttl_conversion() {
        let auth = HttpAuthConfig {
            session_ttl_min: 60,
            ..Default::default()
        };
        assert_eq!(auth.session_ttl().as_secs(), 3600);
    }
}
