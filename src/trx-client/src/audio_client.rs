// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio TCP client that connects to the server's audio port and relays
//! RX/TX Opus frames via broadcast/mpsc channels.

use std::collections::{HashMap, HashSet};
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
    AUDIO_MSG_CW_DECODE, AUDIO_MSG_FT2_DECODE, AUDIO_MSG_FT4_DECODE, AUDIO_MSG_FT8_DECODE,
    AUDIO_MSG_HF_APRS_DECODE, AUDIO_MSG_HISTORY_COMPRESSED, AUDIO_MSG_RX_FRAME,
    AUDIO_MSG_RX_FRAME_CH, AUDIO_MSG_STREAM_INFO, AUDIO_MSG_TX_FRAME, AUDIO_MSG_VCHAN_ALLOCATED,
    AUDIO_MSG_VCHAN_BW, AUDIO_MSG_VCHAN_DESTROYED, AUDIO_MSG_VCHAN_FREQ, AUDIO_MSG_VCHAN_MODE,
    AUDIO_MSG_VCHAN_REMOVE, AUDIO_MSG_VCHAN_SUB, AUDIO_MSG_VCHAN_UNSUB, AUDIO_MSG_VDES_DECODE,
    AUDIO_MSG_WSPR_DECODE,
};
use trx_core::decode::DecodedMessage;
use trx_frontend::VChanAudioCmd;

#[derive(Clone, Debug)]
struct ActiveVChanSub {
    freq_hz: u64,
    mode: String,
    bandwidth_hz: u32,
    hidden: bool,
    decoder_kinds: Vec<String>,
}

/// Per-rig audio task state, tracked by the multi-rig manager.
struct PerRigAudioTask {
    handle: tokio::task::JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
    port: u16,
}

