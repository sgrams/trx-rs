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
    _context: Arc<FrontendRuntimeContext>,
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

            for config in configs {
                if config.mode == SchedulerMode::Disabled {
                    continue;
                }

                let (entry_id, bm_id, center_hz, extra_bm_ids) = match &config.mode {
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
                        let Some(entry) =
                            timespan_active_entry(&config.entries, now_min, config.interleave_min)
                        else {
                            continue;
                        };
                        (
                            Some(entry.id.clone()),
                            entry.bookmark_id.clone(),
                            entry.center_hz,
                            entry.bookmark_ids.clone(),
                        )
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

                if let Err(e) = apply_scheduler_target(
                    &rig_tx,
                    &config.remote,
                    &status_map,
                    &bookmarks,
                    entry_id.as_deref(),
                    &bm_id,
                    center_hz,
                    &extra_bm_ids,
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

    if let Err(e) = apply_scheduler_target(
        rig_tx,
        remote,
        status_map,
        bookmarks,
        status.last_entry_id.as_deref(),
        &bookmark_id,
        status.last_center_hz,
        &status.last_bookmark_ids,
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
        timespan_active_entries, timespan_active_entry, timespan_cycle_slot, ScheduleEntry,
    };

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
}
