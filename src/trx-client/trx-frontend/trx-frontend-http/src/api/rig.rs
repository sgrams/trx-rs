// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Rig control endpoints: status, frequency, mode, PTT, SDR settings, etc.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use actix_web::{get, post, web, HttpResponse, Responder};
use actix_web::{http::header, Error};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use trx_core::radio::freq::Freq;
use trx_core::rig::state::WfmDenoiseLevel;
use trx_core::{RigCommand, RigRequest, RigState};
use trx_frontend::{FrontendRuntimeContext, RemoteRigEntry};
use trx_protocol::parse_mode;

use crate::server::vchan::ClientChannelManager;

use super::{
    active_rig_id_from_context, frontend_meta_from_context, send_command, wait_for_view,
    RemoteQuery, SessionRigManager, SnapshotWithMeta, StatusQuery,
};

// ============================================================================
// Status
// ============================================================================

#[get("/status")]
pub async fn status_api(
    query: web::Query<StatusQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    clients: web::Data<Arc<AtomicUsize>>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<impl Responder, Error> {
    let rx = query
        .remote
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|rid| context.rig_state_rx(rid))
        .unwrap_or_else(|| state.get_ref().clone());
    let snapshot = wait_for_view(rx).await?;
    let combined = SnapshotWithMeta {
        snapshot: &snapshot,
        meta: frontend_meta_from_context(
            clients.load(Ordering::Relaxed),
            context.get_ref().as_ref(),
            None,
        ),
    };
    let json =
        serde_json::to_string(&combined).map_err(actix_web::error::ErrorInternalServerError)?;
    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "application/json"))
        .body(json))
}

// ============================================================================
// Power / VFO / Lock
// ============================================================================

#[post("/toggle_power")]
pub async fn toggle_power(
    query: web::Query<RemoteQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let desired_on = !matches!(state.get_ref().borrow().control.enabled, Some(true));
    let cmd = if desired_on {
        RigCommand::PowerOn
    } else {
        RigCommand::PowerOff
    };
    send_command(&rig_tx, cmd, query.into_inner().remote).await
}

#[post("/toggle_vfo")]
pub async fn toggle_vfo(
    query: web::Query<RemoteQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::ToggleVfo, query.into_inner().remote).await
}

#[post("/lock")]
pub async fn lock_panel(
    query: web::Query<RemoteQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::Lock, query.into_inner().remote).await
}

#[post("/unlock")]
pub async fn unlock_panel(
    query: web::Query<RemoteQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::Unlock, query.into_inner().remote).await
}

// ============================================================================
// Frequency / Mode / PTT
// ============================================================================

#[derive(serde::Deserialize)]
pub struct FreqQuery {
    pub hz: u64,
    pub remote: Option<String>,
}

#[post("/set_freq")]
pub async fn set_freq(
    query: web::Query<FreqQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetFreq(Freq { hz: q.hz }), q.remote).await
}

#[post("/set_center_freq")]
pub async fn set_center_freq(
    query: web::Query<FreqQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(
        &rig_tx,
        RigCommand::SetCenterFreq(Freq { hz: q.hz }),
        q.remote,
    )
    .await
}

#[derive(serde::Deserialize)]
pub struct ModeQuery {
    pub mode: String,
    pub remote: Option<String>,
}

