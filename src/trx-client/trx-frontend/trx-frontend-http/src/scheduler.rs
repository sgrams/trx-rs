// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Background Decoding Scheduler.
//!
//! When no SSE clients are connected the scheduler periodically inspects the
//! current UTC time, selects the matching bookmark from the per-rig config,
//! and issues `SetFreq` + `SetMode` commands to retune the rig automatically.

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

use trx_core::radio::freq::Freq;
use trx_core::rig::command::RigCommand;
use trx_core::RigRequest;
use trx_frontend::FrontendRuntimeContext;

use crate::server::bookmarks::BookmarkStore;

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
    pub bookmark_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SchedulerConfig {
    pub rig_id: String,
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
            PickleDb::load(path, PickleDbDumpPolicy::AutoDump, SerializationMethod::Json)
                .unwrap_or_else(|_| {
                    PickleDb::new(path, PickleDbDumpPolicy::AutoDump, SerializationMethod::Json)
                })
        } else {
            PickleDb::new(path, PickleDbDumpPolicy::AutoDump, SerializationMethod::Json)
        };
        Self {
            db: Arc::new(RwLock::new(db)),
        }
    }

    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .map(|p| p.join("trx-rs").join("scheduler.db"))
            .unwrap_or_else(|| PathBuf::from("scheduler.db"))
    }

    pub fn get(&self, rig_id: &str) -> Option<SchedulerConfig> {
        let db = self.db.read().unwrap_or_else(|e| e.into_inner());
        db.get::<SchedulerConfig>(&format!("sch:{rig_id}"))
    }

    pub fn upsert(&self, config: &SchedulerConfig) -> bool {
        let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
        db.set(&format!("sch:{}", config.rig_id), config).is_ok()
    }

    pub fn remove(&self, rig_id: &str) -> bool {
        let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
        db.rem(&format!("sch:{rig_id}")).unwrap_or(false)
    }

    pub fn list_all(&self) -> Vec<SchedulerConfig> {
        let db = self.db.read().unwrap_or_else(|e| e.into_inner());
        db.iter()
            .filter_map(|kv| {
                if kv.get_key().starts_with("sch:") {
                    kv.get_value::<SchedulerConfig>()
                } else {
                    None
                }
            })
            .collect()
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
    let eps0 = 23.0
        + (26.0
            + (21.448 - jc * (46.8150 + jc * (0.00059 - jc * 0.001813))) / 60.0)
            / 60.0;
    let eps = eps0 + 0.00256 * omega.to_radians().cos();

    // Sun's declination.
    let decl = (eps.to_radians().sin() * lambda.to_radians().sin()).asin();

    // Equation of time (minutes).
    let y = (eps.to_radians() / 2.0).tan().powi(2);
    let l0_rad = l0.to_radians();
    let eot = 4.0
        * (y * (2.0 * l0_rad).sin()
            - 2.0 * m_rad.sin()
            + 4.0 * y * m_rad.sin() * (2.0 * l0_rad).cos()
            - 0.5 * y * y * (4.0 * l0_rad).sin()
            - 1.25 * (2.0 * m_rad).sin())
        .to_degrees();

    // Hour angle for sunrise/sunset (zenith = 90.833°).
    let lat_rad = lat_deg.to_radians();
    let cos_ha = ((PI / 2.0 + 0.833_f64.to_radians()).cos())
        / (lat_rad.cos() * decl.cos())
        - lat_rad.tan() * decl.tan();

    if cos_ha < -1.0 || cos_ha > 1.0 {
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
    if start <= end {
        now_min >= start && now_min < end
    } else {
        // Spans midnight.
        now_min >= start || now_min < end
    }
}

fn timespan_bookmark_id(
    entries: &[ScheduleEntry],
    now_min: f64,
    interleave_min: Option<u32>,
) -> Option<String> {
    let active: Vec<&ScheduleEntry> = entries
        .iter()
        .filter(|e| entry_is_active(e, now_min))
        .collect();

    if active.is_empty() {
        return None;
    }

    // With interleaving and more than one active entry, pick by time slot.
    if active.len() > 1 {
        if let Some(step) = interleave_min.filter(|&s| s > 0) {
            let slot = (now_min as u64 / step as u64) as usize % active.len();
            return Some(active[slot].bookmark_id.clone());
        }
    }

    // Default: first matching entry wins.
    Some(active[0].bookmark_id.clone())
}

/// Current UTC time as minutes since midnight.
fn utc_minutes_now() -> f64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    ((secs % 86400) as f64) / 60.0
}

// ============================================================================
// Scheduler background task
// ============================================================================

/// Status info returned by the `/scheduler/{rig_id}/status` endpoint.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SchedulerStatus {
    pub active: bool,
    pub last_bookmark_id: Option<String>,
    pub last_bookmark_name: Option<String>,
    pub last_applied_utc: Option<i64>,
}

/// Shared mutable state for scheduler status (one entry per rig).
pub type SchedulerStatusMap = Arc<RwLock<HashMap<String, SchedulerStatus>>>;

