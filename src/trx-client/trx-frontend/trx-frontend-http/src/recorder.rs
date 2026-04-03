// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio recorder — writes incoming Opus packets to OGG/Opus files.
//!
//! The recorder subscribes to the same `broadcast::Sender<Bytes>` channels
//! that feed the WebSocket audio endpoint, capturing pre-encoded Opus packets
//! without any re-encoding.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};
use tracing::{error, info, warn};

// ============================================================================
// Configuration
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecorderConfig {
    /// Whether the recorder feature is available.
    pub enabled: bool,
    /// Directory for recorded files. Default: `$XDG_CACHE_HOME/trx-rs/recordings/`.
    pub output_dir: Option<String>,
    /// Maximum duration of a single recording in seconds. None = unlimited.
    pub max_duration_secs: Option<u64>,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            output_dir: None,
            max_duration_secs: None,
        }
    }
}

impl RecorderConfig {
    pub fn resolve_output_dir(&self) -> PathBuf {
        if let Some(ref dir) = self.output_dir {
            PathBuf::from(dir)
        } else {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from(".cache"))
                .join("trx-rs")
                .join("recordings")
        }
    }
}

// ============================================================================
// Recording metadata
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct RecordingInfo {
    pub key: String,
    pub rig_id: String,
    pub vchan_id: Option<String>,
    pub path: String,
    pub started_at: i64,
    pub sample_rate: u32,
    pub channels: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordingResult {
    pub key: String,
    pub path: String,
    pub duration_secs: f64,
    pub bytes_written: u64,
}

/// Audio stream parameters for a recording.
#[derive(Debug, Clone, Copy)]
pub struct AudioParams {
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_duration_ms: u16,
}

// ============================================================================
// OGG/Opus writer
// ============================================================================

/// Minimal OGG/Opus file writer.
///
/// Writes the mandatory OpusHead and OpusTags pages, then wraps each incoming
/// Opus packet in its own OGG page. This produces a valid, seekable OGG Opus
/// stream without pulling in an external OGG crate.
struct OggOpusWriter {
    file: std::fs::File,
    serial: u32,
    page_seq: u32,
    granule_pos: u64,
    samples_per_frame: u64,
    bytes_written: u64,
}

impl OggOpusWriter {
    fn create(
        path: &Path,
        sample_rate: u32,
        channels: u8,
        frame_duration_ms: u16,
    ) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;

        let serial = {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            (ts & 0xFFFF_FFFF) as u32
        };

        let samples_per_frame = (sample_rate as u64) * (frame_duration_ms as u64) / 1000;

        let mut writer = Self {
            file,
            serial,
            page_seq: 0,
            granule_pos: 0,
            samples_per_frame,
            bytes_written: 0,
        };

        writer.write_opus_head(sample_rate, channels)?;
        writer.write_opus_tags()?;

        Ok(writer)
    }

    /// Write the OpusHead identification header (OGG page, BOS).
    fn write_opus_head(&mut self, sample_rate: u32, channels: u8) -> std::io::Result<()> {
        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1); // version
        head.push(channels);
        head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
        head.extend_from_slice(&sample_rate.to_le_bytes()); // input sample rate
        head.extend_from_slice(&0u16.to_le_bytes()); // output gain
        head.push(0); // channel mapping family

        // BOS flag = 0x02
        self.write_ogg_page(0x02, 0, &head)
    }

    /// Write the OpusTags comment header.
    fn write_opus_tags(&mut self) -> std::io::Result<()> {
        let vendor = b"trx-rs";
        let mut tags = Vec::with_capacity(24);
        tags.extend_from_slice(b"OpusTags");
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes()); // no user comments

        self.write_ogg_page(0x00, 0, &tags)
    }

    /// Write a single Opus audio packet as an OGG page.
    fn write_audio_packet(&mut self, opus_data: &[u8]) -> std::io::Result<()> {
        self.granule_pos += self.samples_per_frame;
        self.write_ogg_page(0x00, self.granule_pos, opus_data)
    }

    /// Finalize the stream by writing an EOS page.
    fn finalize(mut self) -> std::io::Result<u64> {
        // Write an empty EOS page.
        self.write_ogg_page(0x04, self.granule_pos, &[])?;
        self.file.flush()?;
        Ok(self.bytes_written)
    }

    /// Write a single OGG page.
    fn write_ogg_page(
        &mut self,
        header_type: u8,
        granule_position: u64,
        data: &[u8],
    ) -> std::io::Result<()> {
        // OGG page header
        let mut header = Vec::with_capacity(27 + 255);
        header.extend_from_slice(b"OggS"); // capture pattern
        header.push(0); // stream structure version
        header.push(header_type); // header type flag
        header.extend_from_slice(&granule_position.to_le_bytes()); // granule position
        header.extend_from_slice(&self.serial.to_le_bytes()); // stream serial number
        header.extend_from_slice(&self.page_seq.to_le_bytes()); // page sequence number
        header.extend_from_slice(&0u32.to_le_bytes()); // CRC (placeholder)
        self.page_seq += 1;

        // Segment table: split data into 255-byte segments.
        let num_segments = if data.is_empty() {
            1
        } else {
            data.len().div_ceil(255)
        };
        // A single packet needs lacing values: full 255-byte segments + final remainder.
        let mut segments = Vec::with_capacity(num_segments);
        let mut remaining = data.len();
        while remaining >= 255 {
            segments.push(255u8);
            remaining -= 255;
        }
        segments.push(remaining as u8);

        header.push(segments.len() as u8); // number of page segments
        header.extend_from_slice(&segments);

        // Compute CRC-32 over header + data
        let crc = ogg_crc32(&header, data);
        header[22..26].copy_from_slice(&crc.to_le_bytes());

        self.file.write_all(&header)?;
        self.file.write_all(data)?;
        self.bytes_written += header.len() as u64 + data.len() as u64;
        Ok(())
    }
}

