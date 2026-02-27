// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use actix_web::{get, post, web, HttpResponse, Responder};
use actix_web::{http::header, Error};
use bytes::Bytes;
use futures_util::stream::{once, select, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::{self, Duration};
use tokio_stream::wrappers::{IntervalStream, WatchStream};

use trx_core::radio::freq::Freq;
use trx_core::rig::{RigAccessMethod, RigCapabilities, RigInfo};
use trx_core::{RigCommand, RigRequest, RigSnapshot, RigState};
use trx_frontend::{FrontendRuntimeContext, RemoteRigEntry};
use trx_protocol::{parse_mode, ClientResponse};

use crate::server::status;

const FAVICON_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/trx-favicon.png"
));
const LOGO_BYTES: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/trx-logo.png"));
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[get("/status")]
pub async fn status_api(
    state: web::Data<watch::Receiver<RigState>>,
    clients: web::Data<Arc<AtomicUsize>>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<impl Responder, Error> {
    let state = wait_for_view(state.get_ref().clone()).await?;
    let json = serde_json::to_string(&state).map_err(actix_web::error::ErrorInternalServerError)?;
    let json = inject_frontend_meta(
        &json,
        clients.load(Ordering::Relaxed),
        context.rigctl_clients.load(Ordering::Relaxed),
        rigctl_addr_from_context(context.get_ref().as_ref()),
        active_rig_id_from_context(context.get_ref().as_ref()),
        rig_ids_from_context(context.get_ref().as_ref()),
        owner_callsign_from_context(context.get_ref().as_ref()),
    );
    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "application/json"))
        .body(json))
}

/// Inject `"clients": N` into a JSON object string.
fn inject_frontend_meta(
    json: &str,
    http_clients: usize,
    rigctl_clients: usize,
    rigctl_addr: Option<String>,
    active_rig_id: Option<String>,
    rig_ids: Vec<String>,
    owner_callsign: Option<String>,
) -> String {
    let mut value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return json.to_string(),
    };

    let Some(map) = value.as_object_mut() else {
        return json.to_string();
    };
    map.insert("clients".to_string(), serde_json::json!(http_clients));
    map.insert(
        "rigctl_clients".to_string(),
        serde_json::json!(rigctl_clients),
    );
    if let Some(addr) = rigctl_addr {
        map.insert("rigctl_addr".to_string(), serde_json::json!(addr));
    }
    if let Some(rig_id) = active_rig_id {
        map.insert("active_rig_id".to_string(), serde_json::json!(rig_id));
    }
    map.insert("rig_ids".to_string(), serde_json::json!(rig_ids));
    if let Some(owner) = owner_callsign {
        map.insert("owner_callsign".to_string(), serde_json::json!(owner));
    }

    serde_json::to_string(&value).unwrap_or_else(|_| json.to_string())
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
        .remote_active_rig_id
        .lock()
        .ok()
        .and_then(|v| v.clone())
}

fn rig_ids_from_context(context: &FrontendRuntimeContext) -> Vec<String> {
    context
        .remote_rigs
        .lock()
        .ok()
        .map(|entries| entries.iter().map(|r| r.rig_id.clone()).collect())
        .unwrap_or_default()
}

fn owner_callsign_from_context(context: &FrontendRuntimeContext) -> Option<String> {
    context.owner_callsign.clone()
}

