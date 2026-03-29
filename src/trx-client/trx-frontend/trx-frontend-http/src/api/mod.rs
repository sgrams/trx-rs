// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! HTTP API endpoints, split into logical submodules.

mod assets;
mod bookmarks;
mod decoder;
mod rig;
mod sse;
mod vchan;

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use actix_web::http::header;
use actix_web::{web, HttpRequest, HttpResponse};
use bytes::Bytes;
use flate2::write::GzEncoder;
use flate2::Compression;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Duration;
use uuid::Uuid;

use trx_core::rig::{RigAccessMethod, RigCapabilities, RigInfo};
use trx_core::{RigCommand, RigRequest, RigSnapshot, RigState};
use trx_frontend::FrontendRuntimeContext;
use trx_protocol::ClientResponse;

use crate::server::status;

// ============================================================================
// Constants
// ============================================================================

const FAVICON_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/trx-favicon.png"
));
const LOGO_BYTES: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/trx-logo.png"));
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

// ============================================================================
// Shared types
// ============================================================================

/// Generic query extractor for endpoints that only need the optional remote.
#[derive(serde::Deserialize)]
pub struct RemoteQuery {
    pub remote: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct StatusQuery {
    pub remote: Option<String>,
}

#[derive(serde::Serialize)]
struct FrontendMeta {
    #[serde(rename = "clients")]
    http_clients: usize,
    rigctl_clients: usize,
    audio_clients: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    rigctl_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_remote: Option<String>,
    remotes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_callsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_website_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_website_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ais_vessel_url_base: Option<String>,
    show_sdr_gain_control: bool,
    initial_map_zoom: u8,
    spectrum_coverage_margin_hz: u32,
    spectrum_usable_span_ratio: f32,
    decode_history_retention_min: u64,
    server_connected: bool,
}

/// Direct-serialize wrapper: flattens snapshot + meta in a single serde pass,
/// avoiding the intermediate `serde_json::Value` round-trip used by
/// `inject_frontend_meta`.  Used on the SSE hot path where state updates
/// arrive at high frequency.
#[derive(serde::Serialize)]
struct SnapshotWithMeta<'a> {
    #[serde(flatten)]
    snapshot: &'a RigSnapshot,
    #[serde(flatten)]
    meta: FrontendMeta,
}

/// Tracks per-SSE-session rig selection so different browser tabs can
/// independently view different rigs without interfering.
#[derive(Default)]
pub struct SessionRigManager {
    /// Maps SSE session UUID → selected rig_id.
    sessions: std::sync::RwLock<HashMap<Uuid, String>>,
}

impl SessionRigManager {
    pub fn register(&self, session_id: Uuid, rig_id: String) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.insert(session_id, rig_id);
        }
    }

    pub fn unregister(&self, session_id: Uuid) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.remove(&session_id);
        }
    }

    pub fn get_rig(&self, session_id: Uuid) -> Option<String> {
        self.sessions
            .read()
            .ok()
            .and_then(|sessions| sessions.get(&session_id).cloned())
    }

    pub fn set_rig(&self, session_id: Uuid, rig_id: String) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.insert(session_id, rig_id);
        }
    }
}

pub type SharedSessionRigManager = Arc<SessionRigManager>;

// ============================================================================
// Shared helper functions
// ============================================================================

