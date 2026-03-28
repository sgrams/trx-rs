// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Background Decoding Scheduler.
//!
//! When no SSE clients are connected, or when every connected user explicitly
//! releases control, the scheduler periodically inspects the current UTC time,
//! selects the matching bookmark from the per-rig config, and issues rig
//! commands to retune and activate the scheduled decoder set automatically.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use actix_web::{delete, get, put, web, HttpResponse, Responder};
use pickledb::{PickleDb, PickleDbDumpPolicy, SerializationMethod};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tracing::{info, warn};
use uuid::Uuid;

use trx_core::radio::freq::Freq;
use trx_core::rig::command::RigCommand;
use trx_core::RigRequest;
use trx_frontend::FrontendRuntimeContext;

use crate::server::bookmarks::BookmarkStoreMap;

// ============================================================================
// Data model
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerMode {
    #[default]
    Disabled,
    Grayline,
    TimeSpan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraylineConfig {
    pub lat: f64,
    pub lon: f64,
    /// Half-width of dawn/dusk transition window in minutes (default 20).
    #[serde(default = "default_transition_window")]
    pub transition_window_min: u32,
    pub day_bookmark_id: Option<String>,
    pub night_bookmark_id: Option<String>,
    pub dawn_bookmark_id: Option<String>,
    pub dusk_bookmark_id: Option<String>,
}

fn default_transition_window() -> u32 {
    20
}

// ============================================================================
// Satellite pass scheduling overlay
// ============================================================================

/// A single satellite to track for automated pass scheduling.
///
/// When a configured satellite's pass is in progress (elevation above
/// `min_elevation_deg`), the scheduler preempts the base Grayline/TimeSpan
/// mode and tunes to the bookmark specified here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SatelliteEntry {
    /// Unique identifier for this entry.
    pub id: String,
    /// Satellite name as it appears in TLE data (e.g. "NOAA 19", "METEOR-M2 3").
    pub satellite: String,
    /// NORAD catalog number (e.g. 33591 for NOAA-19).  Used to match against
    /// pass predictions from the TLE store.
    pub norad_id: u32,
    /// Bookmark ID to apply during the pass.  The bookmark sets frequency,
    /// mode, bandwidth, and — critically — the `decoders` list (e.g.
    /// `["wxsat"]` for NOAA APT, `["lrpt"]` for Meteor LRPT).
    pub bookmark_id: String,
    /// Minimum peak elevation in degrees for a pass to trigger scheduling
    /// (default 5°).  Low-elevation passes produce poor images and may not
    /// be worth the interruption.
    #[serde(default = "default_sat_min_elevation")]
    pub min_elevation_deg: f64,
    /// Priority (lower = higher priority).  When two satellite passes overlap,
    /// the entry with the lowest priority value wins.
    #[serde(default)]
    pub priority: u32,
    /// Optional SDR center frequency override during the pass.
    #[serde(default)]
    pub center_hz: Option<u64>,
    /// Additional bookmark IDs for virtual channels during the pass.
    #[serde(default)]
    pub bookmark_ids: Vec<String>,
}

fn default_sat_min_elevation() -> f64 {
    5.0
}

/// Configuration for the satellite scheduling overlay.
///
/// Satellite entries are checked every scheduler tick.  When an active pass
/// is detected (using the cached pass predictions from the server's TLE
/// store), the satellite entry preempts the base scheduler mode.  Once the
/// pass ends (LOS), the base mode resumes automatically.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SatelliteSchedulerConfig {
    /// Whether satellite pass scheduling is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// How many seconds before predicted AOS to pre-tune (default 60).
    /// Gives the SDR/rig time to settle and the decoder time to lock.
    #[serde(default = "default_pretune_secs")]
    pub pretune_secs: u32,
    /// Satellite entries to schedule.
    #[serde(default)]
    pub entries: Vec<SatelliteEntry>,
}

