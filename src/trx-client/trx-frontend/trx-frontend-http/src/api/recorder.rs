// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! HTTP API endpoints for audio recording.

use std::sync::Arc;

use actix_web::http::header;
use actix_web::{delete, get, post, web, Error, HttpResponse};
use bytes::Bytes;
use tokio::sync::{mpsc, watch};

use trx_core::{RigCommand, RigState};
use trx_frontend::FrontendRuntimeContext;

use super::send_command;
use crate::server::recorder::RecorderManager;

// ============================================================================
// Query types
// ============================================================================

#[derive(serde::Deserialize)]
pub struct RecorderStartQuery {
    pub remote: Option<String>,
    pub vchan_id: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct RecorderStopQuery {
    pub remote: Option<String>,
    pub vchan_id: Option<String>,
}

// ============================================================================
// Endpoints
// ============================================================================

/// Start recording audio for the active rig (or a specific vchan).
#[post("/api/recorder/start")]
pub async fn recorder_start(
    query: web::Query<RecorderStartQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    recorder_mgr: web::Data<Arc<RecorderManager>>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<trx_core::RigRequest>>,
) -> Result<HttpResponse, Error> {
    let rig_id = resolve_rig_id(&context, query.remote.as_deref());
    let vchan_id = query.vchan_id.as_deref();

    // Resolve the audio broadcast sender for this rig/vchan.
    let (audio_tx, sample_rate, channels, frame_duration_ms) =
        resolve_audio_source(&context, &rig_id, vchan_id)?;

    let current_state = state.get_ref().borrow().clone();
    let freq_hz = Some(current_state.status.freq.hz);
    let mode = Some(trx_protocol::mode_to_string(&current_state.status.mode).into_owned());

    let params = crate::server::recorder::AudioParams {
        sample_rate,
        channels,
        frame_duration_ms,
    };

    match recorder_mgr.start(
        &rig_id,
        vchan_id,
        audio_tx,
        params,
        freq_hz,
        mode.as_deref(),
    ) {
        Ok(info) => {
            // Sync recorder_enabled state to the rig.
            let _ = send_command(
                &rig_tx,
                RigCommand::SetRecorderEnabled(true),
                query.remote.clone(),
            )
            .await;
            Ok(HttpResponse::Ok().json(info))
        }
        Err(e) => Ok(HttpResponse::BadRequest().json(serde_json::json!({ "error": e }))),
    }
}

/// Stop recording.
#[post("/api/recorder/stop")]
pub async fn recorder_stop(
    query: web::Query<RecorderStopQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    recorder_mgr: web::Data<Arc<RecorderManager>>,
    rig_tx: web::Data<mpsc::Sender<trx_core::RigRequest>>,
) -> Result<HttpResponse, Error> {
    let rig_id = resolve_rig_id(&context, query.remote.as_deref());
    let vchan_id = query.vchan_id.as_deref();

    match recorder_mgr.stop(&rig_id, vchan_id).await {
        Ok(result) => {
            // Check if any recordings remain active for this rig.
            let still_recording = recorder_mgr
                .list_active()
                .iter()
                .any(|r| r.rig_id == rig_id);
            if !still_recording {
                let _ = send_command(
                    &rig_tx,
                    RigCommand::SetRecorderEnabled(false),
                    query.remote.clone(),
                )
                .await;
            }
            Ok(HttpResponse::Ok().json(result))
        }
        Err(e) => Ok(HttpResponse::BadRequest().json(serde_json::json!({ "error": e }))),
    }
}

/// Get the status of active recordings.
#[get("/api/recorder/status")]
pub async fn recorder_status(
    recorder_mgr: web::Data<Arc<RecorderManager>>,
) -> Result<HttpResponse, Error> {
    let active = recorder_mgr.list_active();
    Ok(HttpResponse::Ok().json(active))
}

/// List recorded files in the output directory.
#[get("/api/recorder/files")]
pub async fn recorder_files(
    recorder_mgr: web::Data<Arc<RecorderManager>>,
) -> Result<HttpResponse, Error> {
    let files = recorder_mgr.list_files();
    Ok(HttpResponse::Ok().json(files))
}

/// Download a recorded file.
#[get("/api/recorder/download/{filename}")]
pub async fn recorder_download(
    path: web::Path<String>,
    recorder_mgr: web::Data<Arc<RecorderManager>>,
) -> Result<HttpResponse, Error> {
    let filename = path.into_inner();
    let file_path = recorder_mgr
        .file_path(&filename)
        .map_err(actix_web::error::ErrorNotFound)?;

    let data = tokio::fs::read(&file_path)
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("read error: {e}")))?;

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "audio/ogg"))
        .insert_header((
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        ))
        .body(Bytes::from(data)))
}

/// Delete a recorded file.
#[delete("/api/recorder/files/{filename}")]
pub async fn recorder_delete(
    path: web::Path<String>,
    recorder_mgr: web::Data<Arc<RecorderManager>>,
) -> Result<HttpResponse, Error> {
    let filename = path.into_inner();
    match recorder_mgr.delete_file(&filename) {
        Ok(()) => Ok(HttpResponse::Ok().json(serde_json::json!({ "deleted": filename }))),
        Err(e) => Ok(HttpResponse::BadRequest().json(serde_json::json!({ "error": e }))),
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn resolve_rig_id(context: &FrontendRuntimeContext, remote: Option<&str>) -> String {
    if let Some(r) = remote {
        return r.to_string();
    }
    context
        .routing
        .active_rig_id
        .lock()
        .ok()
        .and_then(|v| v.clone())
        .unwrap_or_else(|| "default".to_string())
}

fn resolve_audio_source(
    context: &FrontendRuntimeContext,
    rig_id: &str,
    vchan_id: Option<&str>,
) -> Result<(tokio::sync::broadcast::Sender<bytes::Bytes>, u32, u8, u16), Error> {
    if let Some(vchan_uuid_str) = vchan_id {
        // Virtual channel audio.
        let uuid: uuid::Uuid = vchan_uuid_str
            .parse()
            .map_err(|_| actix_web::error::ErrorBadRequest("invalid vchan_id UUID"))?;
        let audio = context
            .vchan
            .audio
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let tx = audio
            .get(&uuid)
            .cloned()
            .ok_or_else(|| actix_web::error::ErrorNotFound("vchan audio not found"))?;
        // Virtual channels use the same stream info as the main rig.
        let (sr, ch, fd) = stream_info_for_rig(context, rig_id);
        Ok((tx, sr, ch, fd))
    } else {
        // Main rig audio — try per-rig first, then default.
        let tx = context
            .rig_audio
            .rx
            .read()
            .ok()
            .and_then(|map| map.get(rig_id).cloned())
            .or_else(|| context.audio.rx.clone())
            .ok_or_else(|| actix_web::error::ErrorNotFound("no audio source for rig"))?;

        let (sr, ch, fd) = stream_info_for_rig(context, rig_id);
        Ok((tx, sr, ch, fd))
    }
}

fn stream_info_for_rig(context: &FrontendRuntimeContext, rig_id: &str) -> (u32, u8, u16) {
    // Try per-rig stream info first.
    if let Some(rx) = context.rig_audio_info_rx(rig_id) {
        if let Some(info) = rx.borrow().as_ref() {
            return (info.sample_rate, info.channels, info.frame_duration_ms);
        }
    }
    // Fall back to the default audio info.
    if let Some(ref info_rx) = context.audio.info {
        if let Some(info) = info_rx.borrow().as_ref() {
            return (info.sample_rate, info.channels, info.frame_duration_ms);
        }
    }
    // Absolute fallback.
    (48000, 2, 20)
}