fn frontend_meta_from_context(
    http_clients: usize,
    context: &FrontendRuntimeContext,
    rig_id: Option<&str>,
) -> FrontendMeta {
    // Use per-rig connection state when available so that only the rig whose
    // server dropped appears disconnected, leaving other rigs unaffected.
    let server_connected = rig_id
        .and_then(|rid| {
            context
                .routing
                .rig_server_connected
                .read()
                .ok()
                .and_then(|m| m.get(rid).copied())
        })
        .unwrap_or_else(|| context.routing.server_connected.load(Ordering::Relaxed));
    FrontendMeta {
        http_clients,
        rigctl_clients: context.rigctl_clients.load(Ordering::Relaxed),
        audio_clients: context.audio.clients.load(Ordering::Relaxed),
        rigctl_addr: rigctl_addr_from_context(context),
        active_remote: active_rig_id_from_context(context),
        remotes: rig_ids_from_context(context),
        owner_callsign: owner_callsign_from_context(context),
        owner_website_url: owner_website_url_from_context(context),
        owner_website_name: owner_website_name_from_context(context),
        ais_vessel_url_base: ais_vessel_url_base_from_context(context),
        show_sdr_gain_control: show_sdr_gain_control_from_context(context),
        initial_map_zoom: initial_map_zoom_from_context(context),
        spectrum_coverage_margin_hz: spectrum_coverage_margin_hz_from_context(context),
        spectrum_usable_span_ratio: spectrum_usable_span_ratio_from_context(context),
        decode_history_retention_min: decode_history_retention_min_from_context(context),
        server_connected,
    }
}

fn rigctl_addr_from_context(context: &FrontendRuntimeContext) -> Option<String> {
    context
        .rigctl_listen_addr
        .lock()
        .ok()
        .and_then(|v| *v)
        .map(|addr| addr.to_string())
}

fn active_rig_id_from_context(context: &FrontendRuntimeContext) -> Option<String> {
    context
        .routing
        .active_rig_id
        .lock()
        .ok()
        .and_then(|v| v.clone())
}

fn rig_ids_from_context(context: &FrontendRuntimeContext) -> Vec<String> {
    context
        .routing
        .remote_rigs
        .lock()
        .ok()
        .map(|entries| entries.iter().map(|r| r.rig_id.clone()).collect())
        .unwrap_or_default()
}

fn owner_callsign_from_context(context: &FrontendRuntimeContext) -> Option<String> {
    context.owner.callsign.clone()
}

fn owner_website_url_from_context(context: &FrontendRuntimeContext) -> Option<String> {
    context.owner.website_url.clone()
}

fn owner_website_name_from_context(context: &FrontendRuntimeContext) -> Option<String> {
    context.owner.website_name.clone()
}

fn ais_vessel_url_base_from_context(context: &FrontendRuntimeContext) -> Option<String> {
    context.owner.ais_vessel_url_base.clone()
}

fn show_sdr_gain_control_from_context(context: &FrontendRuntimeContext) -> bool {
    context.http_ui.show_sdr_gain_control
}

fn initial_map_zoom_from_context(context: &FrontendRuntimeContext) -> u8 {
    context.http_ui.initial_map_zoom
}

fn spectrum_coverage_margin_hz_from_context(context: &FrontendRuntimeContext) -> u32 {
    context.http_ui.spectrum_coverage_margin_hz
}

fn spectrum_usable_span_ratio_from_context(context: &FrontendRuntimeContext) -> f32 {
    context.http_ui.spectrum_usable_span_ratio
}

fn decode_history_retention_min_from_context(context: &FrontendRuntimeContext) -> u64 {
    let default_minutes = context.http_ui.decode_history_retention_min.max(1);
    let Some(active_rig_id) = context
        .routing
        .active_rig_id
        .lock()
        .ok()
        .and_then(|v| v.clone())
    else {
        return default_minutes;
    };
    context
        .http_ui
        .decode_history_retention_min_by_rig
        .get(&active_rig_id)
        .copied()
        .filter(|minutes| *minutes > 0)
        .unwrap_or(default_minutes)
}

/// Base64-encode `data` using the standard alphabet (no line wrapping).
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize]);
        out.push(T[((n >> 12) & 63) as usize]);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize]
        } else {
            b'='
        });
    }
    String::from_utf8(out).expect("base64 output is always valid ASCII")
}

fn request_accepts_html(req: &HttpRequest) -> bool {
    req.headers()
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/html"))
        .unwrap_or(false)
}

