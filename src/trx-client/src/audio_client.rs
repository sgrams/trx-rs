// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio TCP client that connects to the server's audio port and relays
//! RX/TX Opus frames via broadcast/mpsc channels.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time;
use tracing::{info, warn};

use trx_core::audio::{
    read_audio_msg, write_audio_msg, AudioStreamInfo, AUDIO_MSG_APRS_DECODE, AUDIO_MSG_CW_DECODE,
    AUDIO_MSG_FT8_DECODE, AUDIO_MSG_RX_FRAME, AUDIO_MSG_STREAM_INFO, AUDIO_MSG_TX_FRAME,
    AUDIO_MSG_WSPR_DECODE,
};
use trx_core::decode::DecodedMessage;

/// Run the audio client with auto-reconnect.
pub async fn run_audio_client(
    server_host: String,
    default_port: u16,
    rig_ports: HashMap<String, u16>,
    selected_rig_id: Arc<Mutex<Option<String>>>,
    rx_tx: broadcast::Sender<Bytes>,
    mut tx_rx: mpsc::Receiver<Bytes>,
    stream_info_tx: watch::Sender<Option<AudioStreamInfo>>,
    decode_tx: broadcast::Sender<DecodedMessage>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut reconnect_delay = Duration::from_secs(1);

    loop {
        if *shutdown_rx.borrow() {
            info!("Audio client shutting down");
            return;
        }

        let server_addr = resolve_audio_addr(
            &server_host,
            default_port,
            &rig_ports,
            selected_rig_id
                .lock()
                .ok()
                .and_then(|v| v.clone())
                .as_deref(),
        );
        info!("Audio client: connecting to {}", server_addr);
        match TcpStream::connect(&server_addr).await {
            Ok(stream) => {
                reconnect_delay = Duration::from_secs(1);
                if let Err(e) = handle_audio_connection(
                    stream,
                    &server_host,
                    default_port,
                    &rig_ports,
                    &selected_rig_id,
                    &server_addr,
                    &rx_tx,
                    &mut tx_rx,
                    &stream_info_tx,
                    &decode_tx,
                    &mut shutdown_rx,
                )
                .await
                {
                    warn!("Audio connection dropped: {}", e);
                }
            }
            Err(e) => {
                warn!("Audio connect failed: {}", e);
            }
        }

        let _ = stream_info_tx.send(None);
        tokio::select! {
            _ = time::sleep(reconnect_delay) => {}
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => {
                        info!("Audio client shutting down");
                        return;
                    }
                    Ok(()) => {}
                    Err(_) => return,
                }
            }
        }
        reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(10));
    }
}

async fn handle_audio_connection(
    stream: TcpStream,
    server_host: &str,
    default_port: u16,
    rig_ports: &HashMap<String, u16>,
    selected_rig_id: &Arc<Mutex<Option<String>>>,
    connected_addr: &str,
    rx_tx: &broadcast::Sender<Bytes>,
    tx_rx: &mut mpsc::Receiver<Bytes>,
    stream_info_tx: &watch::Sender<Option<AudioStreamInfo>>,
    decode_tx: &broadcast::Sender<DecodedMessage>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> std::io::Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // Read StreamInfo
    let (msg_type, payload) = read_audio_msg(&mut reader).await?;
    if msg_type != AUDIO_MSG_STREAM_INFO {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "expected StreamInfo as first message",
        ));
    }
    let info: AudioStreamInfo = serde_json::from_slice(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    info!(
        "Audio stream info: {}Hz, {} ch, {}ms",
        info.sample_rate, info.channels, info.frame_duration_ms
    );
    let _ = stream_info_tx.send(Some(info));

    // Spawn RX read task
    let rx_tx = rx_tx.clone();
    let decode_tx = decode_tx.clone();
    let mut rx_handle = tokio::spawn(async move {
        loop {
            match read_audio_msg(&mut reader).await {
                Ok((AUDIO_MSG_RX_FRAME, payload)) => {
                    let _ = rx_tx.send(Bytes::from(payload));
                }
                Ok((
                    AUDIO_MSG_APRS_DECODE
                    | AUDIO_MSG_CW_DECODE
                    | AUDIO_MSG_FT8_DECODE
                    | AUDIO_MSG_WSPR_DECODE,
                    payload,
                )) => {
                    if let Ok(msg) = serde_json::from_slice::<DecodedMessage>(&payload) {
                        let _ = decode_tx.send(msg);
                    }
                }
                Ok((msg_type, _)) => {
                    warn!("Audio client: unexpected message type {}", msg_type);
                }
                Err(_) => break,
            }
        }
    });

    // Forward TX frames to server
    let mut rig_check = time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => {
                        rx_handle.abort();
                        return Ok(());
                    }
                    Ok(()) => {}
                    Err(_) => {
                        rx_handle.abort();
                        return Ok(());
                    }
                }
            }
            packet = tx_rx.recv() => {
                match packet {
                    Some(data) => {
                        if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_TX_FRAME, &data).await {
                            warn!("Audio TX write failed: {}", e);
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = &mut rx_handle => {
                break;
            }
            _ = rig_check.tick() => {
                let current_rig = selected_rig_id.lock().ok().and_then(|v| v.clone());
                let desired_addr = resolve_audio_addr(
                    server_host,
                    default_port,
                    rig_ports,
                    current_rig.as_deref(),
                );
                if desired_addr != connected_addr {
                    info!(
                        "Audio client: active rig changed ({} -> {}), reconnecting audio",
                        connected_addr,
                        desired_addr
                    );
                    break;
                }
            }
        }
    }

    Ok(())
}

fn resolve_audio_addr(
    host: &str,
    default_port: u16,
    rig_ports: &HashMap<String, u16>,
    selected_rig_id: Option<&str>,
) -> String {
    let port = selected_rig_id
        .and_then(|rig_id| rig_ports.get(rig_id))
        .copied()
        .unwrap_or(default_port);
    format!("{}:{}", host, port)
}
