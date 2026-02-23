// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
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

use trx_core::decode::{AprsPacket, CwEvent, Ft8Message, WsprMessage};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

fn default_decode_logs_dir() -> String {
    if let Some(data_dir) = dirs::data_dir() {
        return data_dir
            .join("trx-rs")
            .join("decoders")
            .to_string_lossy()
            .to_string();
    }
    "logs/decoders".to_string()
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
                    warn!("decode log reopen failed for {}: {}", self.label, e);
                    return;
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
        let _ = state.writer.flush();
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
}
