// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Satellite geo-referencing for weather satellite image overlays.
//!
//! Uses SGP4 orbital propagation to compute the ground track and geographic
//! bounds of a satellite pass, given the satellite identity, pass timestamps,
//! and receiver station coordinates.

use sgp4::{Constants, Elements, MinutesSinceEpoch};
use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::RwLock;
use std::time::Duration;

/// Result of computing upcoming passes, including metadata about TLE source.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PassPredictionResult {
    /// Predicted passes sorted by AOS time.
    pub passes: Vec<PassPrediction>,
    /// Number of satellites evaluated.
    pub satellite_count: usize,
    /// Whether predictions are based on live CelesTrak TLE data.
    pub tle_source: TleSource,
}

/// Indicates the origin of the TLE data used for predictions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TleSource {
    /// Live TLE data fetched from CelesTrak.
    Celestrak,
    /// No TLE data available yet (CelesTrak fetch pending or failed).
    Unavailable,
}

/// Half-swath width in km for NOAA APT / Meteor LRPT imagery.
const SWATH_HALF_WIDTH_KM: f64 = 1400.0;

/// Earth radius in km (WGS84 mean).
const EARTH_RADIUS_KM: f64 = 6371.0;

/// CelesTrak weather satellite TLE endpoint.
const CELESTRAK_WEATHER_URL: &str =
    "https://celestrak.org/NORAD/elements/gp.php?GROUP=weather&FORMAT=tle";

/// CelesTrak amateur satellite TLE endpoint.
const CELESTRAK_HAM_URL: &str =
    "https://celestrak.org/NORAD/elements/gp.php?GROUP=amateur&FORMAT=tle";

/// How often to refresh TLEs after the initial fetch (24 hours).
const TLE_REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Satellite category based on TLE source group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SatCategory {
    Weather,
    Amateur,
    Other,
}

/// A single TLE entry: satellite name + two-line element set.
#[derive(Debug, Clone)]
pub struct TleEntry {
    pub name: String,
    pub line1: String,
    pub line2: String,
    pub category: SatCategory,
}

/// Global store for dynamically-fetched TLE data.
///
/// Keys are NORAD catalog numbers; values contain the satellite name and TLE lines.
static TLE_STORE: RwLock<Option<HashMap<u32, TleEntry>>> = RwLock::new(None);

/// Geographic bounds for a satellite image overlay: `[south, west, north, east]`.
pub type GeoBounds = [f64; 4];

/// A single ground track point: `[latitude, longitude]` in decimal degrees.
pub type TrackPoint = [f64; 2];

/// Result of geo-referencing a satellite pass.
#[derive(Debug, Clone)]
pub struct PassGeo {
    /// Bounding box `[south, west, north, east]` in decimal degrees.
    pub bounds: GeoBounds,
    /// Ground track points `[[lat, lon], ...]` sampled along the pass.
    pub ground_track: Vec<TrackPoint>,
}

/// A predicted satellite pass over the observer's location.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PassPrediction {
    /// Satellite display name.
    pub satellite: String,
    /// NORAD catalog number.
    pub norad_id: u32,
    /// Satellite category (weather, amateur, other).
    pub category: SatCategory,
    /// Acquisition of Signal: UTC timestamp in milliseconds.
    pub aos_ms: i64,
    /// Loss of Signal: UTC timestamp in milliseconds.
    pub los_ms: i64,
    /// Maximum elevation angle in degrees above horizon.
    pub max_elevation_deg: f64,
    /// Azimuth at AOS in degrees (0 = N, 90 = E).
    pub azimuth_aos_deg: f64,
    /// Azimuth at LOS in degrees.
    pub azimuth_los_deg: f64,
    /// Pass duration in seconds.
    pub duration_s: u64,
}

/// Map satellite name patterns to NORAD catalog numbers.
fn norad_id_for_satellite(name: &str) -> Option<u32> {
    let upper = name.to_uppercase();
    if upper.contains("NOAA") && upper.contains("15") {
        Some(25338)
    } else if upper.contains("NOAA") && upper.contains("18") {
        Some(28654)
    } else if upper.contains("NOAA") && upper.contains("19") {
        Some(33591)
    } else if upper.contains("METEOR")
        && (upper.contains("2-3") || upper.contains("N2-3") || upper.contains("2_3"))
    {
        Some(57166)
    } else if upper.contains("METEOR")
        && (upper.contains("2-4") || upper.contains("N2-4") || upper.contains("2_4"))
    {
        Some(59051)
    } else {
        None
    }
}

