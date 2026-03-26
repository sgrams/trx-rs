// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
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

fn current_timestamp_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn decode_history_retention(context: &FrontendRuntimeContext) -> Duration {
    let default_minutes = context.http_decode_history_retention_min.max(1);
    let minutes = context
        .remote_active_rig_id
        .lock()
        .ok()
        .and_then(|v| v.clone())
        .and_then(|rig_id| {
            context
                .http_decode_history_retention_min_by_rig
                .get(&rig_id)
                .copied()
        })
        .filter(|minutes| *minutes > 0)
        .unwrap_or(default_minutes);
    Duration::from_secs(minutes.saturating_mul(60))
}

fn decode_history_cutoff(context: &FrontendRuntimeContext) -> Instant {
    Instant::now() - decode_history_retention(context)
}

fn prune_aprs_history(
    context: &FrontendRuntimeContext,
    history: &mut VecDeque<(Instant, Option<String>, AprsPacket)>,
) {
    let cutoff = decode_history_cutoff(context);
    while let Some((ts, _, _)) = history.front() {
        if *ts >= cutoff {
            break;
        }
        history.pop_front();
    }
}

fn prune_hf_aprs_history(
    context: &FrontendRuntimeContext,
    history: &mut VecDeque<(Instant, Option<String>, AprsPacket)>,
) {
    let cutoff = decode_history_cutoff(context);
    while let Some((ts, _, _)) = history.front() {
        if *ts >= cutoff {
            break;
        }
        history.pop_front();
    }
}

fn prune_ais_history(
    context: &FrontendRuntimeContext,
    history: &mut VecDeque<(Instant, Option<String>, AisMessage)>,
) {
    let cutoff = decode_history_cutoff(context);
    while let Some((ts, _, _)) = history.front() {
        if *ts >= cutoff {
            break;
        }
        history.pop_front();
    }
}

fn prune_vdes_history(
    context: &FrontendRuntimeContext,
    history: &mut VecDeque<(Instant, Option<String>, VdesMessage)>,
) {
    let cutoff = decode_history_cutoff(context);
    while let Some((ts, _, _)) = history.front() {
        if *ts >= cutoff {
            break;
        }
        history.pop_front();
    }
}

fn active_rig_id(context: &FrontendRuntimeContext) -> Option<String> {
    context
        .remote_active_rig_id
        .lock()
        .ok()
        .and_then(|g| g.clone())
}

fn record_ais(context: &FrontendRuntimeContext, mut msg: AisMessage) {
    if msg.ts_ms.is_none() {
        msg.ts_ms = Some(current_timestamp_ms());
    }
    let rig_id = msg.rig_id.clone().or_else(|| active_rig_id(context));
    let mut history = context
        .ais_history
        .lock()
        .expect("ais history mutex poisoned");
    history.push_back((Instant::now(), rig_id, msg));
    prune_ais_history(context, &mut history);
}

fn record_vdes(context: &FrontendRuntimeContext, mut msg: VdesMessage) {
    if msg.ts_ms.is_none() {
        msg.ts_ms = Some(current_timestamp_ms());
    }
    let rig_id = msg.rig_id.clone().or_else(|| active_rig_id(context));
    let mut history = context
        .vdes_history
        .lock()
        .expect("vdes history mutex poisoned");
    history.push_back((Instant::now(), rig_id, msg));
    prune_vdes_history(context, &mut history);
}

fn prune_cw_history(
    context: &FrontendRuntimeContext,
    history: &mut VecDeque<(Instant, Option<String>, CwEvent)>,
) {
    let cutoff = decode_history_cutoff(context);
    while let Some((ts, _, _)) = history.front() {
        if *ts >= cutoff {
            break;
        }
        history.pop_front();
    }
}

fn prune_ft8_history(
    context: &FrontendRuntimeContext,
    history: &mut VecDeque<(Instant, Option<String>, Ft8Message)>,
) {
    let cutoff = decode_history_cutoff(context);
    while let Some((ts, _, _)) = history.front() {
        if *ts >= cutoff {
            break;
        }
        history.pop_front();
    }
}

fn prune_ft4_history(
    context: &FrontendRuntimeContext,
    history: &mut VecDeque<(Instant, Option<String>, Ft8Message)>,
) {
    let cutoff = decode_history_cutoff(context);
    while let Some((ts, _, _)) = history.front() {
        if *ts >= cutoff {
            break;
        }
        history.pop_front();
    }
}