fn default_pretune_secs() -> u32 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleEntry {
    pub id: String,
    /// Start of window as minutes-since-midnight UTC (e.g. 360 = 06:00).
    pub start_min: u32,
    /// End of window as minutes-since-midnight UTC.  May be < start_min
    /// to represent a window that spans midnight.
    pub end_min: u32,
    /// Primary bookmark (channel 0).  Must not be empty for single-channel
    /// entries; may be empty when `bookmark_ids` provides all channels.
    pub bookmark_id: String,
    #[serde(default)]
    pub label: Option<String>,
    /// Per-entry interleave duration in minutes.  Overrides the config-level
    /// `interleave_min` when set.  Allows each entry to occupy a differently
    /// sized slice of the interleave cycle.
    #[serde(default)]
    pub interleave_min: Option<u32>,
    /// SDR center frequency in Hz.  When set the scheduler issues
    /// `SetCenterFreq` before applying `SetFreq`/`SetMode`.
    #[serde(default)]
    pub center_hz: Option<u64>,
    /// Additional bookmarks to monitor as virtual channels alongside the
    /// primary.  The background task records these in the status so the
    /// frontend can allocate the corresponding virtual channels on connect.
    #[serde(default)]
    pub bookmark_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SchedulerConfig {
    pub remote: String,
    #[serde(default)]
    pub mode: SchedulerMode,
    pub grayline: Option<GraylineConfig>,
    #[serde(default)]
    pub entries: Vec<ScheduleEntry>,
    /// When multiple entries are active simultaneously, cycle through them,
    /// spending this many minutes at each before advancing to the next.
    /// `None` (or 0) disables interleaving — the first matching entry wins.
    #[serde(default)]
    pub interleave_min: Option<u32>,
    /// Satellite pass scheduling overlay.  When enabled, active satellite
    /// passes preempt the base Grayline/TimeSpan mode.  After the pass
    /// ends the base mode resumes automatically.
    #[serde(default)]
    pub satellites: Option<SatelliteSchedulerConfig>,
}

// ============================================================================
// SchedulerStore
// ============================================================================

pub struct SchedulerStore {
    db: Arc<RwLock<PickleDb>>,
}

impl SchedulerStore {
    pub fn open(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let db = if path.exists() {
            PickleDb::load(
                path,
                PickleDbDumpPolicy::AutoDump,
                SerializationMethod::Json,
            )
            .unwrap_or_else(|_| {
                PickleDb::new(
                    path,
                    PickleDbDumpPolicy::AutoDump,
                    SerializationMethod::Json,
                )
            })
        } else {
            PickleDb::new(
                path,
                PickleDbDumpPolicy::AutoDump,
                SerializationMethod::Json,
            )
        };
        Self {
            db: Arc::new(RwLock::new(db)),
        }
    }

    /// Per-rig path: `~/.config/trx-rs/scheduler.{remote}.db`.
    pub fn default_path_for(remote: &str) -> PathBuf {
        dirs::config_dir()
            .map(|p| p.join("trx-rs").join(format!("scheduler.{remote}.db")))
            .unwrap_or_else(|| PathBuf::from(format!("scheduler.{remote}.db")))
    }

    /// Legacy (pre-per-rig) path.
    pub fn legacy_path() -> PathBuf {
        dirs::config_dir()
            .map(|p| p.join("trx-rs").join("scheduler.db"))
            .unwrap_or_else(|| PathBuf::from("scheduler.db"))
    }

    pub fn get_config(&self) -> Option<SchedulerConfig> {
        let db = self.db.read().unwrap_or_else(|e| e.into_inner());
        db.get::<SchedulerConfig>("config")
    }

    pub fn upsert_config(&self, config: &SchedulerConfig) -> bool {
        let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
        db.set("config", config).is_ok()
    }

    pub fn remove_config(&self) -> bool {
        let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
        db.rem("config").unwrap_or(false)
    }
}

/// Manages per-rig scheduler stores, lazily opening them on first access.
pub struct SchedulerStoreMap {
    stores: std::sync::Mutex<HashMap<String, Arc<SchedulerStore>>>,
}

impl SchedulerStoreMap {
    /// Create a new map and run one-time migration from the legacy shared
    /// `scheduler.db` if per-rig files do not yet exist.
    pub fn new(rig_ids: &[&str]) -> Self {
        let map = Self {
            stores: std::sync::Mutex::new(HashMap::new()),
        };
        map.migrate_legacy(rig_ids);
        map
    }

    /// Return the store for `remote`, opening it on first access.
    pub fn store_for(&self, remote: &str) -> Arc<SchedulerStore> {
        let mut stores = self.stores.lock().unwrap_or_else(|e| e.into_inner());
        stores
            .entry(remote.to_owned())
            .or_insert_with(|| {
                let path = SchedulerStore::default_path_for(remote);
                Arc::new(SchedulerStore::open(&path))
            })
            .clone()
    }

    /// List configs from all known per-rig stores.
    pub fn list_all(&self) -> Vec<SchedulerConfig> {
        let stores = self.stores.lock().unwrap_or_else(|e| e.into_inner());
        stores.values().filter_map(|s| s.get_config()).collect()
    }

    /// One-time migration: extract `sch:{remote}` entries from legacy
    /// `scheduler.db` into per-rig files.
    fn migrate_legacy(&self, rig_ids: &[&str]) {
        let legacy = SchedulerStore::legacy_path();
        if !legacy.exists() || rig_ids.is_empty() {
            return;
        }
        let any_exists = rig_ids
            .iter()
            .any(|id| SchedulerStore::default_path_for(id).exists());
        if any_exists {
            return;
        }
        info!("migrating legacy scheduler.db to per-rig files");
        let legacy_store = SchedulerStore::open(&legacy);
        let db = legacy_store.db.read().unwrap_or_else(|e| e.into_inner());
        let configs: Vec<SchedulerConfig> = db
            .iter()
            .filter_map(|kv| {
                if kv.get_key().starts_with("sch:") {
                    kv.get_value::<SchedulerConfig>()
                } else {
                    None
                }
            })
            .collect();
        drop(db);
        for config in &configs {
            let store = self.store_for(&config.remote);
            store.upsert_config(config);
            info!("  migrated scheduler config for '{}'", config.remote);
        }
        let mut migrated = legacy.clone();
        migrated.set_extension("db.migrated");
        let _ = std::fs::rename(&legacy, &migrated);
    }
}

// ============================================================================
// Solar / grayline calculation (NOAA simplified algorithm)
// ============================================================================

/// Returns `(sunrise_min_utc, sunset_min_utc)` for the current UTC day.
/// Both values are in minutes-since-midnight UTC.
/// Returns `None` for polar regions where the sun never rises/sets.
fn sunrise_sunset_today(lat_deg: f64, lon_deg: f64) -> Option<(f64, f64)> {
    use std::f64::consts::PI;

    // Current Unix epoch time in seconds.
    let unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();

    // Julian Day Number for this instant.
    let jd = unix_secs / 86400.0 + 2440587.5;

    // Julian Century from J2000.0.
    let jc = (jd - 2451545.0) / 36525.0;

    // Geometric mean longitude of the sun (degrees).
    let l0 = (280.46646 + jc * (36000.76983 + jc * 0.0003032)).rem_euclid(360.0);

    // Geometric mean anomaly of the sun (degrees).
    let m = 357.52911 + jc * (35999.05029 - 0.0001537 * jc);
    let m_rad = m.to_radians();

    // Equation of center.
    let c = (1.914602 - jc * (0.004817 + 0.000014 * jc)) * m_rad.sin()
        + (0.019993 - 0.000101 * jc) * (2.0 * m_rad).sin()
        + 0.000289 * (3.0 * m_rad).sin();

    // Sun's true longitude.
    let sun_lon = l0 + c;

    // Apparent longitude.
    let omega = 125.04 - 1934.136 * jc;
    let lambda = sun_lon - 0.00569 - 0.00478 * omega.to_radians().sin();

    // Obliquity of the ecliptic.
    let eps0 =
        23.0 + (26.0 + (21.448 - jc * (46.8150 + jc * (0.00059 - jc * 0.001813))) / 60.0) / 60.0;
    let eps = eps0 + 0.00256 * omega.to_radians().cos();

    // Sun's declination.
    let decl = (eps.to_radians().sin() * lambda.to_radians().sin()).asin();

    // Equation of time (minutes).
    let y = (eps.to_radians() / 2.0).tan().powi(2);
    let l0_rad = l0.to_radians();
    let eot = 4.0
        * (y * (2.0 * l0_rad).sin() - 2.0 * m_rad.sin()
            + 4.0 * y * m_rad.sin() * (2.0 * l0_rad).cos()
            - 0.5 * y * y * (4.0 * l0_rad).sin()
            - 1.25 * (2.0 * m_rad).sin())
        .to_degrees();

    // Hour angle for sunrise/sunset (zenith = 90.833°).
    let lat_rad = lat_deg.to_radians();
    let cos_ha = ((PI / 2.0 + 0.833_f64.to_radians()).cos()) / (lat_rad.cos() * decl.cos())
        - lat_rad.tan() * decl.tan();

    if !(-1.0..=1.0).contains(&cos_ha) {
        return None; // Polar day or polar night.
    }

    let ha_deg = cos_ha.acos().to_degrees();

    // Solar noon (minutes from midnight UTC).
    let solar_noon = 720.0 - 4.0 * lon_deg - eot;

    Some((solar_noon - 4.0 * ha_deg, solar_noon + 4.0 * ha_deg))
}

/// Determine which grayline period is active for the given UTC time.
enum GraylinePeriod {
    Dawn,
    Dusk,
    Day,
    Night,
}

fn current_grayline_period(gl: &GraylineConfig, now_min: f64) -> GraylinePeriod {
    match sunrise_sunset_today(gl.lat, gl.lon) {
        Some((sunrise, sunset)) => {
            let hw = gl.transition_window_min as f64 / 2.0;
            let in_dawn = (now_min - sunrise).abs() <= hw;
            let in_dusk = (now_min - sunset).abs() <= hw;
            if in_dawn {
                GraylinePeriod::Dawn
            } else if in_dusk {
                GraylinePeriod::Dusk
            } else if now_min > sunrise + hw && now_min < sunset - hw {
                GraylinePeriod::Day
            } else {
                GraylinePeriod::Night
            }
        }
        // Polar: if sun is above horizon most of the day just treat as Day.
        None => {
            if gl.lat >= 0.0 {
                GraylinePeriod::Day
            } else {
                GraylinePeriod::Night
            }
        }
    }
}

fn grayline_bookmark_id(gl: &GraylineConfig, now_min: f64) -> Option<String> {
    match current_grayline_period(gl, now_min) {
        GraylinePeriod::Dawn => gl.dawn_bookmark_id.clone(),
        GraylinePeriod::Dusk => gl.dusk_bookmark_id.clone(),
        GraylinePeriod::Day => gl.day_bookmark_id.clone(),
        GraylinePeriod::Night => gl.night_bookmark_id.clone(),
    }
}

fn entry_is_active(entry: &ScheduleEntry, now_min: f64) -> bool {
    let start = entry.start_min as f64;
    let end = entry.end_min as f64;
    if start == end {
        // Equal start and end means all-day (24 h window).
        true
    } else if start < end {
        now_min >= start && now_min < end
    } else {
        // Spans midnight.
        now_min >= start || now_min < end
    }
}

fn entry_current_window_start(entry: &ScheduleEntry, now_min: f64) -> f64 {
    let start = entry.start_min as f64;
    let end = entry.end_min as f64;
    if start == end {
        return 0.0;
    }
    if start < end {
        return start;
    }
    if now_min >= start {
        start
    } else {
        start - 1440.0
    }
}

fn timespan_active_entries(entries: &[ScheduleEntry], now_min: f64) -> Vec<&ScheduleEntry> {
    entries
        .iter()
        .filter(|entry| entry_is_active(entry, now_min))
        .collect()
}

fn timespan_cycle_slot(
    active: &[&ScheduleEntry],
    now_min: f64,
    default_interleave: Option<u32>,
) -> Option<usize> {
    if active.len() <= 1 {
        return None;
    }

    let durations: Vec<u32> = active
        .iter()
        .map(|entry| entry.interleave_min.or(default_interleave).unwrap_or(0))
        .collect();
    let cycle: u32 = durations.iter().sum();
    if cycle == 0 {
        return None;
    }

    let overlap_start = active
        .iter()
        .map(|entry| entry_current_window_start(entry, now_min))
        .fold(f64::NEG_INFINITY, f64::max);
    if !overlap_start.is_finite() {
        return None;
    }

    let elapsed = (now_min - overlap_start).rem_euclid(cycle as f64);
    let mut cumulative = 0.0;
    for (idx, duration) in durations.iter().enumerate() {
        cumulative += *duration as f64;
        if elapsed < cumulative {
            return Some(idx);
        }
    }
    durations.len().checked_sub(1)
}

fn timespan_active_entry(
    entries: &[ScheduleEntry],
    now_min: f64,
    default_interleave: Option<u32>,
) -> Option<&ScheduleEntry> {
    let active = timespan_active_entries(entries, now_min);

    if active.is_empty() {
        return None;
    }

    if let Some(idx) = timespan_cycle_slot(&active, now_min, default_interleave) {
        return Some(active[idx]);
    }

    // Default: first matching entry wins.
    Some(active[0])
}

/// Current UTC time as minutes since midnight.
fn utc_minutes_now() -> f64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    (secs % 86400.0) / 60.0
}

