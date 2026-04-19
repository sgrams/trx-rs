// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! SSE stream endpoints: /events (rig state) and /spectrum.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use actix_web::http::header;
use actix_web::Error;
use actix_web::{get, web, HttpResponse};
use bytes::Bytes;
use futures_util::stream::{select, StreamExt};
use tokio::sync::{broadcast, watch};
use tokio::time::{self, Duration};
use tokio_stream::wrappers::{IntervalStream, WatchStream};
use uuid::Uuid;

use trx_core::RigState;
use trx_frontend::FrontendRuntimeContext;
use trx_protocol::MeterUpdate;

use crate::server::vchan::ClientChannelManager;

use super::{
    base64_encode, frontend_meta_from_context, wait_for_view, RemoteQuery, SessionRigManager,
    SnapshotWithMeta,
};

// ============================================================================
// DropStream utility
// ============================================================================

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

// ============================================================================
// Spectrum encoding
// ============================================================================

/// Encode spectrum bins as a compact base64 string of i8 values (1 dB/step).
fn encode_spectrum_frame(frame: &trx_core::rig::state::SpectrumData) -> String {
    let clamped: Vec<u8> = frame
        .bins
        .iter()
        .map(|&v| v.round().clamp(-128.0, 127.0) as i8 as u8)
        .collect();
    let b64 = base64_encode(&clamped);

    let mut out = String::with_capacity(40 + b64.len());
    out.push_str(&frame.center_hz.to_string());
    out.push(',');
    out.push_str(&frame.sample_rate.to_string());
    out.push(',');
    out.push_str(&b64);
    out
}

// ============================================================================
// Scheduler vchannel sync helper
// ============================================================================

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

fn bookmark_decoder_kinds(bookmark: &crate::server::bookmarks::Bookmark) -> Vec<String> {
    trx_protocol::decoders::resolve_bookmark_decoders(&bookmark.decoders, &bookmark.mode, true)
}

// ============================================================================
// /events SSE endpoint
// ============================================================================

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
    // the global default.
    let active_rig_id = query.remote.clone().filter(|s| !s.is_empty()).or_else(|| {
        context
            .routing
            .active_rig_id
            .lock()
            .ok()
            .and_then(|g| g.clone())
    });

    // Subscribe to the per-rig watch channel for this session's rig.
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

// ============================================================================
// /meter SSE endpoint (fast signal-strength stream, ~30 Hz)
// ============================================================================

fn encode_meter_frame(update: &MeterUpdate) -> String {
    // Compact JSON: one-line SSE frame, flushed immediately.
    // Shape: {"sig":-72.3,"ts":12345}
    format!(
        "data: {{\"sig\":{:.2},\"ts\":{}}}\n\n",
        update.sig_dbm, update.ts_ms
    )
}

/// SSE stream for per-rig signal-strength updates.
///
/// Pushed from the server's per-rig meter broadcast; intentionally bypasses
/// the `/events` RigState path so high-rate meter samples are never gated by
/// full-state diffing. Each watch update produces exactly one SSE frame.
#[get("/meter")]
pub async fn meter(
    query: web::Query<RemoteQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    let rig_id = query.remote.clone().filter(|s| !s.is_empty()).or_else(|| {
        context
            .routing
            .active_rig_id
            .lock()
            .ok()
            .and_then(|g| g.clone())
    });

    let rx = match rig_id.as_deref() {
        Some(rid) => context.rig_meter_rx(rid),
        None => return Ok(HttpResponse::NotFound().finish()),
    };

    let updates = WatchStream::new(rx).filter_map(|maybe| {
        let chunk = maybe.as_ref().map(encode_meter_frame);
        std::future::ready(chunk.map(|s| Ok::<Bytes, Error>(Bytes::from(s))))
    });

    // Infrequent keepalive comment; real meter frames carry the heartbeat.
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

// ============================================================================
// /spectrum SSE endpoint
// ============================================================================

/// SSE stream for spectrum data.
#[get("/spectrum")]
pub async fn spectrum(
    query: web::Query<RemoteQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
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
