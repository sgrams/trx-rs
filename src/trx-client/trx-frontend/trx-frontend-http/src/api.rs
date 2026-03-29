// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use actix_web::{delete, get, post, put, web, HttpRequest, HttpResponse, Responder};
use actix_web::{http::header, Error};
use bytes::Bytes;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::stream::{select, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::{self, Duration};
use tokio_stream::wrappers::{IntervalStream, WatchStream};
use uuid::Uuid;

use crate::server::vchan::ClientChannelManager;

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

/// Encode spectrum bins as a compact base64 string of i8 values (1 dB/step).
///
/// Wire format for the `b` SSE event:
///   `{center_hz},{sample_rate},{base64_i8_bins}`
///
/// RDS is intentionally excluded — it changes rarely and is sent via the
/// `/events` state stream instead.
fn encode_spectrum_frame(frame: &trx_core::rig::state::SpectrumData) -> String {
    // Encode directly from the iterator to avoid an intermediate Vec<u8>.
    let clamped: Vec<u8> = frame
        .bins
        .iter()
        .map(|&v| v.round().clamp(-128.0, 127.0) as i8 as u8)
        .collect();
    let b64 = base64_encode(&clamped);

    // Pre-allocate: header digits + 2 commas + base64 body.
    let mut out = String::with_capacity(40 + b64.len());
    out.push_str(&frame.center_hz.to_string());
    out.push(',');
    out.push_str(&frame.sample_rate.to_string());
    out.push(',');
    out.push_str(&b64);
    out
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

#[derive(serde::Deserialize)]
pub struct StatusQuery {
    pub remote: Option<String>,
}

#[get("/status")]
pub async fn status_api(
    query: web::Query<StatusQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    clients: web::Data<Arc<AtomicUsize>>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<impl Responder, Error> {
    // Prefer the per-rig watch channel when a remote is specified,
    // falling back to the global state watch.
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

#[derive(serde::Deserialize)]
pub struct EventsQuery {
    pub remote: Option<String>,
}

#[get("/events")]
#[allow(clippy::too_many_arguments)]
pub async fn events(
    query: web::Query<EventsQuery>,
    state: web::Data<watch::Receiver<RigState>>,
    clients: web::Data<Arc<AtomicUsize>>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
    bookmark_store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    scheduler_status: web::Data<crate::server::scheduler::SchedulerStatusMap>,
    scheduler_control: web::Data<crate::server::scheduler::SharedSchedulerControlManager>,
    session_rig_mgr: web::Data<Arc<SessionRigManager>>,
) -> Result<HttpResponse, Error> {
    let counter = clients.get_ref().clone();
    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;

    // Assign a stable UUID to this SSE session for channel binding.
    let session_id = Uuid::new_v4();
    scheduler_control.register_session(session_id);

    // Use the client-requested remote if provided, otherwise fall back to
    // the global default.  This allows each tab to reconnect SSE for the
    // rig it has selected without mutating global state.
    let active_rig_id = query.remote.clone().filter(|s| !s.is_empty()).or_else(|| {
        context
            .routing
            .active_rig_id
            .lock()
            .ok()
            .and_then(|g| g.clone())
    });

    // Subscribe to the per-rig watch channel for this session's rig,
    // falling back to the global state watch when unavailable.
    let rx = active_rig_id
        .as_deref()
        .and_then(|rid| context.rig_state_rx(rid))
        .unwrap_or_else(|| state.get_ref().clone());
    let initial = wait_for_view(rx.clone()).await?;
    if let Some(ref rid) = active_rig_id {
        session_rig_mgr.register(session_id, rid.clone());
        vchan_mgr.init_rig(
            rid,
            initial.status.freq.hz,
            &format!("{:?}", initial.status.mode),
        );
        sync_scheduler_vchannels(
            vchan_mgr.get_ref().as_ref(),
            bookmark_store_map.get_ref().as_ref(),
            scheduler_status.get_ref(),
            scheduler_control.get_ref().as_ref(),
            rid,
        );
    }

    // Build the prefix burst: rig state → session UUID → initial channels.
    let initial_combined = SnapshotWithMeta {
        snapshot: &initial,
        meta: frontend_meta_from_context(
            count,
            context.get_ref().as_ref(),
            active_rig_id.as_deref(),
        ),
    };
    let initial_json = serde_json::to_string(&initial_combined)
        .map_err(actix_web::error::ErrorInternalServerError)?;

    let mut prefix: Vec<Result<Bytes, Error>> = Vec::new();
    prefix.push(Ok(Bytes::from(format!("data: {initial_json}\n\n"))));
    prefix.push(Ok(Bytes::from(format!(
        "event: session\ndata: {{\"session_id\":\"{session_id}\"}}\n\n"
    ))));
    if let Some(ref rid) = active_rig_id {
        let chans = vchan_mgr.channels(rid);
        if let Ok(json) = serde_json::to_string(&chans) {
            prefix.push(Ok(Bytes::from(format!(
                "event: channels\ndata: {{\"remote\":\"{rid}\",\"channels\":{json}}}\n\n"
            ))));
        }
    }
    let prefix_stream = futures_util::stream::iter(prefix);

    // Live rig-state updates; side-effect: keep primary channel metadata in sync.
    let counter_updates = counter.clone();
    let context_updates = context.get_ref().clone();
    let vchan_updates = vchan_mgr.get_ref().clone();
    let bookmark_store_map_updates = bookmark_store_map.get_ref().clone();
    let scheduler_status_updates = scheduler_status.get_ref().clone();
    let scheduler_control_updates = scheduler_control.get_ref().clone();
    let session_rig_mgr_updates = session_rig_mgr.get_ref().clone();
    let updates = WatchStream::new(rx).filter_map(move |state| {
        let counter = counter_updates.clone();
        let context = context_updates.clone();
        let vchan = vchan_updates.clone();
        let bookmark_store_map = bookmark_store_map_updates.clone();
        let scheduler_status = scheduler_status_updates.clone();
        let scheduler_control = scheduler_control_updates.clone();
        let session_rig_mgr = session_rig_mgr_updates.clone();
        async move {
            state.snapshot().and_then(|v| {
                let rig_id_opt = session_rig_mgr.get_rig(session_id).or_else(|| {
                    context
                        .routing
                        .active_rig_id
                        .lock()
                        .ok()
                        .and_then(|g| g.clone())
                });
                if let Some(ref rig_id) = rig_id_opt {
                    vchan.update_primary(rig_id, v.status.freq.hz, &format!("{:?}", v.status.mode));
                    sync_scheduler_vchannels(
                        vchan.as_ref(),
                        bookmark_store_map.as_ref(),
                        &scheduler_status,
                        scheduler_control.as_ref(),
                        rig_id,
                    );
                }
                let combined = SnapshotWithMeta {
                    snapshot: &v,
                    meta: frontend_meta_from_context(
                        counter.load(Ordering::Relaxed),
                        context.as_ref(),
                        rig_id_opt.as_deref(),
                    ),
                };
                serde_json::to_string(&combined)
                    .ok()
                    .map(|json| Ok::<Bytes, Error>(Bytes::from(format!("data: {json}\n\n"))))
            })
        }
    });

    // Channel-list change events from the virtual channel manager.
    // Only forward events for this SSE session's rig so tabs viewing
    // different rigs don't see each other's channel lists.
    let vchan_change_rx = vchan_mgr.change_tx.subscribe();
    let session_rig_for_chan = active_rig_id.clone();
    let chan_updates = futures_util::stream::unfold(
        (vchan_change_rx, session_rig_for_chan),
        |(mut rx, srig)| async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        if let Some(colon) = msg.find(':') {
                            let rig_id = &msg[..colon];
                            // Skip channel events that belong to a different rig.
                            if let Some(ref expected) = srig {
                                if rig_id != expected.as_str() {
                                    continue;
                                }
                            }
                            let channels_json = &msg[colon + 1..];
                            let payload =
                                format!("{{\"remote\":\"{rig_id}\",\"channels\":{channels_json}}}");
                            return Some((
                                Ok::<Bytes, Error>(Bytes::from(format!(
                                    "event: channels\ndata: {payload}\n\n"
                                ))),
                                (rx, srig),
                            ));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );

    // Send a named "ping" event so the JS heartbeat can observe it.
    let pings = IntervalStream::new(time::interval(Duration::from_secs(5)))
        .map(|_| Ok::<Bytes, Error>(Bytes::from("event: ping\ndata: \n\n")));

    let vchan_drop = vchan_mgr.get_ref().clone();
    let counter_drop = counter.clone();
    let scheduler_control_drop = scheduler_control.get_ref().clone();
    let session_rig_mgr_drop = session_rig_mgr.get_ref().clone();
    let live = select(select(pings, updates), chan_updates);
    let stream = prefix_stream.chain(live);
    let stream = DropStream::new(Box::pin(stream), move || {
        counter_drop.fetch_sub(1, Ordering::Relaxed);
        vchan_drop.release_session(session_id);
        scheduler_control_drop.unregister_session(session_id);
        session_rig_mgr_drop.unregister(session_id);
    });

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CONTENT_ENCODING, "identity"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::CONNECTION, "keep-alive"))
        .streaming(stream))
}

fn sync_scheduler_vchannels(
    vchan_mgr: &ClientChannelManager,
    bookmark_store_map: &crate::server::bookmarks::BookmarkStoreMap,
    scheduler_status: &crate::server::scheduler::SchedulerStatusMap,
    scheduler_control: &crate::server::scheduler::SchedulerControlManager,
    rig_id: &str,
) {
    if !scheduler_control.scheduler_allowed() {
        vchan_mgr.sync_scheduler_channels(rig_id, &[]);
        return;
    }

    let desired = {
        let map = scheduler_status.read().unwrap_or_else(|e| e.into_inner());
        map.get(rig_id)
            .filter(|status| status.active)
            .map(|status| {
                status
                    .last_bookmark_ids
                    .iter()
                    .filter_map(|bookmark_id| {
                        bookmark_store_map
                            .get_for_rig(rig_id, bookmark_id)
                            .map(|bookmark| {
                                (
                                    bookmark_id.clone(),
                                    bookmark.freq_hz,
                                    bookmark.mode.clone(),
                                    bookmark.bandwidth_hz.unwrap_or(0) as u32,
                                    bookmark_decoder_kinds(&bookmark),
                                )
                            })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    vchan_mgr.sync_scheduler_channels(rig_id, &desired);
}

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
    }
}

/// Build the grouped decode history payload from all per-decoder ring-buffers.
/// When `rig_filter` is `Some`, only entries recorded for that rig are included.
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

fn gzip_bytes(payload: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(payload)?;
    encoder.finish()
}

/// `GET /decode/history` — returns the full decode history as gzipped CBOR.
///
/// Separated from the live `/decode` SSE stream so that history replay does
/// not block real-time messages: the client fetches this endpoint in parallel
/// with opening the SSE connection and drains it in the background.
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
    query: web::Query<RemoteQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    // Subscribe to a per-rig spectrum channel when remote is specified,
    // otherwise fall back to the global channel for backward compat.
    let rx = if let Some(ref remote) = query.remote {
        context.rig_spectrum_rx(remote)
    } else {
        context.spectrum.sender.subscribe()
    };
    let mut last_rds_json: Option<String> = None;
    let mut last_vchan_rds_json: Option<String> = None;
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
            if snapshot.vchan_rds_json != last_vchan_rds_json {
                let data = snapshot.vchan_rds_json.as_deref().unwrap_or("null");
                chunk.push_str(&format!("event: rds_vchan\ndata: {data}\n\n"));
                last_vchan_rds_json = snapshot.vchan_rds_json;
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
///
/// Reads cached predictions from the server (fetched via GetSatPasses).
/// Returns an empty `passes` array with an `error` field if predictions
/// are not yet available.
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

// ============================================================================
// Bookmark CRUD endpoints
// ============================================================================

#[derive(serde::Deserialize)]
pub struct BookmarkQuery {
    pub category: Option<String>,
    /// `"general"` for the shared store, or a rig remote name for
    /// the per-rig store.  Omitting defaults to the general store.
    pub scope: Option<String>,
}

/// Resolve which `BookmarkStore` to use based on the `scope` parameter.
///
/// - `scope` absent or `"general"` → general store
/// - `scope` = `"{remote}"` → per-rig store for that remote
fn resolve_bookmark_store(
    scope: Option<&str>,
    store_map: &crate::server::bookmarks::BookmarkStoreMap,
) -> std::sync::Arc<crate::server::bookmarks::BookmarkStore> {
    match scope.filter(|s| !s.is_empty() && *s != "general") {
        Some(remote) => store_map.store_for(remote),
        None => store_map.general().clone(),
    }
}

#[derive(serde::Deserialize)]
pub struct BookmarkScopeQuery {
    pub scope: Option<String>,
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
///
/// Assets are embedded at compile time and never change within a build.
/// We pre-compress each asset once (via `OnceLock`) and serve the cached
/// gzip bytes with a strong ETag derived from the build version tag, so
/// browsers can cache aggressively and validate cheaply with `If-None-Match`.
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

macro_rules! define_gz_cache {
    ($fn_name:ident, $src:expr, $asset_name:literal) => {
        fn $fn_name() -> &'static GzCacheEntry {
            static CACHE: OnceLock<GzCacheEntry> = OnceLock::new();
            CACHE.get_or_init(|| gz_cache_entry($src.as_bytes(), $asset_name))
        }
    };
}

define_gz_cache!(gz_index_html, status::index_html(), "index.html");
define_gz_cache!(gz_style_css, status::STYLE_CSS, "style.css");
define_gz_cache!(gz_app_js, status::APP_JS, "app.js");
define_gz_cache!(
    gz_decode_history_worker_js,
    status::DECODE_HISTORY_WORKER_JS,
    "decode-history-worker.js"
);
define_gz_cache!(
    gz_webgl_renderer_js,
    status::WEBGL_RENDERER_JS,
    "webgl-renderer.js"
);
define_gz_cache!(
    gz_leaflet_ais_tracksymbol_js,
    status::LEAFLET_AIS_TRACKSYMBOL_JS,
    "leaflet-ais-tracksymbol.js"
);
define_gz_cache!(gz_ais_js, status::AIS_JS, "ais.js");
define_gz_cache!(gz_vdes_js, status::VDES_JS, "vdes.js");
define_gz_cache!(gz_aprs_js, status::APRS_JS, "aprs.js");
define_gz_cache!(gz_hf_aprs_js, status::HF_APRS_JS, "hf-aprs.js");
define_gz_cache!(gz_ft8_js, status::FT8_JS, "ft8.js");
define_gz_cache!(gz_ft4_js, status::FT4_JS, "ft4.js");
define_gz_cache!(gz_ft2_js, status::FT2_JS, "ft2.js");
define_gz_cache!(gz_wspr_js, status::WSPR_JS, "wspr.js");
define_gz_cache!(gz_cw_js, status::CW_JS, "cw.js");
define_gz_cache!(gz_sat_js, status::SAT_JS, "sat.js");
define_gz_cache!(gz_bookmarks_js, status::BOOKMARKS_JS, "bookmarks.js");
define_gz_cache!(gz_scheduler_js, status::SCHEDULER_JS, "scheduler.js");
define_gz_cache!(
    gz_sat_scheduler_js,
    status::SAT_SCHEDULER_JS,
    "sat-scheduler.js"
);
define_gz_cache!(
    gz_background_decode_js,
    status::BACKGROUND_DECODE_JS,
    "background-decode.js"
);
define_gz_cache!(gz_vchan_js, status::VCHAN_JS, "vchan.js");

/// A bookmark with its owning scope tag for the list response.
#[derive(serde::Serialize)]
struct BookmarkWithScope {
    #[serde(flatten)]
    bm: crate::server::bookmarks::Bookmark,
    scope: String,
}

#[get("/bookmarks")]
pub async fn list_bookmarks(
    req: HttpRequest,
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkQuery>,
) -> Result<HttpResponse, Error> {
    if request_accepts_html(&req) {
        return Ok(no_cache_response(
            "text/html; charset=utf-8",
            status::index_html(),
        ));
    }
    let scope = query
        .scope
        .as_deref()
        .filter(|s| !s.is_empty() && *s != "general");
    let mut list: Vec<BookmarkWithScope> = match scope {
        Some(remote) => {
            // Rig selected: merge general + rig-specific (rig wins on duplicate IDs).
            let mut map: std::collections::HashMap<String, BookmarkWithScope> = store_map
                .general()
                .list()
                .into_iter()
                .map(|bm| {
                    let id = bm.id.clone();
                    (
                        id,
                        BookmarkWithScope {
                            bm,
                            scope: "general".into(),
                        },
                    )
                })
                .collect();
            for bm in store_map.store_for(remote).list() {
                let id = bm.id.clone();
                map.insert(
                    id,
                    BookmarkWithScope {
                        bm,
                        scope: remote.to_owned(),
                    },
                );
            }
            map.into_values().collect()
        }
        None => store_map
            .general()
            .list()
            .into_iter()
            .map(|bm| BookmarkWithScope {
                bm,
                scope: "general".into(),
            })
            .collect(),
    };
    if let Some(ref cat) = query.category {
        if !cat.is_empty() {
            let cat_lower = cat.to_lowercase();
            list.retain(|item| item.bm.category.to_lowercase() == cat_lower);
        }
    }
    list.sort_by_key(|item| item.bm.freq_hz);
    Ok(HttpResponse::Ok().json(list))
}

#[post("/bookmarks")]
pub async fn create_bookmark(
    req: HttpRequest,
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkScopeQuery>,
    body: web::Json<BookmarkInput>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let store = resolve_bookmark_store(query.scope.as_deref(), store_map.get_ref());
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
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkScopeQuery>,
    body: web::Json<BookmarkInput>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let store = resolve_bookmark_store(query.scope.as_deref(), store_map.get_ref());
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
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkScopeQuery>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let store = resolve_bookmark_store(query.scope.as_deref(), store_map.get_ref());
    let id = path.into_inner();
    if store.remove(&id) {
        Ok(HttpResponse::Ok().json(serde_json::json!({ "deleted": true })))
    } else {
        Err(actix_web::error::ErrorNotFound("bookmark not found"))
    }
}

#[derive(serde::Deserialize)]
struct BatchDeleteRequest {
    ids: Vec<String>,
}

#[post("/bookmarks/batch_delete")]
pub async fn batch_delete_bookmarks(
    req: HttpRequest,
    body: web::Json<BatchDeleteRequest>,
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkScopeQuery>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let store = resolve_bookmark_store(query.scope.as_deref(), store_map.get_ref());
    let mut deleted = 0usize;
    for id in &body.ids {
        if store.remove(id) {
            deleted += 1;
        }
    }
    Ok(HttpResponse::Ok().json(serde_json::json!({ "deleted": deleted })))
}

#[derive(serde::Deserialize)]
struct BatchMoveRequest {
    ids: Vec<String>,
    to: String,
}

#[post("/bookmarks/batch_move")]
pub async fn batch_move_bookmarks(
    req: HttpRequest,
    body: web::Json<BatchMoveRequest>,
    store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    query: web::Query<BookmarkScopeQuery>,
    auth_state: web::Data<crate::server::auth::AuthState>,
) -> Result<HttpResponse, Error> {
    require_control(&req, &auth_state)?;
    let from_store = resolve_bookmark_store(query.scope.as_deref(), store_map.get_ref());
    let to_store = resolve_bookmark_store(Some(body.to.as_str()), store_map.get_ref());
    let mut moved = 0usize;
    for id in &body.ids {
        if let Some(bm) = from_store.get(id) {
            if to_store.insert(&bm) && from_store.remove(id) {
                moved += 1;
            }
        }
    }
    Ok(HttpResponse::Ok().json(serde_json::json!({ "moved": moved })))
}

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

// ---------------------------------------------------------------------------
// Virtual channel CRUD
// ---------------------------------------------------------------------------

#[get("/channels/{remote}")]
pub async fn list_channels(
    path: web::Path<String>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let remote = path.into_inner();
    HttpResponse::Ok().json(vchan_mgr.channels(&remote))
}

#[derive(serde::Deserialize)]
struct AllocateChannelBody {
    session_id: Uuid,
    freq_hz: u64,
    mode: String,
}

#[post("/channels/{remote}")]
pub async fn allocate_channel(
    path: web::Path<String>,
    body: web::Json<AllocateChannelBody>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let remote = path.into_inner();
    match vchan_mgr.allocate(body.session_id, &remote, body.freq_hz, &body.mode) {
        Ok(ch) => HttpResponse::Ok().json(ch),
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

#[delete("/channels/{remote}/{channel_id}")]
pub async fn delete_channel_route(
    path: web::Path<(String, Uuid)>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let (remote, channel_id) = path.into_inner();
    match vchan_mgr.delete_channel(&remote, channel_id) {
        Ok(()) => HttpResponse::Ok().finish(),
        Err(crate::server::vchan::VChanClientError::NotFound) => HttpResponse::NotFound().finish(),
        Err(crate::server::vchan::VChanClientError::Permanent) => {
            HttpResponse::BadRequest().body("cannot remove the primary channel")
        }
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct SubscribeBody {
    session_id: Uuid,
}

#[post("/channels/{remote}/{channel_id}/subscribe")]
pub async fn subscribe_channel(
    path: web::Path<(String, Uuid)>,
    body: web::Json<SubscribeBody>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
    rig_tx: web::Data<mpsc::Sender<RigRequest>>,
    bookmark_store_map: web::Data<Arc<crate::server::bookmarks::BookmarkStoreMap>>,
    scheduler_control: web::Data<crate::server::scheduler::SharedSchedulerControlManager>,
) -> impl Responder {
    let body = body.into_inner();
    let (remote, channel_id) = path.into_inner();
    match vchan_mgr.subscribe_session(body.session_id, &remote, channel_id) {
        Some(ch) => {
            scheduler_control.set_released(body.session_id, false);
            let Some(selected) = vchan_mgr.selected_channel(&remote, channel_id) else {
                return HttpResponse::InternalServerError().body("subscribed channel missing");
            };
            if let Err(err) = apply_selected_channel(
                rig_tx.get_ref(),
                &remote,
                &selected,
                bookmark_store_map.get_ref().as_ref(),
            )
            .await
            {
                return HttpResponse::from_error(err);
            }
            HttpResponse::Ok().json(ch)
        }
        None => HttpResponse::NotFound().finish(),
    }
}

#[derive(serde::Deserialize)]
struct SetChanFreqBody {
    freq_hz: u64,
}

#[put("/channels/{remote}/{channel_id}/freq")]
pub async fn set_vchan_freq(
    path: web::Path<(String, Uuid)>,
    body: web::Json<SetChanFreqBody>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let (remote, channel_id) = path.into_inner();
    match vchan_mgr.set_channel_freq(&remote, channel_id, body.freq_hz) {
        Ok(()) => HttpResponse::Ok().finish(),
        Err(crate::server::vchan::VChanClientError::NotFound) => HttpResponse::NotFound().finish(),
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct SetChanBwBody {
    bandwidth_hz: u32,
}

#[put("/channels/{remote}/{channel_id}/bw")]
pub async fn set_vchan_bw(
    path: web::Path<(String, Uuid)>,
    body: web::Json<SetChanBwBody>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let (remote, channel_id) = path.into_inner();
    match vchan_mgr.set_channel_bandwidth(&remote, channel_id, body.bandwidth_hz) {
        Ok(()) => HttpResponse::Ok().finish(),
        Err(crate::server::vchan::VChanClientError::NotFound) => HttpResponse::NotFound().finish(),
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct SetChanModeBody {
    mode: String,
}

#[put("/channels/{remote}/{channel_id}/mode")]
pub async fn set_vchan_mode(
    path: web::Path<(String, Uuid)>,
    body: web::Json<SetChanModeBody>,
    vchan_mgr: web::Data<Arc<ClientChannelManager>>,
) -> impl Responder {
    let (remote, channel_id) = path.into_inner();
    match vchan_mgr.set_channel_mode(&remote, channel_id, &body.mode) {
        Ok(()) => HttpResponse::Ok().finish(),
        Err(crate::server::vchan::VChanClientError::NotFound) => HttpResponse::NotFound().finish(),
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(index)
        .service(map_index)
        .service(digital_modes_index)
        .service(settings_index)
        .service(about_index)
        .service(status_api)
        .service(list_rigs)
        .service(events)
        .service(decode_history)
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
        .service(set_sdr_gain)
        .service(set_sdr_lna_gain)
        .service(set_sdr_agc)
        .service(set_sdr_squelch)
        .service(set_sdr_noise_blanker)
        .service(set_wfm_deemphasis)
        .service(set_wfm_stereo)
        .service(set_wfm_denoise)
        .service(set_sam_stereo_width)
        .service(set_sam_carrier_sync)
        .service(toggle_aprs_decode)
        .service(toggle_hf_aprs_decode)
        .service(toggle_cw_decode)
        .service(set_cw_auto)
        .service(set_cw_wpm)
        .service(set_cw_tone)
        .service(toggle_ft8_decode)
        .service(toggle_ft4_decode)
        .service(toggle_ft2_decode)
        .service(toggle_wspr_decode)
        .service(toggle_lrpt_decode)
        .service(sat_passes)
        .service(clear_ais_decode)
        .service(clear_vdes_decode)
        .service(clear_aprs_decode)
        .service(clear_hf_aprs_decode)
        .service(clear_cw_decode)
        .service(clear_ft8_decode)
        .service(clear_ft4_decode)
        .service(clear_ft2_decode)
        .service(clear_wspr_decode)
        .service(clear_lrpt_decode)
        .service(select_rig)
        // Bookmark CRUD
        .service(list_bookmarks)
        .service(create_bookmark)
        .service(update_bookmark)
        .service(delete_bookmark)
        .service(batch_delete_bookmarks)
        .service(batch_move_bookmarks)
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
        .service(favicon)
        .service(favicon_png)
        .service(logo)
        .service(style_css)
        .service(app_js)
        .service(decode_history_worker_js)
        .service(webgl_renderer_js)
        .service(leaflet_ais_tracksymbol_js)
        .service(ais_js)
        .service(vdes_js)
        .service(aprs_js)
        .service(hf_aprs_js)
        .service(ft8_js)
        .service(ft4_js)
        .service(ft2_js)
        .service(wspr_js)
        .service(cw_js)
        .service(sat_js)
        .service(bookmarks_js)
        .service(scheduler_js)
        .service(sat_scheduler_js)
        .service(background_decode_js)
        .service(vchan_js)
        // Virtual channels
        .service(list_channels)
        .service(allocate_channel)
        .service(delete_channel_route)
        .service(subscribe_channel)
        .service(set_vchan_freq)
        .service(set_vchan_bw)
        .service(set_vchan_mode)
        // Auth endpoints
        .service(crate::server::auth::login)
        .service(crate::server::auth::logout)
        .service(crate::server::auth::session_status);
}

#[get("/")]
async fn index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", &c.gz, &c.etag)
}

#[get("/map")]
async fn map_index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", &c.gz, &c.etag)
}

#[get("/digital-modes")]
async fn digital_modes_index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", &c.gz, &c.etag)
}

#[get("/settings")]
async fn settings_index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", &c.gz, &c.etag)
}

#[get("/about")]
async fn about_index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", &c.gz, &c.etag)
}

#[get("/favicon.ico")]
async fn favicon() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(FAVICON_BYTES)
}

#[get("/favicon.png")]
async fn favicon_png() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(FAVICON_BYTES)
}

#[get("/logo.png")]
async fn logo() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(LOGO_BYTES)
}

#[get("/style.css")]
async fn style_css(req: HttpRequest) -> impl Responder {
    let c = gz_style_css();
    static_asset_response(&req, "text/css; charset=utf-8", &c.gz, &c.etag)
}

#[get("/app.js")]
async fn app_js(req: HttpRequest) -> impl Responder {
    let c = gz_app_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/decode-history-worker.js")]
async fn decode_history_worker_js(req: HttpRequest) -> impl Responder {
    let c = gz_decode_history_worker_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/webgl-renderer.js")]
async fn webgl_renderer_js(req: HttpRequest) -> impl Responder {
    let c = gz_webgl_renderer_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/leaflet-ais-tracksymbol.js")]
async fn leaflet_ais_tracksymbol_js(req: HttpRequest) -> impl Responder {
    let c = gz_leaflet_ais_tracksymbol_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/aprs.js")]
async fn aprs_js(req: HttpRequest) -> impl Responder {
    let c = gz_aprs_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/hf-aprs.js")]
async fn hf_aprs_js(req: HttpRequest) -> impl Responder {
    let c = gz_hf_aprs_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/ais.js")]
async fn ais_js(req: HttpRequest) -> impl Responder {
    let c = gz_ais_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/vdes.js")]
async fn vdes_js(req: HttpRequest) -> impl Responder {
    let c = gz_vdes_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/ft8.js")]
async fn ft8_js(req: HttpRequest) -> impl Responder {
    let c = gz_ft8_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/ft4.js")]
async fn ft4_js(req: HttpRequest) -> impl Responder {
    let c = gz_ft4_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/ft2.js")]
async fn ft2_js(req: HttpRequest) -> impl Responder {
    let c = gz_ft2_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/wspr.js")]
async fn wspr_js(req: HttpRequest) -> impl Responder {
    let c = gz_wspr_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/cw.js")]
async fn cw_js(req: HttpRequest) -> impl Responder {
    let c = gz_cw_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/sat.js")]
async fn sat_js(req: HttpRequest) -> impl Responder {
    let c = gz_sat_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/bookmarks.js")]
async fn bookmarks_js(req: HttpRequest) -> impl Responder {
    let c = gz_bookmarks_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/scheduler.js")]
async fn scheduler_js(req: HttpRequest) -> impl Responder {
    let c = gz_scheduler_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/sat-scheduler.js")]
async fn sat_scheduler_js(req: HttpRequest) -> impl Responder {
    let c = gz_sat_scheduler_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/background-decode.js")]
async fn background_decode_js(req: HttpRequest) -> impl Responder {
    let c = gz_background_decode_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

#[get("/vchan.js")]
async fn vchan_js(req: HttpRequest) -> impl Responder {
    let c = gz_vchan_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        &c.gz,
        &c.etag,
    )
}

/// Generic query extractor for endpoints that only need the optional remote.
#[derive(serde::Deserialize)]
pub struct RemoteQuery {
    pub remote: Option<String>,
}

async fn send_command(
    rig_tx: &mpsc::Sender<RigRequest>,
    cmd: RigCommand,
    remote: Option<String>,
) -> Result<HttpResponse, Error> {
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
) -> Result<(), Error> {
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

fn bookmark_decoder_state(
    bookmark: &crate::server::bookmarks::Bookmark,
) -> (bool, bool, bool, bool, bool, bool, bool) {
    let mut want_aprs = bookmark.mode.trim().eq_ignore_ascii_case("PKT");
    let mut want_hf_aprs = false;
    let mut want_ft8 = false;
    let mut want_ft4 = false;
    let mut want_ft2 = false;
    let mut want_wspr = false;
    let mut want_lrpt = false;

    for decoder in bookmark
        .decoders
        .iter()
        .map(|item| item.trim().to_ascii_lowercase())
    {
        match decoder.as_str() {
            "aprs" => want_aprs = true,
            "hf-aprs" => want_hf_aprs = true,
            "ft8" => want_ft8 = true,
            "ft4" => want_ft4 = true,
            "ft2" => want_ft2 = true,
            "wspr" => want_wspr = true,
            "lrpt" => want_lrpt = true,
            _ => {}
        }
    }

    (
        want_aprs,
        want_hf_aprs,
        want_ft8,
        want_ft4,
        want_ft2,
        want_wspr,
        want_lrpt,
    )
}

fn bookmark_decoder_kinds(bookmark: &crate::server::bookmarks::Bookmark) -> Vec<String> {
    let mut out = Vec::new();
    for decoder in bookmark
        .decoders
        .iter()
        .map(|item| item.trim().to_ascii_lowercase())
    {
        if matches!(
            decoder.as_str(),
            "aprs" | "ais" | "ft8" | "ft4" | "ft2" | "wspr" | "hf-aprs"
        ) && !out.iter().any(|existing| existing == &decoder)
        {
            out.push(decoder);
        }
    }

    if !out.is_empty() {
        return out;
    }

    match bookmark.mode.trim().to_ascii_uppercase().as_str() {
        "AIS" => vec!["ais".to_string()],
        "PKT" => vec!["aprs".to_string()],
        _ => Vec::new(),
    }
}

async fn apply_selected_channel(
    rig_tx: &mpsc::Sender<RigRequest>,
    remote: &str,
    channel: &crate::server::vchan::SelectedChannel,
    bookmark_store_map: &crate::server::bookmarks::BookmarkStoreMap,
) -> Result<(), Error> {
    send_command_to_rig(
        rig_tx,
        remote,
        RigCommand::SetMode(parse_mode(&channel.mode)),
    )
    .await?;

    if channel.bandwidth_hz > 0 {
        send_command_to_rig(
            rig_tx,
            remote,
            RigCommand::SetBandwidth(channel.bandwidth_hz),
        )
        .await?;
    }

    send_command_to_rig(
        rig_tx,
        remote,
        RigCommand::SetFreq(Freq {
            hz: channel.freq_hz,
        }),
    )
    .await?;

    let Some(bookmark_id) = channel.scheduler_bookmark_id.as_deref() else {
        return Ok(());
    };
    let Some(bookmark) = bookmark_store_map.get_for_rig(remote, bookmark_id) else {
        return Ok(());
    };
    let (want_aprs, want_hf_aprs, want_ft8, want_ft4, want_ft2, want_wspr, want_lrpt) =
        bookmark_decoder_state(&bookmark);
    let desired = [
        RigCommand::SetAprsDecodeEnabled(want_aprs),
        RigCommand::SetHfAprsDecodeEnabled(want_hf_aprs),
        RigCommand::SetFt8DecodeEnabled(want_ft8),
        RigCommand::SetFt4DecodeEnabled(want_ft4),
        RigCommand::SetFt2DecodeEnabled(want_ft2),
        RigCommand::SetWsprDecodeEnabled(want_wspr),
        RigCommand::SetLrptDecodeEnabled(want_lrpt),
    ];
    for cmd in desired {
        send_command_to_rig(rig_tx, remote, cmd).await?;
    }

    Ok(())
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
