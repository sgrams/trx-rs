// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Decoder toggle/clear endpoints and decode history.

use std::sync::Arc;

use actix_web::http::header;
use actix_web::Error;
use actix_web::{get, post, web, HttpResponse, Responder};
use bytes::Bytes;
use futures_util::stream::{select, StreamExt};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::{self, Duration};
use tokio_stream::wrappers::IntervalStream;

use trx_core::{RigCommand, RigRequest, RigState};
use trx_frontend::FrontendRuntimeContext;

use super::{gzip_bytes, send_command, RemoteQuery};

// ============================================================================
// Decoder registry
// ============================================================================

#[get("/decoders")]
pub async fn decoder_registry() -> impl Responder {
    HttpResponse::Ok().json(trx_protocol::DECODER_REGISTRY)
}

// ============================================================================
// Decode history types and helpers
// ============================================================================

#[derive(serde::Serialize)]
struct DecodeHistoryPayload {
    ais: Vec<trx_core::decode::AisMessage>,
    vdes: Vec<trx_core::decode::VdesMessage>,
    aprs: Vec<trx_core::decode::AprsPacket>,
    hf_aprs: Vec<trx_core::decode::AprsPacket>,
    cw: Vec<trx_core::decode::CwEvent>,
    ft8: Vec<trx_core::decode::Ft8Message>,
    ft4: Vec<trx_core::decode::Ft8Message>,
    ft2: Vec<trx_core::decode::Ft8Message>,
    wspr: Vec<trx_core::decode::WsprMessage>,
    wefax: Vec<trx_core::decode::WefaxMessage>,
}

impl DecodeHistoryPayload {
    fn total_messages(&self) -> usize {
        self.ais.len()
            + self.vdes.len()
            + self.aprs.len()
            + self.hf_aprs.len()
            + self.cw.len()
            + self.ft8.len()
            + self.ft4.len()
            + self.ft2.len()
            + self.wspr.len()
            + self.wefax.len()
    }
}

/// Build the grouped decode history payload from all per-decoder ring-buffers.
fn collect_decode_history(
    context: &FrontendRuntimeContext,
    rig_filter: Option<&str>,
) -> DecodeHistoryPayload {
    DecodeHistoryPayload {
        ais: crate::server::audio::snapshot_ais_history(context, rig_filter),
        vdes: crate::server::audio::snapshot_vdes_history(context, rig_filter),
        aprs: crate::server::audio::snapshot_aprs_history(context, rig_filter),
        hf_aprs: crate::server::audio::snapshot_hf_aprs_history(context, rig_filter),
        cw: crate::server::audio::snapshot_cw_history(context, rig_filter),
        ft8: crate::server::audio::snapshot_ft8_history(context, rig_filter),
        ft4: crate::server::audio::snapshot_ft4_history(context, rig_filter),
        ft2: crate::server::audio::snapshot_ft2_history(context, rig_filter),
        wspr: crate::server::audio::snapshot_wspr_history(context, rig_filter),
        wefax: crate::server::audio::snapshot_wefax_history(context, rig_filter),
    }
}

fn encode_cbor_length(out: &mut Vec<u8>, major: u8, value: u64) {
    debug_assert!(major <= 7);
    match value {
        0..=23 => out.push((major << 5) | (value as u8)),
        24..=0xff => {
            out.push((major << 5) | 24);
            out.push(value as u8);
        }
        0x100..=0xffff => {
            out.push((major << 5) | 25);
            out.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push((major << 5) | 26);
            out.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            out.push((major << 5) | 27);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn encode_cbor_json_value(out: &mut Vec<u8>, value: &serde_json::Value) {
    match value {
        serde_json::Value::Null => out.push(0xf6),
        serde_json::Value::Bool(false) => out.push(0xf4),
        serde_json::Value::Bool(true) => out.push(0xf5),
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_u64() {
                encode_cbor_length(out, 0, value);
            } else if let Some(value) = number.as_i64() {
                if value >= 0 {
                    encode_cbor_length(out, 0, value as u64);
                } else {
                    encode_cbor_length(out, 1, value.unsigned_abs() - 1);
                }
            } else if let Some(value) = number.as_f64() {
                out.push(0xfb);
                out.extend_from_slice(&value.to_be_bytes());
            } else {
                out.push(0xf6);
            }
        }
        serde_json::Value::String(text) => {
            encode_cbor_length(out, 3, text.len() as u64);
            out.extend_from_slice(text.as_bytes());
        }
        serde_json::Value::Array(items) => {
            encode_cbor_length(out, 4, items.len() as u64);
            for item in items {
                encode_cbor_json_value(out, item);
            }
        }
        serde_json::Value::Object(map) => {
            encode_cbor_length(out, 5, map.len() as u64);
            for (key, item) in map {
                encode_cbor_length(out, 3, key.len() as u64);
                out.extend_from_slice(key.as_bytes());
                encode_cbor_json_value(out, item);
            }
        }
    }
}

fn encode_decode_history_cbor(
    history: &DecodeHistoryPayload,
) -> Result<Vec<u8>, serde_json::Error> {
    let value = serde_json::to_value(history)?;
    let mut out = Vec::with_capacity(history.total_messages().saturating_mul(96));
    encode_cbor_json_value(&mut out, &value);
    Ok(out)
}

// ============================================================================
// Decode history endpoint
// ============================================================================

/// `GET /decode/history` — returns the full decode history as gzipped CBOR.
#[get("/decode/history")]
pub async fn decode_history(
    context: web::Data<Arc<FrontendRuntimeContext>>,
    query: web::Query<RemoteQuery>,
) -> impl Responder {
    if context.audio.decode_rx.is_none() {
        return HttpResponse::NotFound().body("decode not enabled");
    }
    let rig_filter = query.remote.as_deref().filter(|s| !s.is_empty());
    let history = collect_decode_history(context.get_ref(), rig_filter);
    let cbor = match encode_decode_history_cbor(&history) {
        Ok(cbor) => cbor,
        Err(err) => {
            tracing::error!("failed to encode decode history as CBOR: {err}");
            return HttpResponse::InternalServerError().finish();
        }
    };
    let payload = match gzip_bytes(&cbor) {
        Ok(payload) => payload,
        Err(err) => {
            tracing::error!("failed to gzip decode history payload: {err}");
            return HttpResponse::InternalServerError().finish();
        }
    };
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "application/cbor"))
        .insert_header((header::CONTENT_ENCODING, "gzip"))
        .body(payload)
}