fn no_cache_response<B>(content_type: &'static str, body: B) -> HttpResponse
where
    B: actix_web::body::MessageBody + 'static,
{
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, content_type))
        .insert_header((header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"))
        .insert_header((header::PRAGMA, "no-cache"))
        .insert_header((header::EXPIRES, "0"))
        .body(body)
}

/// Pre-compressed (gzip) + ETag-aware response for immutable embedded assets.
fn static_asset_response(
    req: &HttpRequest,
    content_type: &'static str,
    gz_bytes: &[u8],
    etag: &str,
) -> HttpResponse {
    // Check If-None-Match for conditional GET.
    if let Some(inm) = req.headers().get(header::IF_NONE_MATCH) {
        if let Ok(val) = inm.to_str() {
            if val == etag || val == "*" {
                return HttpResponse::NotModified()
                    .insert_header((header::ETAG, etag.to_owned()))
                    .insert_header((
                        header::CACHE_CONTROL,
                        "public, max-age=86400, must-revalidate",
                    ))
                    .finish();
            }
        }
    }
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, content_type))
        .insert_header((header::CONTENT_ENCODING, "gzip"))
        .insert_header((header::ETAG, etag.to_owned()))
        .insert_header((
            header::CACHE_CONTROL,
            "public, max-age=86400, must-revalidate",
        ))
        .body(Bytes::copy_from_slice(gz_bytes))
}

/// Cache entry for a pre-compressed asset: gzip bytes + ETag string.
struct GzCacheEntry {
    gz: Vec<u8>,
    etag: String,
}

/// Compress `src` with gzip and build an ETag from the build version + asset name.
fn gz_cache_entry(src: &[u8], name: &str) -> GzCacheEntry {
    let mut encoder = GzEncoder::new(Vec::with_capacity(src.len() / 2), Compression::best());
    encoder.write_all(src).expect("gzip compress");
    let gz = encoder.finish().expect("gzip finish");
    let etag = format!("\"{}:{}\"", status::build_version_tag(), name);
    GzCacheEntry { gz, etag }
}

fn require_control(
    req: &HttpRequest,
    auth_state: &crate::server::auth::AuthState,
) -> Result<(), actix_web::Error> {
    if !auth_state.config.enabled {
        return Ok(());
    }
    match crate::server::auth::get_session_role(req, auth_state) {
        Some(crate::server::auth::AuthRole::Control) => Ok(()),
        _ => Err(actix_web::error::ErrorForbidden("control role required")),
    }
}

fn gzip_bytes(payload: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(payload)?;
    encoder.finish()
}

async fn send_command(
    rig_tx: &mpsc::Sender<RigRequest>,
    cmd: RigCommand,
    remote: Option<String>,
) -> Result<HttpResponse, actix_web::Error> {
    let (resp_tx, resp_rx) = oneshot::channel();
    rig_tx
        .send(RigRequest {
            cmd,
            respond_to: resp_tx,
            rig_id_override: remote,
        })
        .await
        .map_err(|e| {
            actix_web::error::ErrorInternalServerError(format!("failed to send to rig: {e:?}"))
        })?;

    let resp = tokio::time::timeout(REQUEST_TIMEOUT, resp_rx)
        .await
        .map_err(|_| actix_web::error::ErrorGatewayTimeout("rig response timeout"))?;

    match resp {
        Ok(Ok(snapshot)) => Ok(HttpResponse::Ok().json(ClientResponse {
            success: true,
            rig_id: None,
            protocol_version: None,
            state: Some(snapshot),
            rigs: None,
            sat_passes: None,
            error: None,
        })),
        Ok(Err(err)) => Ok(HttpResponse::BadRequest().json(ClientResponse {
            success: false,
            rig_id: None,
            protocol_version: None,
            state: None,
            rigs: None,
            sat_passes: None,
            error: Some(err.message),
        })),
        Err(e) => Err(actix_web::error::ErrorInternalServerError(format!(
            "rig response channel error: {e:?}"
        ))),
    }
}