pub fn spawn_scheduler_task(
    context: Arc<FrontendRuntimeContext>,
    rig_tx: mpsc::Sender<RigRequest>,
    store: Arc<SchedulerStore>,
    bookmarks: Arc<BookmarkStore>,
    status_map: SchedulerStatusMap,
) {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(30));
        // Track last applied bookmark per rig to avoid redundant retunes.
        let mut last_applied: HashMap<String, String> = HashMap::new();

        loop {
            interval.tick().await;

            // Skip if any user is currently connected.
            if context
                .sse_clients
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0
            {
                continue;
            }

            let configs = store.list_all();
            let now_min = utc_minutes_now();

            for config in configs {
                if config.mode == SchedulerMode::Disabled {
                    continue;
                }

                let target_bm_id = match &config.mode {
                    SchedulerMode::Disabled => continue,
                    SchedulerMode::Grayline => config
                        .grayline
                        .as_ref()
                        .and_then(|gl| grayline_bookmark_id(gl, now_min)),
                    SchedulerMode::TimeSpan => {
                        timespan_bookmark_id(&config.entries, now_min, config.interleave_min)
                    }
                };

                let Some(bm_id) = target_bm_id else { continue };

                // Already at this bookmark — skip.
                if last_applied.get(&config.rig_id) == Some(&bm_id) {
                    continue;
                }

                let Some(bm) = bookmarks.get(&bm_id) else {
                    warn!(
                        "scheduler: bookmark '{}' not found for rig '{}'",
                        bm_id, config.rig_id
                    );
                    continue;
                };

                info!(
                    "scheduler: rig '{}' → bookmark '{}' ({} Hz {})",
                    config.rig_id, bm.name, bm.freq_hz, bm.mode
                );

                // Apply SetFreq.
                if let Err(e) = scheduler_send(
                    &rig_tx,
                    RigCommand::SetFreq(Freq { hz: bm.freq_hz }),
                    config.rig_id.clone(),
                )
                .await
                {
                    warn!("scheduler: SetFreq failed for '{}': {:?}", config.rig_id, e);
                    continue;
                }

                // Apply SetMode.
                {
                    let mode = trx_protocol::parse_mode(&bm.mode);
                    if let Err(e) = scheduler_send(
                        &rig_tx,
                        RigCommand::SetMode(mode),
                        config.rig_id.clone(),
                    )
                    .await
                    {
                        warn!(
                            "scheduler: SetMode failed for '{}': {:?}",
                            config.rig_id, e
                        );
                    }
                }

                last_applied.insert(config.rig_id.clone(), bm_id.clone());

                // Update status map.
                let now_ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                {
                    let mut map = status_map.write().unwrap_or_else(|e| e.into_inner());
                    map.insert(
                        config.rig_id.clone(),
                        SchedulerStatus {
                            active: true,
                            last_bookmark_id: Some(bm_id),
                            last_bookmark_name: Some(bm.name),
                            last_applied_utc: Some(now_ts),
                        },
                    );
                }
            }
        }
    });
}

/// Send a single RigCommand from the scheduler context (fire-and-forget style).
async fn scheduler_send(
    rig_tx: &mpsc::Sender<RigRequest>,
    cmd: RigCommand,
    rig_id: String,
) -> Result<(), String> {
    let (resp_tx, resp_rx) = oneshot::channel();
    rig_tx
        .send(RigRequest {
            cmd,
            respond_to: resp_tx,
            rig_id_override: Some(rig_id),
        })
        .await
        .map_err(|e| format!("send error: {e:?}"))?;

    let _ = tokio::time::timeout(Duration::from_secs(10), resp_rx).await;
    Ok(())
}

// ============================================================================
// HTTP handlers
// ============================================================================

/// GET /scheduler/{rig_id}
#[get("/scheduler/{rig_id}")]
pub async fn get_scheduler(
    path: web::Path<String>,
    store: web::Data<Arc<SchedulerStore>>,
) -> impl Responder {
    let rig_id = path.into_inner();
    let config = store.get(&rig_id).unwrap_or(SchedulerConfig {
        rig_id: rig_id.clone(),
        mode: SchedulerMode::Disabled,
        grayline: None,
        entries: vec![],
        interleave_min: None,
    });
    HttpResponse::Ok().json(config)
}

/// PUT /scheduler/{rig_id}
#[put("/scheduler/{rig_id}")]
pub async fn put_scheduler(
    path: web::Path<String>,
    body: web::Json<SchedulerConfig>,
    store: web::Data<Arc<SchedulerStore>>,
) -> impl Responder {
    let rig_id = path.into_inner();
    let mut config = body.into_inner();
    config.rig_id = rig_id;
    if store.upsert(&config) {
        HttpResponse::Ok().json(config)
    } else {
        HttpResponse::InternalServerError().body("failed to save scheduler config")
    }
}

/// DELETE /scheduler/{rig_id}
#[delete("/scheduler/{rig_id}")]
pub async fn delete_scheduler(
    path: web::Path<String>,
    store: web::Data<Arc<SchedulerStore>>,
) -> impl Responder {
    let rig_id = path.into_inner();
    store.remove(&rig_id);
    HttpResponse::Ok().json(serde_json::json!({ "deleted": true }))
}

/// GET /scheduler/{rig_id}/status
#[get("/scheduler/{rig_id}/status")]
pub async fn get_scheduler_status(
    path: web::Path<String>,
    status_map: web::Data<SchedulerStatusMap>,
) -> impl Responder {
    let rig_id = path.into_inner();
    let map = status_map.read().unwrap_or_else(|e| e.into_inner());
    let status = map.get(&rig_id).cloned().unwrap_or_default();
    HttpResponse::Ok().json(status)
}
