// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use actix_web::{get, post, web, HttpResponse, Responder};
use actix_web::{http::header, Error};
use bytes::Bytes;
use futures_util::stream::{once, select, StreamExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{self, Duration};
use tokio_stream::wrappers::{IntervalStream, WatchStream};

use trx_core::radio::freq::Freq;
use trx_core::rig::{RigAccessMethod, RigCapabilities, RigInfo};
use trx_core::{ClientResponse, RigCommand, RigMode, RigRequest, RigSnapshot, RigState};

use crate::server::status;

const FAVICON_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/trx-favicon.png"
));
const LOGO_BYTES: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/trx-logo.png"));

#[get("/status")]
pub async fn status_api(
    state: web::Data<watch::Receiver<RigState>>,
) -> Result<impl Responder, Error> {
    let state = wait_for_view(state.get_ref().clone()).await?;
    Ok(HttpResponse::Ok().json(state))
}

/// Inject `"clients": N` into a JSON object string.
fn inject_clients(json: &str, count: usize) -> String {
    // Fast path: insert after the opening '{'.
    if let Some(pos) = json.find('{') {
        let mut out = String::with_capacity(json.len() + 20);
        out.push_str(&json[..=pos]);
        out.push_str(&format!("\"clients\":{count},"));
        out.push_str(&json[pos + 1..]);
        out
    } else {
        json.to_string()
    }
}

#[get("/events")]
pub async fn events(
    state: web::Data<watch::Receiver<RigState>>,
    clients: web::Data<Arc<AtomicUsize>>,
) -> Result<HttpResponse, Error> {
    let rx = state.get_ref().clone();
    let initial = wait_for_view(rx.clone()).await?;

    let counter = clients.get_ref().clone();
    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;

    let initial_json =
        serde_json::to_string(&initial).map_err(actix_web::error::ErrorInternalServerError)?;
    let initial_json = inject_clients(&initial_json, count);
    let initial_stream =
        once(async move { Ok::<Bytes, Error>(Bytes::from(format!("data: {initial_json}\n\n"))) });

    let counter_updates = counter.clone();
    let updates = WatchStream::new(rx).filter_map(move |state| {
        let counter = counter_updates.clone();
        async move {
            state.snapshot().and_then(|v| {
                serde_json::to_string(&v).ok().map(|json| {
                    let json = inject_clients(&json, counter.load(Ordering::Relaxed));
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

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(index)
        .service(status_api)
        .service(events)
        .service(toggle_power)
        .service(toggle_vfo)
        .service(lock_panel)
        .service(unlock_panel)
        .service(set_freq)
        .service(set_mode)
        .service(set_ptt)
        .service(set_tx_limit)
        .service(crate::server::audio::audio_ws)
        .service(favicon)
        .service(logo)
        .service(style_css)
        .service(app_js)
        .service(aprs_js)
        .service(cw_js);
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
        .insert_header((header::CONTENT_TYPE, "application/javascript; charset=utf-8"))
        .body(status::APP_JS)
}

#[get("/aprs.js")]
async fn aprs_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "application/javascript; charset=utf-8"))
        .body(status::APRS_JS)
}

#[get("/cw.js")]
async fn cw_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "application/javascript; charset=utf-8"))
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

    let resp = tokio::time::timeout(Duration::from_secs(8), resp_rx)
        .await
        .map_err(|_| actix_web::error::ErrorGatewayTimeout("rig response timeout"))?;

    match resp {
        Ok(Ok(snapshot)) => Ok(HttpResponse::Ok().json(ClientResponse {
            success: true,
            state: Some(snapshot),
            error: None,
        })),
        Ok(Err(err)) => Ok(HttpResponse::BadRequest().json(ClientResponse {
            success: false,
            state: None,
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
    loop {
        match time::timeout_at(deadline, rx.changed()).await {
            Ok(Ok(())) => {
                if let Some(view) = rx.borrow().snapshot() {
                    return Ok(view);
                }
            }
            _ => break,
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
        server_latitude: state.server_latitude,
        server_longitude: state.server_longitude,
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
            },
            access: RigAccessMethod::Serial {
                path: "".into(),
                baud: 0,
            },
        }
    }
}

fn parse_mode(s: &str) -> RigMode {
    match s.to_ascii_uppercase().as_str() {
        "LSB" => RigMode::LSB,
        "USB" => RigMode::USB,
        "CW" => RigMode::CW,
        "CWR" => RigMode::CWR,
        "AM" => RigMode::AM,
        "FM" => RigMode::FM,
        "WFM" => RigMode::WFM,
        "DIG" | "DIGI" => RigMode::DIG,
        "PKT" | "PACKET" => RigMode::PKT,
        other => RigMode::Other(other.to_string()),
    }
}