// ============================================================================
// Scheduler background task
// ============================================================================

/// Check whether any configured satellite has an active pass right now,
/// using the cached pass predictions from the server.
///
/// Returns the highest-priority (lowest `.priority` value) satellite entry
/// whose pass is currently in progress.  A pass is considered "in progress"
/// from `aos_ms - pretune_secs` through `los_ms`, and only if its peak
/// elevation meets the entry's `min_elevation_deg` threshold.
fn active_satellite_entry<'a>(
    sat_cfg: &'a SatelliteSchedulerConfig,
    cached_passes: &Option<trx_core::geo::PassPredictionResult>,
) -> Option<&'a SatelliteEntry> {
    let predictions = cached_passes.as_ref()?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let pretune_ms = sat_cfg.pretune_secs as i64 * 1000;

    let mut best: Option<&SatelliteEntry> = None;

    for entry in &sat_cfg.entries {
        // Find any pass for this satellite's NORAD ID that is currently active.
        let pass_active = predictions.passes.iter().any(|pass| {
            pass.norad_id == entry.norad_id
                && pass.max_elevation_deg >= entry.min_elevation_deg
                && now_ms >= (pass.aos_ms - pretune_ms)
                && now_ms < pass.los_ms
        });

        if pass_active {
            match best {
                Some(current_best) if entry.priority < current_best.priority => {
                    best = Some(entry);
                }
                None => {
                    best = Some(entry);
                }
                _ => {}
            }
        }
    }

    best
}