#[get("/events")]
pub async fn events(
    state: web::Data<watch::Receiver<RigState>>,
    clients: web::Data<Arc<AtomicUsize>>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    let rx = state.get_ref().clone();
    let initial = wait_for_view(rx.clone()).await?;

    let counter = clients.get_ref().clone();
    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;

    let initial_json =
        serde_json::to_string(&initial).map_err(actix_web::error::ErrorInternalServerError)?;
    let initial_json = inject_frontend_meta(
        &initial_json,
        count,
        context.rigctl_clients.load(Ordering::Relaxed),
        rigctl_addr_from_context(context.get_ref().as_ref()),
        active_rig_id_from_context(context.get_ref().as_ref()),
        rig_ids_from_context(context.get_ref().as_ref()),
        owner_callsign_from_context(context.get_ref().as_ref()),
    );
    let initial_stream =
        once(async move { Ok::<Bytes, Error>(Bytes::from(format!("data: {initial_json}\n\n"))) });

    let counter_updates = counter.clone();
    let context_updates = context.get_ref().clone();
    let updates = WatchStream::new(rx).filter_map(move |state| {
        let counter = counter_updates.clone();
        let context = context_updates.clone();
        async move {
            state.snapshot().and_then(|v| {
                serde_json::to_string(&v).ok().map(|json| {
                    let json = inject_frontend_meta(
                        &json,
                        counter.load(Ordering::Relaxed),
                        context.rigctl_clients.load(Ordering::Relaxed),
                        rigctl_addr_from_context(context.as_ref()),
                        active_rig_id_from_context(context.as_ref()),
                        rig_ids_from_context(context.as_ref()),
                        owner_callsign_from_context(context.as_ref()),
                    );
                    Ok::<Bytes, Error>(Bytes::from(format!("data: {json}\n\n")))
                })
            })
        }
    });

    let pings = IntervalStream::new(time::interval(Duration::from_secs(5)))
        .map(|_| Ok::<Bytes, Error>(Bytes::from(": ping\n\n")));

    // Wrap stream to decrement counter on drop.
    let counter_drop = counter.clone();
    let stream = initial_stream.chain(select(pings, updates));
    let stream = DropStream::new(Box::pin(stream), move || {
        counter_drop.fetch_sub(1, Ordering::Relaxed);
    });

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::CONNECTION, "keep-alive"))
        .streaming(stream))
}

#[get("/decode")]
pub async fn decode_events(
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    let Some(decode_rx) = crate::server::audio::subscribe_decode(context.get_ref()) else {
        tracing::warn!("/decode requested but decode channel not set (audio disabled?)");
        return Ok(HttpResponse::NotFound().body("decode not enabled"));
    };
    tracing::info!("/decode SSE client connected");

    let history = {
        let mut out = Vec::new();
        out.extend(
            crate::server::audio::snapshot_aprs_history(context.get_ref())
                .into_iter()
                .map(trx_core::decode::DecodedMessage::Aprs),
        );
        out.extend(
            crate::server::audio::snapshot_cw_history(context.get_ref())
                .into_iter()
                .map(trx_core::decode::DecodedMessage::Cw),
        );
        out.extend(
            crate::server::audio::snapshot_ft8_history(context.get_ref())
                .into_iter()
                .map(trx_core::decode::DecodedMessage::Ft8),
        );
        out.extend(
            crate::server::audio::snapshot_wspr_history(context.get_ref())
                .into_iter()
                .map(trx_core::decode::DecodedMessage::Wspr),
        );
        out
    };

    let history_stream = futures_util::stream::iter(history.into_iter().filter_map(|msg| {
        serde_json::to_string(&msg)
            .ok()
            .map(|json| Ok::<Bytes, Error>(Bytes::from(format!("data: {json}\n\n"))))
    }));

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

    let stream = history_stream.chain(select(pings, decode_stream));

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::CONNECTION, "keep-alive"))
        .streaming(stream))
}

/// A stream wrapper that calls a callback when dropped.
struct DropStream<I> {
    inner: std::pin::Pin<Box<dyn futures_util::Stream<Item = I> + 'static>>,
    on_drop: Option<Box<dyn FnOnce() + Send>>,
}

impl<I> DropStream<I> {
    fn new<S, F>(inner: std::pin::Pin<Box<S>>, on_drop: F) -> Self
    where
        S: futures_util::Stream<Item = I> + 'static,
        F: FnOnce() + Send + 'static,
    {
        Self {
            inner,
            on_drop: Some(Box::new(on_drop)),
        }
    }
}

impl<I> Drop for DropStream<I> {
    fn drop(&mut self) {
        if let Some(f) = self.on_drop.take() {
            f();
        }
    }
}

