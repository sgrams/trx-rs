// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Configuration file support for trx-server.
//!
//! Config is loaded from the `[trx-server]` section of `trx-rs.toml`.
//! Default search order:
//! 1. Path specified via `--config` CLI argument
//! 2. `./trx-rs.toml`
//! 3. `~/.config/trx-rs/trx-rs.toml`
//! 4. `/etc/trx-rs/trx-rs.toml`

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trx_app::{ConfigError, ConfigFile};
pub use trx_decode_log::DecodeLogsConfig;

use trx_core::rig::state::RigMode;

/// Per-rig instance configuration for multi-rig setups.
///
/// Each entry in `[[rigs]]` becomes one of these.  The flat top-level
/// `[rig]` / `[audio]` / `[sdr]` / `[pskreporter]` / `[aprsfi]` /
/// `[behavior]` / `[decode_logs]` fields are still supported via
/// `ServerConfig::resolved_rigs()` which synthesises a single-element list
/// with `id = "default"` when `rigs` is empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RigInstanceConfig {
    /// Stable rig identifier used in protocol routing.
    pub id: String,
    /// Display name for the rig (e.g., "HF Transceiver", "VHF/UHF SDR").
    /// If not specified, defaults to the rig id.
    pub name: Option<String>,
    /// Rig backend configuration.
    pub rig: RigConfig,
    /// Polling and retry behavior.
    pub behavior: BehaviorConfig,
    /// Audio streaming configuration for this rig.
    pub audio: AudioConfig,
    /// SDR pipeline configuration (only used when [rigs.rig.access] type = "sdr").
    pub sdr: SdrConfig,
    /// PSK Reporter uplink for this rig.
    pub pskreporter: PskReporterConfig,
    /// APRS-IS IGate uplink for this rig.
    pub aprsfi: AprsFiConfig,
    /// Decoder file logging for this rig.
    pub decode_logs: DecodeLogsConfig,
}

impl RigInstanceConfig {
    /// Get the display name for this rig.
    /// Returns the configured name if set, otherwise the id.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// Top-level server configuration structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// General settings
    pub general: GeneralConfig,
    /// Rig backend configuration (legacy flat; use [[rigs]] for multi-rig)
    pub rig: RigConfig,
    /// Polling and retry behavior (legacy flat)
    pub behavior: BehaviorConfig,
    /// TCP listener configuration
    pub listen: ListenConfig,
    /// Audio streaming configuration (legacy flat)
    pub audio: AudioConfig,
    /// PSK Reporter uplink configuration (legacy flat)
    pub pskreporter: PskReporterConfig,
    /// APRS-IS IGate uplink configuration (legacy flat)
    pub aprsfi: AprsFiConfig,
    /// Decoder file logging configuration (legacy flat)
    pub decode_logs: DecodeLogsConfig,
    /// SDR pipeline configuration (legacy flat; used when [rig.access] type = "sdr").
    pub sdr: SdrConfig,
    /// Multi-rig instance list. When non-empty, takes priority over the flat fields.
    #[serde(rename = "rigs", default)]
    pub rigs: Vec<RigInstanceConfig>,
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
    /// SoapySDR device args string (for sdr access), e.g. "driver=rtlsdr".
    pub args: Option<String>,
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
            port: 4530,
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
            port: 4531,
            rx_enabled: true,
            tx_enabled: true,
            device: None,
            sample_rate: 48000,
            channels: 2,
            frame_duration_ms: 20,
            bitrate_bps: 256000,
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

/// Top-level SDR configuration (only used when [rig.access] type = "sdr").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SdrConfig {
    /// SoapySDR IQ capture sample rate (Hz). Must be supported by the device.
    pub sample_rate: u32,
    /// Hardware IF filter bandwidth (Hz).
    pub bandwidth: u32,
    /// WFM deemphasis time constant in microseconds (50 or 75).
    pub wfm_deemphasis_us: u32,
    /// SDR tunes this many Hz below the dial frequency to keep signal off DC.
    pub center_offset_hz: i64,
    /// Gain configuration.
    pub gain: SdrGainConfig,
    /// Virtual receiver channels (at least one required when SDR backend is active).
    pub channels: Vec<SdrChannelConfig>,
}

impl Default for SdrConfig {
    fn default() -> Self {
        Self {
            sample_rate: 1_920_000,
            bandwidth: 1_500_000,
            wfm_deemphasis_us: 50,
            center_offset_hz: 100_000,
            gain: SdrGainConfig::default(),
            channels: Vec::new(),
        }
    }
}

