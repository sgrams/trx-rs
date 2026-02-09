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
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use actix_web::{get, web, Error, HttpRequest, HttpResponse};
use actix_ws::Message;
use bytes::Bytes;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::warn;

use trx_core::audio::AudioStreamInfo;
use trx_core::decode::{AprsPacket, CwEvent, DecodedMessage, Ft8Message};

struct AudioChannels {
    rx: broadcast::Sender<Bytes>,
    tx: mpsc::Sender<Bytes>,
    info: watch::Receiver<Option<AudioStreamInfo>>,
}

fn audio_channels() -> &'static Mutex<Option<AudioChannels>> {
    static CHANNELS: OnceLock<Mutex<Option<AudioChannels>>> = OnceLock::new();
    CHANNELS.get_or_init(|| Mutex::new(None))
}

/// Set the audio channels from the client main. Must be called before the
/// HTTP server starts if audio is enabled.
pub fn set_audio_channels(
    rx: broadcast::Sender<Bytes>,
    tx: mpsc::Sender<Bytes>,
    info: watch::Receiver<Option<AudioStreamInfo>>,
) {
    let mut ch = audio_channels()
        .lock()
        .expect("audio channels mutex poisoned");
    *ch = Some(AudioChannels { rx, tx, info });
}

fn decode_channel() -> &'static Mutex<Option<broadcast::Sender<DecodedMessage>>> {
    static CHANNEL: OnceLock<Mutex<Option<broadcast::Sender<DecodedMessage>>>> = OnceLock::new();
    CHANNEL.get_or_init(|| Mutex::new(None))
}

const HISTORY_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

fn aprs_history() -> &'static Mutex<VecDeque<(Instant, AprsPacket)>> {
    static HISTORY: OnceLock<Mutex<VecDeque<(Instant, AprsPacket)>>> = OnceLock::new();
    HISTORY.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn cw_history() -> &'static Mutex<VecDeque<(Instant, CwEvent)>> {
    static HISTORY: OnceLock<Mutex<VecDeque<(Instant, CwEvent)>>> = OnceLock::new();
    HISTORY.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn ft8_history() -> &'static Mutex<VecDeque<(Instant, Ft8Message)>> {
    static HISTORY: OnceLock<Mutex<VecDeque<(Instant, Ft8Message)>>> = OnceLock::new();
    HISTORY.get_or_init(|| Mutex::new(VecDeque::new()))
}

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

fn record_aprs(pkt: AprsPacket) {
    let mut history = aprs_history().lock().expect("aprs history mutex poisoned");
    history.push_back((Instant::now(), pkt));
    prune_aprs_history(&mut history);
}

fn record_cw(event: CwEvent) {
    let mut history = cw_history().lock().expect("cw history mutex poisoned");
    history.push_back((Instant::now(), event));
    prune_cw_history(&mut history);
}

fn record_ft8(msg: Ft8Message) {
    let mut history = ft8_history().lock().expect("ft8 history mutex poisoned");
    history.push_back((Instant::now(), msg));
    prune_ft8_history(&mut history);
}

pub fn snapshot_aprs_history() -> Vec<AprsPacket> {
    let mut history = aprs_history().lock().expect("aprs history mutex poisoned");
    prune_aprs_history(&mut history);
    history.iter().map(|(_, pkt)| pkt.clone()).collect()
}

pub fn snapshot_cw_history() -> Vec<CwEvent> {
    let mut history = cw_history().lock().expect("cw history mutex poisoned");
    prune_cw_history(&mut history);
    history.iter().map(|(_, evt)| evt.clone()).collect()
}

pub fn snapshot_ft8_history() -> Vec<Ft8Message> {
    let mut history = ft8_history().lock().expect("ft8 history mutex poisoned");
    prune_ft8_history(&mut history);
    history.iter().map(|(_, msg)| msg.clone()).collect()
}

pub fn clear_aprs_history() {
    let mut history = aprs_history().lock().expect("aprs history mutex poisoned");
    history.clear();
}

pub fn clear_cw_history() {
    let mut history = cw_history().lock().expect("cw history mutex poisoned");
    history.clear();
}

pub fn clear_ft8_history() {
    let mut history = ft8_history().lock().expect("ft8 history mutex poisoned");
    history.clear();
}

/// Set the decode broadcast channel from the client main.
pub fn set_decode_channel(tx: broadcast::Sender<DecodedMessage>) {
    let mut ch = decode_channel()
        .lock()
        .expect("decode channel mutex poisoned");
    *ch = Some(tx);
    start_decode_history_collector();
}

/// Subscribe to the decode broadcast channel, if available.
pub fn subscribe_decode() -> Option<broadcast::Receiver<DecodedMessage>> {
    let ch = decode_channel()
        .lock()
        .expect("decode channel mutex poisoned");
    ch.as_ref().map(|tx| tx.subscribe())
}

fn start_decode_history_collector() {
    static STARTED: OnceLock<Mutex<bool>> = OnceLock::new();
    let started = STARTED.get_or_init(|| Mutex::new(false));
    let mut started_guard = started.lock().expect("decode history start mutex poisoned");
    if *started_guard {
        return;
    }
    *started_guard = true;

    let ch = decode_channel()
        .lock()
        .expect("decode channel mutex poisoned");
    let Some(tx) = ch.as_ref().cloned() else {
        return;
    };

    tokio::spawn(async move {
        let mut rx = tx.subscribe();
        loop {
            match rx.recv().await {
                Ok(msg) => match msg {
                    DecodedMessage::Aprs(pkt) => record_aprs(pkt),
                    DecodedMessage::Cw(evt) => record_cw(evt),
                    DecodedMessage::Ft8(msg) => record_ft8(msg),
                },
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

#[get("/audio")]
pub async fn audio_ws(req: HttpRequest, body: web::Payload) -> Result<HttpResponse, Error> {
    let channels = audio_channels().lock().expect("audio channels mutex poisoned");
    let Some(ref ch) = *channels else {
        return Ok(HttpResponse::NotFound().body("audio not enabled"));
    };

    // Plain GET probe (no WebSocket upgrade) — return 204 to signal audio is available
    if !req.headers().contains_key("upgrade") {
        return Ok(HttpResponse::NoContent().finish());
    }

    let mut rx_sub = ch.rx.subscribe();
    let tx_sender = ch.tx.clone();
    let mut info_rx = ch.info.clone();
    drop(channels);

    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, body)?;

    // Spawn the WebSocket handler
    actix_web::rt::spawn(async move {
        // Wait for stream info and send as first text message
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

        // Spawn RX forwarding task
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

        // Read TX frames from browser
        while let Some(Ok(msg)) = msg_stream.recv().await {
            match msg {
                Message::Binary(data) => {
                    let _ = tx_sender.send(data).await;
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