async fn send_command_to_rig(
    rig_tx: &mpsc::Sender<RigRequest>,
    remote: &str,
    cmd: RigCommand,
) -> Result<(), actix_web::Error> {
    let (resp_tx, resp_rx) = oneshot::channel();
    rig_tx
        .send(RigRequest {
            cmd,
            respond_to: resp_tx,
            rig_id_override: Some(remote.to_string()),
        })
        .await
        .map_err(|e| {
            actix_web::error::ErrorInternalServerError(format!("failed to send to rig: {e:?}"))
        })?;

    let resp = tokio::time::timeout(REQUEST_TIMEOUT, resp_rx)
        .await
        .map_err(|_| actix_web::error::ErrorGatewayTimeout("rig response timeout"))?;

    match resp {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(actix_web::error::ErrorBadRequest(err.message)),
        Err(e) => Err(actix_web::error::ErrorInternalServerError(format!(
            "rig response channel error: {e:?}"
        ))),
    }
}

async fn wait_for_view(mut rx: watch::Receiver<RigState>) -> Result<RigSnapshot, actix_web::Error> {
    if let Some(view) = rx.borrow().snapshot() {
        return Ok(view);
    }

    // Wait up to 5 seconds for a valid snapshot; fall back to a placeholder
    // so the SSE stream starts immediately and the browser isn't left hanging.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while let Ok(Ok(())) = tokio::time::timeout_at(deadline, rx.changed()).await {
        if let Some(view) = rx.borrow().snapshot() {
            return Ok(view);
        }
    }

    // Fallback: build a minimal snapshot if rig info is missing.
    let state = rx.borrow().clone();
    Ok(RigSnapshot {
        info: state
            .rig_info
            .clone()
            .unwrap_or_else(|| RigInfoPlaceholder.into()),
        status: state.status,
        band: None,
        enabled: state.control.enabled,
        initialized: state.initialized,
        server_callsign: state.server_callsign,
        server_version: state.server_version,
        server_build_date: state.server_build_date,
        server_latitude: state.server_latitude,
        server_longitude: state.server_longitude,
        pskreporter_status: state.pskreporter_status,
        aprs_is_status: state.aprs_is_status,
        decoders: state.decoders.clone(),
        cw_auto: state.cw_auto,
        cw_wpm: state.cw_wpm,
        cw_tone_hz: state.cw_tone_hz,
        filter: state.filter.clone(),
        spectrum: None,
        vchan_rds: None,
    })
}

struct RigInfoPlaceholder;

impl Default for RigInfoPlaceholder {
    fn default() -> Self {
        RigInfoPlaceholder
    }
}

impl From<RigInfoPlaceholder> for RigInfo {
    fn from(_: RigInfoPlaceholder) -> Self {
        RigInfo {
            manufacturer: "Unknown".to_string(),
            model: "Rig".to_string(),
            revision: "".to_string(),
            capabilities: RigCapabilities {
                min_freq_step_hz: 1,
                supported_bands: vec![],
                supported_modes: vec![],
                num_vfos: 0,
                lock: false,
                lockable: false,
                attenuator: false,
                preamp: false,
                rit: false,
                rpt: false,
                split: false,
                tx: false,
                tx_limit: false,
                vfo_switch: false,
                filter_controls: false,
                signal_meter: false,
            },
            access: RigAccessMethod::Serial {
                path: "".into(),
                baud: 0,
            },
        }
    }
}