impl<I> futures_util::Stream for DropStream<I> {
    type Item = I;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// SSE stream for spectrum data.
/// Emits JSON `SpectrumData` payloads when the latest frame changes.
/// Emits `null` when spectrum data becomes unavailable.
#[get("/spectrum")]
pub async fn spectrum(
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    let context_updates = context.get_ref().clone();
    let mut last_json: Option<String> = None;
    let updates =
        IntervalStream::new(time::interval(Duration::from_millis(200))).filter_map(move |_| {
            let context = context_updates.clone();
            std::future::ready({
                let next_json = context
                    .spectrum
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().and_then(|s| serde_json::to_string(s).ok()));

                let payload = match (last_json.as_ref(), next_json) {
                    (Some(prev), Some(next)) if prev == &next => None,
                    (_, Some(next)) => {
                        last_json = Some(next.clone());
                        Some(next)
                    }
                    (Some(_), None) => {
                        last_json = None;
                        Some("null".to_string())
                    }
                    (None, None) => None,
                };

                payload.map(|json| Ok::<Bytes, Error>(Bytes::from(format!("data: {json}\n\n"))))
            })
        });

    let pings = IntervalStream::new(time::interval(Duration::from_secs(15)))
        .map(|_| Ok::<Bytes, Error>(Bytes::from(": ping\n\n")));

    let stream = select(pings, updates);

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::CONNECTION, "keep-alive"))
        .streaming(stream))
}

#[post("/toggle_power")]
pub async fn toggle_power(
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let desired_on = !matches!(state.get_ref().borrow().control.enabled, Some(true));
    let cmd = if desired_on {
        RigCommand::PowerOn
    } else {
        RigCommand::PowerOff
    };
    send_command(&rig_tx, cmd).await
}

#[post("/toggle_vfo")]
pub async fn toggle_vfo(
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::ToggleVfo).await
}

#[post("/lock")]
pub async fn lock_panel(
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::Lock).await
}

#[post("/unlock")]
pub async fn unlock_panel(
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::Unlock).await
}

#[derive(serde::Deserialize)]
pub struct FreqQuery {
    pub hz: u64,
}

#[post("/set_freq")]
pub async fn set_freq(
    query: web::Query<FreqQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetFreq(Freq { hz: query.hz })).await
}

#[post("/set_center_freq")]
pub async fn set_center_freq(
    query: web::Query<FreqQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetCenterFreq(Freq { hz: query.hz })).await
}

#[derive(serde::Deserialize)]
pub struct ModeQuery {
    pub mode: String,
}

#[post("/set_mode")]
pub async fn set_mode(
    query: web::Query<ModeQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let mode = parse_mode(&query.mode);
    send_command(&rig_tx, RigCommand::SetMode(mode)).await
}

#[derive(serde::Deserialize)]
pub struct PttQuery {
    pub ptt: String,
}

#[post("/set_ptt")]
pub async fn set_ptt(
    query: web::Query<PttQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let ptt = match query.ptt.to_ascii_lowercase().as_str() {
        "1" | "true" | "on" => Ok(true),
        "0" | "false" | "off" => Ok(false),
        other => Err(actix_web::error::ErrorBadRequest(format!(
            "invalid ptt parameter: {other}"
        ))),
    }?;
    send_command(&rig_tx, RigCommand::SetPtt(ptt)).await
}

#[derive(serde::Deserialize)]
pub struct TxLimitQuery {
    pub limit: u8,
}

#[post("/set_tx_limit")]
pub async fn set_tx_limit(
    query: web::Query<TxLimitQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetTxLimit(query.limit)).await
}

#[derive(serde::Deserialize)]
pub struct BandwidthQuery {
    pub hz: u32,
}

#[post("/set_bandwidth")]
pub async fn set_bandwidth(
    query: web::Query<BandwidthQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetBandwidth(query.hz)).await
}

#[derive(serde::Deserialize)]
pub struct FirTapsQuery {
    pub taps: u32,
}

#[post("/set_fir_taps")]
pub async fn set_fir_taps(
    query: web::Query<FirTapsQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetFirTaps(query.taps)).await
}

