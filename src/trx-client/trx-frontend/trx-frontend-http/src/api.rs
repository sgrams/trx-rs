// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use actix_web::{delete, get, post, put, web, HttpRequest, HttpResponse, Responder};
use actix_web::{http::header, Error};
use bytes::Bytes;
use futures_util::stream::{once, select, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::{self, Duration};
use tokio_stream::wrappers::{IntervalStream, WatchStream};

use trx_core::radio::freq::Freq;
use trx_core::rig::state::WfmDenoiseLevel;
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
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] } else { b'=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] } else { b'=' });
    }
    // SAFETY: output contains only ASCII base64 characters.
    unsafe { String::from_utf8_unchecked(out) }
}

/// Encode spectrum bins as a compact base64 string of i8 values (1 dB/step).
///
/// Wire format for the `b` SSE event:
///   `{center_hz},{sample_rate},{base64_i8_bins}`
///
/// RDS is intentionally excluded — it changes rarely and is sent via the
/// `/events` state stream instead.
fn encode_spectrum_frame(frame: &trx_core::rig::state::SpectrumData) -> String {
    let bytes: Vec<u8> = frame
        .bins
        .iter()
        .map(|&v| v.round().clamp(-128.0, 127.0) as i8 as u8)
        .collect();
    let b64 = base64_encode(&bytes);
    format!("{},{},{b64}", frame.center_hz, frame.sample_rate)
}

struct FrontendMeta {
    http_clients: usize,
    rigctl_clients: usize,
    rigctl_addr: Option<String>,
    active_rig_id: Option<String>,
    rig_ids: Vec<String>,
    owner_callsign: Option<String>,
    owner_website_url: Option<String>,
    owner_website_name: Option<String>,
    ais_vessel_url_base: Option<String>,
    show_sdr_gain_control: bool,
    initial_map_zoom: u8,
    spectrum_coverage_margin_hz: u32,
    spectrum_usable_span_ratio: f32,
}

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
        frontend_meta_from_context(clients.load(Ordering::Relaxed), context.get_ref().as_ref()),
    );
    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "application/json"))
        .body(json))
}

/// Append frontend meta fields to an already-serialised JSON object string.
///
/// Avoids a full parse→modify→reserialise cycle (two serde round-trips per
/// event) by working directly at the string level: strip the closing `}`,
/// serialize only the extra fields once, and re-close the object.
fn inject_frontend_meta(json: &str, meta: FrontendMeta) -> String {
    let trimmed = json.trim_end();
    let Some(base) = trimmed.strip_suffix('}') else {
        return json.to_string();
    };

    // Build only the extra key-value pairs as a JSON fragment.
    let mut extra = serde_json::Map::new();
    extra.insert("clients".into(), serde_json::json!(meta.http_clients));
    extra.insert("rigctl_clients".into(), serde_json::json!(meta.rigctl_clients));
    if let Some(v) = meta.rigctl_addr { extra.insert("rigctl_addr".into(), serde_json::json!(v)); }
    if let Some(v) = meta.active_rig_id { extra.insert("active_rig_id".into(), serde_json::json!(v)); }
    extra.insert("rig_ids".into(), serde_json::json!(meta.rig_ids));
    if let Some(v) = meta.owner_callsign { extra.insert("owner_callsign".into(), serde_json::json!(v)); }
    if let Some(v) = meta.owner_website_url { extra.insert("owner_website_url".into(), serde_json::json!(v)); }
    if let Some(v) = meta.owner_website_name { extra.insert("owner_website_name".into(), serde_json::json!(v)); }
    if let Some(v) = meta.ais_vessel_url_base { extra.insert("ais_vessel_url_base".into(), serde_json::json!(v)); }
    extra.insert("show_sdr_gain_control".into(), serde_json::json!(meta.show_sdr_gain_control));
    extra.insert("initial_map_zoom".into(), serde_json::json!(meta.initial_map_zoom));
    extra.insert("spectrum_coverage_margin_hz".into(), serde_json::json!(meta.spectrum_coverage_margin_hz));
    extra.insert("spectrum_usable_span_ratio".into(), serde_json::json!(meta.spectrum_usable_span_ratio));

    // Serialize the extra map, strip its outer braces, and splice in.
    let extra_json = match serde_json::to_string(&extra) {
        Ok(s) => s,
        Err(_) => return json.to_string(),
    };
    // extra_json = {"k":v,...}  →  strip { and }
    let inner = &extra_json[1..extra_json.len() - 1];
    format!("{base},{inner}}}")
}

