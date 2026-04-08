// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Server-side decoder file logging (APRS / CW / FT8 / WSPR).
//!
//! Provides [`DecodeLogsConfig`] for TOML configuration and [`DecoderLoggers`]
//! for writing JSON-Lines log files with automatic daily rotation.

use std::fs::{create_dir_all, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;

use trx_core::decode::{AprsPacket, CwEvent, Ft8Message, WefaxMessage, WsprMessage};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

fn default_decode_logs_dir() -> String {
    if let Some(cache_dir) = dirs::cache_dir() {
        return cache_dir
            .join("trx-rs")
            .join("decoders")
            .to_string_lossy()
            .to_string();
    }
    ".cache/trx-rs/decoders".to_string()
}

/// Server-side decoder file logging configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DecodeLogsConfig {
    /// Whether decoder file logging is enabled
    pub enabled: bool,
    /// Base directory for log files
    pub dir: String,
    /// APRS decoder log filename
    pub aprs_file: String,
    /// CW decoder log filename
    pub cw_file: String,
    /// FT8 decoder log filename
    pub ft8_file: String,
    /// WSPR decoder log filename
    pub wspr_file: String,
    /// WEFAX decoder log filename
    pub wefax_file: String,
}

impl Default for DecodeLogsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: default_decode_logs_dir(),
            aprs_file: "TRXRS-APRS-%YYYY%-%MM%-%DD%.log".to_string(),
            cw_file: "TRXRS-CW-%YYYY%-%MM%-%DD%.log".to_string(),
            ft8_file: "TRXRS-FT8-%YYYY%-%MM%-%DD%.log".to_string(),
            wspr_file: "TRXRS-WSPR-%YYYY%-%MM%-%DD%.log".to_string(),
            wefax_file: "TRXRS-WEFAX-%YYYY%-%MM%-%DD%.log".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// File logger (private)
// ---------------------------------------------------------------------------

struct DecoderFileLogger {
    base_dir: PathBuf,
    file_template: String,
    state: Mutex<DecoderFileState>,
    label: &'static str,
}

struct DecoderFileState {
    current_file_name: String,
    writer: BufWriter<File>,
}

impl DecoderFileLogger {
    fn resolve_file_name(template: &str) -> String {
        let now = Utc::now();
        template
            .replace("%YYYY%", &now.format("%Y").to_string())
            .replace("%MM%", &now.format("%m").to_string())
            .replace("%DD%", &now.format("%d").to_string())
    }

    fn open_writer(path: &Path, label: &'static str) -> Result<BufWriter<File>, String> {
        if let Some(parent) = path.parent() {
            create_dir_all(parent)
                .map_err(|e| format!("create {} log dir '{}': {}", label, parent.display(), e))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("open {} log '{}': {}", label, path.display(), e))?;
        Ok(BufWriter::new(file))
    }

    fn open(base_dir: &Path, template: &str, label: &'static str) -> Result<Self, String> {
        let file_name = Self::resolve_file_name(template);
        let path = base_dir.join(&file_name);
        let writer = Self::open_writer(&path, label)?;
        Ok(Self {
            base_dir: base_dir.to_path_buf(),
            file_template: template.to_string(),
            state: Mutex::new(DecoderFileState {
                current_file_name: file_name,
                writer,
            }),
            label,
        })
    }

    fn write_payload<T: Serialize>(&self, payload: &T) {
        let ts_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_millis() as u64,
            Err(_) => 0,
        };
        let line = json!({
            "ts_ms": ts_ms,
            "decoder": self.label,
            "payload": payload,
        });
        let Ok(mut state) = self.state.lock() else {
            warn!("decode log mutex poisoned for {}", self.label);
            return;
        };

        let next_file_name = Self::resolve_file_name(&self.file_template);
        if next_file_name != state.current_file_name {
            let next_path = self.base_dir.join(&next_file_name);
            match Self::open_writer(&next_path, self.label) {
                Ok(next_writer) => {
                    state.current_file_name = next_file_name;
                    state.writer = next_writer;
                }
                Err(e) => {
                    warn!(
                        "decode log rotation failed for {}, keeping current writer: {}",
                        self.label, e
                    );
                    // Keep the old writer rather than silently dropping writes.
                }
            }
        }

