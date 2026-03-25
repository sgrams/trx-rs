// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::fmt::Write as _;
use std::path::Path;

use toml_edit::DocumentMut;

/// Known top-level keys for a standalone server config.
const SERVER_KEYS: &[&str] = &[
    "general",
    "rig",
    "rigs",
    "behavior",
    "listen",
    "audio",
    "sdr",
    "pskreporter",
    "aprsfi",
    "decode_logs",
];

/// Known top-level keys for a standalone client config.
const CLIENT_KEYS: &[&str] = &["general", "remote", "remotes", "frontends"];

/// Known top-level keys for a combined trx-rs.toml.
const COMBINED_KEYS: &[&str] = &["trx-server", "trx-client"];

/// Known sub-keys within [general] (server).
const SERVER_GENERAL_KEYS: &[&str] = &["callsign", "log_level", "latitude", "longitude"];

/// Known sub-keys within [general] (client).
const CLIENT_GENERAL_KEYS: &[&str] = &[
    "callsign",
    "log_level",
    "website_url",
    "website_name",
    "ais_vessel_url_base",
];

/// Known sub-keys within [rig].
const RIG_KEYS: &[&str] = &["model", "initial_freq_hz", "initial_mode", "access"];

/// Known sub-keys within [rig.access].
const ACCESS_KEYS: &[&str] = &["type", "port", "baud", "host", "tcp_port", "args"];

/// Known sub-keys within [listen].
const LISTEN_KEYS: &[&str] = &["enabled", "listen", "port", "auth"];

/// Known sub-keys within [audio] (server).
const AUDIO_KEYS: &[&str] = &[
    "enabled",
    "listen",
    "port",
    "rx_enabled",
    "tx_enabled",
    "device",
    "sample_rate",
    "channels",
    "frame_duration_ms",
    "bitrate_bps",
];

/// Known sub-keys within [behavior].
const BEHAVIOR_KEYS: &[&str] = &[
    "poll_interval_ms",
    "poll_interval_tx_ms",
    "max_retries",
    "retry_base_delay_ms",
    "vfo_prime",
];

/// Known sub-keys within [remote].
const REMOTE_KEYS: &[&str] = &["url", "rig_id", "auth", "poll_interval_ms"];