fn frontend_meta_from_context(
    http_clients: usize,
    context: &FrontendRuntimeContext,
) -> FrontendMeta {
    FrontendMeta {
        http_clients,
        rigctl_clients: context.rigctl_clients.load(Ordering::Relaxed),
        rigctl_addr: rigctl_addr_from_context(context),
        active_rig_id: active_rig_id_from_context(context),
        rig_ids: rig_ids_from_context(context),
        owner_callsign: owner_callsign_from_context(context),
        owner_website_url: owner_website_url_from_context(context),
        owner_website_name: owner_website_name_from_context(context),
        ais_vessel_url_base: ais_vessel_url_base_from_context(context),
        show_sdr_gain_control: show_sdr_gain_control_from_context(context),
        initial_map_zoom: initial_map_zoom_from_context(context),
        spectrum_coverage_margin_hz: spectrum_coverage_margin_hz_from_context(context),
        spectrum_usable_span_ratio: spectrum_usable_span_ratio_from_context(context),
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

fn owner_website_url_from_context(context: &FrontendRuntimeContext) -> Option<String> {
    context.owner_website_url.clone()
}

fn owner_website_name_from_context(context: &FrontendRuntimeContext) -> Option<String> {
    context.owner_website_name.clone()
}

fn ais_vessel_url_base_from_context(context: &FrontendRuntimeContext) -> Option<String> {
    context.ais_vessel_url_base.clone()
}

fn show_sdr_gain_control_from_context(context: &FrontendRuntimeContext) -> bool {
    context.http_show_sdr_gain_control
}

fn initial_map_zoom_from_context(context: &FrontendRuntimeContext) -> u8 {
    context.http_initial_map_zoom
}

fn spectrum_coverage_margin_hz_from_context(context: &FrontendRuntimeContext) -> u32 {
    context.http_spectrum_coverage_margin_hz
}

fn spectrum_usable_span_ratio_from_context(context: &FrontendRuntimeContext) -> f32 {
    context.http_spectrum_usable_span_ratio
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
        frontend_meta_from_context(count, context.get_ref().as_ref()),
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
                        frontend_meta_from_context(
                            counter.load(Ordering::Relaxed),
                            context.as_ref(),
                        ),
                    );
                    Ok::<Bytes, Error>(Bytes::from(format!("data: {json}\n\n")))
                })
            })
        }
    });

    // Send a named "ping" event so the JS heartbeat can observe it (SSE
    // comments like ": ping" are not exposed by EventSource.onmessage).
    let pings = IntervalStream::new(time::interval(Duration::from_secs(5)))
        .map(|_| Ok::<Bytes, Error>(Bytes::from("event: ping\ndata: \n\n")));

    // Wrap stream to decrement counter on drop.
    let counter_drop = counter.clone();
    let stream = initial_stream.chain(select(pings, updates));
    let stream = DropStream::new(Box::pin(stream), move || {
        counter_drop.fetch_sub(1, Ordering::Relaxed);
    });

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CONTENT_ENCODING, "identity"))
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
            crate::server::audio::snapshot_ais_history(context.get_ref())
                .into_iter()
                .map(trx_core::decode::DecodedMessage::Ais),
        );
        out.extend(
            crate::server::audio::snapshot_vdes_history(context.get_ref())
                .into_iter()
                .map(trx_core::decode::DecodedMessage::Vdes),
        );
        out.extend(
            crate::server::audio::snapshot_aprs_history(context.get_ref())
                .into_iter()
                .map(trx_core::decode::DecodedMessage::Aprs),
        );
        out.extend(
            crate::server::audio::snapshot_hf_aprs_history(context.get_ref())
                .into_iter()
                .map(trx_core::decode::DecodedMessage::HfAprs),
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

    let history_done =
        once(async { Ok::<Bytes, Error>(Bytes::from("event: history_done\ndata: {}\n\n")) });

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

    let stream = history_stream.chain(history_done).chain(select(pings, decode_stream));

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CONTENT_ENCODING, "identity"))
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
///
/// Emits compact binary frames as named SSE event `b`:
///   `event: b\ndata: {center_hz},{sample_rate},{base64_i8_bins}[|{rds_json}]\n\n`
/// Bins are quantized to i8 (1 dB/step, −128…+127 dBFS) for ~5× bandwidth
/// reduction versus full-precision JSON.
///
/// Emits an unnamed `data: null` event when spectrum data becomes unavailable.
#[get("/spectrum")]
pub async fn spectrum(
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    // Subscribe to the watch channel: each client gets its own receiver and is
    // woken exactly when new spectrum data is pushed (no 40 ms polling needed).
    let rx = context.spectrum.subscribe();
    let mut last_rds_json: Option<String> = None;
    let mut last_had_frame = false;
    let updates = WatchStream::new(rx).filter_map(move |snapshot| {
        let sse_chunk: Option<String> = if let Some(ref frame) = snapshot.frame {
            last_had_frame = true;
            let mut chunk = format!("event: b\ndata: {}\n\n", encode_spectrum_frame(frame));
            // rds_json is pre-serialised at ingestion; append an `rds` event
            // only when the payload changes for this particular client.
            if snapshot.rds_json != last_rds_json {
                let data = snapshot.rds_json.as_deref().unwrap_or("null");
                chunk.push_str(&format!("event: rds\ndata: {data}\n\n"));
                last_rds_json = snapshot.rds_json;
            }
            Some(chunk)
        } else if last_had_frame {
            last_had_frame = false;
            Some("data: null\n\n".to_string())
        } else {
            None
        };
        std::future::ready(sse_chunk.map(|s| Ok::<Bytes, Error>(Bytes::from(s))))
    });

    let pings = IntervalStream::new(time::interval(Duration::from_secs(15)))
        .map(|_| Ok::<Bytes, Error>(Bytes::from(": ping\n\n")));

    let stream = select(pings, updates);

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CONTENT_ENCODING, "identity"))
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