/// Status info returned by the `/scheduler/{rig_id}/status` endpoint.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SchedulerStatus {
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_entry_id: Option<String>,
    pub last_bookmark_id: Option<String>,
    pub last_bookmark_name: Option<String>,
    pub last_applied_utc: Option<i64>,
    /// Center frequency applied with the last slot (SDR only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_center_hz: Option<u64>,
    /// Additional bookmark IDs active alongside the primary (virtual channels).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_bookmark_ids: Vec<String>,
    /// When a satellite pass triggered this status, the satellite name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_satellite: Option<String>,
    /// NORAD catalog number of the active satellite.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_satellite_norad_id: Option<u32>,
}

#[allow(clippy::too_many_arguments)]
async fn apply_scheduler_target(
    rig_tx: &mpsc::Sender<RigRequest>,
    remote: &str,
    status_map: &SchedulerStatusMap,
    bookmarks: &BookmarkStoreMap,
    entry_id: Option<&str>,
    bookmark_id: &str,
    center_hz: Option<u64>,
    extra_bm_ids: &[String],
    active_satellite: Option<(&str, u32)>,
) -> Result<SchedulerStatus, String> {
    let bookmark = bookmarks
        .get_for_rig(remote, bookmark_id)
        .ok_or_else(|| format!("bookmark '{bookmark_id}' not found for remote '{remote}'"))?;

    let extra_bookmarks: Vec<_> = extra_bm_ids
        .iter()
        .filter_map(|id| bookmarks.get_for_rig(remote, id))
        .collect();

    if let Some(chz) = center_hz {
        scheduler_send(
            rig_tx,
            RigCommand::SetCenterFreq(Freq { hz: chz }),
            remote.to_string(),
        )
        .await?;
    }

    scheduler_send(
        rig_tx,
        RigCommand::SetFreq(Freq {
            hz: bookmark.freq_hz,
        }),
        remote.to_string(),
    )
    .await?;

    scheduler_send(
        rig_tx,
        RigCommand::SetMode(trx_protocol::parse_mode(&bookmark.mode)),
        remote.to_string(),
    )
    .await?;

    if let Some(bandwidth_hz) = bookmark
        .bandwidth_hz
        .filter(|bw| *bw > 0 && *bw <= u32::MAX as u64)
        .map(|bw| bw as u32)
    {
        scheduler_send(
            rig_tx,
            RigCommand::SetBandwidth(bandwidth_hz),
            remote.to_string(),
        )
        .await?;
    }

    apply_scheduler_decoders(rig_tx, remote, &bookmark, &extra_bookmarks).await;

    let status = SchedulerStatus {
        active: true,
        last_entry_id: entry_id.map(str::to_string),
        last_bookmark_id: Some(bookmark_id.to_string()),
        last_bookmark_name: Some(bookmark.name.clone()),
        last_applied_utc: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        ),
        last_center_hz: center_hz,
        last_bookmark_ids: extra_bm_ids.to_vec(),
        active_satellite: active_satellite.map(|(name, _)| name.to_string()),
        active_satellite_norad_id: active_satellite.map(|(_, id)| id),
    };

    {
        let mut map = status_map.write().unwrap_or_else(|e| e.into_inner());
        map.insert(remote.to_string(), status.clone());
    }

    Ok(status)
}

/// Shared mutable state for scheduler status (one entry per rig).
pub type SchedulerStatusMap = Arc<RwLock<HashMap<String, SchedulerStatus>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppliedTarget {
    bookmark_id: String,
    center_hz: Option<u64>,
    extra_bookmark_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SchedulerControlSummary {
    pub connected_sessions: usize,
    pub released_sessions: usize,
    pub all_released: bool,
    pub current_session_released: bool,
}

#[derive(Default)]
pub struct SchedulerControlManager {
    sessions: RwLock<HashMap<Uuid, bool>>,
}

impl SchedulerControlManager {
    pub fn register_session(&self, session_id: Uuid) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.insert(session_id, true);
        }
    }

    pub fn unregister_session(&self, session_id: Uuid) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.remove(&session_id);
        }
    }

    pub fn set_released(&self, session_id: Uuid, released: bool) -> SchedulerControlSummary {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.insert(session_id, released);
        }
        self.summary(Some(session_id))
    }

    pub fn summary(&self, session_id: Option<Uuid>) -> SchedulerControlSummary {
        let Ok(sessions) = self.sessions.read() else {
            return SchedulerControlSummary::default();
        };
        let connected_sessions = sessions.len();
        let released_sessions = sessions.values().filter(|released| **released).count();
        let all_released = connected_sessions > 0 && released_sessions == connected_sessions;
        let current_session_released = session_id
            .and_then(|id| sessions.get(&id).copied())
            .unwrap_or(false);
        SchedulerControlSummary {
            connected_sessions,
            released_sessions,
            all_released,
            current_session_released,
        }
    }

    pub fn scheduler_allowed(&self) -> bool {
        let summary = self.summary(None);
        summary.connected_sessions == 0 || summary.all_released
    }

    pub fn has_active_user_control(&self) -> bool {
        let summary = self.summary(None);
        summary.connected_sessions > 0 && !summary.all_released
    }
}