/// Known sub-keys within [frontends].
const FRONTENDS_KEYS: &[&str] = &["http", "rigctl", "http_json", "audio"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectedType {
    Server,
    Client,
    Combined,
    Unknown,
}

impl std::fmt::Display for DetectedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server => write!(f, "server"),
            Self::Client => write!(f, "client"),
            Self::Combined => write!(f, "combined"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

pub fn check_file(path: &Path) -> Result<String, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

    // Step 1: TOML syntax check
    let doc: DocumentMut = content
        .parse()
        .map_err(|e| format!("{}: TOML syntax error: {}", path.display(), e))?;

    let mut report = String::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    let table = doc.as_table();

    // Step 2: Detect config type
    let detected = detect_type(table);
    writeln!(report, "{}: valid TOML", path.display()).unwrap();
    writeln!(report, "  Detected type: {}", detected).unwrap();

    // Step 3: Structural validation
    match detected {
        DetectedType::Server => {
            check_unknown_keys(table, SERVER_KEYS, "", &mut warnings);
            check_server_sections(table, "", &mut warnings, &mut errors);
        }
        DetectedType::Client => {
            check_unknown_keys(table, CLIENT_KEYS, "", &mut warnings);
            check_client_sections(table, "", &mut warnings, &mut errors);
        }
        DetectedType::Combined => {
            check_unknown_keys(table, COMBINED_KEYS, "", &mut warnings);
            if let Some(server) = table.get("trx-server").and_then(|v| v.as_table()) {
                check_unknown_keys(server, SERVER_KEYS, "[trx-server].", &mut warnings);
                check_server_sections(server, "[trx-server].", &mut warnings, &mut errors);
            }
            if let Some(client) = table.get("trx-client").and_then(|v| v.as_table()) {
                check_unknown_keys(client, CLIENT_KEYS, "[trx-client].", &mut warnings);
                check_client_sections(client, "[trx-client].", &mut warnings, &mut errors);
            }
        }
        DetectedType::Unknown => {
            warnings.push("Could not detect config type. Expected server, client, or combined (trx-rs.toml) layout.".to_string());
        }
    }

    // Step 4: Format report
    for w in &warnings {
        writeln!(report, "  warning: {}", w).unwrap();
    }
    for e in &errors {
        writeln!(report, "  error: {}", e).unwrap();
    }

    if errors.is_empty() && warnings.is_empty() {
        writeln!(report, "  No issues found.").unwrap();
    } else {
        writeln!(
            report,
            "  {} warning(s), {} error(s)",
            warnings.len(),
            errors.len()
        )
        .unwrap();
    }

    if errors.is_empty() {
        Ok(report)
    } else {
        Err(report)
    }
}

fn detect_type(table: &toml_edit::Table) -> DetectedType {
    if table.contains_key("trx-server") || table.contains_key("trx-client") {
        return DetectedType::Combined;
    }
    let keys: Vec<&str> = table.iter().map(|(k, _)| k).collect();

    let server_score = keys.iter().filter(|k| SERVER_KEYS.contains(k)).count();
    let client_score = keys.iter().filter(|k| CLIENT_KEYS.contains(k)).count();

    // Use distinguishing keys to break ties
    if keys.contains(&"rig") || keys.contains(&"rigs") || keys.contains(&"listen") {
        return DetectedType::Server;
    }
    if keys.contains(&"remote") || keys.contains(&"remotes") || keys.contains(&"frontends") {
        return DetectedType::Client;
    }

    if server_score > client_score {
        DetectedType::Server
    } else if client_score > server_score {
        DetectedType::Client
    } else if server_score > 0 {
        DetectedType::Server
    } else {
        DetectedType::Unknown
    }
}

fn check_unknown_keys(
    table: &toml_edit::Table,
    known: &[&str],
    prefix: &str,
    warnings: &mut Vec<String>,
) {
    for (key, _) in table.iter() {
        if !known.contains(&key) {
            warnings.push(format!("{}unknown key '{}'", prefix, key));
        }
    }
}

fn check_server_sections(
    table: &toml_edit::Table,
    prefix: &str,
    warnings: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    if let Some(general) = table.get("general").and_then(|v| v.as_table()) {
        check_unknown_keys(general, SERVER_GENERAL_KEYS, &format!("{}[general].", prefix), warnings);
        validate_log_level(general, &format!("{}[general]", prefix), errors);
        validate_coordinates(general, &format!("{}[general]", prefix), errors);
    }

    if let Some(rig) = table.get("rig").and_then(|v| v.as_table()) {
        check_unknown_keys(rig, RIG_KEYS, &format!("{}[rig].", prefix), warnings);
        if let Some(access) = rig.get("access").and_then(|v| v.as_table()) {
            check_unknown_keys(access, ACCESS_KEYS, &format!("{}[rig.access].", prefix), warnings);
            validate_access(access, &format!("{}[rig.access]", prefix), errors);
        }
    }

    if let Some(listen) = table.get("listen").and_then(|v| v.as_table()) {
        check_unknown_keys(listen, LISTEN_KEYS, &format!("{}[listen].", prefix), warnings);
        validate_port(listen, "port", &format!("{}[listen]", prefix), errors);
    }

    if let Some(audio) = table.get("audio").and_then(|v| v.as_table()) {
        check_unknown_keys(audio, AUDIO_KEYS, &format!("{}[audio].", prefix), warnings);
        validate_port(audio, "port", &format!("{}[audio]", prefix), errors);
    }

    if let Some(behavior) = table.get("behavior").and_then(|v| v.as_table()) {
        check_unknown_keys(behavior, BEHAVIOR_KEYS, &format!("{}[behavior].", prefix), warnings);
    }
}

fn check_client_sections(
    table: &toml_edit::Table,
    prefix: &str,
    warnings: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    if let Some(general) = table.get("general").and_then(|v| v.as_table()) {
        check_unknown_keys(general, CLIENT_GENERAL_KEYS, &format!("{}[general].", prefix), warnings);
        validate_log_level(general, &format!("{}[general]", prefix), errors);
    }

    if let Some(remote) = table.get("remote").and_then(|v| v.as_table()) {
        check_unknown_keys(remote, REMOTE_KEYS, &format!("{}[remote].", prefix), warnings);
    }

    if let Some(frontends) = table.get("frontends").and_then(|v| v.as_table()) {
        check_unknown_keys(frontends, FRONTENDS_KEYS, &format!("{}[frontends].", prefix), warnings);
        if let Some(http) = frontends.get("http").and_then(|v| v.as_table()) {
            validate_port(http, "port", &format!("{}[frontends.http]", prefix), errors);
        }
        if let Some(rigctl) = frontends.get("rigctl").and_then(|v| v.as_table()) {
            validate_port(rigctl, "port", &format!("{}[frontends.rigctl]", prefix), errors);
        }
    }
}

// ── Value validators ────────────────────────────────────────────────────

fn validate_log_level(table: &toml_edit::Table, context: &str, errors: &mut Vec<String>) {
    if let Some(level) = table.get("log_level").and_then(|v| v.as_str()) {
        if !["trace", "debug", "info", "warn", "error"].contains(&level) {
            errors.push(format!(
                "{}.log_level '{}' is invalid (expected: trace, debug, info, warn, error)",
                context, level
            ));
        }
    }
}

fn validate_coordinates(table: &toml_edit::Table, context: &str, errors: &mut Vec<String>) {
    if let Some(lat) = table.get("latitude").and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64))) {
        if !(-90.0..=90.0).contains(&lat) {
            errors.push(format!("{}.latitude {} is out of range (-90..90)", context, lat));
        }
    }
    if let Some(lon) = table.get("longitude").and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64))) {
        if !(-180.0..=180.0).contains(&lon) {
            errors.push(format!(
                "{}.longitude {} is out of range (-180..180)",
                context, lon
            ));
        }
    }

    let has_lat = table.contains_key("latitude");
    let has_lon = table.contains_key("longitude");
    if has_lat != has_lon {
        errors.push(format!(
            "{}: latitude and longitude must be set together or both omitted",
            context
        ));
    }
}