#[post("/toggle_aprs_decode")]
pub async fn toggle_aprs_decode(
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().aprs_decode_enabled;
    send_command(&rig_tx, RigCommand::SetAprsDecodeEnabled(!enabled)).await
}

#[post("/toggle_cw_decode")]
pub async fn toggle_cw_decode(
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().cw_decode_enabled;
    send_command(&rig_tx, RigCommand::SetCwDecodeEnabled(!enabled)).await
}

#[derive(serde::Deserialize)]
pub struct CwAutoQuery {
    pub enabled: bool,
}

#[post("/set_cw_auto")]
pub async fn set_cw_auto(
    query: web::Query<CwAutoQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetCwAuto(query.enabled)).await
}

#[derive(serde::Deserialize)]
pub struct CwWpmQuery {
    pub wpm: u32,
}

#[post("/set_cw_wpm")]
pub async fn set_cw_wpm(
    query: web::Query<CwWpmQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetCwWpm(query.wpm)).await
}

#[derive(serde::Deserialize)]
pub struct CwToneQuery {
    pub tone_hz: u32,
}

#[post("/set_cw_tone")]
pub async fn set_cw_tone(
    query: web::Query<CwToneQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetCwToneHz(query.tone_hz)).await
}

#[post("/toggle_ft8_decode")]
pub async fn toggle_ft8_decode(
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().ft8_decode_enabled;
    send_command(&rig_tx, RigCommand::SetFt8DecodeEnabled(!enabled)).await
}

#[post("/toggle_wspr_decode")]
pub async fn toggle_wspr_decode(
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().wspr_decode_enabled;
    send_command(&rig_tx, RigCommand::SetWsprDecodeEnabled(!enabled)).await
}

#[post("/clear_ft8_decode")]
pub async fn clear_ft8_decode(
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_ft8_history(context.get_ref());
    send_command(&rig_tx, RigCommand::ResetFt8Decoder).await
}

#[post("/clear_wspr_decode")]
pub async fn clear_wspr_decode(
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_wspr_history(context.get_ref());
    send_command(&rig_tx, RigCommand::ResetWsprDecoder).await
}

#[post("/clear_aprs_decode")]
pub async fn clear_aprs_decode(
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_aprs_history(context.get_ref());
    send_command(&rig_tx, RigCommand::ResetAprsDecoder).await
}

#[post("/clear_cw_decode")]
pub async fn clear_cw_decode(
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_cw_history(context.get_ref());
    send_command(&rig_tx, RigCommand::ResetCwDecoder).await
}

#[derive(serde::Serialize)]
struct RigListItem {
    rig_id: String,
    display_name: Option<String>,
    manufacturer: String,
    model: String,
    initialized: bool,
}

#[derive(serde::Serialize)]
struct RigListResponse {
    active_rig_id: Option<String>,
    rigs: Vec<RigListItem>,
}

fn build_rig_list_payload(context: &FrontendRuntimeContext) -> RigListResponse {
    let active_rig_id = active_rig_id_from_context(context);
    let rigs = context
        .remote_rigs
        .lock()
        .ok()
        .map(|entries| entries.iter().map(map_rig_entry).collect())
        .unwrap_or_default();
    RigListResponse {
        active_rig_id,
        rigs,
    }
}