/// OGG CRC-32 (polynomial 0x04C11DB7, direct algorithm).
fn ogg_crc32(header: &[u8], data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for i in 0..256u32 {
            let mut r = i << 24;
            for _ in 0..8 {
                r = if r & 0x80000000 != 0 {
                    (r << 1) ^ 0x04C11DB7
                } else {
                    r << 1
                };
            }
            t[i as usize] = r;
        }
        t
    });

    let mut crc = 0u32;
    for &b in header.iter().chain(data.iter()) {
        crc = (crc << 8) ^ table[((crc >> 24) ^ (b as u32)) as usize];
    }
    crc
}

// ============================================================================
// RecorderHandle
// ============================================================================

struct RecorderHandle {
    stop_tx: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<Option<RecordingResult>>,
    info: RecordingInfo,
}

// ============================================================================
// RecorderManager
// ============================================================================

pub struct RecorderManager {
    recordings: Mutex<HashMap<String, RecorderHandle>>,
    config: RecorderConfig,
}

impl RecorderManager {
    pub fn new(config: RecorderConfig) -> Self {
        Self {
            recordings: Mutex::new(HashMap::new()),
            config,
        }
    }

    /// Build a recording key from rig_id and optional vchan_id.
    fn make_key(rig_id: &str, vchan_id: Option<&str>) -> String {
        match vchan_id {
            Some(v) => format!("{rig_id}:{v}"),
            None => rig_id.to_string(),
        }
    }

