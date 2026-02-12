// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio WebSocket endpoint for the HTTP frontend.
//!
//! Exposes `/audio` which upgrades to a WebSocket:
//! - First text message: JSON `AudioStreamInfo`
//! - Subsequent binary messages: raw Opus packets (RX)
//! - Browser sends binary messages: raw Opus packets (TX)

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use actix_web::{get, web, Error, HttpRequest, HttpResponse};
use actix_ws::Message;
use bytes::Bytes;
use tokio::sync::broadcast;
use tracing::warn;

use trx_core::decode::{AprsPacket, CwEvent, DecodedMessage, Ft8Message};
use trx_frontend::FrontendRuntimeContext;

const HISTORY_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

fn prune_aprs_history(history: &mut VecDeque<(Instant, AprsPacket)>) {
    while let Some((ts, _)) = history.front() {
        if ts.elapsed() <= HISTORY_RETENTION {
            break;
        }
        history.pop_front();
    }
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

fn record_aprs(context: &FrontendRuntimeContext, pkt: AprsPacket) {
    let mut history = context
        .aprs_history
        .lock()
        .expect("aprs history mutex poisoned");
    history.push_back((Instant::now(), pkt));
    prune_aprs_history(&mut history);
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

pub fn snapshot_aprs_history(context: &FrontendRuntimeContext) -> Vec<AprsPacket> {
    let mut history = context
        .aprs_history
        .lock()
        .expect("aprs history mutex poisoned");
    prune_aprs_history(&mut history);
    history.iter().map(|(_, pkt)| pkt.clone()).collect()
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

pub fn clear_aprs_history(context: &FrontendRuntimeContext) {
    let mut history = context
        .aprs_history
        .lock()
        .expect("aprs history mutex poisoned");
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
                    DecodedMessage::Aprs(pkt) => record_aprs(&context, pkt),
                    DecodedMessage::Cw(evt) => record_cw(&context, evt),
                    DecodedMessage::Ft8(msg) => record_ft8(&context, msg),
                },
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

#[get("/audio")]
pub async fn audio_ws(
    req: HttpRequest,
    body: web::Payload,
    context: web::Data<Arc<FrontendRuntimeContext>>,
) -> Result<HttpResponse, Error> {
    let Some(rx) = context.audio_rx.as_ref() else {
        return Ok(HttpResponse::NotFound().body("audio not enabled"));
    };
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

    let mut rx_sub = rx.subscribe();

    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, body)?;

    actix_web::rt::spawn(async move {
        let info = loop {
            if let Some(info) = info_rx.borrow().clone() {
                break info;
            }
            if info_rx.changed().await.is_err() {
                let _ = session.close(None).await;
                return;
            }
        };

        let info_json = match serde_json::to_string(&info) {
            Ok(j) => j,
            Err(_) => {
                let _ = session.close(None).await;
                return;
            }
        };
        if session.text(info_json).await.is_err() {
            return;
        }

        let mut rx_session = session.clone();
        let rx_handle = actix_web::rt::spawn(async move {
            loop {
                match rx_sub.recv().await {
                    Ok(packet) => {
                        if rx_session.binary(packet).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Audio WS: dropped {} RX frames", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        while let Some(Ok(msg)) = msg_stream.recv().await {
            match msg {
                Message::Binary(data) => {
                    let _ = tx_sender.send(Bytes::from(data.to_vec())).await;
                }
                Message::Close(_) => break,
                _ => {}
            }
        }

        rx_handle.abort();
        let _ = session.close(None).await;
    });

    Ok(response)
}