fn map_rig_entry(entry: &RemoteRigEntry) -> RigListItem {
    RigListItem {
        rig_id: entry.rig_id.clone(),
        display_name: entry.display_name.clone(),
        manufacturer: entry.state.info.manufacturer.clone(),
        model: entry.state.info.model.clone(),
        initialized: entry.state.initialized,
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
    pub rig_id: String,
}

#[post("/select_rig")]
pub async fn select_rig(
    query: web::Query<SelectRigQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    let rig_id = query.rig_id.trim();
    if rig_id.is_empty() {
        return Err(actix_web::error::ErrorBadRequest(
            "rig_id must not be empty",
        ));
    }

    let known = context
        .remote_rigs
        .lock()
        .ok()
        .map(|entries| entries.iter().any(|entry| entry.rig_id == rig_id))
        .unwrap_or(false);
    if !known {
        return Err(actix_web::error::ErrorBadRequest(format!(
            "unknown rig_id: {rig_id}"
        )));
    }

    if let Ok(mut active) = context.remote_active_rig_id.lock() {
        *active = Some(rig_id.to_string());
    }

    Ok(HttpResponse::Ok().json(build_rig_list_payload(context.get_ref().as_ref())))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(index)
        .service(status_api)
        .service(list_rigs)
        .service(events)
        .service(decode_events)
        .service(spectrum)
        .service(toggle_power)
        .service(toggle_vfo)
        .service(lock_panel)
        .service(unlock_panel)
        .service(set_freq)
        .service(set_center_freq)
        .service(set_mode)
        .service(set_ptt)
        .service(set_tx_limit)
        .service(set_bandwidth)
        .service(set_fir_taps)
        .service(toggle_aprs_decode)
        .service(toggle_cw_decode)
        .service(set_cw_auto)
        .service(set_cw_wpm)
        .service(set_cw_tone)
        .service(toggle_ft8_decode)
        .service(toggle_wspr_decode)
        .service(clear_aprs_decode)
        .service(clear_cw_decode)
        .service(clear_ft8_decode)
        .service(clear_wspr_decode)
        .service(select_rig)
        .service(crate::server::audio::audio_ws)
        .service(favicon)
        .service(logo)
        .service(style_css)
        .service(app_js)
        .service(aprs_js)
        .service(ft8_js)
        .service(wspr_js)
        .service(cw_js)
        // Auth endpoints
        .service(crate::server::auth::login)
        .service(crate::server::auth::logout)
        .service(crate::server::auth::session_status);
}

#[get("/")]
async fn index() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/html; charset=utf-8"))
        .body(status::index_html())
}

#[get("/favicon.ico")]
async fn favicon() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .body(FAVICON_BYTES)
}

#[get("/logo.png")]
async fn logo() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .body(LOGO_BYTES)
}

#[get("/style.css")]
async fn style_css() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/css; charset=utf-8"))
        .body(status::STYLE_CSS)
}

#[get("/app.js")]
async fn app_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ))
        .body(status::APP_JS)
}

#[get("/aprs.js")]
async fn aprs_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ))
        .body(status::APRS_JS)
}

#[get("/ft8.js")]
async fn ft8_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ))
        .body(status::FT8_JS)
}

#[get("/wspr.js")]
async fn wspr_js() -> impl Responder {
    HttpResponse::Ok()
        .content_type("application/javascript; charset=utf-8")
        .insert_header((header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"))
        .insert_header((header::PRAGMA, "no-cache"))
        .insert_header((header::EXPIRES, "0"))
        .body(status::WSPR_JS)
}

#[get("/cw.js")]
async fn cw_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ))
        .body(status::CW_JS)
}

async fn send_command(
    rig_tx: &mpsc::Sender<RigRequest>,
    cmd: RigCommand,
) -> Result<HttpResponse, Error> {
    let (resp_tx, resp_rx) = oneshot::channel();
    rig_tx
        .send(RigRequest {
            cmd,
            respond_to: resp_tx,
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
            state: Some(snapshot),
            rigs: None,
            error: None,
        })),
        Ok(Err(err)) => Ok(HttpResponse::BadRequest().json(ClientResponse {
            success: false,
            rig_id: None,
            state: None,
            rigs: None,
            error: Some(err.message),
        })),
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
    let deadline = time::Instant::now() + Duration::from_secs(5);
    while let Ok(Ok(())) = time::timeout_at(deadline, rx.changed()).await {
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
        aprs_decode_enabled: state.aprs_decode_enabled,
        cw_decode_enabled: state.cw_decode_enabled,
        cw_auto: state.cw_auto,
        cw_wpm: state.cw_wpm,
        cw_tone_hz: state.cw_tone_hz,
        ft8_decode_enabled: state.ft8_decode_enabled,
        wspr_decode_enabled: state.wspr_decode_enabled,
        filter: state.filter.clone(),
        spectrum: None,
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