fn validate_port(
    table: &toml_edit::Table,
    key: &str,
    context: &str,
    errors: &mut Vec<String>,
) {
    if let Some(port) = table.get(key).and_then(|v| v.as_integer()) {
        if let Some(enabled) = table.get("enabled").and_then(|v| v.as_bool()) {
            if enabled && port <= 0 {
                errors.push(format!("{}.{} must be > 0 when enabled", context, key));
            }
        }
        if !(0..=65535).contains(&port) {
            errors.push(format!(
                "{}.{} {} is out of range (0..65535)",
                context, key, port
            ));
        }
    }
}

fn validate_access(table: &toml_edit::Table, context: &str, errors: &mut Vec<String>) {
    if let Some(access_type) = table.get("type").and_then(|v| v.as_str()) {
        if !["serial", "tcp", "sdr"].contains(&access_type) {
            errors.push(format!(
                "{}.type '{}' is invalid (expected: serial, tcp, sdr)",
                context, access_type
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn check_toml(content: &str) -> Result<String, String> {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        check_file(f.path())
    }

    #[test]
    fn test_valid_server_config() {
        let result = check_toml(
            r#"
[general]
callsign = "W1AW"
log_level = "info"

[rig]
model = "ft817"

[rig.access]
type = "serial"
port = "/dev/ttyUSB0"
baud = 9600

[listen]
enabled = true
port = 4530
"#,
        );
        assert!(result.is_ok());
        let report = result.unwrap();
        assert!(report.contains("Detected type: server"));
        assert!(report.contains("No issues found"));
    }

    #[test]
    fn test_valid_client_config() {
        let result = check_toml(
            r#"
[general]
callsign = "W1AW"

[remote]
url = "localhost:4530"

[frontends.http]
enabled = true
port = 8080
"#,
        );
        assert!(result.is_ok());
        let report = result.unwrap();
        assert!(report.contains("Detected type: client"));
    }

    #[test]
    fn test_valid_combined_config() {
        let result = check_toml(
            r#"
[trx-server.general]
callsign = "W1AW"

[trx-client.general]
callsign = "W1AW"

[trx-client.remote]
url = "localhost:4530"
"#,
        );
        assert!(result.is_ok());
        let report = result.unwrap();
        assert!(report.contains("Detected type: combined"));
    }

    #[test]
    fn test_invalid_toml_syntax() {
        let result = check_toml("this is not [valid toml");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("TOML syntax error"));
    }

    #[test]
    fn test_unknown_key_warning() {
        let result = check_toml(
            r#"
[general]
callsign = "W1AW"

[rig]
model = "ft817"

[bogus_section]
foo = "bar"
"#,
        );
        assert!(result.is_ok());
        let report = result.unwrap();
        assert!(report.contains("unknown key 'bogus_section'"));
    }

    #[test]
    fn test_invalid_log_level() {
        let result = check_toml(
            r#"
[general]
log_level = "verbose"

[rig]
model = "ft817"
"#,
        );
        assert!(result.is_err());
        let report = result.unwrap_err();
        assert!(report.contains("log_level 'verbose' is invalid"));
    }

    #[test]
    fn test_latitude_without_longitude() {
        let result = check_toml(
            r#"
[general]
latitude = 45.0

[rig]
model = "ft817"
"#,
        );
        assert!(result.is_err());
        let report = result.unwrap_err();
        assert!(report.contains("latitude and longitude must be set together"));
    }

    #[test]
    fn test_latitude_out_of_range() {
        let result = check_toml(
            r#"
[general]
latitude = 95.0
longitude = 10.0

[rig]
model = "ft817"
"#,
        );
        assert!(result.is_err());
        let report = result.unwrap_err();
        assert!(report.contains("latitude 95 is out of range"));
    }

    #[test]
    fn test_invalid_access_type() {
        let result = check_toml(
            r#"
[rig]
model = "ft817"

[rig.access]
type = "usb"
"#,
        );
        assert!(result.is_err());
        let report = result.unwrap_err();
        assert!(report.contains("type 'usb' is invalid"));
    }
}