    /// Start recording the given audio stream.
    pub fn start(
        &self,
        rig_id: &str,
        vchan_id: Option<&str>,
        audio_rx: broadcast::Sender<Bytes>,
        params: AudioParams,
        freq_hz: Option<u64>,
        mode: Option<&str>,
    ) -> Result<RecordingInfo, String> {
        if !self.config.enabled {
            return Err("recorder is disabled".into());
        }

        let key = Self::make_key(rig_id, vchan_id);

        let mut recordings = self.recordings.lock().unwrap_or_else(|e| e.into_inner());
        if recordings.contains_key(&key) {
            return Err(format!("already recording: {key}"));
        }

        let output_dir = self.config.resolve_output_dir();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let ts = chrono_timestamp(now.as_secs());

        let filename = {
            let freq_part = freq_hz.map(|f| format!("_{f}")).unwrap_or_default();
            let mode_part = mode.map(|m| format!("_{m}")).unwrap_or_default();
            let vchan_part = vchan_id.map(|v| format!("_vchan-{v}")).unwrap_or_default();
            format!("{rig_id}{freq_part}{mode_part}{vchan_part}_{ts}.ogg")
        };
        let path = output_dir.join(&filename);

        let (stop_tx, stop_rx) = watch::channel(false);
        let rx = audio_rx.subscribe();
        let path_clone = path.clone();
        let max_duration = self.config.max_duration_secs;
        let key_clone = key.clone();

        let handle = tokio::task::spawn_blocking(move || {
            run_recorder(&key_clone, &path_clone, rx, stop_rx, params, max_duration)
        });

        let started_at = now.as_secs() as i64;
        let info = RecordingInfo {
            key: key.clone(),
            rig_id: rig_id.to_string(),
            vchan_id: vchan_id.map(str::to_string),
            path: path.to_string_lossy().into_owned(),
            started_at,
            sample_rate: params.sample_rate,
            channels: params.channels,
        };

        recordings.insert(
            key,
            RecorderHandle {
                stop_tx,
                handle,
                info: info.clone(),
            },
        );

        Ok(info)
    }

    /// Stop a recording and return the result.
    pub async fn stop(
        &self,
        rig_id: &str,
        vchan_id: Option<&str>,
    ) -> Result<RecordingResult, String> {
        let key = Self::make_key(rig_id, vchan_id);
        let handle = {
            let mut recordings = self.recordings.lock().unwrap_or_else(|e| e.into_inner());
            recordings.remove(&key)
        };
        match handle {
            Some(h) => {
                let _ = h.stop_tx.send(true);
                match h.handle.await {
                    Ok(Some(result)) => Ok(result),
                    Ok(None) => Err("recording failed".into()),
                    Err(e) => Err(format!("recorder task panicked: {e}")),
                }
            }
            None => Err(format!("no active recording: {key}")),
        }
    }

    /// List active recordings.
    pub fn list_active(&self) -> Vec<RecordingInfo> {
        let recordings = self.recordings.lock().unwrap_or_else(|e| e.into_inner());
        recordings.values().map(|h| h.info.clone()).collect()
    }