/// Multi-rig audio manager: spawns/tears down per-rig audio client tasks on
/// demand as rigs appear/disappear from the known_rigs list.  Each rig with
/// an `audio_port` gets its own TCP connection.
#[allow(clippy::too_many_arguments)]
pub async fn run_multi_rig_audio_manager(
    server_host: String,
    default_port: u16,
    rig_ports: HashMap<String, u16>,
    // Per-rig server host overrides (short_name -> host) for multi-server mode.
    rig_server_hosts: HashMap<String, String>,
    selected_rig_id: Arc<Mutex<Option<String>>>,
    known_rigs: Arc<Mutex<Vec<RemoteRigEntry>>>,
    global_rx_tx: broadcast::Sender<Bytes>,
    tx_rx: mpsc::Receiver<Bytes>,
    global_stream_info_tx: watch::Sender<Option<AudioStreamInfo>>,
    decode_tx: broadcast::Sender<DecodedMessage>,
    replay_history_sink: Option<Arc<dyn Fn(DecodedMessage) + Send + Sync>>,
    mut shutdown_rx: watch::Receiver<bool>,
    vchan_audio: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>,
    _vchan_cmd_rx: mpsc::UnboundedReceiver<VChanAudioCmd>,
    vchan_destroyed_tx: Option<broadcast::Sender<Uuid>>,
    rig_audio_rx: Arc<RwLock<HashMap<String, broadcast::Sender<Bytes>>>>,
    rig_audio_info: Arc<RwLock<HashMap<String, watch::Sender<Option<AudioStreamInfo>>>>>,
    rig_vchan_audio_cmd: Arc<RwLock<HashMap<String, mpsc::UnboundedSender<VChanAudioCmd>>>>,
) {
    // TX frames from the microphone go to the selected rig only.
    // We wrap the single tx_rx receiver so the per-rig task for the selected
    // rig can consume it.
    let tx_rx = Arc::new(tokio::sync::Mutex::new(tx_rx));

    let mut active_tasks: HashMap<String, PerRigAudioTask> = HashMap::new();
    let mut poll_interval = time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                // Collect current known rigs and their audio ports.
                let current_rigs: HashMap<String, u16> = known_rigs
                    .lock()
                    .ok()
                    .map(|entries| {
                        entries.iter().map(|e| {
                            let port = rig_ports.get(&e.rig_id).copied()
                                .or(e.audio_port)
                                .unwrap_or(default_port);
                            (e.rig_id.clone(), port)
                        }).collect()
                    })
                    .unwrap_or_default();

                // Tear down tasks for rigs that are no longer present or
                // whose port has changed.
                let to_remove: Vec<String> = active_tasks.keys()
                    .filter(|id| {
                        match current_rigs.get(*id) {
                            None => true,
                            Some(port) => active_tasks.get(*id)
                                .is_none_or(|t| t.port != *port),
                        }
                    })
                    .cloned()
                    .collect();
                for rig_id in &to_remove {
                    if let Some(task) = active_tasks.remove(rig_id) {
                        let _ = task.shutdown_tx.send(true);
                        task.handle.abort();
                        info!("Audio client: stopped task for rig {}", rig_id);
                    }
                }

                // Spawn tasks for new rigs.
                for (rig_id, port) in &current_rigs {
                    if active_tasks.contains_key(rig_id) {
                        continue;
                    }

                    let (per_rig_shutdown_tx, per_rig_shutdown_rx) = watch::channel(false);

                    // Ensure per-rig broadcast and info channels exist.
                    let per_rig_rx_tx = {
                        let mut map = rig_audio_rx.write().unwrap();
                        map.entry(rig_id.clone())
                            .or_insert_with(|| broadcast::channel::<Bytes>(256).0)
                            .clone()
                    };
                    let per_rig_info_tx = {
                        let mut map = rig_audio_info.write().unwrap();
                        map.entry(rig_id.clone())
                            .or_insert_with(|| watch::channel(None).0)
                            .clone()
                    };

                    // Create per-rig vchan cmd channel.
                    let (per_rig_vchan_tx, per_rig_vchan_rx) =
                        mpsc::unbounded_channel::<VChanAudioCmd>();
                    if let Ok(mut map) = rig_vchan_audio_cmd.write() {
                        map.insert(rig_id.clone(), per_rig_vchan_tx);
                    }

                    let host = rig_server_hosts
                        .get(rig_id)
                        .unwrap_or(&server_host);
                    let addr = format!("{}:{}", host, port);
                    let rig_id_clone = rig_id.clone();
                    let global_rx_tx_clone = global_rx_tx.clone();
                    let global_info_tx_clone = global_stream_info_tx.clone();
                    let selected_clone = selected_rig_id.clone();
                    let decode_tx_clone = decode_tx.clone();
                    let replay_sink = replay_history_sink.clone();
                    let vchan_audio_clone = vchan_audio.clone();
                    let vchan_destroyed_clone = vchan_destroyed_tx.clone();
                    let tx_rx_clone = tx_rx.clone();

                    let handle = tokio::spawn(async move {
                        run_single_rig_audio_client(
                            addr,
                            rig_id_clone,
                            selected_clone,
                            per_rig_rx_tx,
                            per_rig_info_tx,
                            global_rx_tx_clone,
                            global_info_tx_clone,
                            tx_rx_clone,
                            decode_tx_clone,
                            replay_sink,
                            per_rig_shutdown_rx,
                            vchan_audio_clone,
                            per_rig_vchan_rx,
                            vchan_destroyed_clone,
                        )
                        .await;
                    });

                    info!("Audio client: started task for rig {} ({}:{})", rig_id, host, port);
                    active_tasks.insert(rig_id.clone(), PerRigAudioTask {
                        handle,
                        shutdown_tx: per_rig_shutdown_tx,
                        port: *port,
                    });
                }
            }
            changed = shutdown_rx.changed() => {
                if matches!(changed, Ok(()) | Err(_)) && *shutdown_rx.borrow() {
                    // Shut down all per-rig tasks.
                    for (rig_id, task) in active_tasks.drain() {
                        let _ = task.shutdown_tx.send(true);
                        task.handle.abort();
                        info!("Audio client: shutdown task for rig {}", rig_id);
                    }
                    return;
                }
            }
        }
    }
}