// ============================================================================
// Decode SSE stream
// ============================================================================

#[get("/decode")]
pub async fn decode_events(
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    let Some(decode_rx) = crate::server::audio::subscribe_decode(context.get_ref()) else {
        tracing::warn!("/decode requested but decode channel not set (audio disabled?)");
        return Ok(HttpResponse::NotFound().body("decode not enabled"));
    };
    tracing::info!("/decode SSE client connected");

    let decode_stream = futures_util::stream::unfold(decode_rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if let Ok(json) = serde_json::to_string(&msg) {
                        return Some((
                            Ok::<Bytes, Error>(Bytes::from(format!("data: {json}\n\n"))),
                            rx,
                        ));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    let pings = IntervalStream::new(time::interval(Duration::from_secs(15)))
        .map(|_| Ok::<Bytes, Error>(Bytes::from(": ping\n\n")));

    let stream = select(pings, decode_stream);

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CONTENT_ENCODING, "identity"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::CONNECTION, "keep-alive"))
        .streaming(stream))
}

// ============================================================================
// Decoder toggle endpoints
// ============================================================================

#[post("/toggle_aprs_decode")]
pub async fn toggle_aprs_decode(
    query: web::Query<RemoteQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().decoders.aprs_decode_enabled;
    send_command(
        &rig_tx,
        RigCommand::SetAprsDecodeEnabled(!enabled),
        query.into_inner().remote,
    )
    .await
}

#[post("/toggle_hf_aprs_decode")]
pub async fn toggle_hf_aprs_decode(
    query: web::Query<RemoteQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().decoders.hf_aprs_decode_enabled;
    send_command(
        &rig_tx,
        RigCommand::SetHfAprsDecodeEnabled(!enabled),
        query.into_inner().remote,
    )
    .await
}

#[post("/toggle_cw_decode")]
pub async fn toggle_cw_decode(
    query: web::Query<RemoteQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().decoders.cw_decode_enabled;
    send_command(
        &rig_tx,
        RigCommand::SetCwDecodeEnabled(!enabled),
        query.into_inner().remote,
    )
    .await
}

#[derive(serde::Deserialize)]
pub struct CwAutoQuery {
    pub enabled: bool,
    pub remote: Option<String>,
}

#[post("/set_cw_auto")]
pub async fn set_cw_auto(
    query: web::Query<CwAutoQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetCwAuto(q.enabled), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct CwWpmQuery {
    pub wpm: u32,
    pub remote: Option<String>,
}

#[post("/set_cw_wpm")]
pub async fn set_cw_wpm(
    query: web::Query<CwWpmQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetCwWpm(q.wpm), q.remote).await
}

#[derive(serde::Deserialize)]
pub struct CwToneQuery {
    pub tone_hz: u32,
    pub remote: Option<String>,
}

#[post("/set_cw_tone")]
pub async fn set_cw_tone(
    query: web::Query<CwToneQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let q = query.into_inner();
    send_command(&rig_tx, RigCommand::SetCwToneHz(q.tone_hz), q.remote).await
}