#[derive(serde::Deserialize)]
pub struct SdrGainQuery {
    pub db: f64,
}

#[post("/set_sdr_gain")]
pub async fn set_sdr_gain(
    query: web::Query<SdrGainQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetSdrGain(query.db)).await
}

#[derive(serde::Deserialize)]
pub struct SdrSquelchQuery {
    pub enabled: bool,
    pub threshold_db: f64,
}

#[post("/set_sdr_squelch")]
pub async fn set_sdr_squelch(
    query: web::Query<SdrSquelchQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(
        &rig_tx,
        RigCommand::SetSdrSquelch {
            enabled: query.enabled,
            threshold_db: query.threshold_db,
        },
    )
    .await
}

#[derive(serde::Deserialize)]
pub struct WfmDeemphasisQuery {
    pub us: u32,
}

#[post("/set_wfm_deemphasis")]
pub async fn set_wfm_deemphasis(
    query: web::Query<WfmDeemphasisQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetWfmDeemphasis(query.us)).await
}

#[derive(serde::Deserialize)]
pub struct WfmStereoQuery {
    pub enabled: bool,
}

#[post("/set_wfm_stereo")]
pub async fn set_wfm_stereo(
    query: web::Query<WfmStereoQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetWfmStereo(query.enabled)).await
}

#[derive(serde::Deserialize)]
pub struct WfmDenoiseQuery {
    pub level: WfmDenoiseLevel,
}

#[post("/set_wfm_denoise")]
pub async fn set_wfm_denoise(
    query: web::Query<WfmDenoiseQuery>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    send_command(&rig_tx, RigCommand::SetWfmDenoise(query.level)).await
}