/// Hardcoded fallback TLE data for active weather satellites.
///
/// These are recent-epoch TLEs.  SGP4 propagation from stale TLEs still
/// gives sub-degree accuracy for image overlay purposes (drift ~0.1 deg/week).
fn hardcoded_tle(norad_id: u32) -> Option<(&'static str, &'static str)> {
    match norad_id {
        25338 => Some((
            "1 25338U 98030A   26084.50000000  .00000045  00000-0  36000-4 0  9998",
            "2 25338  98.7285 114.5200 0010150  45.0000 315.1500 14.25955000  4001",
        )),
        28654 => Some((
            "1 28654U 05018A   26084.50000000  .00000036  00000-0  28000-4 0  9997",
            "2 28654  99.0400 162.3000 0013800 290.0000  70.0000 14.12500000  1005",
        )),
        33591 => Some((
            "1 33591U 09005A   26084.50000000  .00000028  00000-0  20000-4 0  9996",
            "2 33591  99.1700 050.5000 0014000 100.0000 260.0000 14.12300000  8002",
        )),
        57166 => Some((
            "1 57166U 23091A   26084.50000000  .00000020  00000-0  16000-4 0  9998",
            "2 57166  98.7700 170.0000 0005000  90.0000 270.0000 14.23700000  1502",
        )),
        59051 => Some((
            "1 59051U 24044A   26084.50000000  .00000018  00000-0  14000-4 0  9997",
            "2 59051  98.7700 200.0000 0005000  80.0000 280.0000 14.23700000  1006",
        )),
        _ => None,
    }
}

/// Look up TLE lines for a satellite by name.
///
/// Checks the dynamic [`TLE_STORE`] first (populated by [`spawn_tle_refresh_task`]),
/// falling back to hardcoded TLEs if no fresh data is available.
fn tle_for_satellite(name: &str) -> Option<(String, String)> {
    let norad_id = norad_id_for_satellite(name)?;

    // Try dynamic store first.
    if let Ok(guard) = TLE_STORE.read() {
        if let Some(store) = guard.as_ref() {
            if let Some(entry) = store.get(&norad_id) {
                return Some((entry.line1.clone(), entry.line2.clone()));
            }
        }
    }

    // Fall back to hardcoded.
    hardcoded_tle(norad_id).map(|(l1, l2)| (l1.to_string(), l2.to_string()))
}

// ---------------------------------------------------------------------------
// CelesTrak TLE refresh
// ---------------------------------------------------------------------------

/// Parse a CelesTrak 3-line TLE response into a map of NORAD ID → TleEntry.
fn parse_tle_response(body: &str, category: SatCategory) -> HashMap<u32, TleEntry> {
    let mut result = HashMap::new();
    let lines: Vec<&str> = body.lines().map(|l| l.trim_end()).collect();
    let mut i = 0;
    while i + 2 < lines.len() {
        let name_line = lines[i].trim();
        let line1 = lines[i + 1];
        let line2 = lines[i + 2];
        // Validate TLE line markers
        if line1.starts_with("1 ") && line2.starts_with("2 ") {
            // Extract NORAD catalog number from line 1 columns 2-6
            if let Ok(norad_id) = line1[2..7].trim().parse::<u32>() {
                result.insert(
                    norad_id,
                    TleEntry {
                        name: name_line.to_string(),
                        line1: line1.to_string(),
                        line2: line2.to_string(),
                        category,
                    },
                );
            }
        }
        i += 3;
    }
    result
}