#[post("/set_mode")]
pub async fn set_mode(
    query: web::Query<ModeQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    let mode = parse_mode(&q.mode);
    send_command(&rig_tx, RigCommand::SetMode(mode), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct PttQuery {
    pub ptt: String,
    pub remote: Option<String>,
}

#[post("/set_ptt")]
pub async fn set_ptt(
    query: web::Query<PttQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    let ptt = match q.ptt.to_ascii_lowercase().as_str() {
        "1" | "true" | "on" => Ok(true),
        "0" | "false" | "off" => Ok(false),
        other => Err(actix_web::error::ErrorBadRequest(format!(
            "invalid ptt parameter: {other}"
        ))),
    }?;
    send_command(&rig_tx, RigCommand::SetPtt(ptt), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct TxLimitQuery {
    pub limit: u8,
    pub remote: Option<String>,
}

#[post("/set_tx_limit")]
pub async fn set_tx_limit(
    query: web::Query<TxLimitQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetTxLimit(q.limit), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct BandwidthQuery {
    pub hz: u32,
    pub remote: Option<String>,
}

#[post("/set_bandwidth")]
pub async fn set_bandwidth(
    query: web::Query<BandwidthQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetBandwidth(q.hz), q.remote).await
}

// ============================================================================
// SDR settings
// ============================================================================

#[derive(serde::Deserialize)]
pub struct SdrGainQuery {
    pub db: f64,
    pub remote: Option<String>,
}

#[post("/set_sdr_gain")]
pub async fn set_sdr_gain(
    query: web::Query<SdrGainQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetSdrGain(q.db), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct SdrLnaGainQuery {
    pub db: f64,
    pub remote: Option<String>,
}

#[post("/set_sdr_lna_gain")]
pub async fn set_sdr_lna_gain(
    query: web::Query<SdrLnaGainQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetSdrLnaGain(q.db), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct SdrAgcQuery {
    pub enabled: bool,
    pub remote: Option<String>,
}

#[post("/set_sdr_agc")]
pub async fn set_sdr_agc(
    query: web::Query<SdrAgcQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetSdrAgc(q.enabled), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct SdrSquelchQuery {
    pub enabled: bool,
    pub threshold_db: f64,
    pub remote: Option<String>,
}

#[post("/set_sdr_squelch")]
pub async fn set_sdr_squelch(
    query: web::Query<SdrSquelchQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(
        &rig_tx,
        RigCommand::SetSdrSquelch {
            enabled: q.enabled,
            threshold_db: q.threshold_db,
        },
        q.remote,
    )
    .await
}

#[derive(serde::Deserialize)]
pub struct SdrNoiseBlankerQuery {
    pub enabled: bool,
    pub threshold: f64,
    pub remote: Option<String>,
}

#[post("/set_sdr_noise_blanker")]
pub async fn set_sdr_noise_blanker(
    query: web::Query<SdrNoiseBlankerQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(
        &rig_tx,
        RigCommand::SetSdrNoiseBlanker {
            enabled: q.enabled,
            threshold: q.threshold,
        },
        q.remote,
    )
    .await
}

// ============================================================================
// WFM / SAM settings
// ============================================================================

#[derive(serde::Deserialize)]
pub struct WfmDeemphasisQuery {
    pub us: u32,
    pub remote: Option<String>,
}

#[post("/set_wfm_deemphasis")]
pub async fn set_wfm_deemphasis(
    query: web::Query<WfmDeemphasisQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetWfmDeemphasis(q.us), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct WfmStereoQuery {
    pub enabled: bool,
    pub remote: Option<String>,
}

#[post("/set_wfm_stereo")]
pub async fn set_wfm_stereo(
    query: web::Query<WfmStereoQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetWfmStereo(q.enabled), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct WfmDenoiseQuery {
    pub level: WfmDenoiseLevel,
    pub remote: Option<String>,
}

#[post("/set_wfm_denoise")]
pub async fn set_wfm_denoise(
    query: web::Query<WfmDenoiseQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetWfmDenoise(q.level), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct SamStereoWidthQuery {
    pub width: f32,
    pub remote: Option<String>,
}

#[post("/set_sam_stereo_width")]
pub async fn set_sam_stereo_width(
    query: web::Query<SamStereoWidthQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetSamStereoWidth(q.width), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct SamCarrierSyncQuery {
    pub enabled: bool,
    pub remote: Option<String>,
}

#[post("/set_sam_carrier_sync")]
pub async fn set_sam_carrier_sync(
    query: web::Query<SamCarrierSyncQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetSamCarrierSync(q.enabled), q.remote).await
}

// ============================================================================
// Rig list / selection
// ============================================================================

#[derive(serde::Serialize)]
struct RigListItem {
    remote: String,
    display_name: Option<String>,
    manufacturer: String,
    model: String,
    initialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    latitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    longitude: Option<f64>,
}

#[derive(serde::Serialize)]
struct RigListResponse {
    active_remote: Option<String>,
    rigs: Vec<RigListItem>,
}

fn build_rig_list_payload(context: &FrontendRuntimeContext) -> RigListResponse {
    let active_remote = active_rig_id_from_context(context);
    let rigs = context
        .routing
        .remote_rigs
        .lock()
        .ok()
        .map(|entries| entries.iter().map(map_rig_entry).collect())
        .unwrap_or_default();
    RigListResponse {
        active_remote,
        rigs,
    }
}

fn map_rig_entry(entry: &RemoteRigEntry) -> RigListItem {
    RigListItem {
        remote: entry.rig_id.clone(),
        display_name: entry.display_name.clone(),
        manufacturer: entry.state.info.manufacturer.clone(),
        model: entry.state.info.model.clone(),
        initialized: entry.state.initialized,
        latitude: entry.state.server_latitude,
        longitude: entry.state.server_longitude,
    }
}

#[get("/rigs")]
pub async fn list_rigs(
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    Ok(HttpResponse::Ok().json(build_rig_list_payload(context.get_ref().as_ref())))
}

#[derive(serde::Deserialize)]
pub struct SelectRigQuery {
    pub remote: String,
    pub session_id: Option<String>,
}

#[post("/select_rig")]
pub async fn select_rig(
    query: web::Query<SelectRigQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
    session_rig_mgr: web::Data<Arc<SessionRigManager>>,
) -> Result<HttpResponse, Error> {
    let remote = query.remote.trim();
    if remote.is_empty() {
        return Err(actix_web::error::ErrorBadRequest(
            "remote must not be empty",
        ));
    }

    let known = context
        .routing
        .remote_rigs
        .lock()
        .ok()
        .map(|entries| entries.iter().any(|entry| entry.rig_id == remote))
        .unwrap_or(false);
    if !known {
        return Err(actix_web::error::ErrorBadRequest(format!(
            "unknown remote: {remote}"
        )));
    }

    // Only update per-session rig selection — never mutate the global
    // active rig so that other tabs/sessions are unaffected.
    if let Some(ref sid) = query.session_id {
        if let Ok(uuid) = Uuid::parse_str(sid) {
            session_rig_mgr.set_rig(uuid, remote.to_string());
        }
    }

    // Broadcast the channel list for the newly selected rig so all SSE
    // clients receive the correct virtual channels immediately.
    let chans = vchan_mgr.channels(remote);
    if let Ok(json) = serde_json::to_string(&chans) {
        let _ = vchan_mgr.change_tx.send(format!("{remote}:{json}"));
    }

    Ok(HttpResponse::Ok().json(build_rig_list_payload(context.get_ref().as_ref())))
}

// ============================================================================
// Satellite passes
// ============================================================================

#[derive(serde::Serialize)]
struct SatPassesResponse {
    passes: Vec<trx_core::geo::PassPrediction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Number of satellites evaluated for predictions.
    satellite_count: usize,
    /// Source of the TLE data used: "celestrak" or "unavailable".
    tle_source: trx_core::geo::TleSource,
}

/// Return predicted passes for all known satellites over the next 24 h.
#[get("/sat_passes")]
pub async fn sat_passes(context: web::Data<Arc<FrontendRuntimeContext>>) -> impl Responder {
    let cached = context
        .routing
        .sat_passes
        .read()
        .ok()
        .and_then(|g| g.clone());
    match cached {
        Some(result) => {
            let error = match result.tle_source {
                trx_core::geo::TleSource::Unavailable => {
                    Some("TLE data not yet available — waiting for CelesTrak fetch".to_string())
                }
                trx_core::geo::TleSource::Celestrak => None,
            };
            web::Json(SatPassesResponse {
                passes: result.passes,
                error,
                satellite_count: result.satellite_count,
                tle_source: result.tle_source,
            })
        }
        None => web::Json(SatPassesResponse {
            passes: vec![],
            error: Some("Satellite predictions not yet available from server".to_string()),
            satellite_count: 0,
            tle_source: trx_core::geo::TleSource::Unavailable,
        }),
    }
}
