// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio WebSocket endpoint for the HTTP frontend.
//!
//! Exposes `/audio` which upgrades to a WebSocket:
//! - First text message: JSON `AudioStreamInfo`
//! - Subsequent binary messages: raw Opus packets (RX)
//! - Browser sends binary messages: raw Opus packets (TX)

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use actix_web::{get, web, Error, HttpRequest, HttpResponse};
use actix_ws::Message;
use bytes::Bytes;
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::warn;
use uuid::Uuid;

use trx_core::decode::{
    AisMessage, AprsPacket, CwEvent, DecodedMessage, Ft8Message, VdesMessage, WsprMessage,
};
use trx_frontend::FrontendRuntimeContext;

const HISTORY_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
/// Maximum number of raw AIS messages kept in the ring buffer.
/// AIS vessels can transmit every 2 s, so without a cap the buffer grows
/// unboundedly. 10 000 entries covers ~100 active vessels at 2-second intervals
/// for ~3 minutes — enough for a realistic snapshot while bounding memory use.
const AIS_HISTORY_MAX: usize = 10_000;

fn current_timestamp_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn prune_aprs_history(history: &mut VecDeque<(Instant, AprsPacket)>) {
    while let Some((ts, _)) = history.front() {
        if ts.elapsed() <= HISTORY_RETENTION {
            break;
        }
        history.pop_front();
    }
}

fn prune_hf_aprs_history(history: &mut VecDeque<(Instant, AprsPacket)>) {
    while let Some((ts, _)) = history.front() {
        if ts.elapsed() <= HISTORY_RETENTION {
            break;
        }
        history.pop_front();
    }
}

fn prune_ais_history(history: &mut VecDeque<(Instant, AisMessage)>) {
    while let Some((ts, _)) = history.front() {
        if ts.elapsed() <= HISTORY_RETENTION {
            break;
        }
        history.pop_front();
    }
}

fn prune_vdes_history(history: &mut VecDeque<(Instant, VdesMessage)>) {
    while let Some((ts, _)) = history.front() {
        if ts.elapsed() <= HISTORY_RETENTION {
            break;
        }
        history.pop_front();
    }
}

fn record_ais(context: &FrontendRuntimeContext, mut msg: AisMessage) {
    if msg.ts_ms.is_none() {
        msg.ts_ms = Some(current_timestamp_ms());
    }
    let mut history = context
        .ais_history
        .lock()
        .expect("ais history mutex poisoned");
    history.push_back((Instant::now(), msg));
    prune_ais_history(&mut history);
    if history.len() > AIS_HISTORY_MAX {
        history.pop_front();
    }
}

fn record_vdes(context: &FrontendRuntimeContext, mut msg: VdesMessage) {
    if msg.ts_ms.is_none() {
        msg.ts_ms = Some(current_timestamp_ms());
    }
    let mut history = context
        .vdes_history
        .lock()
        .expect("vdes history mutex poisoned");
    history.push_back((Instant::now(), msg));
    prune_vdes_history(&mut history);
}

fn prune_cw_history(history: &mut VecDeque<(Instant, CwEvent)>) {
    while let Some((ts, _)) = history.front() {
        if ts.elapsed() <= HISTORY_RETENTION {
            break;
        }
        history.pop_front();
    }
}

fn prune_ft8_history(history: &mut VecDeque<(Instant, Ft8Message)>) {
    while let Some((ts, _)) = history.front() {
        if ts.elapsed() <= HISTORY_RETENTION {
            break;
        }
        history.pop_front();
    }
}

fn prune_wspr_history(history: &mut VecDeque<(Instant, WsprMessage)>) {
    while let Some((ts, _)) = history.front() {
        if ts.elapsed() <= HISTORY_RETENTION {
            break;
        }
        history.pop_front();
    }
}

fn record_aprs(context: &FrontendRuntimeContext, mut pkt: AprsPacket) {
    if pkt.ts_ms.is_none() {
        pkt.ts_ms = Some(current_timestamp_ms());
    }
    let mut history = context
        .aprs_history
        .lock()
        .expect("aprs history mutex poisoned");
    history.push_back((Instant::now(), pkt));
    prune_aprs_history(&mut history);
}

fn record_hf_aprs(context: &FrontendRuntimeContext, mut pkt: AprsPacket) {
    if pkt.ts_ms.is_none() {
        pkt.ts_ms = Some(current_timestamp_ms());
    }
    let mut history = context
        .hf_aprs_history
        .lock()
        .expect("hf_aprs history mutex poisoned");
    history.push_back((Instant::now(), pkt));
    prune_hf_aprs_history(&mut history);
}

fn record_cw(context: &FrontendRuntimeContext, event: CwEvent) {
    let mut history = context
        .cw_history
        .lock()
        .expect("cw history mutex poisoned");
    history.push_back((Instant::now(), event));
    prune_cw_history(&mut history);
}