/// Gain control mode for the SDR device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SdrGainConfig {
    /// "auto" (hardware AGC) or "manual" (fixed dB).
    pub mode: String,
    /// Gain in dB; effective only when mode = "manual".
    pub value: f64,
    /// Optional hard ceiling for the applied hardware gain in dB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value: Option<f64>,
}

impl Default for SdrGainConfig {
    fn default() -> Self {
        Self {
            mode: "auto".to_string(),
            value: 30.0,
            max_value: None,
        }
    }
}

/// One virtual receiver channel within the wideband IQ stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SdrChannelConfig {
    /// Human-readable identifier used in logs.
    pub id: String,
    /// Frequency offset from the dial frequency (Hz). Primary channel should use 0.
    pub offset_hz: i64,
    /// Demodulation mode: "auto" (follows RigCat set_mode) or a fixed RigMode string
    /// (e.g. "USB", "FM").
    pub mode: String,
    /// One-sided bandwidth of the post-demod audio BPF (Hz).
    pub audio_bandwidth_hz: u32,
    /// FIR filter tap count. Higher = sharper roll-off. Default 64.
    pub fir_taps: usize,
    /// CW tone centre frequency in the audio domain (Hz). Default 700.
    pub cw_center_hz: u32,
    /// Pre-demod bandwidth for WFM only (Hz). Default 75000.
    pub wfm_bandwidth_hz: u32,
    /// Decoder names that receive this channel's PCM frames.
    /// Valid values: "ft8", "wspr", "aprs", "cw".
    pub decoders: Vec<String>,
    /// If true, encode this channel's audio as Opus and stream over TCP.
    /// At most one channel may set this to true.
    pub stream_opus: bool,
}