#[post("/toggle_aprs_decode")]
pub async fn toggle_aprs_decode(
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().aprs_decode_enabled;
    send_command(&rig_tx, RigCommand::SetAprsDecodeEnabled(!enabled)).await
}

#[post("/toggle_hf_aprs_decode")]
pub async fn toggle_hf_aprs_decode(
    state: web::Data<watch::Receiver<RigState>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    let enabled = state.get_ref().borrow().hf_aprs_decode_enabled;
    send_command(&rig_tx, RigCommand::SetHfAprsDecodeEnabled(!enabled)).await
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

#[post("/clear_hf_aprs_decode")]
pub async fn clear_hf_aprs_decode(
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_hf_aprs_history(context.get_ref());
    send_command(&rig_tx, RigCommand::ResetHfAprsDecoder).await
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
    context: web::Data<Arc<FrontendRuntimeContext>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
) -> Result<HttpResponse, Error> {
    crate::server::audio::clear_cw_history(context.get_ref());
    send_command(&rig_tx, RigCommand::ResetCwDecoder).await
}

// ============================================================================
// Bookmark CRUD endpoints
// ============================================================================

#[derive(serde::Deserialize)]
pub struct BookmarkQuery {
    pub category: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct BookmarkInput {
    pub name: String,
    pub freq_hz: u64,
    pub mode: String,
    pub bandwidth_hz: Option<u64>,
    pub locator: Option<String>,
    pub comment: Option<String>,
    pub category: Option<String>,
    pub decoders: Option<Vec<String>>,
}

fn require_control(
    req: &HttpRequest,
    auth_state: &crate::server::auth::AuthState,
) -> Result<(), Error> {
    if !auth_state.config.enabled {
        return Ok(());
    }
    match crate::server::auth::get_session_role(req, auth_state) {
        Some(crate::server::auth::AuthRole::Control) => Ok(()),
        _ => Err(actix_web::error::ErrorForbidden("control role required")),
    }
}

fn gen_bookmark_id() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

fn normalize_bookmark_locator(locator: Option<String>) -> Option<String> {
    locator.and_then(|value| {
        let trimmed = value.trim().to_uppercase();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[get("/bookmarks")]
pub async fn list_bookmarks(
    store: web::Data<Arc<crate::server::bookmarks::BookmarkStore>>,
    query: web::Query<BookmarkQuery>,
) -> Result<HttpResponse, Error> {
    let mut list = store.list();
    if let Some(ref cat) = query.category {
        if !cat.is_empty() {
            let cat_lower = cat.to_lowercase();
            list.retain(|bm| bm.category.to_lowercase() == cat_lower);
        }
    }
    list.sort_by_key(|bm| bm.freq_hz);
    Ok(HttpResponse::Ok().json(list))
}

#[post("/bookmarks")]
pub async fn create_bookmark(
    req: HttpRequest,
    store: web::Data<Arc<crate::server::bookmarks::BookmarkStore>>,
    body: web::Json<BookmarkInput>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    if store.freq_taken(body.freq_hz, None) {
        return Err(actix_web::error::ErrorConflict(
            "a bookmark for that frequency already exists",
        ));
    }
    let bm = crate::server::bookmarks::Bookmark {
        id: gen_bookmark_id(),
        name: body.name.clone(),
        freq_hz: body.freq_hz,
        mode: body.mode.clone(),
        bandwidth_hz: body.bandwidth_hz,
        locator: normalize_bookmark_locator(body.locator.clone()),
        comment: body.comment.clone().unwrap_or_default(),
        category: body.category.clone().unwrap_or_default(),
        decoders: body.decoders.clone().unwrap_or_default(),
    };
    if store.insert(&bm) {
        Ok(HttpResponse::Created().json(bm))
    } else {
        Err(actix_web::error::ErrorInternalServerError(
            "failed to save bookmark",
        ))
    }
}

#[put("/bookmarks/{id}")]
pub async fn update_bookmark(
    req: HttpRequest,
    path: web::Path<String>,
    store: web::Data<Arc<crate::server::bookmarks::BookmarkStore>>,
    body: web::Json<BookmarkInput>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let id = path.into_inner();
    if store.freq_taken(body.freq_hz, Some(&id)) {
        return Err(actix_web::error::ErrorConflict(
            "a bookmark for that frequency already exists",
        ));
    }
    let bm = crate::server::bookmarks::Bookmark {
        id: id.clone(),
        name: body.name.clone(),
        freq_hz: body.freq_hz,
        mode: body.mode.clone(),
        bandwidth_hz: body.bandwidth_hz,
        locator: normalize_bookmark_locator(body.locator.clone()),
        comment: body.comment.clone().unwrap_or_default(),
        category: body.category.clone().unwrap_or_default(),
        decoders: body.decoders.clone().unwrap_or_default(),
    };
    if store.upsert(&id, &bm) {
        Ok(HttpResponse::Ok().json(bm))
    } else {
        Err(actix_web::error::ErrorNotFound("bookmark not found"))
    }
}

#[delete("/bookmarks/{id}")]
pub async fn delete_bookmark(
    req: HttpRequest,
    path: web::Path<String>,
    store: web::Data<Arc<crate::server::bookmarks::BookmarkStore>>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let id = path.into_inner();
    if store.remove(&id) {
        Ok(HttpResponse::Ok().json(serde_json::json!({ "deleted": true })))
    } else {
        Err(actix_web::error::ErrorNotFound("bookmark not found"))
    }
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
        .service(set_sdr_gain)
        .service(set_sdr_squelch)
        .service(set_wfm_deemphasis)
        .service(set_wfm_stereo)
        .service(set_wfm_denoise)
        .service(toggle_aprs_decode)
        .service(toggle_hf_aprs_decode)
        .service(toggle_cw_decode)
        .service(set_cw_auto)
        .service(set_cw_wpm)
        .service(set_cw_tone)
        .service(toggle_ft8_decode)
        .service(toggle_wspr_decode)
        .service(clear_ais_decode)
        .service(clear_vdes_decode)
        .service(clear_aprs_decode)
        .service(clear_hf_aprs_decode)
        .service(clear_cw_decode)
        .service(clear_ft8_decode)
        .service(clear_wspr_decode)
        .service(select_rig)
        // Bookmark CRUD
        .service(list_bookmarks)
        .service(create_bookmark)
        .service(update_bookmark)
        .service(delete_bookmark)
        .service(crate::server::audio::audio_ws)
        .service(favicon)
        .service(favicon_png)
        .service(logo)
        .service(style_css)
        .service(app_js)
        .service(webgl_renderer_js)
        .service(leaflet_ais_tracksymbol_js)
        .service(ais_js)
        .service(vdes_js)
        .service(aprs_js)
        .service(hf_aprs_js)
        .service(ft8_js)
        .service(wspr_js)
        .service(cw_js)
        .service(bookmarks_js)
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

#[get("/favicon.png")]
async fn favicon_png() -> impl Responder {
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

#[get("/webgl-renderer.js")]
async fn webgl_renderer_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ))
        .body(status::WEBGL_RENDERER_JS)
}

#[get("/leaflet-ais-tracksymbol.js")]
async fn leaflet_ais_tracksymbol_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ))
        .body(status::LEAFLET_AIS_TRACKSYMBOL_JS)
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

#[get("/hf-aprs.js")]
async fn hf_aprs_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ))
        .body(status::HF_APRS_JS)
}

#[get("/ais.js")]
async fn ais_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ))
        .body(status::AIS_JS)
}

#[get("/vdes.js")]
async fn vdes_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ))
        .body(status::VDES_JS)
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

#[get("/bookmarks.js")]
async fn bookmarks_js() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ))
        .body(status::BOOKMARKS_JS)
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
            rig_id_override: None,
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
        hf_aprs_decode_enabled: state.hf_aprs_decode_enabled,
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