fn record_ft8(context: &FrontendRuntimeContext, msg: Ft8Message) {
    let mut history = context
        .ft8_history
        .lock()
        .expect("ft8 history mutex poisoned");
    history.push_back((Instant::now(), msg));
    prune_ft8_history(&mut history);
}

fn record_wspr(context: &FrontendRuntimeContext, msg: WsprMessage) {
    let mut history = context
        .wspr_history
        .lock()
        .expect("wspr history mutex poisoned");
    history.push_back((Instant::now(), msg));
    prune_wspr_history(&mut history);
}

pub fn snapshot_aprs_history(context: &FrontendRuntimeContext) -> Vec<AprsPacket> {
    let mut history = context
        .aprs_history
        .lock()
        .expect("aprs history mutex poisoned");
    prune_aprs_history(&mut history);
    history.iter().map(|(_, pkt)| pkt.clone()).collect()
}

pub fn snapshot_hf_aprs_history(context: &FrontendRuntimeContext) -> Vec<AprsPacket> {
    let mut history = context
        .hf_aprs_history
        .lock()
        .expect("hf_aprs history mutex poisoned");
    prune_hf_aprs_history(&mut history);
    history.iter().map(|(_, pkt)| pkt.clone()).collect()
}

/// Return the latest message per MMSI seen within the retention window.
///
/// AIS vessels transmit every 2–30 s; returning every individual message would
/// produce a response too large to be useful. One entry per vessel matches
/// what the map shows (current position/state) and keeps the response compact.
/// The returned vec is sorted ascending by `ts_ms` so the client can replay
/// in chronological order.
pub fn snapshot_ais_history(context: &FrontendRuntimeContext) -> Vec<AisMessage> {
    let mut history = context
        .ais_history
        .lock()
        .expect("ais history mutex poisoned");
    prune_ais_history(&mut history);
    // Iterate oldest-first; later entries overwrite earlier ones so the
    // HashMap always holds the newest message per MMSI.
    let mut latest: HashMap<u32, AisMessage> = HashMap::new();
    for (_, msg) in history.iter() {
        latest.insert(msg.mmsi, msg.clone());
    }
    let mut out: Vec<AisMessage> = latest.into_values().collect();
    out.sort_by_key(|m| m.ts_ms.unwrap_or(0));
    out
}

pub fn snapshot_vdes_history(context: &FrontendRuntimeContext) -> Vec<VdesMessage> {
    let mut history = context
        .vdes_history
        .lock()
        .expect("vdes history mutex poisoned");
    prune_vdes_history(&mut history);
    history.iter().map(|(_, msg)| msg.clone()).collect()
}

pub fn snapshot_cw_history(context: &FrontendRuntimeContext) -> Vec<CwEvent> {
    let mut history = context
        .cw_history
        .lock()
        .expect("cw history mutex poisoned");
    prune_cw_history(&mut history);
    history.iter().map(|(_, evt)| evt.clone()).collect()
}

pub fn snapshot_ft8_history(context: &FrontendRuntimeContext) -> Vec<Ft8Message> {
    let mut history = context
        .ft8_history
        .lock()
        .expect("ft8 history mutex poisoned");
    prune_ft8_history(&mut history);
    history.iter().map(|(_, msg)| msg.clone()).collect()
}

pub fn snapshot_wspr_history(context: &FrontendRuntimeContext) -> Vec<WsprMessage> {
    let mut history = context
        .wspr_history
        .lock()
        .expect("wspr history mutex poisoned");
    prune_wspr_history(&mut history);
    history.iter().map(|(_, msg)| msg.clone()).collect()
}

pub fn clear_aprs_history(context: &FrontendRuntimeContext) {
    let mut history = context
        .aprs_history
        .lock()
        .expect("aprs history mutex poisoned");
    history.clear();
}

pub fn clear_hf_aprs_history(context: &FrontendRuntimeContext) {
    let mut history = context
        .hf_aprs_history
        .lock()
        .expect("hf_aprs history mutex poisoned");
    history.clear();
}

pub fn clear_ais_history(context: &FrontendRuntimeContext) {
    let mut history = context
        .ais_history
        .lock()
        .expect("ais history mutex poisoned");
    history.clear();
}

pub fn clear_vdes_history(context: &FrontendRuntimeContext) {
    let mut history = context
        .vdes_history
        .lock()
        .expect("vdes history mutex poisoned");
    history.clear();
}

pub fn clear_cw_history(context: &FrontendRuntimeContext) {
    let mut history = context
        .cw_history
        .lock()
        .expect("cw history mutex poisoned");
    history.clear();
}

