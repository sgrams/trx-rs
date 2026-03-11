// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio TCP client that connects to the server's audio port and relays
//! RX/TX Opus frames via broadcast/mpsc channels.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use bytes::Bytes;
use flate2::read::GzDecoder;
use std::io::Read as _;
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time;
use tracing::{info, warn};
use trx_frontend::RemoteRigEntry;

use uuid::Uuid;

use trx_core::audio::{
    parse_vchan_audio_frame, parse_vchan_uuid_msg, read_audio_msg, write_audio_msg,
    write_vchan_uuid_msg, AudioStreamInfo, AUDIO_MSG_AIS_DECODE, AUDIO_MSG_APRS_DECODE,
    AUDIO_MSG_CW_DECODE, AUDIO_MSG_FT8_DECODE, AUDIO_MSG_HF_APRS_DECODE,
    AUDIO_MSG_HISTORY_COMPRESSED, AUDIO_MSG_RX_FRAME, AUDIO_MSG_RX_FRAME_CH,
    AUDIO_MSG_STREAM_INFO, AUDIO_MSG_TX_FRAME, AUDIO_MSG_VCHAN_ALLOCATED, AUDIO_MSG_VCHAN_FREQ,
    AUDIO_MSG_VCHAN_MODE, AUDIO_MSG_VCHAN_REMOVE, AUDIO_MSG_VCHAN_SUB, AUDIO_MSG_VCHAN_UNSUB,
    AUDIO_MSG_VDES_DECODE, AUDIO_MSG_WSPR_DECODE,
};
use trx_core::decode::DecodedMessage;
use trx_frontend::VChanAudioCmd;

