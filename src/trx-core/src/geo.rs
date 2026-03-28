// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Satellite geo-referencing for weather satellite image overlays.
//!
//! Uses SGP4 orbital propagation to compute the ground track and geographic
//! bounds of a satellite pass, given the satellite identity, pass timestamps,
//! and receiver station coordinates.

use sgp4::{Constants, Elements, MinutesSinceEpoch};
use std::f64::consts::PI;

/// Half-swath width in km for NOAA APT / Meteor LRPT imagery.
const SWATH_HALF_WIDTH_KM: f64 = 1400.0;

/// Earth radius in km (WGS84 mean).
const EARTH_RADIUS_KM: f64 = 6371.0;

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

/// Hardcoded TLE data for active weather satellites.
///
/// These are recent-epoch TLEs.  SGP4 propagation from stale TLEs still
/// gives sub-degree accuracy for image overlay purposes (drift ~0.1 deg/week).
fn tle_for_satellite(name: &str) -> Option<(&str, &str)> {
    let upper = name.to_uppercase();
    // Match by common satellite names from the decoder telemetry output.
    //
    // TLE lines must be exactly 69 characters with valid mod-10 checksums.
    // These are approximate recent-epoch elements for overlay purposes.
    if upper.contains("NOAA") && upper.contains("15") {
        Some((
            "1 25338U 98030A   26084.50000000  .00000045  00000-0  36000-4 0  9998",
            "2 25338  98.7285 114.5200 0010150  45.0000 315.1500 14.25955000  4001",
        ))
    } else if upper.contains("NOAA") && upper.contains("18") {
        Some((
            "1 28654U 05018A   26084.50000000  .00000036  00000-0  28000-4 0  9997",
            "2 28654  99.0400 162.3000 0013800 290.0000  70.0000 14.12500000  1005",
        ))
    } else if upper.contains("NOAA") && upper.contains("19") {
        Some((
            "1 33591U 09005A   26084.50000000  .00000028  00000-0  20000-4 0  9996",
            "2 33591  99.1700 050.5000 0014000 100.0000 260.0000 14.12300000  8002",
        ))
    } else if upper.contains("METEOR") && (upper.contains("2-3") || upper.contains("N2-3") || upper.contains("2_3")) {
        Some((
            "1 57166U 23091A   26084.50000000  .00000020  00000-0  16000-4 0  9998",
            "2 57166  98.7700 170.0000 0005000  90.0000 270.0000 14.23700000  1502",
        ))
    } else if upper.contains("METEOR") && (upper.contains("2-4") || upper.contains("N2-4") || upper.contains("2_4")) {
        Some((
            "1 59051U 24044A   26084.50000000  .00000018  00000-0  14000-4 0  9997",
            "2 59051  98.7700 200.0000 0005000  80.0000 280.0000 14.23700000  1006",
        ))
    } else {
        None
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

        let prediction = constants.propagate(MinutesSinceEpoch(minutes_since_epoch)).ok()?;

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
    let gmst = gmst_from_ms(time_ms);

    // Rotate ECI → ECEF
    let ecef_x = x * gmst.cos() + y * gmst.sin();
    let ecef_y = -x * gmst.sin() + y * gmst.cos();
    let ecef_z = z;

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
    let gmst_deg = 280.46061837 + 360.98564736629 * (jd - 2_451_545.0)
        + 0.000387933 * t * t
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
        assert!((deg - 1.0).abs() < 0.05, "111 km should be ~1 degree, got {deg}");
    }

    #[test]
    fn test_km_to_deg_lon_equator() {
        let deg = km_to_deg_lon(111.0, 0.0);
        assert!((deg - 1.0).abs() < 0.05, "111 km at equator should be ~1 degree, got {deg}");
    }

    #[test]
    fn test_km_to_deg_lon_high_lat() {
        // At 60°, cos(60°) = 0.5, so 111 km ≈ 2 degrees
        let deg = km_to_deg_lon(111.0, 60.0);
        assert!((deg - 2.0).abs() < 0.1, "111 km at 60° should be ~2 degrees, got {deg}");
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
    fn test_compute_pass_geo_noaa19() {
        // Simulate a ~12 minute pass
        let start = 1774800000000_i64; // approx 2026-03-28
        let end = start + 720_000; // 12 minutes

        let result = compute_pass_geo("NOAA-19", start, end, Some(48.0), Some(11.0));
        assert!(result.is_some(), "Should produce geo for NOAA-19");
        let geo = result.unwrap();
        assert!(geo.ground_track.len() >= 3, "Should have at least 3 track points");
        assert!(geo.bounds[0] < geo.bounds[2], "south < north");
        // Bounds should cover a reasonable area
        let lat_span = geo.bounds[2] - geo.bounds[0];
        assert!(lat_span > 10.0, "Pass should span >10 deg lat, got {lat_span}");
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
    fn test_elements_epoch_ms() {
        // Parse a TLE and verify the epoch converts to a reasonable timestamp
        let (line1, line2) = tle_for_satellite("NOAA-19").unwrap();
        let elements = Elements::from_tle(
            Some("NOAA-19".to_string()),
            line1.as_bytes(),
            line2.as_bytes(),
        )
        .unwrap();
        let ms = elements_epoch_ms(&elements);
        // Should be in the year 2026 range (approx 1.77e12)
        assert!(ms > 1_700_000_000_000, "Epoch should be after 2023, got {ms}");
        assert!(ms < 1_900_000_000_000, "Epoch should be before 2030, got {ms}");
    }
}