/// Audio client for a single rig.  Maintains its own TCP connection with
/// auto-reconnect, publishes RX frames to both per-rig and (if selected)
/// global broadcast channels.
#[allow(clippy::too_many_arguments)]
async fn run_single_rig_audio_client(
    server_addr: String,
    rig_id: String,
    selected_rig_id: Arc<Mutex<Option<String>>>,
    per_rig_rx_tx: broadcast::Sender<Bytes>,
    per_rig_info_tx: watch::Sender<Option<AudioStreamInfo>>,
    global_rx_tx: broadcast::Sender<Bytes>,
    global_info_tx: watch::Sender<Option<AudioStreamInfo>>,
    tx_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Bytes>>>,
    decode_tx: broadcast::Sender<DecodedMessage>,
    replay_history_sink: Option<Arc<dyn Fn(DecodedMessage) + Send + Sync>>,
    mut shutdown_rx: watch::Receiver<bool>,
    vchan_audio: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>,
    mut vchan_cmd_rx: mpsc::UnboundedReceiver<VChanAudioCmd>,
    vchan_destroyed_tx: Option<broadcast::Sender<Uuid>>,
) {
    let mut reconnect_delay = Duration::from_secs(1);
    let mut active_subs: HashMap<Uuid, ActiveVChanSub> = HashMap::new();

    let is_selected = |sel: &Arc<Mutex<Option<String>>>, rid: &str| -> bool {
        sel.lock()
            .ok()
            .and_then(|v| v.clone())
            .is_some_and(|s| s == rid)
    };

    loop {
        if *shutdown_rx.borrow() {
            info!("Audio client [{}]: shutting down", rig_id);
            return;
        }

        info!("Audio client [{}]: connecting to {}", rig_id, server_addr);
        match TcpStream::connect(&server_addr).await {
            Ok(stream) => {
                reconnect_delay = Duration::from_secs(1);
                if let Err(e) = handle_single_rig_connection(
                    stream,
                    &rig_id,
                    &selected_rig_id,
                    &per_rig_rx_tx,
                    &per_rig_info_tx,
                    &global_rx_tx,
                    &global_info_tx,
                    &tx_rx,
                    &decode_tx,
                    replay_history_sink.clone(),
                    &mut shutdown_rx,
                    &vchan_audio,
                    &mut vchan_cmd_rx,
                    &mut active_subs,
                    &vchan_destroyed_tx,
                )
                .await
                {
                    warn!("Audio connection [{}] dropped: {}", rig_id, e);
                }
            }
            Err(e) => {
                warn!("Audio connect [{}] failed: {}", rig_id, e);
            }
        }

        let _ = per_rig_info_tx.send(None);
        if is_selected(&selected_rig_id, &rig_id) {
            let _ = global_info_tx.send(None);
        }

        tokio::select! {
            _ = time::sleep(reconnect_delay) => {}
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => {
                        info!("Audio client [{}]: shutting down", rig_id);
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

/// Handle a single TCP connection for one rig.  Similar to `handle_audio_connection`
/// but publishes to per-rig channels directly and mirrors to global when selected.
#[allow(clippy::too_many_arguments)]
async fn handle_single_rig_connection(
    stream: TcpStream,
    rig_id: &str,
    selected_rig_id: &Arc<Mutex<Option<String>>>,
    per_rig_rx_tx: &broadcast::Sender<Bytes>,
    per_rig_info_tx: &watch::Sender<Option<AudioStreamInfo>>,
    global_rx_tx: &broadcast::Sender<Bytes>,
    global_info_tx: &watch::Sender<Option<AudioStreamInfo>>,
    tx_rx: &Arc<tokio::sync::Mutex<mpsc::Receiver<Bytes>>>,
    decode_tx: &broadcast::Sender<DecodedMessage>,
    replay_history_sink: Option<Arc<dyn Fn(DecodedMessage) + Send + Sync>>,
    shutdown_rx: &mut watch::Receiver<bool>,
    vchan_audio: &Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>,
    vchan_cmd_rx: &mut mpsc::UnboundedReceiver<VChanAudioCmd>,
    active_subs: &mut HashMap<Uuid, ActiveVChanSub>,
    vchan_destroyed_tx: &Option<broadcast::Sender<Uuid>>,
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
        "Audio stream info [{}]: {}Hz, {} ch, {}ms",
        rig_id, info.sample_rate, info.channels, info.frame_duration_ms
    );
    let _ = per_rig_info_tx.send(Some(info.clone()));

    // Mirror to global if this is the selected rig.
    let is_selected = selected_rig_id
        .lock()
        .ok()
        .and_then(|v| v.clone())
        .is_some_and(|s| s == rig_id);
    if is_selected {
        let _ = global_info_tx.send(Some(info));
    }

    // Re-subscribe active virtual channels on reconnect.
    let mut resubscribed: HashSet<Uuid> = HashSet::new();
    for (&uuid, sub) in active_subs.iter() {
        let json = serde_json::json!({
            "uuid": uuid.to_string(),
            "freq_hz": sub.freq_hz,
            "mode": sub.mode,
            "hidden": sub.hidden,
            "decoder_kinds": sub.decoder_kinds,
            "bandwidth_hz": sub.bandwidth_hz,
        });
        if let Ok(payload) = serde_json::to_vec(&json) {
            if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_SUB, &payload).await {
                warn!("Audio vchan reconnect SUB write failed [{}]: {}", rig_id, e);
                return Err(e);
            }
        }
        if sub.bandwidth_hz > 0 {
            let bw_json =
                serde_json::json!({ "uuid": uuid.to_string(), "bandwidth_hz": sub.bandwidth_hz });
            if let Ok(payload) = serde_json::to_vec(&bw_json) {
                if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_BW, &payload).await {
                    warn!("Audio vchan reconnect BW write failed [{}]: {}", rig_id, e);
                    return Err(e);
                }
            }
        }
        resubscribed.insert(uuid);
    }

    // Spawn RX read task — publishes to per-rig and (when selected) global.
    let per_rig_rx_clone = per_rig_rx_tx.clone();
    let global_rx_clone = global_rx_tx.clone();
    let selected_for_rx = selected_rig_id.clone();
    let rig_id_for_rx = rig_id.to_string();
    let decode_tx_clone = decode_tx.clone();
    let vchan_audio_rx: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>> =
        Arc::clone(vchan_audio);
    let vchan_destroyed_for_rx = vchan_destroyed_tx.clone();
    let mut rx_handle = tokio::spawn(async move {
        loop {
            match read_audio_msg(&mut reader).await {
                Ok((AUDIO_MSG_RX_FRAME, payload)) => {
                    let data = Bytes::from(payload);
                    // Always publish to per-rig channel.
                    let _ = per_rig_rx_clone.send(data.clone());
                    // Mirror to global if this rig is currently selected.
                    let sel = selected_for_rx
                        .lock()
                        .ok()
                        .and_then(|v| v.clone())
                        .is_some_and(|s| s == rig_id_for_rx);
                    if sel {
                        let _ = global_rx_clone.send(data);
                    }
                }
                Ok((AUDIO_MSG_RX_FRAME_CH, payload)) => {
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
                    if let Ok(uuid) = parse_vchan_uuid_msg(&payload) {
                        if let Ok(mut map) = vchan_audio_rx.write() {
                            map.entry(uuid)
                                .or_insert_with(|| broadcast::channel::<Bytes>(64).0);
                        }
                    }
                }
                Ok((AUDIO_MSG_VCHAN_DESTROYED, payload)) => {
                    if let Ok(uuid) = parse_vchan_uuid_msg(&payload) {
                        if let Ok(mut map) = vchan_audio_rx.write() {
                            map.remove(&uuid);
                        }
                        if let Some(ref tx) = vchan_destroyed_for_rx {
                            let _ = tx.send(uuid);
                        }
                    }
                }
                Ok((AUDIO_MSG_HISTORY_COMPRESSED, payload)) => {
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
                                if let Some(ref sink) = replay_history_sink {
                                    sink(msg);
                                }
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
                    | AUDIO_MSG_FT4_DECODE
                    | AUDIO_MSG_FT2_DECODE
                    | AUDIO_MSG_WSPR_DECODE,
                    payload,
                )) => {
                    if let Ok(msg) = serde_json::from_slice::<DecodedMessage>(&payload) {
                        let _ = decode_tx_clone.send(msg);
                    }
                }
                Ok((msg_type, _)) => {
                    warn!(
                        "Audio client [{}]: unexpected message type {:#04x}",
                        rig_id_for_rx, msg_type
                    );
                }
                Err(_) => break,
            }
        }
    });

    // Forward TX frames (only when we are the selected rig) and vchan commands.
    let rig_id_owned = rig_id.to_string();
    loop {
        // Only the selected rig should consume TX frames from the mic.
        let is_sel = selected_rig_id
            .lock()
            .ok()
            .and_then(|v| v.clone())
            .is_some_and(|s| s == rig_id_owned);

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
            packet = async {
                if is_sel {
                    tx_rx.lock().await.recv().await
                } else {
                    // Not selected — don't consume TX frames; pend forever.
                    std::future::pending().await
                }
            } => {
                match packet {
                    Some(data) => {
                        if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_TX_FRAME, &data).await {
                            warn!("Audio TX write failed [{}]: {}", rig_id_owned, e);
                            break;
                        }
                    }
                    None => break,
                }
            }
            cmd = vchan_cmd_rx.recv() => {
                match cmd {
                    Some(VChanAudioCmd::Subscribe { uuid, freq_hz, mode, bandwidth_hz, decoder_kinds }) => {
                        active_subs.insert(uuid, ActiveVChanSub {
                            freq_hz,
                            mode: mode.clone(),
                            bandwidth_hz,
                            hidden: false,
                            decoder_kinds: decoder_kinds.clone(),
                        });
                        if resubscribed.remove(&uuid) {
                        } else {
                            let json = serde_json::json!({
                                "uuid": uuid.to_string(),
                                "freq_hz": freq_hz,
                                "mode": mode,
                                "hidden": false,
                                "decoder_kinds": decoder_kinds,
                                "bandwidth_hz": bandwidth_hz,
                            });
                            if let Ok(payload) = serde_json::to_vec(&json) {
                                if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_SUB, &payload).await {
                                    warn!("Audio vchan SUB write failed [{}]: {}", rig_id_owned, e);
                                    break;
                                }
                            }
                        }
                    }
                    Some(VChanAudioCmd::SubscribeBackground { uuid, freq_hz, mode, bandwidth_hz, decoder_kinds }) => {
                        active_subs.insert(uuid, ActiveVChanSub {
                            freq_hz,
                            mode: mode.clone(),
                            bandwidth_hz,
                            hidden: true,
                            decoder_kinds: decoder_kinds.clone(),
                        });
                        if resubscribed.remove(&uuid) {
                        } else {
                            let json = serde_json::json!({
                                "uuid": uuid.to_string(),
                                "freq_hz": freq_hz,
                                "mode": mode,
                                "hidden": true,
                                "decoder_kinds": decoder_kinds,
                                "bandwidth_hz": bandwidth_hz,
                            });
                            if let Ok(payload) = serde_json::to_vec(&json) {
                                if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_SUB, &payload).await {
                                    warn!("Audio background SUB write failed [{}]: {}", rig_id_owned, e);
                                    break;
                                }
                            }
                        }
                    }
                    Some(VChanAudioCmd::Unsubscribe(uuid)) => {
                        if let Err(e) = write_vchan_uuid_msg(&mut writer, AUDIO_MSG_VCHAN_UNSUB, uuid).await {
                            warn!("Audio vchan UNSUB write failed [{}]: {}", rig_id_owned, e);
                            break;
                        }
                    }
                    Some(VChanAudioCmd::Remove(uuid)) => {
                        if let Err(e) = write_vchan_uuid_msg(&mut writer, AUDIO_MSG_VCHAN_REMOVE, uuid).await {
                            warn!("Audio vchan REMOVE write failed [{}]: {}", rig_id_owned, e);
                            break;
                        }
                        if let Ok(mut map) = vchan_audio.write() {
                            map.remove(&uuid);
                        }
                        active_subs.remove(&uuid);
                    }
                    Some(VChanAudioCmd::SetFreq { uuid, freq_hz }) => {
                        if let Some(entry) = active_subs.get_mut(&uuid) {
                            entry.freq_hz = freq_hz;
                        }
                        let json = serde_json::json!({ "uuid": uuid.to_string(), "freq_hz": freq_hz });
                        if let Ok(payload) = serde_json::to_vec(&json) {
                            if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_FREQ, &payload).await {
                                warn!("Audio vchan FREQ write failed [{}]: {}", rig_id_owned, e);
                                break;
                            }
                        }
                    }
                    Some(VChanAudioCmd::SetMode { uuid, mode }) => {
                        if let Some(entry) = active_subs.get_mut(&uuid) {
                            entry.mode = mode.clone();
                        }
                        let json = serde_json::json!({ "uuid": uuid.to_string(), "mode": mode });
                        if let Ok(payload) = serde_json::to_vec(&json) {
                            if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_MODE, &payload).await {
                                warn!("Audio vchan MODE write failed [{}]: {}", rig_id_owned, e);
                                break;
                            }
                        }
                    }
                    Some(VChanAudioCmd::SetBandwidth { uuid, bandwidth_hz }) => {
                        if let Some(entry) = active_subs.get_mut(&uuid) {
                            entry.bandwidth_hz = bandwidth_hz;
                        }
                        let json = serde_json::json!({ "uuid": uuid.to_string(), "bandwidth_hz": bandwidth_hz });
                        if let Ok(payload) = serde_json::to_vec(&json) {
                            if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_BW, &payload).await {
                                warn!("Audio vchan BW write failed [{}]: {}", rig_id_owned, e);
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
        }
    }

    Ok(())
}