pub fn clear_ft8_history(context: &FrontendRuntimeContext) {
    let mut history = context
        .ft8_history
        .lock()
        .expect("ft8 history mutex poisoned");
    history.clear();
}

pub fn clear_wspr_history(context: &FrontendRuntimeContext) {
    let mut history = context
        .wspr_history
        .lock()
        .expect("wspr history mutex poisoned");
    history.clear();
}

pub fn subscribe_decode(
    context: &FrontendRuntimeContext,
) -> Option<broadcast::Receiver<DecodedMessage>> {
    context.decode_rx.as_ref().map(|tx| tx.subscribe())
}

pub fn start_decode_history_collector(context: Arc<FrontendRuntimeContext>) {
    if context
        .decode_collector_started
        .swap(true, Ordering::AcqRel)
    {
        return;
    }

    let Some(tx) = context.decode_rx.as_ref().cloned() else {
        return;
    };

    tokio::spawn(async move {
        let mut rx = tx.subscribe();
        loop {
            match rx.recv().await {
                Ok(msg) => match msg {
                    DecodedMessage::Ais(msg) => record_ais(&context, msg),
                    DecodedMessage::Vdes(msg) => record_vdes(&context, msg),
                    DecodedMessage::Aprs(pkt) => record_aprs(&context, pkt),
                    DecodedMessage::HfAprs(pkt) => record_hf_aprs(&context, pkt),
                    DecodedMessage::Cw(evt) => record_cw(&context, evt),
                    DecodedMessage::Ft8(msg) => record_ft8(&context, msg),
                    DecodedMessage::Wspr(msg) => record_wspr(&context, msg),
                },
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

#[derive(Deserialize)]
pub struct AudioQuery {
    pub channel_id: Option<Uuid>,
}

#[get("/audio")]
pub async fn audio_ws(
    req: HttpRequest,
    body: web::Payload,
    query: web::Query<AudioQuery>,
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    let Some(tx_sender) = context.audio_tx.as_ref().cloned() else {
        return Ok(HttpResponse::NotFound().body("audio not enabled"));
    };
    let Some(mut info_rx) = context.audio_info.as_ref().cloned() else {
        return Ok(HttpResponse::NotFound().body("audio not enabled"));
    };

    // Plain GET probe (no WebSocket upgrade) - return 204 to signal audio is available.
    if !req.headers().contains_key("upgrade") {
        return Ok(HttpResponse::NoContent().finish());
    }

    // If a channel_id is specified, subscribe to the per-channel broadcaster.
    // Otherwise fall back to the primary RX broadcast.
    let rx_sub: broadcast::Receiver<Bytes> = if let Some(ch_id) = query.channel_id {
        match context.vchan_audio.read() {
            Ok(map) => match map.get(&ch_id) {
                Some(tx) => tx.subscribe(),
                None => {
                    return Ok(HttpResponse::NotFound().body("channel not found"));
                }
            },
            Err(_) => return Ok(HttpResponse::InternalServerError().finish()),
        }
    } else {
        let Some(rx) = context.audio_rx.as_ref() else {
            return Ok(HttpResponse::NotFound().body("audio not enabled"));
        };
        rx.subscribe()
    };
    let mut rx_sub = rx_sub;

    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, body)?;

    actix_web::rt::spawn(async move {
        let mut current_info = loop {
            if let Some(info) = info_rx.borrow().clone() {
                break info;
            }
            if info_rx.changed().await.is_err() {
                let _ = session.close(None).await;
                return;
            }
        };

        let info_json = match serde_json::to_string(&current_info) {
            Ok(j) => j,
            Err(_) => {
                let _ = session.close(None).await;
                return;
            }
        };
        if session.text(info_json).await.is_err() {
            return;
        }

        loop {
            tokio::select! {
                changed = info_rx.changed() => {
                    match changed {
                        Ok(()) => {
                            let Some(next_info) = info_rx.borrow().clone() else {
                                continue;
                            };
                            let changed = next_info.sample_rate != current_info.sample_rate
                                || next_info.channels != current_info.channels
                                || next_info.frame_duration_ms != current_info.frame_duration_ms;
                            if changed {
                                current_info = next_info;
                                let info_json = match serde_json::to_string(&current_info) {
                                    Ok(j) => j,
                                    Err(_) => break,
                                };
                                if session.text(info_json).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                packet = rx_sub.recv() => {
                    match packet {
                        Ok(packet) => {
                            if session.binary(packet).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Audio WS: dropped {} RX frames", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                msg = msg_stream.recv() => {
                    match msg {
                        Some(Ok(Message::Binary(data))) => {
                            let _ = tx_sender.send(Bytes::from(data.to_vec())).await;
                        }
                        Some(Ok(Message::Close(_))) => break,
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => break,
                    }
                }
            }
        }
        let _ = session.close(None).await;
    });

    Ok(response)
}