        if serde_json::to_writer(&mut state.writer, &line).is_err() {
            warn!("decode log serialization failed for {}", self.label);
            return;
        }
        if state.writer.write_all(b"\n").is_err() {
            warn!("decode log write failed for {}", self.label);
            return;
        }
        if let Err(e) = state.writer.flush() {
            warn!("decode log flush failed for {}: {}", self.label, e);
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Aggregate logger for all four server-side decoders.
pub struct DecoderLoggers {
    aprs: DecoderFileLogger,
    cw: DecoderFileLogger,
    ft8: DecoderFileLogger,
    wspr: DecoderFileLogger,
    wefax: DecoderFileLogger,
}

impl DecoderLoggers {
    /// Create loggers from config, or return `None` when logging is disabled.
    pub fn from_config(cfg: &DecodeLogsConfig) -> Result<Option<Arc<Self>>, String> {
        if !cfg.enabled {
            return Ok(None);
        }

        let base_dir = PathBuf::from(cfg.dir.trim());
        create_dir_all(&base_dir)
            .map_err(|e| format!("create decode log dir '{}': {}", base_dir.display(), e))?;

        let loggers = Self {
            aprs: DecoderFileLogger::open(&base_dir, &cfg.aprs_file, "aprs")?,
            cw: DecoderFileLogger::open(&base_dir, &cfg.cw_file, "cw")?,
            ft8: DecoderFileLogger::open(&base_dir, &cfg.ft8_file, "ft8")?,
            wspr: DecoderFileLogger::open(&base_dir, &cfg.wspr_file, "wspr")?,
            wefax: DecoderFileLogger::open(&base_dir, &cfg.wefax_file, "wefax")?,
        };

        Ok(Some(Arc::new(loggers)))
    }

    pub fn log_aprs(&self, pkt: &AprsPacket) {
        self.aprs.write_payload(pkt);
    }

    pub fn log_cw(&self, evt: &CwEvent) {
        self.cw.write_payload(evt);
    }

    pub fn log_ft8(&self, msg: &Ft8Message) {
        self.ft8.write_payload(msg);
    }

    pub fn log_wspr(&self, msg: &WsprMessage) {
        self.wspr.write_payload(msg);
    }

    pub fn log_wefax(&self, msg: &WefaxMessage) {
        self.wefax.write_payload(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_file_name_substitutes_date_tokens() {
        let template = "LOG-%YYYY%-%MM%-%DD%.log";
        let resolved = DecoderFileLogger::resolve_file_name(template);
        // Must not contain any template tokens
        assert!(!resolved.contains("%YYYY%"));
        assert!(!resolved.contains("%MM%"));
        assert!(!resolved.contains("%DD%"));
        // Must end with .log
        assert!(resolved.ends_with(".log"));
        // Must start with LOG-
        assert!(resolved.starts_with("LOG-"));
        // Year should be 4 digits
        let parts: Vec<&str> = resolved
            .trim_start_matches("LOG-")
            .trim_end_matches(".log")
            .split('-')
            .collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 4); // YYYY
        assert_eq!(parts[1].len(), 2); // MM
        assert_eq!(parts[2].len(), 2); // DD
    }

    #[test]
    fn from_config_disabled_returns_none() {
        let cfg = DecodeLogsConfig {
            enabled: false,
            ..Default::default()
        };
        let result = DecoderLoggers::from_config(&cfg).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn from_config_enabled_creates_loggers() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DecodeLogsConfig {
            enabled: true,
            dir: dir.path().to_string_lossy().to_string(),
            ..Default::default()
        };
        let result = DecoderLoggers::from_config(&cfg).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn log_ft8_writes_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DecodeLogsConfig {
            enabled: true,
            dir: dir.path().to_string_lossy().to_string(),
            ft8_file: "ft8-test.log".to_string(),
            ..Default::default()
        };
        let loggers = DecoderLoggers::from_config(&cfg).unwrap().unwrap();

        let msg = Ft8Message {
            rig_id: None,
            ts_ms: 1000,
            snr_db: -12.0,
            dt_s: 0.1,
            freq_hz: 1234.0,
            message: "CQ SP2SJG JO93".to_string(),
        };
        loggers.log_ft8(&msg);

        // Read back the log file
        let log_path = dir.path().join("ft8-test.log");
        let content = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["decoder"], "ft8");
        assert!(parsed["ts_ms"].is_number());
        assert_eq!(parsed["payload"]["message"], "CQ SP2SJG JO93");
        assert_eq!(parsed["payload"]["snr_db"], -12.0);
    }

    #[test]
    fn log_aprs_writes_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DecodeLogsConfig {
            enabled: true,
            dir: dir.path().to_string_lossy().to_string(),
            aprs_file: "aprs-test.log".to_string(),
            ..Default::default()
        };
        let loggers = DecoderLoggers::from_config(&cfg).unwrap().unwrap();

        let pkt = AprsPacket {
            rig_id: None,
            ts_ms: Some(2000),
            src_call: "N0CALL".to_string(),
            dest_call: "APRS".to_string(),
            path: "WIDE1-1".to_string(),
            info: ">Test".to_string(),
            info_bytes: b">Test".to_vec(),
            packet_type: "Status".to_string(),
            crc_ok: true,
            lat: None,
            lon: None,
            symbol_table: None,
            symbol_code: None,
        };
        loggers.log_aprs(&pkt);

        let log_path = dir.path().join("aprs-test.log");
        let content = std::fs::read_to_string(&log_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed["decoder"], "aprs");
        assert_eq!(parsed["payload"]["src_call"], "N0CALL");
    }

    #[test]
    fn default_config_has_template_tokens() {
        let cfg = DecodeLogsConfig::default();
        assert!(cfg.ft8_file.contains("%YYYY%"));
        assert!(cfg.aprs_file.contains("%MM%"));
        assert!(cfg.cw_file.contains("%DD%"));
        assert!(!cfg.enabled);
    }
}