/// Run the audio client with auto-reconnect.
#[allow(clippy::too_many_arguments)]
pub async fn run_audio_client(
    server_host: String,
    default_port: u16,
    rig_ports: HashMap<String, u16>,
    selected_rig_id: Arc<Mutex<Option<String>>>,
    known_rigs: Arc<Mutex<Vec<RemoteRigEntry>>>,
    rx_tx: broadcast::Sender<Bytes>,
    mut tx_rx: mpsc::Receiver<Bytes>,
    stream_info_tx: watch::Sender<Option<AudioStreamInfo>>,
    decode_tx: broadcast::Sender<DecodedMessage>,
    mut shutdown_rx: watch::Receiver<bool>,
    vchan_audio: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>,
    mut vchan_cmd_rx: mpsc::Receiver<VChanAudioCmd>,
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
            &known_rigs,
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
                    &known_rigs,
                    &server_addr,
                    &rx_tx,
                    &mut tx_rx,
                    &stream_info_tx,
                    &decode_tx,
                    &mut shutdown_rx,
                    &vchan_audio,
                    &mut vchan_cmd_rx,
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

#[allow(clippy::too_many_arguments)]
async fn handle_audio_connection(
    stream: TcpStream,
    server_host: &str,
    default_port: u16,
    rig_ports: &HashMap<String, u16>,
    selected_rig_id: &Arc<Mutex<Option<String>>>,
    known_rigs: &Arc<Mutex<Vec<RemoteRigEntry>>>,
    connected_addr: &str,
    rx_tx: &broadcast::Sender<Bytes>,
    tx_rx: &mut mpsc::Receiver<Bytes>,
    stream_info_tx: &watch::Sender<Option<AudioStreamInfo>>,
    decode_tx: &broadcast::Sender<DecodedMessage>,
    shutdown_rx: &mut watch::Receiver<bool>,
    vchan_audio: &Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>,
    vchan_cmd_rx: &mut mpsc::Receiver<VChanAudioCmd>,
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
    let vchan_audio_rx: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>> = Arc::clone(vchan_audio);
    let mut rx_handle = tokio::spawn(async move {
        loop {
            match read_audio_msg(&mut reader).await {
                Ok((AUDIO_MSG_RX_FRAME, payload)) => {
                    let _ = rx_tx.send(Bytes::from(payload));
                }
                Ok((AUDIO_MSG_RX_FRAME_CH, payload)) => {
                    // Route per-channel Opus frame to the correct broadcaster.
                    if let Ok((uuid, opus)) = parse_vchan_audio_frame(&payload) {
                        let pkt = Bytes::copy_from_slice(opus);
                        if let Ok(map) = vchan_audio_rx.read() {
                            if let Some(tx) = map.get(&uuid) {
                                let _ = tx.send(pkt);
                            }
                        }
                    }
                }
                Ok((AUDIO_MSG_VCHAN_ALLOCATED, payload)) => {
                    // Server confirmed a virtual channel is ready; ensure a
                    // broadcaster entry exists in the shared map.
                    if let Ok(uuid) = parse_vchan_uuid_msg(&payload) {
                        if let Ok(mut map) = vchan_audio_rx.write() {
                            map.entry(uuid)
                                .or_insert_with(|| broadcast::channel::<Bytes>(64).0);
                        }
                    }
                }
                Ok((AUDIO_MSG_HISTORY_COMPRESSED, payload)) => {
                    // Decompress gzip blob, then iterate the embedded framed messages.
                    let mut decompressed = Vec::new();
                    if GzDecoder::new(payload.as_slice())
                        .read_to_end(&mut decompressed)
                        .is_ok()
                    {
                        let mut pos = 0;
                        while pos + 5 <= decompressed.len() {
                            let _msg_type = decompressed[pos];
                            let len = u32::from_be_bytes([
                                decompressed[pos + 1],
                                decompressed[pos + 2],
                                decompressed[pos + 3],
                                decompressed[pos + 4],
                            ]) as usize;
                            pos += 5;
                            if pos + len > decompressed.len() {
                                break;
                            }
                            let json = &decompressed[pos..pos + len];
                            if let Ok(msg) = serde_json::from_slice::<DecodedMessage>(json) {
                                let _ = decode_tx.send(msg);
                            }
                            pos += len;
                        }
                    }
                }
                Ok((
                    AUDIO_MSG_VDES_DECODE
                    | AUDIO_MSG_AIS_DECODE
                    | AUDIO_MSG_APRS_DECODE
                    | AUDIO_MSG_HF_APRS_DECODE
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
                    warn!("Audio client: unexpected message type {:#04x}", msg_type);
                }
                Err(_) => break,
            }
        }
    });

    // Forward TX frames and VChanAudioCmds to server.
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
            cmd = vchan_cmd_rx.recv() => {
                match cmd {
                    Some(VChanAudioCmd::Subscribe { uuid, freq_hz, mode }) => {
                        let json = serde_json::json!({
                            "uuid": uuid.to_string(),
                            "freq_hz": freq_hz,
                            "mode": mode,
                        });
                        if let Ok(payload) = serde_json::to_vec(&json) {
                            if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_SUB, &payload).await {
                                warn!("Audio vchan SUB write failed: {}", e);
                                break;
                            }
                        }
                    }
                    Some(VChanAudioCmd::Unsubscribe(uuid)) => {
                        if let Err(e) = write_vchan_uuid_msg(&mut writer, AUDIO_MSG_VCHAN_UNSUB, uuid).await {
                            warn!("Audio vchan UNSUB write failed: {}", e);
                            break;
                        }
                    }
                    Some(VChanAudioCmd::Remove(uuid)) => {
                        if let Err(e) = write_vchan_uuid_msg(&mut writer, AUDIO_MSG_VCHAN_REMOVE, uuid).await {
                            warn!("Audio vchan REMOVE write failed: {}", e);
                            break;
                        }
                        // Clean up local broadcaster.
                        if let Ok(mut map) = vchan_audio.write() {
                            map.remove(&uuid);
                        }
                    }
                    Some(VChanAudioCmd::SetFreq { uuid, freq_hz }) => {
                        let json = serde_json::json!({ "uuid": uuid.to_string(), "freq_hz": freq_hz });
                        if let Ok(payload) = serde_json::to_vec(&json) {
                            if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_FREQ, &payload).await {
                                warn!("Audio vchan FREQ write failed: {}", e);
                                break;
                            }
                        }
                    }
                    Some(VChanAudioCmd::SetMode { uuid, mode }) => {
                        let json = serde_json::json!({ "uuid": uuid.to_string(), "mode": mode });
                        if let Ok(payload) = serde_json::to_vec(&json) {
                            if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_MODE, &payload).await {
                                warn!("Audio vchan MODE write failed: {}", e);
                                break;
                            }
                        }
                    }
                    None => {}
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
                    known_rigs,
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
    known_rigs: &Arc<Mutex<Vec<RemoteRigEntry>>>,
    selected_rig_id: Option<&str>,
) -> String {
    let port = selected_rig_id
        .and_then(|rig_id| {
            rig_ports.get(rig_id).copied().or_else(|| {
                known_rigs.lock().ok().and_then(|entries| {
                    entries
                        .iter()
                        .find(|entry| entry.rig_id == rig_id)
                        .and_then(|entry| entry.audio_port)
                })
            })
        })
        .unwrap_or(default_port);
    format!("{}:{}", host, port)
}