impl Default for SdrChannelConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            offset_hz: 0,
            mode: "auto".to_string(),
            audio_bandwidth_hz: 3000,
            fir_taps: 64,
            cw_center_hz: 700,
            wfm_bandwidth_hz: 75_000,
            decoders: Vec::new(),
            stream_opus: false,
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
            if self.pskreporter.receiver_locator.is_none()
                && (self.general.latitude.is_none() || self.general.longitude.is_none())
            {
                return Err(
                    "[pskreporter] enabled requires either [pskreporter].receiver_locator \
                     or [general].latitude and [general].longitude"
                        .to_string(),
                );
            }
        }

        if self.aprsfi.enabled {
            if self.aprsfi.host.trim().is_empty() {
                return Err("[aprsfi].host must not be empty".to_string());
            }
            if self.aprsfi.port == 0 {
                return Err("[aprsfi].port must be > 0".to_string());
            }
        }

        if let Some(max_gain) = self.sdr.gain.max_value {
            if !max_gain.is_finite() {
                return Err("[sdr.gain].max_value must be finite".to_string());
            }
            if max_gain < 0.0 {
                return Err("[sdr.gain].max_value must be >= 0".to_string());
            }
        }

        // Multi-rig uniqueness checks.
        if !self.rigs.is_empty() {
            let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut seen_ports: std::collections::HashSet<u16> = std::collections::HashSet::new();
            for rig in &self.rigs {
                // Check for explicit duplicate IDs (empty IDs are auto-generated later).
                if !rig.id.trim().is_empty() && !seen_ids.insert(rig.id.clone()) {
                    return Err(format!("[[rigs]] duplicate rig id: \"{}\"", rig.id));
                }
                if rig.audio.enabled && !seen_ports.insert(rig.audio.port) {
                    return Err(format!(
                        "[[rigs]] duplicate audio port {} (rig id: \"{}\")",
                        rig.audio.port, rig.id
                    ));
                }
                if let Some(max_gain) = rig.sdr.gain.max_value {
                    if !max_gain.is_finite() {
                        return Err(format!(
                            "[[rigs]] [sdr.gain].max_value must be finite (rig id: \"{}\")",
                            rig.id
                        ));
                    }
                    if max_gain < 0.0 {
                        return Err(format!(
                            "[[rigs]] [sdr.gain].max_value must be >= 0 (rig id: \"{}\")",
                            rig.id
                        ));
                    }
                }
            }
        }

        if self.decode_logs.enabled {
            if self.decode_logs.dir.trim().is_empty() {
                return Err("[decode_logs].dir must not be empty when enabled".to_string());
            }
            if self.decode_logs.aprs_file.trim().is_empty()
                || self.decode_logs.cw_file.trim().is_empty()
                || self.decode_logs.ft8_file.trim().is_empty()
                || self.decode_logs.wspr_file.trim().is_empty()
            {
                return Err("[decode_logs] file names must not be empty when enabled".to_string());
            }
        }

        Ok(())
    }

    /// Validate SDR-specific config rules (see SDR.md §11).
    /// Returns a Vec of error strings; empty means valid.
    pub fn validate_sdr(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Only validate if access type is "sdr"
        let is_sdr = self.rig.access.access_type.as_deref() == Some("sdr");
        if !is_sdr {
            return errors;
        }

        // args must be non-empty
        if self
            .rig
            .access
            .args
            .as_deref()
            .map(str::is_empty)
            .unwrap_or(true)
        {
            errors.push("[rig.access] args must be non-empty for type = \"sdr\"".into());
        }

        // sample_rate must be non-zero
        if self.sdr.sample_rate == 0 {
            errors.push("[sdr] sample_rate must be > 0".into());
        }

        // Every channel's IF must fit within the captured bandwidth
        let half_rate = self.sdr.sample_rate as i64 / 2;
        for ch in &self.sdr.channels {
            let channel_if = self.sdr.center_offset_hz + ch.offset_hz;
            if channel_if.abs() >= half_rate {
                errors.push(format!(
                    "[sdr.channels] id=\"{}\" IF frequency {} Hz exceeds Nyquist limit ±{} Hz",
                    ch.id, channel_if, half_rate
                ));
            }
        }

        // At most one channel may have stream_opus = true
        let opus_count = self.sdr.channels.iter().filter(|c| c.stream_opus).count();
        if opus_count > 1 {
            errors.push(format!(
                "[sdr.channels] at most one channel may have stream_opus = true (found {})",
                opus_count
            ));
        }

        // tx_enabled must be false with SDR backend
        if self.audio.tx_enabled {
            errors.push("[audio] tx_enabled must be false when using the soapysdr backend".into());
        }

        // Decoder names must not appear in more than one channel
        let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for ch in &self.sdr.channels {
            for dec in &ch.decoders {
                if let Some(prev_id) = seen.get(dec) {
                    errors.push(format!(
                        "[sdr.channels] decoder \"{}\" appears in both \"{}\" and \"{}\"",
                        dec, prev_id, ch.id
                    ));
                } else {
                    seen.insert(dec.clone(), ch.id.clone());
                }
            }
        }

        errors
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

    /// Return the effective list of rig instances to spawn.
    ///
    /// When `[[rigs]]` entries are present they are returned as-is.
    /// Otherwise the legacy flat `[rig]` / `[audio]` / … fields are synthesised
    /// into a single `RigInstanceConfig` with `id = "default"`.
    pub fn resolved_rigs(&self) -> Vec<RigInstanceConfig> {
        if !self.rigs.is_empty() {
            // Auto-generate IDs for rigs that don't have explicit ones.
            return self
                .rigs
                .iter()
                .enumerate()
                .map(|(idx, rig)| {
                    let id = if rig.id.trim().is_empty() {
                        // Generate ID from model name with counter.
                        let model = rig.rig.model.as_deref().unwrap_or("unknown").to_lowercase();
                        format!("{}_{}", model, idx)
                    } else {
                        rig.id.clone()
                    };

                    RigInstanceConfig { id, ..rig.clone() }
                })
                .collect();
        }
        vec![RigInstanceConfig {
            id: "default".to_string(),
            name: None,
            rig: self.rig.clone(),
            behavior: self.behavior.clone(),
            audio: self.audio.clone(),
            sdr: self.sdr.clone(),
            pskreporter: self.pskreporter.clone(),
            aprsfi: self.aprsfi.clone(),
            decode_logs: self.decode_logs.clone(),
        }]
    }

    /// Generate an example configuration wrapped under the `[trx-server]`
    /// section header, suitable for use in a combined `trx-rs.toml` file.
    pub fn example_combined_toml() -> String {
        #[derive(serde::Serialize)]
        struct Wrapper {
            #[serde(rename = "trx-server")]
            inner: ServerConfig,
        }
        let example = ServerConfig {
            general: GeneralConfig {
                callsign: Some("N0CALL".to_string()),
                log_level: Some("info".to_string()),
                latitude: Some(52.2297),
                longitude: Some(21.0122),
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
                    args: None,
                },
            },
            behavior: BehaviorConfig::default(),
            listen: ListenConfig::default(),
            audio: AudioConfig::default(),
            pskreporter: PskReporterConfig::default(),
            aprsfi: AprsFiConfig::default(),
            decode_logs: DecodeLogsConfig::default(),
            sdr: SdrConfig::default(),
            rigs: Vec::new(),
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
        "sdr" => {
            // SDR-specific validation is handled by validate_sdr()
        }
        other => {
            return Err(format!(
                "[rig.access].type '{}' is invalid (expected 'serial', 'tcp', or 'sdr')",
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
    fn section_key() -> &'static str {
        "trx-server"
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
        assert_eq!(config.listen.port, 4530);
        assert!(config.listen.auth.tokens.is_empty());
        assert!(config.audio.enabled);
        assert_eq!(config.audio.port, 4531);
        assert_eq!(config.audio.sample_rate, 48000);
        assert!(!config.pskreporter.enabled);
        assert_eq!(config.pskreporter.port, 4739);
        assert!(!config.aprsfi.enabled);
        assert_eq!(config.aprsfi.host, "rotate.aprs.net");
        assert_eq!(config.aprsfi.port, 14580);
        assert_eq!(config.aprsfi.passcode, -1);
        assert!(!config.decode_logs.enabled);
        assert!(std::path::Path::new(&config.decode_logs.dir)
            .ends_with(std::path::Path::new("decoders")));
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
    fn test_example_combined_toml_parses() {
        let example = ServerConfig::example_combined_toml();
        let table: toml::Table = toml::from_str(&example).unwrap();
        let section = toml::to_string(table.get("trx-server").unwrap()).unwrap();
        let _config: ServerConfig = toml::from_str(&section).unwrap();
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

    #[test]
    fn test_validate_pskreporter_requires_locator_source() {
        let mut config = ServerConfig::default();
        config.rig.access.port = Some("/dev/ttyUSB0".to_string());
        config.rig.access.baud = Some(9600);
        config.pskreporter.enabled = true;
        config.pskreporter.receiver_locator = None;
        config.general.latitude = None;
        config.general.longitude = None;
        assert!(config.validate().is_err());

        config.general.latitude = Some(52.0);
        config.general.longitude = Some(21.0);
        assert!(config.validate().is_ok());
    }

    // --- SDR-11: validate_sdr() unit tests ---

    fn sdr_config_with_access(args: &str) -> ServerConfig {
        let mut cfg = ServerConfig::default();
        cfg.rig.access.access_type = Some("sdr".to_string());
        cfg.rig.access.args = Some(args.to_string());
        cfg.audio.tx_enabled = false;
        cfg.sdr.sample_rate = 1_920_000;
        cfg.sdr.center_offset_hz = 200_000;
        cfg
    }

    fn add_channel(
        cfg: &mut ServerConfig,
        id: &str,
        offset_hz: i64,
        stream_opus: bool,
        decoders: Vec<String>,
    ) {
        cfg.sdr.channels.push(SdrChannelConfig {
            id: id.to_string(),
            offset_hz,
            stream_opus,
            decoders,
            ..SdrChannelConfig::default()
        });
    }

    #[test]
    fn test_sdr_validate_ok_minimal() {
        let mut cfg = sdr_config_with_access("driver=rtlsdr");
        add_channel(&mut cfg, "primary", 0, false, vec![]);
        let errors = cfg.validate_sdr();
        assert!(errors.is_empty(), "expected no errors, got: {:?}", errors);
    }

    #[test]
    fn test_sdr_validate_non_sdr_skips() {
        let cfg = ServerConfig::default();
        let errors = cfg.validate_sdr();
        assert!(
            errors.is_empty(),
            "expected no errors for non-sdr config, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_sdr_validate_empty_args() {
        let cfg = sdr_config_with_access("");
        let errors = cfg.validate_sdr();
        assert_eq!(
            errors.len(),
            1,
            "expected exactly 1 error, got: {:?}",
            errors
        );
        assert!(
            errors[0].contains("args"),
            "expected error to mention 'args', got: {}",
            errors[0]
        );
    }

    #[test]
    fn test_sdr_validate_missing_args() {
        let mut cfg = sdr_config_with_access("placeholder");
        cfg.rig.access.args = None;
        let errors = cfg.validate_sdr();
        assert_eq!(
            errors.len(),
            1,
            "expected exactly 1 error, got: {:?}",
            errors
        );
        assert!(
            errors[0].contains("args"),
            "expected error to mention 'args', got: {}",
            errors[0]
        );
    }

    #[test]
    fn test_sdr_validate_zero_sample_rate() {
        let mut cfg = sdr_config_with_access("driver=rtlsdr");
        cfg.sdr.sample_rate = 0;
        let errors = cfg.validate_sdr();
        assert!(
            errors.iter().any(|e| e.contains("sample_rate")),
            "expected error mentioning 'sample_rate', got: {:?}",
            errors
        );
    }

    #[test]
    fn test_sdr_validate_channel_if_out_of_range() {
        // sample_rate=1_000_000 => Nyquist=500_000
        // center_offset_hz=0, offset_hz=600_000 => IF=600_000 > 500_000
        let mut cfg = sdr_config_with_access("driver=rtlsdr");
        cfg.sdr.sample_rate = 1_000_000;
        cfg.sdr.center_offset_hz = 0;
        add_channel(&mut cfg, "ch_high", 600_000, false, vec![]);
        let errors = cfg.validate_sdr();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("ch_high") && (e.contains("Nyquist") || e.contains("exceeds"))),
            "expected error mentioning channel id and Nyquist/exceeds, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_sdr_validate_channel_if_negative_out_of_range() {
        // sample_rate=1_000_000 => Nyquist=500_000
        // center_offset_hz=0, offset_hz=-600_000 => IF=-600_000, abs=600_000 > 500_000
        let mut cfg = sdr_config_with_access("driver=rtlsdr");
        cfg.sdr.sample_rate = 1_000_000;
        cfg.sdr.center_offset_hz = 0;
        add_channel(&mut cfg, "ch_low", -600_000, false, vec![]);
        let errors = cfg.validate_sdr();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("ch_low") && (e.contains("Nyquist") || e.contains("exceeds"))),
            "expected error mentioning channel id and Nyquist/exceeds, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_sdr_validate_channel_if_exactly_nyquist_is_invalid() {
        // sample_rate=1_000_000 => Nyquist=500_000
        // IF=500_000 is NOT strictly less than 500_000 => invalid
        let mut cfg = sdr_config_with_access("driver=rtlsdr");
        cfg.sdr.sample_rate = 1_000_000;
        cfg.sdr.center_offset_hz = 0;
        add_channel(&mut cfg, "ch_nyquist", 500_000, false, vec![]);
        let errors = cfg.validate_sdr();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("ch_nyquist")
                    && (e.contains("Nyquist") || e.contains("exceeds"))),
            "expected error for IF exactly at Nyquist, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_sdr_validate_dual_stream_opus() {
        let mut cfg = sdr_config_with_access("driver=rtlsdr");
        add_channel(&mut cfg, "ch1", 0, true, vec![]);
        add_channel(&mut cfg, "ch2", 10_000, true, vec![]);
        let errors = cfg.validate_sdr();
        assert!(
            errors.iter().any(|e| e.contains("stream_opus")),
            "expected error mentioning 'stream_opus', got: {:?}",
            errors
        );
    }

    #[test]
    fn test_sdr_validate_tx_enabled_with_sdr() {
        let mut cfg = sdr_config_with_access("driver=rtlsdr");
        cfg.audio.tx_enabled = true;
        let errors = cfg.validate_sdr();
        assert!(
            errors.iter().any(|e| e.contains("tx_enabled")),
            "expected error mentioning 'tx_enabled', got: {:?}",
            errors
        );
    }

    #[test]
    fn test_sdr_validate_duplicate_decoder() {
        let mut cfg = sdr_config_with_access("driver=rtlsdr");
        add_channel(&mut cfg, "ch1", 0, false, vec!["ft8".to_string()]);
        add_channel(&mut cfg, "ch2", 10_000, false, vec!["ft8".to_string()]);
        let errors = cfg.validate_sdr();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("ft8") || e.contains("decoder")),
            "expected error mentioning 'ft8' or 'decoder', got: {:?}",
            errors
        );
    }

    #[test]
    fn test_sdr_validate_multiple_errors() {
        let mut cfg = sdr_config_with_access("placeholder");
        cfg.rig.access.args = None;
        cfg.sdr.sample_rate = 0;
        cfg.audio.tx_enabled = true;
        let errors = cfg.validate_sdr();
        assert_eq!(
            errors.len(),
            3,
            "expected exactly 3 errors, got: {:?}",
            errors
        );
    }

    // --- MR-08: multi-rig config tests ---

    #[test]
    fn test_resolved_rigs_legacy_flat_fields() {
        let mut cfg = ServerConfig::default();
        cfg.rig.model = Some("ft817".to_string());
        cfg.rig.access.access_type = Some("serial".to_string());
        cfg.rig.access.port = Some("/dev/ttyUSB0".to_string());
        cfg.rig.access.baud = Some(9600);

        let rigs = cfg.resolved_rigs();
        assert_eq!(rigs.len(), 1);
        assert_eq!(rigs[0].id, "default");
        assert_eq!(rigs[0].rig.model, Some("ft817".to_string()));
    }

    #[test]
    fn test_resolved_rigs_multi_rig_toml() {
        let toml_str = r#"
[general]
callsign = "W1AW"

[[rigs]]
id = "hf"

[rigs.rig]
model = "ft450d"
initial_freq_hz = 14074000

[rigs.rig.access]
type = "serial"
port = "/dev/ttyUSB0"
baud = 9600

[rigs.audio]
port = 4531

[[rigs]]
id = "sdr"

[rigs.rig]
model = "soapysdr"

[rigs.rig.access]
type = "sdr"
args = "driver=rtlsdr"

[rigs.audio]
port = 4532
"#;
        let cfg: ServerConfig = toml::from_str(toml_str).unwrap();
        let rigs = cfg.resolved_rigs();
        assert_eq!(rigs.len(), 2);
        assert_eq!(rigs[0].id, "hf");
        assert_eq!(rigs[0].rig.model, Some("ft450d".to_string()));
        assert_eq!(rigs[0].audio.port, 4531);
        assert_eq!(rigs[1].id, "sdr");
        assert_eq!(rigs[1].rig.model, Some("soapysdr".to_string()));
        assert_eq!(rigs[1].audio.port, 4532);
    }

    #[test]
    fn test_validate_rejects_duplicate_rig_ids() {
        let toml_str = r#"
[[rigs]]
id = "rig1"
[rigs.rig]
model = "ft817"
[rigs.rig.access]
type = "serial"
port = "/dev/ttyUSB0"
baud = 9600
[rigs.audio]
port = 4531

[[rigs]]
id = "rig1"
[rigs.rig]
model = "ft450d"
[rigs.rig.access]
type = "serial"
port = "/dev/ttyUSB1"
baud = 9600
[rigs.audio]
port = 4532
"#;
        let cfg: ServerConfig = toml::from_str(toml_str).unwrap();
        let result = cfg.validate();
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("duplicate rig id"),
            "expected error about duplicate rig id"
        );
    }

    #[test]
    fn test_validate_rejects_duplicate_audio_ports() {
        let toml_str = r#"
[[rigs]]
id = "rig1"
[rigs.rig]
model = "ft817"
[rigs.rig.access]
type = "serial"
port = "/dev/ttyUSB0"
baud = 9600
[rigs.audio]
port = 4531

[[rigs]]
id = "rig2"
[rigs.rig]
model = "ft450d"
[rigs.rig.access]
type = "serial"
port = "/dev/ttyUSB1"
baud = 9600
[rigs.audio]
port = 4531
"#;
        let cfg: ServerConfig = toml::from_str(toml_str).unwrap();
        let result = cfg.validate();
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("duplicate audio port"),
            "expected error about duplicate audio port"
        );
    }

    #[test]
    fn test_validate_accepts_multi_rig_unique_ids_and_ports() {
        let toml_str = r#"
[[rigs]]
id = "hf"
[rigs.rig]
model = "ft450d"
[rigs.rig.access]
type = "serial"
port = "/dev/ttyUSB0"
baud = 9600
[rigs.audio]
port = 4531

[[rigs]]
id = "sdr"
[rigs.rig]
model = "soapysdr"
[rigs.rig.access]
type = "sdr"
args = "driver=rtlsdr"
[rigs.audio]
port = 4532
"#;
        let cfg: ServerConfig = toml::from_str(toml_str).unwrap();
        // validate() uses the flat [rig] field for rig-level checks; multi-rig
        // validation focuses on ID/port uniqueness. The flat [rig] is default
        // (no model), so the access check is skipped when both fields are absent.
        assert!(
            cfg.validate().is_ok(),
            "expected Ok for valid multi-rig config"
        );
    }
}