pub type SharedSchedulerControlManager = Arc<SchedulerControlManager>;

pub fn spawn_scheduler_task(
    context: Arc<FrontendRuntimeContext>,
    rig_tx: mpsc::Sender<RigRequest>,
    store: Arc<SchedulerStoreMap>,
    bookmarks: Arc<BookmarkStoreMap>,
    status_map: SchedulerStatusMap,
    control: SharedSchedulerControlManager,
) {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(30));
        // Track the full last applied target per rig to avoid redundant retunes
        // while still honoring center-frequency or extra-channel changes.
        let mut last_applied: HashMap<String, AppliedTarget> = HashMap::new();
        let mut last_control_allowed = false;

        loop {
            interval.tick().await;

            // Skip while at least one connected user is still holding control.
            let scheduler_allowed = control.scheduler_allowed();
            if scheduler_allowed && !last_control_allowed {
                last_applied.clear();
            }
            last_control_allowed = scheduler_allowed;
            if !scheduler_allowed {
                continue;
            }

            let configs = store.list_all();
            let now_min = utc_minutes_now();

            // Read cached satellite pass predictions once per tick.
            let cached_passes = context.sat_passes.read().ok().and_then(|g| g.clone());

            for config in configs {
                if config.mode == SchedulerMode::Disabled
                    && config.satellites.as_ref().is_none_or(|s| !s.enabled)
                {
                    continue;
                }

                // ── Satellite preemption check ──
                // When a satellite pass is active, it overrides the base mode.
                let sat_override = config
                    .satellites
                    .as_ref()
                    .filter(|s| s.enabled)
                    .and_then(|sat_cfg| {
                        active_satellite_entry(sat_cfg, &cached_passes)
                    });

                let (entry_id, bm_id, center_hz, extra_bm_ids) = if let Some(sat_entry) =
                    sat_override
                {
                    info!(
                        "scheduler: satellite pass active for '{}' (NORAD {}), preempting base mode",
                        sat_entry.satellite, sat_entry.norad_id
                    );
                    (
                        Some(format!("sat:{}", sat_entry.id)),
                        sat_entry.bookmark_id.clone(),
                        sat_entry.center_hz,
                        sat_entry.bookmark_ids.clone(),
                    )
                } else if config.mode == SchedulerMode::Disabled {
                    // Satellites-only config with no active pass — nothing to do.
                    continue;
                } else {
                    match &config.mode {
                        SchedulerMode::Disabled => continue,
                        SchedulerMode::Grayline => {
                            let Some(bm_id) = config
                                .grayline
                                .as_ref()
                                .and_then(|gl| grayline_bookmark_id(gl, now_min))
                            else {
                                continue;
                            };
                            (None, bm_id, None, Vec::new())
                        }
                        SchedulerMode::TimeSpan => {
                            let Some(entry) = timespan_active_entry(
                                &config.entries,
                                now_min,
                                config.interleave_min,
                            ) else {
                                continue;
                            };
                            (
                                Some(entry.id.clone()),
                                entry.bookmark_id.clone(),
                                entry.center_hz,
                                entry.bookmark_ids.clone(),
                            )
                        }
                    }
                };

                let target = AppliedTarget {
                    bookmark_id: bm_id.clone(),
                    center_hz,
                    extra_bookmark_ids: extra_bm_ids.clone(),
                };

                // Already at this exact scheduled target — skip.
                if last_applied.get(&config.remote) == Some(&target) {
                    continue;
                }

                let Some(bm) = bookmarks.get_for_rig(&config.remote, &bm_id) else {
                    warn!(
                        "scheduler: bookmark '{}' not found for remote '{}'",
                        bm_id, config.remote
                    );
                    continue;
                };

                info!(
                    "scheduler: remote '{}' → bookmark '{}' ({} Hz {})",
                    config.remote, bm.name, bm.freq_hz, bm.mode
                );

                let sat_info = sat_override
                    .map(|e| (e.satellite.as_str(), e.norad_id));

                if let Err(e) = apply_scheduler_target(
                    &rig_tx,
                    &config.remote,
                    &status_map,
                    &bookmarks,
                    entry_id.as_deref(),
                    &bm_id,
                    center_hz,
                    &extra_bm_ids,
                    sat_info,
                )
                .await
                {
                    warn!(
                        "scheduler: failed to apply target for '{}': {e}",
                        config.remote
                    );
                    continue;
                }

                last_applied.insert(config.remote.clone(), target);
            }
        }
    });
}

async fn apply_scheduler_decoders(
    rig_tx: &mpsc::Sender<RigRequest>,
    remote: &str,
    bookmark: &crate::server::bookmarks::Bookmark,
    extra_bookmarks: &[crate::server::bookmarks::Bookmark],
) {
    let mut want_aprs = bookmark.mode.trim().eq_ignore_ascii_case("PKT");
    let mut want_hf_aprs = false;
    let mut want_ft8 = false;
    let mut want_ft4 = false;
    let mut want_ft2 = false;
    let mut want_wspr = false;
    let mut want_wxsat = false;
    let mut want_lrpt = false;

    let mut update_from = |bm: &crate::server::bookmarks::Bookmark| {
        for decoder in bm
            .decoders
            .iter()
            .map(|item| item.trim().to_ascii_lowercase())
        {
            match decoder.as_str() {
                "aprs" => want_aprs = true,
                "hf-aprs" => want_hf_aprs = true,
                "ft8" => want_ft8 = true,
                "ft4" => want_ft4 = true,
                "ft2" => want_ft2 = true,
                "wspr" => want_wspr = true,
                "wxsat" | "noaa" | "apt" => want_wxsat = true,
                "lrpt" | "meteor" => want_lrpt = true,
                _ => {}
            }
        }
    };

    update_from(bookmark);
    for bm in extra_bookmarks {
        update_from(bm);
    }

    let desired = [
        ("APRS", RigCommand::SetAprsDecodeEnabled(want_aprs)),
        ("HF APRS", RigCommand::SetHfAprsDecodeEnabled(want_hf_aprs)),
        ("FT8", RigCommand::SetFt8DecodeEnabled(want_ft8)),
        ("FT4", RigCommand::SetFt4DecodeEnabled(want_ft4)),
        ("FT2", RigCommand::SetFt2DecodeEnabled(want_ft2)),
        ("WSPR", RigCommand::SetWsprDecodeEnabled(want_wspr)),
        ("WxSat", RigCommand::SetWxsatDecodeEnabled(want_wxsat)),
        ("LRPT", RigCommand::SetLrptDecodeEnabled(want_lrpt)),
    ];

    for (label, cmd) in desired {
        if let Err(e) = scheduler_send(rig_tx, cmd, remote.to_string()).await {
            warn!(
                "scheduler: Set{label}DecodeEnabled failed for '{}': {:?}",
                remote, e
            );
        }
    }
}