#[post("/toggle_ft8_decode")]
pub async fn toggle_ft8_decode(
    query: web::Query<RemoteQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().decoders.ft8_decode_enabled;
    send_command(
        &rig_tx,
        RigCommand::SetFt8DecodeEnabled(!enabled),
        query.into_inner().remote,
    )
    .await
}

#[post("/toggle_ft4_decode")]
pub async fn toggle_ft4_decode(
    query: web::Query<RemoteQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().decoders.ft4_decode_enabled;
    send_command(
        &rig_tx,
        RigCommand::SetFt4DecodeEnabled(!enabled),
        query.into_inner().remote,
    )
    .await
}

#[post("/toggle_ft2_decode")]
pub async fn toggle_ft2_decode(
    query: web::Query<RemoteQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().decoders.ft2_decode_enabled;
    send_command(
        &rig_tx,
        RigCommand::SetFt2DecodeEnabled(!enabled),
        query.into_inner().remote,
    )
    .await
}

#[post("/toggle_wspr_decode")]
pub async fn toggle_wspr_decode(
    query: web::Query<RemoteQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().decoders.wspr_decode_enabled;
    send_command(
        &rig_tx,
        RigCommand::SetWsprDecodeEnabled(!enabled),
        query.into_inner().remote,
    )
    .await
}

#[post("/toggle_lrpt_decode")]
pub async fn toggle_lrpt_decode(
    query: web::Query<RemoteQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().decoders.lrpt_decode_enabled;
    send_command(
        &rig_tx,
        RigCommand::SetLrptDecodeEnabled(!enabled),
        query.into_inner().remote,
    )
    .await
}

#[post("/toggle_wefax_decode")]
pub async fn toggle_wefax_decode(
    query: web::Query<RemoteQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().decoders.wefax_decode_enabled;
    send_command(
        &rig_tx,
        RigCommand::SetWefaxDecodeEnabled(!enabled),
        query.into_inner().remote,
    )
    .await
}

// ============================================================================
// Decoder clear endpoints
// ============================================================================

#[post("/clear_wefax_decode")]
pub async fn clear_wefax_decode(
    query: web::Query<RemoteQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(
        &rig_tx,
        RigCommand::ResetWefaxDecoder,
        query.into_inner().remote,
    )
    .await
}

#[post("/clear_lrpt_decode")]
pub async fn clear_lrpt_decode(
    query: web::Query<RemoteQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(
        &rig_tx,
        RigCommand::ResetLrptDecoder,
        query.into_inner().remote,
    )
    .await
}

#[post("/clear_ft8_decode")]
pub async fn clear_ft8_decode(
    query: web::Query<RemoteQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_ft8_history(context.get_ref());
    send_command(
        &rig_tx,
        RigCommand::ResetFt8Decoder,
        query.into_inner().remote,
    )
    .await
}

#[post("/clear_ft4_decode")]
pub async fn clear_ft4_decode(
    query: web::Query<RemoteQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_ft4_history(context.get_ref());
    send_command(
        &rig_tx,
        RigCommand::ResetFt4Decoder,
        query.into_inner().remote,
    )
    .await
}

#[post("/clear_ft2_decode")]
pub async fn clear_ft2_decode(
    query: web::Query<RemoteQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_ft2_history(context.get_ref());
    send_command(
        &rig_tx,
        RigCommand::ResetFt2Decoder,
        query.into_inner().remote,
    )
    .await
}

#[post("/clear_wspr_decode")]
pub async fn clear_wspr_decode(
    query: web::Query<RemoteQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_wspr_history(context.get_ref());
    send_command(
        &rig_tx,
        RigCommand::ResetWsprDecoder,
        query.into_inner().remote,
    )
    .await
}

#[post("/clear_aprs_decode")]
pub async fn clear_aprs_decode(
    query: web::Query<RemoteQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_aprs_history(context.get_ref());
    send_command(
        &rig_tx,
        RigCommand::ResetAprsDecoder,
        query.into_inner().remote,
    )
    .await
}

#[post("/clear_hf_aprs_decode")]
pub async fn clear_hf_aprs_decode(
    query: web::Query<RemoteQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_hf_aprs_history(context.get_ref());
    send_command(
        &rig_tx,
        RigCommand::ResetHfAprsDecoder,
        query.into_inner().remote,
    )
    .await
}

#[post("/clear_ais_decode")]
pub async fn clear_ais_decode(
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_ais_history(context.get_ref());
    Ok(HttpResponse::Ok().finish())
}

#[post("/clear_vdes_decode")]
pub async fn clear_vdes_decode(
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_vdes_history(context.get_ref());
    Ok(HttpResponse::Ok().finish())
}

#[post("/clear_cw_decode")]
pub async fn clear_cw_decode(
    query: web::Query<RemoteQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_cw_history(context.get_ref());
    send_command(
        &rig_tx,
        RigCommand::ResetCwDecoder,
        query.into_inner().remote,
    )
    .await
}