// ============================================================================
// Route configuration
// ============================================================================

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(rig::status_api)
        .service(rig::list_rigs)
        .service(rig::select_rig)
        .service(rig::toggle_power)
        .service(rig::toggle_vfo)
        .service(rig::lock_panel)
        .service(rig::unlock_panel)
        .service(rig::set_freq)
        .service(rig::set_center_freq)
        .service(rig::set_mode)
        .service(rig::set_ptt)
        .service(rig::set_tx_limit)
        .service(rig::set_bandwidth)
        .service(rig::set_sdr_gain)
        .service(rig::set_sdr_lna_gain)
        .service(rig::set_sdr_agc)
        .service(rig::set_sdr_squelch)
        .service(rig::set_sdr_noise_blanker)
        .service(rig::set_wfm_deemphasis)
        .service(rig::set_wfm_stereo)
        .service(rig::set_wfm_denoise)
        .service(rig::set_sam_stereo_width)
        .service(rig::set_sam_carrier_sync)
        .service(rig::sat_passes)
        // SSE streams
        .service(sse::events)
        .service(sse::spectrum)
        // Decoder endpoints
        .service(decoder::decode_history)
        .service(decoder::decode_events)
        .service(decoder::toggle_aprs_decode)
        .service(decoder::toggle_hf_aprs_decode)
        .service(decoder::toggle_cw_decode)
        .service(decoder::set_cw_auto)
        .service(decoder::set_cw_wpm)
        .service(decoder::set_cw_tone)
        .service(decoder::toggle_ft8_decode)
        .service(decoder::toggle_ft4_decode)
        .service(decoder::toggle_ft2_decode)
        .service(decoder::toggle_wspr_decode)
        .service(decoder::toggle_lrpt_decode)
        .service(decoder::clear_ais_decode)
        .service(decoder::clear_vdes_decode)
        .service(decoder::clear_aprs_decode)
        .service(decoder::clear_hf_aprs_decode)
        .service(decoder::clear_cw_decode)
        .service(decoder::clear_ft8_decode)
        .service(decoder::clear_ft4_decode)
        .service(decoder::clear_ft2_decode)
        .service(decoder::clear_wspr_decode)
        .service(decoder::clear_lrpt_decode)
        // Bookmark CRUD
        .service(bookmarks::list_bookmarks)
        .service(bookmarks::create_bookmark)
        .service(bookmarks::update_bookmark)
        .service(bookmarks::delete_bookmark)
        .service(bookmarks::batch_delete_bookmarks)
        .service(bookmarks::batch_move_bookmarks)
        // Scheduler
        .service(crate::server::scheduler::get_scheduler)
        .service(crate::server::scheduler::put_scheduler)
        .service(crate::server::scheduler::delete_scheduler)
        .service(crate::server::scheduler::get_scheduler_status)
        .service(crate::server::scheduler::put_scheduler_activate_entry)
        .service(crate::server::scheduler::get_scheduler_control)
        .service(crate::server::scheduler::put_scheduler_control)
        .service(crate::server::background_decode::get_background_decode)
        .service(crate::server::background_decode::put_background_decode)
        .service(crate::server::background_decode::delete_background_decode)
        .service(crate::server::background_decode::get_background_decode_status)
        .service(crate::server::audio::audio_ws)
        // Static assets
        .service(assets::index)
        .service(assets::map_index)
        .service(assets::digital_modes_index)
        .service(assets::settings_index)
        .service(assets::about_index)
        .service(assets::favicon)
        .service(assets::favicon_png)
        .service(assets::logo)
        .service(assets::style_css)
        .service(assets::app_js)
        .service(assets::decode_history_worker_js)
        .service(assets::webgl_renderer_js)
        .service(assets::leaflet_ais_tracksymbol_js)
        .service(assets::ais_js)
        .service(assets::vdes_js)
        .service(assets::aprs_js)
        .service(assets::hf_aprs_js)
        .service(assets::ft8_js)
        .service(assets::ft4_js)
        .service(assets::ft2_js)
        .service(assets::wspr_js)
        .service(assets::cw_js)
        .service(assets::sat_js)
        .service(assets::bookmarks_js)
        .service(assets::scheduler_js)
        .service(assets::sat_scheduler_js)
        .service(assets::background_decode_js)
        .service(assets::vchan_js)
        .service(assets::bandplan_json)
        // Virtual channels
        .service(vchan::list_channels)
        .service(vchan::allocate_channel)
        .service(vchan::delete_channel_route)
        .service(vchan::subscribe_channel)
        .service(vchan::set_vchan_freq)
        .service(vchan::set_vchan_bw)
        .service(vchan::set_vchan_mode)
        // Auth endpoints
        .service(crate::server::auth::login)
        .service(crate::server::auth::logout)
        .service(crate::server::auth::session_status);
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test as actix_test;
    use actix_web::{web, App};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::Arc;
    use tokio::sync::{mpsc, watch};
    use trx_core::rig::state::{DecoderConfig, DecoderResetSeqs};
    use trx_core::rig::{RigAccessMethod, RigCapabilities, RigControl, RigInfo};
    use trx_core::{RigCommand, RigError, RigMode, RigRequest, RigState};

    /// Build a minimal `RigState` with rig_info populated so that
    /// `snapshot()` returns `Some`.
    fn make_rig_state() -> RigState {
        RigState {
            rig_info: Some(RigInfo {
                manufacturer: "Test".to_string(),
                model: "TestRig".to_string(),
                revision: "1.0".to_string(),
                capabilities: RigCapabilities {
                    min_freq_step_hz: 1,
                    supported_bands: vec![],
                    supported_modes: vec![RigMode::USB],
                    num_vfos: 1,
                    lock: false,
                    lockable: false,
                    attenuator: false,
                    preamp: false,
                    rit: false,
                    rpt: false,
                    split: false,
                    tx: false,
                    tx_limit: false,
                    vfo_switch: false,
                    filter_controls: false,
                    signal_meter: false,
                },
                access: RigAccessMethod::Serial {
                    path: "/dev/null".into(),
                    baud: 9600,
                },
            }),
            status: trx_core::rig::RigStatus::default(),
            initialized: true,
            control: RigControl::default(),
            server_callsign: Some("TEST0CALL".to_string()),
            server_version: None,
            server_build_date: None,
            server_latitude: None,
            server_longitude: None,
            pskreporter_status: None,
            aprs_is_status: None,
            decoders: DecoderConfig::default(),
            cw_auto: false,
            cw_wpm: 20,
            cw_tone_hz: 700,
            filter: None,
            spectrum: None,
            vchan_rds: None,
            reset_seqs: DecoderResetSeqs::default(),
        }
    }

    /// Build a minimal `FrontendRuntimeContext` with sensible defaults.
    fn make_context() -> Arc<trx_frontend::FrontendRuntimeContext> {
        Arc::new(trx_frontend::FrontendRuntimeContext {
            audio: trx_frontend::AudioContext::default(),
            decode_history: trx_frontend::DecodeHistoryContext::default(),
            http_auth: trx_frontend::HttpAuthConfig::default(),
            http_ui: trx_frontend::HttpUiConfig::default(),
            routing: trx_frontend::RigRoutingContext::default(),
            owner: trx_frontend::OwnerInfo {
                callsign: Some("TEST0CALL".to_string()),
                website_url: None,
                website_name: None,
                ais_vessel_url_base: None,
            },
            vchan: trx_frontend::VChanContext::default(),
            spectrum: trx_frontend::SpectrumContext::default(),
            rig_audio: trx_frontend::PerRigAudioContext::default(),
            sse_clients: Arc::new(AtomicUsize::new(0)),
            rigctl_clients: Arc::new(AtomicUsize::new(0)),
            rigctl_listen_addr: Arc::new(std::sync::Mutex::new(None)),
            decode_collector_started: AtomicBool::new(false),
        })
    }

    /// Spawn a background task that receives `RigRequest`s and responds with
    /// a snapshot built from the given `RigState`.
    fn spawn_rig_responder(mut rx: mpsc::Receiver<RigRequest>, state: RigState) {
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let snapshot = state.snapshot().unwrap();
                let _ = req.respond_to.send(Ok(snapshot));
            }
        });
    }

    // ======================================================================
    // Pure function tests
    // ======================================================================

    #[test]
    fn test_base64_encode_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn test_base64_encode_standard_vectors() {
        // RFC 4648 test vectors
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn test_session_rig_manager_register_and_get() {
        let mgr = SessionRigManager::default();
        let id = uuid::Uuid::new_v4();
        mgr.register(id, "rig-1".to_string());
        assert_eq!(mgr.get_rig(id), Some("rig-1".to_string()));
    }

    #[test]
    fn test_session_rig_manager_set_overrides() {
        let mgr = SessionRigManager::default();
        let id = uuid::Uuid::new_v4();
        mgr.register(id, "rig-1".to_string());
        mgr.set_rig(id, "rig-2".to_string());
        assert_eq!(mgr.get_rig(id), Some("rig-2".to_string()));
    }

    #[test]
    fn test_session_rig_manager_unregister() {
        let mgr = SessionRigManager::default();
        let id = uuid::Uuid::new_v4();
        mgr.register(id, "rig-1".to_string());
        mgr.unregister(id);
        assert_eq!(mgr.get_rig(id), None);
    }

    #[test]
    fn test_session_rig_manager_unknown_session() {
        let mgr = SessionRigManager::default();
        let id = uuid::Uuid::new_v4();
        assert_eq!(mgr.get_rig(id), None);
    }

    // ======================================================================
    // Endpoint tests using actix_web::test
    // ======================================================================

    /// GET /status returns 200 with valid JSON containing rig snapshot fields.
    #[actix_web::test]
    async fn test_status_endpoint_returns_json() {
        let state = make_rig_state();
        let (state_tx, state_rx) = watch::channel(state);
        let context = make_context();
        let clients = Arc::new(AtomicUsize::new(0));

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(state_rx))
                .app_data(web::Data::new(clients))
                .app_data(web::Data::new(context))
                .service(rig::status_api),
        )
        .await;

        let req = actix_test::TestRequest::get().uri("/status").to_request();
        let resp = actix_test::call_service(&app, req).await;

        assert_eq!(resp.status(), 200);
        let body = actix_test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Verify key fields from the snapshot are present
        assert!(json.get("info").is_some(), "response should contain 'info'");
        assert!(
            json.get("status").is_some(),
            "response should contain 'status'"
        );
        assert_eq!(json["info"]["manufacturer"], "Test");
        assert_eq!(json["info"]["model"], "TestRig");

        // Verify frontend meta fields are injected
        assert!(
            json.get("server_connected").is_some(),
            "response should contain frontend meta"
        );

        drop(state_tx);
    }

    /// POST /set_freq with a valid frequency returns 200 and a success response.
    #[actix_web::test]
    async fn test_set_freq_valid() {
        let state = make_rig_state();
        let (rig_tx, rig_rx) = mpsc::channel::<RigRequest>(16);
        spawn_rig_responder(rig_rx, state);

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(rig_tx))
                .service(rig::set_freq),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/set_freq?hz=14074000")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;

        assert_eq!(resp.status(), 200);
        let body = actix_test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);
        assert!(json.get("state").is_some(), "response should include state");
    }

    /// POST /set_freq?hz=0 is accepted (the handler does not validate
    /// frequency ranges; that is delegated to the backend).
    #[actix_web::test]
    async fn test_set_freq_zero_hz() {
        let state = make_rig_state();
        let (rig_tx, rig_rx) = mpsc::channel::<RigRequest>(16);
        spawn_rig_responder(rig_rx, state);

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(rig_tx))
                .service(rig::set_freq),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/set_freq?hz=0")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    /// POST /set_freq without the required `hz` query parameter returns 400.
    #[actix_web::test]
    async fn test_set_freq_missing_hz_returns_400() {
        let (rig_tx, _rig_rx) = mpsc::channel::<RigRequest>(16);

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(rig_tx))
                .service(rig::set_freq),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/set_freq")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400);
    }

    /// POST /set_freq?hz=notanumber returns 400 (deserialization failure).
    #[actix_web::test]
    async fn test_set_freq_invalid_hz_returns_400() {
        let (rig_tx, _rig_rx) = mpsc::channel::<RigRequest>(16);

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(rig_tx))
                .service(rig::set_freq),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/set_freq?hz=notanumber")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400);
    }

    /// POST /set_freq returns an error when the rig backend rejects the command.
    #[actix_web::test]
    async fn test_set_freq_backend_error() {
        let (rig_tx, mut rig_rx) = mpsc::channel::<RigRequest>(16);

        // Spawn a responder that always returns an error
        tokio::spawn(async move {
            while let Some(req) = rig_rx.recv().await {
                let _ = req
                    .respond_to
                    .send(Err(RigError::transient("frequency out of range")));
            }
        });

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(rig_tx))
                .service(rig::set_freq),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/set_freq?hz=99999999999")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400);
        let body = actix_test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], false);
        assert!(
            json["error"]
                .as_str()
                .unwrap()
                .contains("frequency out of range"),
            "error message should describe the failure"
        );
    }

    /// POST /toggle_ft8_decode sends the correct command and returns success.
    #[actix_web::test]
    async fn test_toggle_ft8_decode() {
        let state = make_rig_state();
        let (state_tx, state_rx) = watch::channel(state.clone());
        let (rig_tx, mut rig_rx) = mpsc::channel::<RigRequest>(16);

        // Verify the command sent is SetFt8DecodeEnabled with the toggled value
        tokio::spawn(async move {
            if let Some(req) = rig_rx.recv().await {
                match &req.cmd {
                    RigCommand::SetFt8DecodeEnabled(enabled) => {
                        // ft8_decode_enabled defaults to false, so toggle should send true
                        assert!(*enabled, "should toggle from false to true");
                    }
                    other => panic!("unexpected command: {:?}", other),
                }
                let snapshot = state.snapshot().unwrap();
                let _ = req.respond_to.send(Ok(snapshot));
            }
        });

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(state_rx))
                .app_data(web::Data::new(rig_tx))
                .service(decoder::toggle_ft8_decode),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/toggle_ft8_decode")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body = actix_test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);

        drop(state_tx);
    }

    /// POST /set_mode with a valid mode returns 200.
    #[actix_web::test]
    async fn test_set_mode_valid() {
        let state = make_rig_state();
        let (rig_tx, rig_rx) = mpsc::channel::<RigRequest>(16);
        spawn_rig_responder(rig_rx, state);

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(rig_tx))
                .service(rig::set_mode),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/set_mode?mode=LSB")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body = actix_test::read_body(resp).await;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);
    }

    /// POST /set_ptt with valid values returns 200.
    #[actix_web::test]
    async fn test_set_ptt_on_off() {
        let state = make_rig_state();
        let (rig_tx, rig_rx) = mpsc::channel::<RigRequest>(16);
        spawn_rig_responder(rig_rx, state);

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(rig_tx))
                .service(rig::set_ptt),
        )
        .await;

        // Test PTT on
        let req = actix_test::TestRequest::post()
            .uri("/set_ptt?ptt=true")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        // Test PTT off
        let req = actix_test::TestRequest::post()
            .uri("/set_ptt?ptt=0")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    /// POST /set_ptt with an invalid value returns 400.
    #[actix_web::test]
    async fn test_set_ptt_invalid_value() {
        let (rig_tx, _rig_rx) = mpsc::channel::<RigRequest>(16);

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(rig_tx))
                .service(rig::set_ptt),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/set_ptt?ptt=maybe")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400);
    }

    /// POST /toggle_vfo sends the ToggleVfo command and returns success.
    #[actix_web::test]
    async fn test_toggle_vfo() {
        let state = make_rig_state();
        let (rig_tx, rig_rx) = mpsc::channel::<RigRequest>(16);
        spawn_rig_responder(rig_rx, state);

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(rig_tx))
                .service(rig::toggle_vfo),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/toggle_vfo")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }

    /// Verify that send_command returns 500 when the rig channel is closed.
    #[actix_web::test]
    async fn test_set_freq_channel_closed() {
        let (rig_tx, rig_rx) = mpsc::channel::<RigRequest>(16);
        drop(rig_rx); // Close the receiver

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(rig_tx))
                .service(rig::set_freq),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/set_freq?hz=7074000")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 500);
    }
}