async fn apply_last_scheduler_cycle(
    rig_tx: &mpsc::Sender<RigRequest>,
    remote: &str,
    status_map: &SchedulerStatusMap,
    bookmarks: &BookmarkStoreMap,
) {
    let status = {
        let Ok(map) = status_map.read() else {
            return;
        };
        map.get(remote).cloned()
    };

    let Some(status) = status else {
        return;
    };
    let Some(bookmark_id) = status.last_bookmark_id else {
        return;
    };
    if bookmarks.get_for_rig(remote, &bookmark_id).is_none() {
        warn!(
            "scheduler: last bookmark '{}' not found for remote '{}'",
            bookmark_id, remote
        );
        return;
    }

    let sat_info = status
        .active_satellite
        .as_deref()
        .zip(status.active_satellite_norad_id);

    if let Err(e) = apply_scheduler_target(
        rig_tx,
        remote,
        status_map,
        bookmarks,
        status.last_entry_id.as_deref(),
        &bookmark_id,
        status.last_center_hz,
        &status.last_bookmark_ids,
        sat_info,
    )
    .await
    {
        warn!("scheduler: restore failed for '{}': {e}", remote);
    }
}

/// Send a single RigCommand from the scheduler context (fire-and-forget style).
async fn scheduler_send(
    rig_tx: &mpsc::Sender<RigRequest>,
    cmd: RigCommand,
    remote: String,
) -> Result<(), String> {
    let (resp_tx, resp_rx) = oneshot::channel();
    rig_tx
        .send(RigRequest {
            cmd,
            respond_to: resp_tx,
            rig_id_override: Some(remote),
        })
        .await
        .map_err(|e| format!("send error: {e:?}"))?;

    let _ = tokio::time::timeout(Duration::from_secs(10), resp_rx).await;
    Ok(())
}

// ============================================================================
// HTTP handlers
// ============================================================================

/// GET /scheduler/{remote}
#[get("/scheduler/{remote}")]
pub async fn get_scheduler(
    path: web::Path<String>,
    store_map: web::Data<Arc<SchedulerStoreMap>>,
) -> impl Responder {
    let remote = path.into_inner();
    let config = store_map
        .store_for(&remote)
        .get_config()
        .unwrap_or(SchedulerConfig {
            remote: remote.clone(),
            mode: SchedulerMode::Disabled,
            grayline: None,
            entries: vec![],
            interleave_min: None,
            satellites: None,
        });
    HttpResponse::Ok().json(config)
}

/// PUT /scheduler/{remote}
#[put("/scheduler/{remote}")]
pub async fn put_scheduler(
    path: web::Path<String>,
    body: web::Json<SchedulerConfig>,
    store_map: web::Data<Arc<SchedulerStoreMap>>,
) -> impl Responder {
    let remote = path.into_inner();
    let mut config = body.into_inner();
    config.remote = remote.clone();
    if store_map.store_for(&remote).upsert_config(&config) {
        HttpResponse::Ok().json(config)
    } else {
        HttpResponse::InternalServerError().body("failed to save scheduler config")
    }
}

/// DELETE /scheduler/{remote}
#[delete("/scheduler/{remote}")]
pub async fn delete_scheduler(
    path: web::Path<String>,
    store_map: web::Data<Arc<SchedulerStoreMap>>,
) -> impl Responder {
    let remote = path.into_inner();
    store_map.store_for(&remote).remove_config();
    HttpResponse::Ok().json(serde_json::json!({ "deleted": true }))
}

/// GET /scheduler/{remote}/status
#[get("/scheduler/{remote}/status")]
pub async fn get_scheduler_status(
    path: web::Path<String>,
    status_map: web::Data<SchedulerStatusMap>,
) -> impl Responder {
    let remote = path.into_inner();
    let map = status_map.read().unwrap_or_else(|e| e.into_inner());
    let status = map.get(&remote).cloned().unwrap_or_default();
    HttpResponse::Ok().json(status)
}

#[derive(Deserialize)]
pub struct SchedulerActivateEntryRequest {
    pub entry_id: String,
}