fn prune_ft2_history(
    context: &FrontendRuntimeContext,
    history: &mut VecDeque<(Instant, Option<String>, Ft8Message)>,
) {
    let cutoff = decode_history_cutoff(context);
    while let Some((ts, _, _)) = history.front() {
        if *ts >= cutoff {
            break;
        }
        history.pop_front();
    }
}

fn prune_wspr_history(
    context: &FrontendRuntimeContext,
    history: &mut VecDeque<(Instant, Option<String>, WsprMessage)>,
) {
    let cutoff = decode_history_cutoff(context);
    while let Some((ts, _, _)) = history.front() {
        if *ts >= cutoff {
            break;
        }
        history.pop_front();
    }
}

fn record_aprs(context: &FrontendRuntimeContext, mut pkt: AprsPacket) {
    if pkt.ts_ms.is_none() {
        pkt.ts_ms = Some(current_timestamp_ms());
    }
    let rig_id = pkt.rig_id.clone().or_else(|| active_rig_id(context));
    let mut history = context
        .aprs_history
        .lock()
        .expect("aprs history mutex poisoned");
    history.push_back((Instant::now(), rig_id, pkt));
    prune_aprs_history(context, &mut history);
}

fn record_hf_aprs(context: &FrontendRuntimeContext, mut pkt: AprsPacket) {
    if pkt.ts_ms.is_none() {
        pkt.ts_ms = Some(current_timestamp_ms());
    }
    let rig_id = pkt.rig_id.clone().or_else(|| active_rig_id(context));
    let mut history = context
        .hf_aprs_history
        .lock()
        .expect("hf_aprs history mutex poisoned");
    history.push_back((Instant::now(), rig_id, pkt));
    prune_hf_aprs_history(context, &mut history);
}

fn record_cw(context: &FrontendRuntimeContext, event: CwEvent) {
    let rig_id = event.rig_id.clone().or_else(|| active_rig_id(context));
    let mut history = context
        .cw_history
        .lock()
        .expect("cw history mutex poisoned");
    history.push_back((Instant::now(), rig_id, event));
    prune_cw_history(context, &mut history);
}

fn record_ft8(context: &FrontendRuntimeContext, msg: Ft8Message) {
    let rig_id = msg.rig_id.clone().or_else(|| active_rig_id(context));
    let mut history = context
        .ft8_history
        .lock()
        .expect("ft8 history mutex poisoned");
    history.push_back((Instant::now(), rig_id, msg));
    prune_ft8_history(context, &mut history);
}

fn record_ft4(context: &FrontendRuntimeContext, msg: Ft8Message) {
    let rig_id = msg.rig_id.clone().or_else(|| active_rig_id(context));
    let mut history = context
        .ft4_history
        .lock()
        .expect("ft4 history mutex poisoned");
    history.push_back((Instant::now(), rig_id, msg));
    prune_ft4_history(context, &mut history);
}

fn record_ft2(context: &FrontendRuntimeContext, msg: Ft8Message) {
    let rig_id = msg.rig_id.clone().or_else(|| active_rig_id(context));
    let mut history = context
        .ft2_history
        .lock()
        .expect("ft2 history mutex poisoned");
    history.push_back((Instant::now(), rig_id, msg));
    prune_ft2_history(context, &mut history);
}

fn record_wspr(context: &FrontendRuntimeContext, msg: WsprMessage) {
    let rig_id = msg.rig_id.clone().or_else(|| active_rig_id(context));
    let mut history = context
        .wspr_history
        .lock()
        .expect("wspr history mutex poisoned");
    history.push_back((Instant::now(), rig_id, msg));
    prune_wspr_history(context, &mut history);
}

/// Returns `true` if the entry's rig_id matches the optional filter.
/// `None` filter means "all rigs".
fn matches_rig_filter(entry_rig: Option<&str>, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(f) => entry_rig == Some(f),
    }
}

pub fn snapshot_aprs_history(
    context: &FrontendRuntimeContext,
    rig_filter: Option<&str>,
) -> Vec<AprsPacket> {
    let mut history = context
        .aprs_history
        .lock()
        .expect("aprs history mutex poisoned");
    prune_aprs_history(context, &mut history);
    history
        .iter()
        .filter(|(_, rid, _)| matches_rig_filter(rid.as_deref(), rig_filter))
        .map(|(_, _, pkt)| pkt.clone())
        .collect()
}