    /// List recorded files in the output directory.
    pub fn list_files(&self) -> Vec<RecordedFile> {
        let dir = self.config.resolve_output_dir();
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "ogg") {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    files.push(RecordedFile { name, size });
                }
            }
        }
        files.sort_by(|a, b| b.name.cmp(&a.name)); // newest first
        files
    }

    /// Resolve and validate a filename, returning the full path.
    ///
    /// Rejects path traversal attempts and files outside the output directory.
    fn validate_filename(&self, filename: &str) -> Result<PathBuf, String> {
        if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
            return Err("invalid filename".into());
        }
        if !filename.ends_with(".ogg") {
            return Err("only .ogg files are accessible".into());
        }
        let dir = self.config.resolve_output_dir();
        let path = dir.join(filename);
        if !path.exists() {
            return Err(format!("file not found: {filename}"));
        }
        Ok(path)
    }

    /// Get the full path to a recorded file for download.
    pub fn file_path(&self, filename: &str) -> Result<PathBuf, String> {
        self.validate_filename(filename)
    }

    /// Delete a recorded file.
    pub fn delete_file(&self, filename: &str) -> Result<(), String> {
        let path = self.validate_filename(filename)?;
        std::fs::remove_file(&path).map_err(|e| format!("failed to delete: {e}"))
    }

    /// Check if a recording is active for the given key.
    pub fn is_recording(&self, rig_id: &str, vchan_id: Option<&str>) -> bool {
        let key = Self::make_key(rig_id, vchan_id);
        let recordings = self.recordings.lock().unwrap_or_else(|e| e.into_inner());
        recordings.contains_key(&key)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordedFile {
    pub name: String,
    pub size: u64,
}

// ============================================================================
// Recording task (runs in spawn_blocking)
// ============================================================================

fn run_recorder(
    key: &str,
    path: &Path,
    mut rx: broadcast::Receiver<Bytes>,
    mut stop_rx: watch::Receiver<bool>,
    params: AudioParams,
    max_duration_secs: Option<u64>,
) -> Option<RecordingResult> {
    let mut writer = match OggOpusWriter::create(
        path,
        params.sample_rate,
        params.channels,
        params.frame_duration_ms,
    ) {
        Ok(w) => w,
        Err(e) => {
            error!("Recorder [{key}]: failed to create file {path:?}: {e}");
            return None;
        }
    };

    info!("Recorder [{key}]: started → {}", path.display());

    let start = std::time::Instant::now();
    let max_dur = max_duration_secs.map(std::time::Duration::from_secs);
    let mut packets: u64 = 0;

    // Use a small runtime to bridge async broadcast → blocking writer.
    let rt = tokio::runtime::Handle::current();

    loop {
        // Check stop signal.
        if *stop_rx.borrow() {
            break;
        }

        // Check max duration.
        if let Some(max) = max_dur {
            if start.elapsed() >= max {
                info!("Recorder [{key}]: max duration reached");
                break;
            }
        }

        // Receive next Opus packet (blocking in spawn_blocking context).
        let packet = rt.block_on(async {
            tokio::select! {
                result = rx.recv() => Some(result),
                _ = stop_rx.changed() => None,
            }
        });

        match packet {
            Some(Ok(data)) => {
                if let Err(e) = writer.write_audio_packet(&data) {
                    error!("Recorder [{key}]: write error: {e}");
                    break;
                }
                packets += 1;
            }
            Some(Err(broadcast::error::RecvError::Lagged(n))) => {
                warn!("Recorder [{key}]: dropped {n} packets (lag)");
                // Continue recording despite lag.
            }
            Some(Err(broadcast::error::RecvError::Closed)) => {
                info!("Recorder [{key}]: audio channel closed");
                break;
            }
            None => {
                // Stop signal received.
                break;
            }
        }
    }

    let duration_secs = start.elapsed().as_secs_f64();
    let bytes_written = match writer.finalize() {
        Ok(n) => n,
        Err(e) => {
            error!("Recorder [{key}]: finalize error: {e}");
            0
        }
    };

    info!(
        "Recorder [{key}]: stopped — {packets} packets, {duration_secs:.1}s, {} bytes",
        bytes_written
    );

    Some(RecordingResult {
        key: key.to_string(),
        path: path.to_string_lossy().into_owned(),
        duration_secs,
        bytes_written,
    })
}

// ============================================================================
// Helpers
// ============================================================================

/// Format a Unix timestamp as `YYYY-MM-DD_HH-MM-SS`.
fn chrono_timestamp(epoch_secs: u64) -> String {
    let secs = epoch_secs;
    let days = secs / 86400;
    let time = secs % 86400;
    let hours = time / 3600;
    let minutes = (time % 3600) / 60;
    let seconds = time % 60;

    // Simple Gregorian calendar calculation from epoch days.
    let (y, m, d) = epoch_days_to_ymd(days as i64);
    format!("{y:04}-{m:02}-{d:02}_{hours:02}-{minutes:02}-{seconds:02}")
}

fn epoch_days_to_ymd(days: i64) -> (i32, u32, u32) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}