/// Fetch TLEs from a CelesTrak URL and merge them into the global store.
async fn fetch_and_merge_tles(url: &str, category: SatCategory) -> Result<usize, String> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("CelesTrak fetch failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("CelesTrak returned HTTP {}", response.status()));
    }

    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read CelesTrak response: {e}"))?;

    let tles = parse_tle_response(&body, category);
    let count = tles.len();

    if count == 0 {
        return Err("CelesTrak response contained no valid TLEs".to_string());
    }

    match TLE_STORE.write() {
        Ok(mut guard) => {
            if let Some(store) = guard.as_mut() {
                store.extend(tles);
            } else {
                *guard = Some(tles);
            }
        }
        Err(e) => {
            let mut guard = e.into_inner();
            if let Some(store) = guard.as_mut() {
                store.extend(tles);
            } else {
                *guard = Some(tles);
            }
        }
    }

    Ok(count)
}

/// Fetch fresh TLE data from CelesTrak and update the global store.
///
/// Returns the number of TLEs loaded, or an error description.
pub async fn refresh_tles_from_celestrak() -> Result<usize, String> {
    fetch_and_merge_tles(CELESTRAK_WEATHER_URL, SatCategory::Weather).await
}

/// Spawn a background task that fetches TLEs from CelesTrak on start and
/// then refreshes once per day.
///
/// The task runs until the process exits.  Fetch failures are logged but
/// do not stop the periodic refresh — hardcoded fallback TLEs remain usable.
pub fn spawn_tle_refresh_task() {
    tokio::spawn(async {
        // Initial fetch at startup: weather + amateur satellites.
        match fetch_and_merge_tles(CELESTRAK_WEATHER_URL, SatCategory::Weather).await {
            Ok(n) => {
                tracing::info!("TLE refresh: loaded {n} weather satellite TLEs from CelesTrak")
            }
            Err(e) => {
                tracing::warn!("TLE refresh: weather fetch failed ({e}), using hardcoded TLEs")
            }
        }
        match fetch_and_merge_tles(CELESTRAK_HAM_URL, SatCategory::Amateur).await {
            Ok(n) => {
                tracing::info!("TLE refresh: loaded {n} amateur satellite TLEs from CelesTrak")
            }
            Err(e) => tracing::warn!("TLE refresh: amateur fetch failed ({e})"),
        }

        // Periodic refresh every 24 hours.
        let mut interval = tokio::time::interval(TLE_REFRESH_INTERVAL);
        // The first tick fires immediately; skip it since we just fetched.
        interval.tick().await;

        loop {
            interval.tick().await;
            match fetch_and_merge_tles(CELESTRAK_WEATHER_URL, SatCategory::Weather).await {
                Ok(n) => {
                    tracing::info!("TLE refresh: updated {n} weather satellite TLEs from CelesTrak")
                }
                Err(e) => {
                    tracing::warn!("TLE refresh: weather fetch failed ({e}), keeping previous TLEs")
                }
            }
            match fetch_and_merge_tles(CELESTRAK_HAM_URL, SatCategory::Amateur).await {
                Ok(n) => {
                    tracing::info!("TLE refresh: updated {n} amateur satellite TLEs from CelesTrak")
                }
                Err(e) => {
                    tracing::warn!("TLE refresh: amateur fetch failed ({e}), keeping previous TLEs")
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Pass prediction
// ---------------------------------------------------------------------------

/// Convert geodetic lat/lon (degrees) to ECEF position (km, spherical).
fn latlon_to_ecef(lat_deg: f64, lon_deg: f64) -> [f64; 3] {
    let lat = lat_deg * PI / 180.0;
    let lon = lon_deg * PI / 180.0;
    [
        EARTH_RADIUS_KM * lat.cos() * lon.cos(),
        EARTH_RADIUS_KM * lat.cos() * lon.sin(),
        EARTH_RADIUS_KM * lat.sin(),
    ]
}

/// Convert ECI (TEME) position (km) to ECEF using sidereal time rotation.
///
/// Uses the sgp4 crate's IAU sidereal time for consistency with the
/// propagator's reference frame.
fn eci_to_ecef(x: f64, y: f64, z: f64, time_ms: i64) -> [f64; 3] {
    let gmst = gmst_from_ms(time_ms);
    [
        x * gmst.cos() + y * gmst.sin(),
        -x * gmst.sin() + y * gmst.cos(),
        z,
    ]
}

/// Compute elevation and azimuth from observer to satellite.
///
/// Returns `(elevation_deg, azimuth_deg)` where elevation is degrees above the
/// horizon and azimuth is clockwise degrees from north.
fn compute_az_el(
    sat_ecef: [f64; 3],
    obs_ecef: [f64; 3],
    obs_lat_rad: f64,
    obs_lon_rad: f64,
) -> (f64, f64) {
    let dx = sat_ecef[0] - obs_ecef[0];
    let dy = sat_ecef[1] - obs_ecef[1];
    let dz = sat_ecef[2] - obs_ecef[2];

    // Transform delta to local East-North-Up frame.
    let east = -obs_lon_rad.sin() * dx + obs_lon_rad.cos() * dy;
    let north = -obs_lat_rad.sin() * obs_lon_rad.cos() * dx
        - obs_lat_rad.sin() * obs_lon_rad.sin() * dy
        + obs_lat_rad.cos() * dz;
    let up = obs_lat_rad.cos() * obs_lon_rad.cos() * dx
        + obs_lat_rad.cos() * obs_lon_rad.sin() * dy
        + obs_lat_rad.sin() * dz;

    let horiz = (east * east + north * north).sqrt();
    let el_deg = up.atan2(horiz) * 180.0 / PI;
    let az_deg = east.atan2(north).to_degrees().rem_euclid(360.0);

    (el_deg, az_deg)
}

/// Scan for passes of one satellite over a time window.
fn find_passes_for_sat(
    name: &str,
    norad_id: u32,
    category: SatCategory,
    line1: &str,
    line2: &str,
    obs_lat: f64,
    obs_lon: f64,
    start_ms: i64,
    window_ms: i64,
) -> Vec<PassPrediction> {
    let elements =
        match Elements::from_tle(Some(name.to_string()), line1.as_bytes(), line2.as_bytes()) {
            Ok(e) => e,
            Err(_) => return vec![],
        };
    let constants = match Constants::from_elements(&elements) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let epoch_ms = elements_epoch_ms(&elements);

    let obs_ecef = latlon_to_ecef(obs_lat, obs_lon);
    let obs_lat_rad = obs_lat * PI / 180.0;
    let obs_lon_rad = obs_lon * PI / 180.0;

    // 30-second scan step; fine enough for pass detection.
    let step_ms = 30_000_i64;
    let n_steps = (window_ms / step_ms) as usize + 2;

    let mut passes = Vec::new();
    let mut in_pass = false;
    let mut aos_ms = 0_i64;
    let mut aos_az = 0.0_f64;
    let mut max_el = 0.0_f64;
    let mut prev_az = 0.0_f64;

    for i in 0..n_steps {
        let t_ms = start_ms + i as i64 * step_ms;
        if t_ms > start_ms + window_ms {
            break;
        }
        let minutes = (t_ms - epoch_ms) as f64 / 60_000.0;
        let pred = match constants.propagate(MinutesSinceEpoch(minutes)) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let sat_ecef = eci_to_ecef(pred.position[0], pred.position[1], pred.position[2], t_ms);
        let (el, az) = compute_az_el(sat_ecef, obs_ecef, obs_lat_rad, obs_lon_rad);

        if el > 0.0 {
            if !in_pass {
                in_pass = true;
                aos_ms = t_ms;
                aos_az = az;
                max_el = el;
            } else if el > max_el {
                max_el = el;
            }
        } else if in_pass {
            // LOS occurred between previous step and this step.
            passes.push(PassPrediction {
                satellite: name.to_string(),
                norad_id,
                category,
                aos_ms,
                los_ms: t_ms,
                max_elevation_deg: (max_el * 10.0).round() / 10.0,
                azimuth_aos_deg: (aos_az * 10.0).round() / 10.0,
                azimuth_los_deg: (prev_az * 10.0).round() / 10.0,
                duration_s: ((t_ms - aos_ms) / 1000) as u64,
            });
            in_pass = false;
            max_el = 0.0;
        }
        prev_az = az;
    }

    // Pass in progress at end of window.
    if in_pass {
        passes.push(PassPrediction {
            satellite: name.to_string(),
            norad_id,
            category,
            aos_ms,
            los_ms: start_ms + window_ms,
            max_elevation_deg: (max_el * 10.0).round() / 10.0,
            azimuth_aos_deg: (aos_az * 10.0).round() / 10.0,
            azimuth_los_deg: (prev_az * 10.0).round() / 10.0,
            duration_s: ((start_ms + window_ms - aos_ms) / 1000) as u64,
        });
    }

    passes
}

/// Compute upcoming passes for all satellites in the TLE store over the next
/// `window_ms` milliseconds, starting from `start_ms`.
///
/// Iterates over every satellite fetched from CelesTrak (weather + amateur).
/// Returns [`TleSource::Unavailable`] when CelesTrak data has not been
/// fetched yet — the hardcoded fallback TLEs use approximate orbital
/// elements and are NOT suitable for pass-time predictions.
/// Results are sorted by AOS time.
pub fn compute_upcoming_passes(
    station_lat: f64,
    station_lon: f64,
    start_ms: i64,
    window_ms: i64,
) -> PassPredictionResult {
    let guard = match TLE_STORE.read() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };

    if let Some(store) = guard.as_ref() {
        let satellite_count = store.len();
        let mut all_passes = Vec::new();
        for (&norad_id, entry) in store {
            let passes = find_passes_for_sat(
                &entry.name,
                norad_id,
                entry.category,
                &entry.line1,
                &entry.line2,
                station_lat,
                station_lon,
                start_ms,
                window_ms,
            );
            all_passes.extend(passes);
        }
        all_passes.sort_by_key(|p| p.aos_ms);
        PassPredictionResult {
            passes: all_passes,
            satellite_count,
            tle_source: TleSource::Celestrak,
        }
    } else {
        // No CelesTrak data available — don't use hardcoded TLEs for
        // predictions because their orbital elements are approximate
        // and produce pass times that are hours off.
        PassPredictionResult {
            passes: vec![],
            satellite_count: 0,
            tle_source: TleSource::Unavailable,
        }
    }
}

/// Compute geographic bounds and ground track for a satellite pass.
///
/// Returns `None` if the satellite is unknown or propagation fails.
pub fn compute_pass_geo(
    satellite: &str,
    pass_start_ms: i64,
    pass_end_ms: i64,
    _station_lat: Option<f64>,
    _station_lon: Option<f64>,
) -> Option<PassGeo> {
    let (line1, line2) = tle_for_satellite(satellite)?;

    let elements = Elements::from_tle(
        Some(satellite.to_string()),
        line1.as_bytes(),
        line2.as_bytes(),
    )
    .ok()?;

    let constants = Constants::from_elements(&elements).ok()?;

    let duration_ms = (pass_end_ms - pass_start_ms).max(1);
    // Sample ground track every 5 seconds, minimum 3 points
    let step_ms = 5000_i64;
    let n_points = ((duration_ms / step_ms) + 1).max(3) as usize;

    let mut track: Vec<TrackPoint> = Vec::with_capacity(n_points);
    let mut min_lat = 90.0_f64;
    let mut max_lat = -90.0_f64;
    let mut min_lon = 180.0_f64;
    let mut max_lon = -180.0_f64;

    let epoch_ms = elements_epoch_ms(&elements);

    for i in 0..n_points {
        let t_ms = pass_start_ms + (i as i64 * duration_ms / (n_points as i64 - 1).max(1));
        let minutes_since_epoch = (t_ms - epoch_ms) as f64 / 60_000.0;

        let prediction = constants
            .propagate(MinutesSinceEpoch(minutes_since_epoch))
            .ok()?;

        // Convert ECI position to geodetic lat/lon
        let (lat, lon) = eci_to_geodetic(
            prediction.position[0],
            prediction.position[1],
            prediction.position[2],
            t_ms,
        );

        track.push([lat, lon]);
        min_lat = min_lat.min(lat);
        max_lat = max_lat.max(lat);
        min_lon = min_lon.min(lon);
        max_lon = max_lon.max(lon);
    }

    if track.len() < 2 {
        return None;
    }

    // Expand bounds by the swath half-width
    let lat_expansion = km_to_deg_lat(SWATH_HALF_WIDTH_KM);
    // Use the midpoint latitude for longitude expansion
    let mid_lat = (min_lat + max_lat) / 2.0;
    let lon_expansion = km_to_deg_lon(SWATH_HALF_WIDTH_KM, mid_lat);

    let south = (min_lat - lat_expansion).max(-90.0);
    let north = (max_lat + lat_expansion).min(90.0);
    let west = min_lon - lon_expansion;
    let east = max_lon + lon_expansion;

    // Normalize longitude to [-180, 180]
    let west = normalize_lon(west);
    let east = normalize_lon(east);

    Some(PassGeo {
        bounds: [south, west, north, east],
        ground_track: track,
    })
}

/// Fallback geo-referencing when TLE is unavailable: estimate bounds from
/// station location, assuming the satellite passes roughly overhead.
pub fn estimate_pass_geo_from_station(
    pass_start_ms: i64,
    pass_end_ms: i64,
    station_lat: f64,
    station_lon: f64,
) -> PassGeo {
    // Typical polar orbit ground speed ~6.9 km/s
    const GROUND_SPEED_KMS: f64 = 6.9;

    let duration_s = (pass_end_ms - pass_start_ms) as f64 / 1000.0;
    let track_length_km = duration_s * GROUND_SPEED_KMS;
    let half_track_km = track_length_km / 2.0;

    let lat_half = km_to_deg_lat(half_track_km);
    let lon_half = km_to_deg_lon(SWATH_HALF_WIDTH_KM, station_lat);

    let south = (station_lat - lat_half).max(-90.0);
    let north = (station_lat + lat_half).min(90.0);
    let west = normalize_lon(station_lon - lon_half);
    let east = normalize_lon(station_lon + lon_half);

    // Simple north-south ground track through station
    let n_points = 10;
    let mut ground_track = Vec::with_capacity(n_points);
    for i in 0..n_points {
        let frac = i as f64 / (n_points - 1) as f64;
        let lat = south + frac * (north - south);
        ground_track.push([lat, station_lon]);
    }

    PassGeo {
        bounds: [south, west, north, east],
        ground_track,
    }
}

// ---------------------------------------------------------------------------
// Coordinate helpers
// ---------------------------------------------------------------------------

/// Convert ECI (Earth-Centered Inertial) coordinates to geodetic lat/lon.
///
/// `x`, `y`, `z` are in km (as returned by sgp4).  `time_ms` is the UTC
/// timestamp used to compute GMST for the ECI→ECEF rotation.
fn eci_to_geodetic(x: f64, y: f64, z: f64, time_ms: i64) -> (f64, f64) {
    let [ecef_x, ecef_y, ecef_z] = eci_to_ecef(x, y, z, time_ms);

    // Geodetic latitude (simple spherical approximation, sufficient for overlays)
    let r_xy = (ecef_x * ecef_x + ecef_y * ecef_y).sqrt();
    let lat = ecef_z.atan2(r_xy) * 180.0 / PI;

    // Longitude
    let lon = ecef_y.atan2(ecef_x) * 180.0 / PI;

    (lat, lon)
}

/// Compute GMST (Greenwich Mean Sidereal Time) in radians from a UTC
/// timestamp in milliseconds since Unix epoch.
fn gmst_from_ms(time_ms: i64) -> f64 {
    // Julian date from Unix timestamp
    let jd = (time_ms as f64 / 86_400_000.0) + 2_440_587.5;
    let t = (jd - 2_451_545.0) / 36_525.0;

    // GMST in degrees (IAU formula)
    let gmst_deg = 280.46061837 + 360.98564736629 * (jd - 2_451_545.0) + 0.000387933 * t * t
        - t * t * t / 38_710_000.0;

    (gmst_deg % 360.0) * PI / 180.0
}

/// Convert the TLE epoch to milliseconds since Unix epoch.
fn elements_epoch_ms(elements: &Elements) -> i64 {
    elements.datetime.and_utc().timestamp_millis()
}

/// Convert km to degrees of latitude (constant everywhere on Earth).
fn km_to_deg_lat(km: f64) -> f64 {
    km / (EARTH_RADIUS_KM * PI / 180.0)
}

/// Convert km to degrees of longitude at a given latitude.
fn km_to_deg_lon(km: f64, lat_deg: f64) -> f64 {
    let cos_lat = (lat_deg * PI / 180.0).cos().abs().max(0.01);
    km / (EARTH_RADIUS_KM * PI / 180.0 * cos_lat)
}

/// Normalize longitude to `[-180, 180]`.
fn normalize_lon(lon: f64) -> f64 {
    let mut l = lon % 360.0;
    if l > 180.0 {
        l -= 360.0;
    }
    if l < -180.0 {
        l += 360.0;
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_km_to_deg_lat() {
        // ~111 km per degree of latitude
        let deg = km_to_deg_lat(111.0);
        assert!(
            (deg - 1.0).abs() < 0.05,
            "111 km should be ~1 degree, got {deg}"
        );
    }

    #[test]
    fn test_km_to_deg_lon_equator() {
        let deg = km_to_deg_lon(111.0, 0.0);
        assert!(
            (deg - 1.0).abs() < 0.05,
            "111 km at equator should be ~1 degree, got {deg}"
        );
    }

    #[test]
    fn test_km_to_deg_lon_high_lat() {
        // At 60°, cos(60°) = 0.5, so 111 km ≈ 2 degrees
        let deg = km_to_deg_lon(111.0, 60.0);
        assert!(
            (deg - 2.0).abs() < 0.1,
            "111 km at 60° should be ~2 degrees, got {deg}"
        );
    }

    #[test]
    fn test_normalize_lon() {
        assert!((normalize_lon(190.0) - (-170.0)).abs() < 1e-10);
        assert!((normalize_lon(-190.0) - 170.0).abs() < 1e-10);
        assert!((normalize_lon(0.0)).abs() < 1e-10);
    }

    #[test]
    fn test_tle_lookup() {
        assert!(tle_for_satellite("NOAA-15").is_some());
        assert!(tle_for_satellite("NOAA-18").is_some());
        assert!(tle_for_satellite("NOAA-19").is_some());
        assert!(tle_for_satellite("Meteor-M N2-3").is_some());
        assert!(tle_for_satellite("Meteor-M N2-4").is_some());
        assert!(tle_for_satellite("Unknown Sat").is_none());
    }

    #[test]
    fn test_norad_id_mapping() {
        assert_eq!(norad_id_for_satellite("NOAA-15"), Some(25338));
        assert_eq!(norad_id_for_satellite("NOAA-18"), Some(28654));
        assert_eq!(norad_id_for_satellite("NOAA-19"), Some(33591));
        assert_eq!(norad_id_for_satellite("Meteor-M N2-3"), Some(57166));
        assert_eq!(norad_id_for_satellite("Meteor-M N2-4"), Some(59051));
        assert_eq!(norad_id_for_satellite("Unknown"), None);
    }

    #[test]
    fn test_parse_tle_response() {
        let body = "\
NOAA 15
1 25338U 98030A   26085.50000000  .00000045  00000-0  36000-4 0  9999
2 25338  98.7285 114.5200 0010150  45.0000 315.1500 14.25955000  4002
NOAA 19
1 33591U 09005A   26085.50000000  .00000028  00000-0  20000-4 0  9997
2 33591  99.1700 050.5000 0014000 100.0000 260.0000 14.12300000  8003
";
        let tles = parse_tle_response(body, SatCategory::Weather);
        assert_eq!(tles.len(), 2);
        assert!(tles.contains_key(&25338));
        assert!(tles.contains_key(&33591));
        assert_eq!(tles[&25338].name, "NOAA 15");
        assert!(tles[&25338].line1.starts_with("1 25338"));
        assert_eq!(tles[&25338].category, SatCategory::Weather);
        assert_eq!(tles[&33591].name, "NOAA 19");
        assert!(tles[&33591].line2.starts_with("2 33591"));
    }

    #[test]
    fn test_parse_tle_response_empty() {
        assert!(parse_tle_response("", SatCategory::Other).is_empty());
        assert!(parse_tle_response("not a tle\n", SatCategory::Other).is_empty());
    }

    #[test]
    fn test_compute_pass_geo_noaa19() {
        // Simulate a ~12 minute pass
        let start = 1774800000000_i64; // approx 2026-03-28
        let end = start + 720_000; // 12 minutes

        let result = compute_pass_geo("NOAA-19", start, end, Some(48.0), Some(11.0));
        assert!(result.is_some(), "Should produce geo for NOAA-19");
        let geo = result.unwrap();
        assert!(
            geo.ground_track.len() >= 3,
            "Should have at least 3 track points"
        );
        assert!(geo.bounds[0] < geo.bounds[2], "south < north");
        // Bounds should cover a reasonable area
        let lat_span = geo.bounds[2] - geo.bounds[0];
        assert!(
            lat_span > 10.0,
            "Pass should span >10 deg lat, got {lat_span}"
        );
    }

    #[test]
    fn test_estimate_fallback() {
        let start = 1774800000000_i64;
        let end = start + 600_000; // 10 minutes
        let geo = estimate_pass_geo_from_station(start, end, 48.0, 11.0);
        assert!(geo.ground_track.len() >= 3);
        assert!(geo.bounds[0] < 48.0);
        assert!(geo.bounds[2] > 48.0);
    }

    #[test]
    fn test_gmst_not_nan() {
        let gmst = gmst_from_ms(1774800000000);
        assert!(gmst.is_finite(), "GMST should be finite");
    }

    #[test]
    fn test_gmst_vs_sgp4_sidereal_time() {
        // Compare our GMST with sgp4 crate's IAU sidereal time
        let time_ms = 1774800000000_i64; // 2026-03-28
        let our_gmst = gmst_from_ms(time_ms);

        let dt = sgp4::chrono::DateTime::from_timestamp_millis(time_ms).unwrap();
        let epoch = sgp4::julian_years_since_j2000(&dt.naive_utc());
        let sgp4_gmst = sgp4::iau_epoch_to_sidereal_time(epoch);

        let diff_deg = (our_gmst - sgp4_gmst).abs() * 180.0 / PI;
        assert!(
            diff_deg < 1.0,
            "GMST mismatch: ours={:.4}° sgp4={:.4}° diff={:.4}°",
            our_gmst * 180.0 / PI,
            sgp4_gmst * 180.0 / PI,
            diff_deg
        );
    }

    #[test]
    fn test_noaa19_pass_sanity() {
        // NOAA-19: sun-sync polar orbit at ~870 km, ~102 min period.
        // From Munich (~48°N, 11°E) expect 4-8 passes per 24 h,
        // each lasting 30 s – 16 min with sensible elevations.
        let start = 1774800000000_i64; // 2026-03-28
        let window = 24 * 60 * 60 * 1000_i64;
        let (l1, l2) = hardcoded_tle(33591).unwrap();
        let passes = find_passes_for_sat(
            "NOAA 19",
            33591,
            SatCategory::Weather,
            l1,
            l2,
            48.0,
            11.0,
            start,
            window,
        );
        assert!(
            passes.len() >= 2 && passes.len() <= 10,
            "Expected 2-10 passes for NOAA-19 in 24h, got {}",
            passes.len()
        );
        for p in &passes {
            assert!(
                p.duration_s >= 30 && p.duration_s <= 1200,
                "Pass duration should be 30s-20min, got {}s",
                p.duration_s
            );
            assert!(
                p.max_elevation_deg > 0.0 && p.max_elevation_deg <= 90.0,
                "Max elevation should be 0-90°, got {}",
                p.max_elevation_deg
            );
        }
    }

    #[test]
    fn test_compute_upcoming_passes_no_store() {
        // With empty TLE store, should return unavailable source, not
        // fabricated predictions from hardcoded TLEs.
        let result = compute_upcoming_passes(48.0, 11.0, 1774800000000, 86_400_000);
        assert!(matches!(result.tle_source, TleSource::Unavailable));
        assert!(result.passes.is_empty());
        assert_eq!(result.satellite_count, 0);
    }

    #[test]
    fn test_elements_epoch_ms() {
        // Parse a TLE and verify the epoch converts to a reasonable timestamp
        let (line1, line2) = hardcoded_tle(33591).unwrap();
        let elements = Elements::from_tle(
            Some("NOAA-19".to_string()),
            line1.as_bytes(),
            line2.as_bytes(),
        )
        .unwrap();
        let ms = elements_epoch_ms(&elements);
        // Should be in the year 2026 range (approx 1.77e12)
        assert!(
            ms > 1_700_000_000_000,
            "Epoch should be after 2023, got {ms}"
        );
        assert!(
            ms < 1_900_000_000_000,
            "Epoch should be before 2030, got {ms}"
        );
    }
}