pub fn snapshot_hf_aprs_history(
    context: &FrontendRuntimeContext,
    rig_filter: Option<&str>,
) -> Vec<AprsPacket> {
    let mut history = context
        .hf_aprs_history
        .lock()
        .expect("hf_aprs history mutex poisoned");
    prune_hf_aprs_history(context, &mut history);
    history
        .iter()
        .filter(|(_, rid, _)| matches_rig_filter(rid.as_deref(), rig_filter))
        .map(|(_, _, pkt)| pkt.clone())
        .collect()
}

/// Return the latest message per MMSI seen within the retention window.
///
/// AIS vessels transmit every 2–30 s; returning every individual message would
/// produce a response too large to be useful. One entry per vessel matches
/// what the map shows (current position/state) and keeps the response compact.
/// The returned vec is sorted ascending by `ts_ms` so the client can replay
/// in chronological order.
pub fn snapshot_ais_history(
    context: &FrontendRuntimeContext,
    rig_filter: Option<&str>,
) -> Vec<AisMessage> {
    let mut history = context
        .ais_history
        .lock()
        .expect("ais history mutex poisoned");
    prune_ais_history(context, &mut history);
    // Iterate oldest-first; later entries overwrite earlier ones so the
    // HashMap always holds the newest message per MMSI.
    let mut latest: HashMap<u32, AisMessage> = HashMap::new();
    for (_, rid, msg) in history.iter() {
        if matches_rig_filter(rid.as_deref(), rig_filter) {
            latest.insert(msg.mmsi, msg.clone());
        }
    }
    let mut out: Vec<AisMessage> = latest.into_values().collect();
    out.sort_by_key(|m| m.ts_ms.unwrap_or(0));
    out
}

pub fn snapshot_vdes_history(
    context: &FrontendRuntimeContext,
    rig_filter: Option<&str>,
) -> Vec<VdesMessage> {
    let mut history = context
        .vdes_history
        .lock()
        .expect("vdes history mutex poisoned");
    prune_vdes_history(context, &mut history);
    history
        .iter()
        .filter(|(_, rid, _)| matches_rig_filter(rid.as_deref(), rig_filter))
        .map(|(_, _, msg)| msg.clone())
        .collect()
}

pub fn snapshot_cw_history(
    context: &FrontendRuntimeContext,
    rig_filter: Option<&str>,
) -> Vec<CwEvent> {
    let mut history = context
        .cw_history
        .lock()
        .expect("cw history mutex poisoned");
    prune_cw_history(context, &mut history);
    history
        .iter()
        .filter(|(_, rid, _)| matches_rig_filter(rid.as_deref(), rig_filter))
        .map(|(_, _, evt)| evt.clone())
        .collect()
}

pub fn snapshot_ft8_history(
    context: &FrontendRuntimeContext,
    rig_filter: Option<&str>,
) -> Vec<Ft8Message> {
    let mut history = context
        .ft8_history
        .lock()
        .expect("ft8 history mutex poisoned");
    prune_ft8_history(context, &mut history);
    history
        .iter()
        .filter(|(_, rid, _)| matches_rig_filter(rid.as_deref(), rig_filter))
        .map(|(_, _, msg)| msg.clone())
        .collect()
}

pub fn snapshot_ft4_history(
    context: &FrontendRuntimeContext,
    rig_filter: Option<&str>,
) -> Vec<Ft8Message> {
    let mut history = context
        .ft4_history
        .lock()
        .expect("ft4 history mutex poisoned");
    prune_ft4_history(context, &mut history);
    history
        .iter()
        .filter(|(_, rid, _)| matches_rig_filter(rid.as_deref(), rig_filter))
        .map(|(_, _, msg)| msg.clone())
        .collect()
}

pub fn snapshot_ft2_history(
    context: &FrontendRuntimeContext,
    rig_filter: Option<&str>,
) -> Vec<Ft8Message> {
    let mut history = context
        .ft2_history
        .lock()
        .expect("ft2 history mutex poisoned");
    prune_ft2_history(context, &mut history);
    history
        .iter()
        .filter(|(_, rid, _)| matches_rig_filter(rid.as_deref(), rig_filter))
        .map(|(_, _, msg)| msg.clone())
        .collect()
}

