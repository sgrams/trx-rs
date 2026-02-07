// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio WebSocket endpoint for the HTTP frontend.
//!
//! Exposes `/audio` which upgrades to a WebSocket:
//! - First text message: JSON `AudioStreamInfo`
//! - Subsequent binary messages: raw Opus packets (RX)
//! - Browser sends binary messages: raw Opus packets (TX)

use std::sync::{Mutex, OnceLock};

use actix_web::{get, web, Error, HttpRequest, HttpResponse};
use actix_ws::Message;
use bytes::Bytes;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::warn;

use trx_core::audio::AudioStreamInfo;

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

#[get("/audio")]
pub async fn audio_ws(req: HttpRequest, body: web::Payload) -> Result<HttpResponse, Error> {
    let channels = audio_channels().lock().expect("audio channels mutex poisoned");
    let Some(ref ch) = *channels else {
        return Ok(HttpResponse::NotFound().body("audio not enabled"));
    };

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