/// PUT /scheduler/{rig_id}/activate
#[put("/scheduler/{remote}/activate")]
pub async fn put_scheduler_activate_entry(
    path: web::Path<String>,
    body: web::Json<SchedulerActivateEntryRequest>,
    store_map: web::Data<Arc<SchedulerStoreMap>>,
    status_map: web::Data<SchedulerStatusMap>,
    bookmarks: web::Data<Arc<BookmarkStoreMap>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> impl Responder {
    let rig_id = path.into_inner();
    let Some(config) = store_map.store_for(&rig_id).get_config() else {
        return HttpResponse::NotFound().body("scheduler config not found");
    };
    if config.mode != SchedulerMode::TimeSpan {
        return HttpResponse::BadRequest().body("scheduler mode is not time_span");
    }

    let entry_id = body.entry_id.trim();
    let Some(entry) = config.entries.iter().find(|entry| entry.id == entry_id) else {
        return HttpResponse::NotFound().body("scheduler entry not found");
    };
    if entry.bookmark_id.trim().is_empty() {
        return HttpResponse::BadRequest().body("scheduler entry has no primary bookmark");
    }

    match apply_scheduler_target(
        rig_tx.get_ref(),
        &rig_id,
        status_map.get_ref(),
        bookmarks.get_ref().as_ref(),
        Some(&entry.id),
        &entry.bookmark_id,
        entry.center_hz,
        &entry.bookmark_ids,
        None,
    )
    .await
    {
        Ok(status) => HttpResponse::Ok().json(status),
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[derive(Deserialize)]
pub struct SchedulerControlQuery {
    pub session_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct SchedulerControlUpdate {
    pub session_id: Uuid,
    pub released: bool,
    #[serde(default)]
    pub remote: Option<String>,
}

#[get("/scheduler-control")]
pub async fn get_scheduler_control(
    query: web::Query<SchedulerControlQuery>,
    control: web::Data<SharedSchedulerControlManager>,
) -> impl Responder {
    HttpResponse::Ok().json(control.summary(query.session_id))
}

#[put("/scheduler-control")]
pub async fn put_scheduler_control(
    body: web::Json<SchedulerControlUpdate>,
    control: web::Data<SharedSchedulerControlManager>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
    status_map: web::Data<SchedulerStatusMap>,
    bookmarks: web::Data<Arc<BookmarkStoreMap>>,
) -> impl Responder {
    let body = body.into_inner();
    let summary = control.set_released(body.session_id, body.released);
    if body.released && summary.all_released {
        if let Some(remote) = body.remote.as_deref() {
            apply_last_scheduler_cycle(
                rig_tx.get_ref(),
                remote,
                status_map.get_ref(),
                bookmarks.get_ref().as_ref(),
            )
            .await;
        }
    }
    HttpResponse::Ok().json(summary)
}

#[cfg(test)]
mod tests {
    use super::{
        active_satellite_entry, timespan_active_entries, timespan_active_entry,
        timespan_cycle_slot, SatelliteEntry, SatelliteSchedulerConfig, ScheduleEntry,
    };
    use trx_core::geo::{PassPrediction, PassPredictionResult, SatCategory, TleSource};

    fn entry(
        id: &str,
        start_min: u32,
        end_min: u32,
        bookmark_id: &str,
        center_hz: Option<u64>,
        interleave_min: Option<u32>,
    ) -> ScheduleEntry {
        ScheduleEntry {
            id: id.to_string(),
            start_min,
            end_min,
            bookmark_id: bookmark_id.to_string(),
            label: None,
            interleave_min,
            center_hz,
            bookmark_ids: Vec::new(),
        }
    }

    #[test]
    fn timespan_active_entry_returns_selected_overlap_entry() {
        let entries = vec![
            entry("slot-a", 0, 0, "bm-shared", Some(144_500_000), Some(10)),
            entry("slot-b", 0, 0, "bm-shared", Some(144_300_000), Some(10)),
        ];

        let active = timespan_active_entry(&entries, 15.0, None).expect("active entry");
        assert_eq!(active.id, "slot-b");
        assert_eq!(active.center_hz, Some(144_300_000));
    }

    #[test]
    fn timespan_active_entry_returns_first_match_without_interleave() {
        let entries = vec![
            entry("slot-a", 60, 120, "bm-a", Some(14_100_000), None),
            entry("slot-b", 60, 120, "bm-b", Some(14_200_000), None),
        ];

        let active = timespan_active_entry(&entries, 90.0, None).expect("active entry");
        assert_eq!(active.id, "slot-a");
        assert_eq!(active.center_hz, Some(14_100_000));
    }

    #[test]
    fn timespan_cycle_is_anchored_to_overlap_start() {
        let entries = vec![
            entry("slot-a", 60, 180, "bm-a", Some(14_100_000), Some(10)),
            entry("slot-b", 90, 180, "bm-b", Some(14_200_000), Some(10)),
        ];

        let active = timespan_active_entry(&entries, 100.0, None).expect("active entry");
        assert_eq!(active.id, "slot-b");
    }

    #[test]
    fn timespan_cycle_handles_overlap_spanning_midnight() {
        let entries = vec![
            entry("slot-a", 1380, 120, "bm-a", Some(14_100_000), Some(10)),
            entry("slot-b", 0, 120, "bm-b", Some(14_200_000), Some(10)),
        ];

        let active_entries = timespan_active_entries(&entries, 5.0);
        assert_eq!(active_entries.len(), 2);
        let slot = timespan_cycle_slot(&active_entries, 5.0, None).expect("cycle slot");
        assert_eq!(slot, 0);

        let active = timespan_active_entry(&entries, 10.0, None).expect("active entry");
        assert_eq!(active.id, "slot-b");
    }

    // ── Satellite scheduling tests ─────────────────────────────────

    fn sat_entry(id: &str, satellite: &str, norad_id: u32, min_el: f64, priority: u32) -> SatelliteEntry {
        SatelliteEntry {
            id: id.to_string(),
            satellite: satellite.to_string(),
            norad_id,
            bookmark_id: format!("bm-{id}"),
            min_elevation_deg: min_el,
            priority,
            center_hz: None,
            bookmark_ids: Vec::new(),
        }
    }

    fn pass(satellite: &str, norad_id: u32, aos_ms: i64, los_ms: i64, max_el: f64) -> PassPrediction {
        PassPrediction {
            satellite: satellite.to_string(),
            norad_id,
            category: SatCategory::Weather,
            aos_ms,
            los_ms,
            max_elevation_deg: max_el,
            azimuth_aos_deg: 0.0,
            azimuth_los_deg: 180.0,
            duration_s: ((los_ms - aos_ms) / 1000) as u64,
        }
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    #[test]
    fn satellite_no_active_pass_returns_none() {
        let cfg = SatelliteSchedulerConfig {
            enabled: true,
            pretune_secs: 60,
            entries: vec![sat_entry("noaa19", "NOAA 19", 33591, 5.0, 0)],
        };
        let now = now_ms();
        // Pass is in the future (starts 1 hour from now).
        let predictions = Some(PassPredictionResult {
            passes: vec![pass("NOAA 19", 33591, now + 3_600_000, now + 4_200_000, 45.0)],
            satellite_count: 1,
            tle_source: TleSource::Celestrak,
        });

        assert!(active_satellite_entry(&cfg, &predictions).is_none());
    }

    #[test]
    fn satellite_active_pass_returns_entry() {
        let cfg = SatelliteSchedulerConfig {
            enabled: true,
            pretune_secs: 60,
            entries: vec![sat_entry("noaa19", "NOAA 19", 33591, 5.0, 0)],
        };
        let now = now_ms();
        // Pass is currently active.
        let predictions = Some(PassPredictionResult {
            passes: vec![pass("NOAA 19", 33591, now - 300_000, now + 300_000, 45.0)],
            satellite_count: 1,
            tle_source: TleSource::Celestrak,
        });

        let result = active_satellite_entry(&cfg, &predictions);
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "noaa19");
    }

    #[test]
    fn satellite_pretune_activates_before_aos() {
        let cfg = SatelliteSchedulerConfig {
            enabled: true,
            pretune_secs: 120, // 2 minutes pretune
            entries: vec![sat_entry("noaa19", "NOAA 19", 33591, 5.0, 0)],
        };
        let now = now_ms();
        // AOS is 60 seconds from now — within the 120s pretune window.
        let predictions = Some(PassPredictionResult {
            passes: vec![pass("NOAA 19", 33591, now + 60_000, now + 660_000, 30.0)],
            satellite_count: 1,
            tle_source: TleSource::Celestrak,
        });

        let result = active_satellite_entry(&cfg, &predictions);
        assert!(result.is_some(), "should activate during pretune window");
    }

    #[test]
    fn satellite_low_elevation_pass_skipped() {
        let cfg = SatelliteSchedulerConfig {
            enabled: true,
            pretune_secs: 60,
            entries: vec![sat_entry("noaa19", "NOAA 19", 33591, 20.0, 0)], // min 20°
        };
        let now = now_ms();
        // Pass has only 10° max elevation — below threshold.
        let predictions = Some(PassPredictionResult {
            passes: vec![pass("NOAA 19", 33591, now - 300_000, now + 300_000, 10.0)],
            satellite_count: 1,
            tle_source: TleSource::Celestrak,
        });

        assert!(
            active_satellite_entry(&cfg, &predictions).is_none(),
            "pass below min elevation should be skipped"
        );
    }

    #[test]
    fn satellite_priority_selects_lower_value() {
        let cfg = SatelliteSchedulerConfig {
            enabled: true,
            pretune_secs: 60,
            entries: vec![
                sat_entry("noaa19", "NOAA 19", 33591, 5.0, 10),
                sat_entry("meteor", "METEOR-M2 3", 57166, 5.0, 1), // higher priority
            ],
        };
        let now = now_ms();
        // Both satellites have active passes.
        let predictions = Some(PassPredictionResult {
            passes: vec![
                pass("NOAA 19", 33591, now - 300_000, now + 300_000, 40.0),
                pass("METEOR-M2 3", 57166, now - 200_000, now + 400_000, 35.0),
            ],
            satellite_count: 2,
            tle_source: TleSource::Celestrak,
        });

        let result = active_satellite_entry(&cfg, &predictions);
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "meteor", "lower priority value should win");
    }

    #[test]
    fn satellite_no_predictions_returns_none() {
        let cfg = SatelliteSchedulerConfig {
            enabled: true,
            pretune_secs: 60,
            entries: vec![sat_entry("noaa19", "NOAA 19", 33591, 5.0, 0)],
        };
        // No predictions available yet.
        assert!(active_satellite_entry(&cfg, &None).is_none());
    }

    #[test]
    fn satellite_unmatched_norad_id_ignored() {
        let cfg = SatelliteSchedulerConfig {
            enabled: true,
            pretune_secs: 60,
            entries: vec![sat_entry("noaa19", "NOAA 19", 33591, 5.0, 0)],
        };
        let now = now_ms();
        // Active pass for a different satellite.
        let predictions = Some(PassPredictionResult {
            passes: vec![pass("NOAA 18", 28654, now - 300_000, now + 300_000, 50.0)],
            satellite_count: 1,
            tle_source: TleSource::Celestrak,
        });

        assert!(
            active_satellite_entry(&cfg, &predictions).is_none(),
            "should not match on different NORAD ID"
        );
    }

    #[test]
    fn satellite_config_deserializes_with_defaults() {
        let json = r#"{
            "remote": "rig1",
            "mode": "grayline"
        }"#;
        let config: super::SchedulerConfig = serde_json::from_str(json).unwrap();
        assert!(config.satellites.is_none());
    }

    #[test]
    fn satellite_config_deserializes_full() {
        let json = r#"{
            "remote": "rig1",
            "mode": "disabled",
            "satellites": {
                "enabled": true,
                "pretune_secs": 90,
                "entries": [
                    {
                        "id": "noaa19-apt",
                        "satellite": "NOAA 19",
                        "norad_id": 33591,
                        "bookmark_id": "bm-noaa19",
                        "min_elevation_deg": 15.0,
                        "priority": 0
                    },
                    {
                        "id": "meteor-lrpt",
                        "satellite": "METEOR-M2 3",
                        "norad_id": 57166,
                        "bookmark_id": "bm-meteor",
                        "priority": 1
                    }
                ]
            }
        }"#;
        let config: super::SchedulerConfig = serde_json::from_str(json).unwrap();
        let sat = config.satellites.unwrap();
        assert!(sat.enabled);
        assert_eq!(sat.pretune_secs, 90);
        assert_eq!(sat.entries.len(), 2);
        assert_eq!(sat.entries[0].satellite, "NOAA 19");
        assert_eq!(sat.entries[0].norad_id, 33591);
        assert_eq!(sat.entries[0].min_elevation_deg, 15.0);
        // Second entry should get default min_elevation_deg.
        assert_eq!(sat.entries[1].min_elevation_deg, 5.0);
    }
}