pub fn snapshot_wspr_history(
    context: &FrontendRuntimeContext,
    rig_filter: Option<&str>,
) -> Vec<WsprMessage> {
    let mut history = context
        .wspr_history
        .lock()
        .expect("wspr history mutex poisoned");
    prune_wspr_history(context, &mut history);
    history
        .iter()
        .filter(|(_, rid, _)| matches_rig_filter(rid.as_deref(), rig_filter))
        .map(|(_, _, msg)| msg.clone())
        .collect()
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

pub fn clear_ft4_history(context: &FrontendRuntimeContext) {
    let mut history = context
        .ft4_history
        .lock()
        .expect("ft4 history mutex poisoned");
    history.clear();
}

pub fn clear_ft2_history(context: &FrontendRuntimeContext) {
    let mut history = context
        .ft2_history
        .lock()
        .expect("ft2 history mutex poisoned");
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
                    DecodedMessage::Ft4(msg) => record_ft4(&context, msg),
                    DecodedMessage::Ft2(msg) => record_ft2(&context, msg),
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
    pub remote: Option<String>,
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

    // Plain GET probe (no WebSocket upgrade) - return 204 to signal audio is available.
    if !req.headers().contains_key("upgrade") {
        return Ok(HttpResponse::NoContent().finish());
    }

    // If a channel_id is specified, subscribe to the per-channel broadcaster.
    // The entry is created asynchronously when AUDIO_MSG_VCHAN_ALLOCATED arrives
    // from the server, which may lag the HTTP allocation by up to ~100 ms.
    // Poll for up to 2 s so a tight JS timer doesn't race and get a 404.
    let (rx_sub, mut info_rx): (
        broadcast::Receiver<Bytes>,
        tokio::sync::watch::Receiver<Option<trx_core::audio::AudioStreamInfo>>,
    ) = if let Some(ch_id) = query.channel_id {
        let info_rx = if let Some(ref remote) = query.remote {
            context.rig_audio_info_rx(remote)
        } else {
            context.audio_info.as_ref().cloned()
        };
        let Some(info_rx) = info_rx else {
            return Ok(HttpResponse::NotFound().body("audio not enabled"));
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let rx_sub = loop {
            match context.vchan_audio.read() {
                Ok(map) => {
                    if let Some(tx) = map.get(&ch_id) {
                        break tx.subscribe();
                    }
                }
                Err(_) => return Ok(HttpResponse::InternalServerError().finish()),
            }
            if Instant::now() >= deadline {
                return Ok(HttpResponse::NotFound().body("channel not found"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        (rx_sub, info_rx)
    } else if let Some(ref remote) = query.remote {
        // Per-rig audio: subscribe to the specific rig's broadcast.
        // Do NOT fall back to global — that would silently deliver the wrong
        // rig's audio. Wait briefly for the per-rig channel to appear (it is
        // lazily created by the audio relay sync task every 500ms).
        let deadline = Instant::now() + Duration::from_secs(3);
        let (rx_sub, info_rx) = loop {
            if let (Some(rx), Some(info_rx)) = (
                context.rig_audio_subscribe(remote),
                context.rig_audio_info_rx(remote),
            ) {
                break (rx, info_rx);
            }
            if Instant::now() >= deadline {
                return Ok(
                    HttpResponse::NotFound().body(format!("audio not available for rig {remote}"))
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        (rx_sub, info_rx)
    } else {
        let Some(info_rx) = context.audio_info.as_ref().cloned() else {
            return Ok(HttpResponse::NotFound().body("audio not enabled"));
        };
        let Some(rx) = context.audio_rx.as_ref() else {
            return Ok(HttpResponse::NotFound().body("audio not enabled"));
        };
        (rx.subscribe(), info_rx)
    };
    let mut rx_sub = rx_sub;

    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, body)?;

    let audio_clients = context.audio_clients.clone();
    audio_clients.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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
                                || next_info.frame_duration_ms != current_info.frame_duration_ms
                                || next_info.bitrate_bps != current_info.bitrate_bps;
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
        audio_clients.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    });

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::AudioQuery;
    use uuid::Uuid;

    #[test]
    fn audio_query_accepts_remote() {
        let query: AudioQuery =
            serde_json::from_str(r#"{"remote":"lidzbark-vhf"}"#).expect("query parse");
        assert_eq!(query.remote.as_deref(), Some("lidzbark-vhf"));
    }

    #[test]
    fn audio_query_accepts_channel_id_with_remote() {
        let channel_id = Uuid::new_v4();
        let query: AudioQuery = serde_json::from_str(&format!(
            r#"{{"channel_id":"{channel_id}","remote":"lidzbark-vhf"}}"#
        ))
        .expect("query parse");
        assert_eq!(query.channel_id, Some(channel_id));
        assert_eq!(query.remote.as_deref(), Some("lidzbark-vhf"));
    }
}
