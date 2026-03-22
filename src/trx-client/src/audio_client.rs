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
    replay_history_sink: Option<Arc<dyn Fn(DecodedMessage) + Send + Sync>>,
    mut shutdown_rx: watch::Receiver<bool>,
    vchan_audio: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>,
    mut vchan_cmd_rx: mpsc::UnboundedReceiver<VChanAudioCmd>,
    vchan_destroyed_tx: Option<broadcast::Sender<Uuid>>,
) {
    let mut reconnect_delay = Duration::from_secs(1);
    // Active virtual-channel subscriptions, keyed by UUID, re-sent to the
    // server on every audio TCP reconnect.
    let mut active_subs: HashMap<Uuid, ActiveVChanSub> = HashMap::new();

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
                    replay_history_sink.clone(),
                    &mut shutdown_rx,
                    &vchan_audio,
                    &mut vchan_cmd_rx,
                    &mut active_subs,
                    &vchan_destroyed_tx,
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
        "Audio stream info: {}Hz, {} ch, {}ms",
        info.sample_rate, info.channels, info.frame_duration_ms
    );
    let _ = stream_info_tx.send(Some(info));

    // On reconnect: re-subscribe all previously active virtual channels.
    // Track which UUIDs were pre-sent so we don't duplicate them when the
    // same Subscribe command arrives from the mpsc queue.
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
                warn!("Audio vchan reconnect SUB write failed: {}", e);
                return Err(e);
            }
        }
        // Re-apply non-default bandwidth after re-subscribing.
        if sub.bandwidth_hz > 0 {
            let bw_json =
                serde_json::json!({ "uuid": uuid.to_string(), "bandwidth_hz": sub.bandwidth_hz });
            if let Ok(payload) = serde_json::to_vec(&bw_json) {
                if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_BW, &payload).await {
                    warn!("Audio vchan reconnect BW write failed: {}", e);
                    return Err(e);
                }
            }
        }
        resubscribed.insert(uuid);
    }

    // Spawn RX read task
    let rx_tx = rx_tx.clone();
    let decode_tx = decode_tx.clone();
    let vchan_audio_rx: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>> =
        Arc::clone(vchan_audio);
    let vchan_destroyed_for_rx = vchan_destroyed_tx.clone();
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
                Ok((AUDIO_MSG_VCHAN_DESTROYED, payload)) => {
                    if let Ok(uuid) = parse_vchan_uuid_msg(&payload) {
                        // Remove the broadcaster so audio_ws gets no more frames.
                        if let Ok(mut map) = vchan_audio_rx.write() {
                            map.remove(&uuid);
                        }
                        // Notify the HTTP frontend so it removes the channel from
                        // ClientChannelManager (triggers SSE channels event).
                        if let Some(ref tx) = vchan_destroyed_for_rx {
                            let _ = tx.send(uuid);
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
                    Some(VChanAudioCmd::Subscribe { uuid, freq_hz, mode, bandwidth_hz, decoder_kinds }) => {
                        active_subs.insert(uuid, ActiveVChanSub {
                            freq_hz,
                            mode: mode.clone(),
                            bandwidth_hz,
                            hidden: false,
                            decoder_kinds: decoder_kinds.clone(),
                        });
                        // Skip if already re-sent during reconnect initialization.
                        if resubscribed.remove(&uuid) {
                            // Already sent above; don't duplicate.
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
                                    warn!("Audio vchan SUB write failed: {}", e);
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
                            // Already sent above; don't duplicate.
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
                                    warn!("Audio background SUB write failed: {}", e);
                                    break;
                                }
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
                        active_subs.remove(&uuid);
                    }
                    Some(VChanAudioCmd::SetFreq { uuid, freq_hz }) => {
                        if let Some(entry) = active_subs.get_mut(&uuid) {
                            entry.freq_hz = freq_hz;
                        }
                        let json = serde_json::json!({ "uuid": uuid.to_string(), "freq_hz": freq_hz });
                        if let Ok(payload) = serde_json::to_vec(&json) {
                            if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_FREQ, &payload).await {
                                warn!("Audio vchan FREQ write failed: {}", e);
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
                                warn!("Audio vchan MODE write failed: {}", e);
                                break;
                            }
                        }
                    }
                    Some(VChanAudioCmd::SetBandwidth { uuid, bandwidth_hz }) => {
                        // Persist for reconnect.
                        if let Some(entry) = active_subs.get_mut(&uuid) {
                            entry.bandwidth_hz = bandwidth_hz;
                        }
                        let json = serde_json::json!({ "uuid": uuid.to_string(), "bandwidth_hz": bandwidth_hz });
                        if let Ok(payload) = serde_json::to_vec(&json) {
                            if let Err(e) = write_audio_msg(&mut writer, AUDIO_MSG_VCHAN_BW, &payload).await {
                                warn!("Audio vchan BW write failed: {}", e);
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

/// Multi-rig audio manager: spawns/tears down per-rig audio client tasks on
/// demand as rigs appear/disappear from the known_rigs list.
#[allow(clippy::too_many_arguments)]
pub async fn run_multi_rig_audio_manager(
    server_host: String,
    default_port: u16,
    rig_ports: HashMap<String, u16>,
    selected_rig_id: Arc<Mutex<Option<String>>>,
    known_rigs: Arc<Mutex<Vec<RemoteRigEntry>>>,
    global_rx_tx: broadcast::Sender<Bytes>,
    tx_rx: mpsc::Receiver<Bytes>,
    global_stream_info_tx: watch::Sender<Option<AudioStreamInfo>>,
    decode_tx: broadcast::Sender<DecodedMessage>,
    replay_history_sink: Option<Arc<dyn Fn(DecodedMessage) + Send + Sync>>,
    shutdown_rx: watch::Receiver<bool>,
    vchan_audio: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>,
    vchan_cmd_rx: mpsc::UnboundedReceiver<VChanAudioCmd>,
    vchan_destroyed_tx: Option<broadcast::Sender<Uuid>>,
    rig_audio_rx: Arc<RwLock<HashMap<String, broadcast::Sender<Bytes>>>>,
    rig_audio_info: Arc<RwLock<HashMap<String, watch::Sender<Option<AudioStreamInfo>>>>>,
    rig_vchan_audio_cmd: Arc<RwLock<HashMap<String, mpsc::UnboundedSender<VChanAudioCmd>>>>,
) {
    // Per-rig vchan command routing: create per-rig senders that relay into the
    // single global vchan_cmd channel. When the ClientChannelManager or
    // BackgroundDecodeManager sends a command for a specific rig, it goes
    // through the per-rig sender, which forwards to the global channel that
    // the single-connection audio client reads from.
    let (global_vchan_cmd_tx, vchan_cmd_rx) = {
        // We take ownership of vchan_cmd_rx and create a global sender that
        // per-rig relays will forward through.
        let (tx, rx) = mpsc::unbounded_channel::<VChanAudioCmd>();
        // Spawn relay from the original vchan_cmd_rx (from main.rs).
        let mut orig_rx = vchan_cmd_rx;
        let tx_for_orig = tx.clone();
        tokio::spawn(async move {
            while let Some(cmd) = orig_rx.recv().await {
                let _ = tx_for_orig.send(cmd);
            }
        });
        (tx, rx)
    };

    // Populate per-rig vchan senders for known rigs and keep them in sync.
    let rig_vchan_for_sync = rig_vchan_audio_cmd.clone();
    let known_rigs_for_vchan = known_rigs.clone();
    let global_vchan_for_sync = global_vchan_cmd_tx.clone();
    let mut vchan_sync_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let rig_ids: Vec<String> = known_rigs_for_vchan
                        .lock()
                        .ok()
                        .map(|entries| entries.iter().map(|e| e.rig_id.clone()).collect())
                        .unwrap_or_default();
                    if let Ok(mut map) = rig_vchan_for_sync.write() {
                        for rig_id in &rig_ids {
                            if !map.contains_key(rig_id) {
                                // Create a per-rig sender that relays to global.
                                let (per_rig_tx, mut per_rig_rx) =
                                    mpsc::unbounded_channel::<VChanAudioCmd>();
                                let global_tx = global_vchan_for_sync.clone();
                                tokio::spawn(async move {
                                    while let Some(cmd) = per_rig_rx.recv().await {
                                        let _ = global_tx.send(cmd);
                                    }
                                });
                                map.insert(rig_id.clone(), per_rig_tx);
                            }
                        }
                        // Remove senders for rigs no longer present.
                        let active: std::collections::HashSet<&str> =
                            rig_ids.iter().map(|s| s.as_str()).collect();
                        map.retain(|id, _| active.contains(id.as_str()));
                    }
                }
                changed = vchan_sync_shutdown.changed() => {
                    if matches!(changed, Ok(()) | Err(_)) && *vchan_sync_shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    });

    // For now, delegate to the existing single-connection audio client.
    // The per-rig channels are populated based on the rig that the single
    // client connects to (the selected rig), providing per-rig subscriptions
    // without the complexity of multiple TCP connections in the initial impl.
    //
    // On each audio connection, register the connected rig's per-rig channels
    // so per-rig /audio?rig_id= subscribers get data.
    let selected_clone = selected_rig_id.clone();
    let rig_audio_rx_clone = rig_audio_rx.clone();
    let rig_audio_info_clone = rig_audio_info.clone();

    // Spawn a task that keeps per-rig maps in sync with the selected rig.
    let mut sync_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        let mut last_rig: Option<String> = None;
        let mut interval = time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let current = selected_clone.lock().ok().and_then(|v| v.clone());
                    if current != last_rig {
                        // Ensure per-rig broadcast exists for new rig.
                        if let Some(ref rig_id) = current {
                            if let Ok(mut map) = rig_audio_rx_clone.write() {
                                map.entry(rig_id.clone())
                                    .or_insert_with(|| broadcast::channel::<Bytes>(256).0);
                            }
                            if let Ok(mut map) = rig_audio_info_clone.write() {
                                map.entry(rig_id.clone())
                                    .or_insert_with(|| watch::channel(None).0);
                            }
                        }
                        last_rig = current;
                    }
                    // Mirror global audio data to the current rig's per-rig channel.
                    // (The actual mirroring happens in the RX read task below.)
                }
                changed = sync_shutdown.changed() => {
                    if matches!(changed, Ok(()) | Err(_)) && *sync_shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    });

    // Wrap the global_rx_tx in a relay that also publishes to per-rig channels.
    let (relay_rx_tx, _) = broadcast::channel::<Bytes>(256);
    let relay_clone = relay_rx_tx.clone();
    let rig_audio_rx_for_relay = rig_audio_rx.clone();
    let selected_for_relay = selected_rig_id.clone();
    let mut relay_sub = global_rx_tx.subscribe();
    let mut relay_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = relay_sub.recv() => {
                    match result {
                        Ok(data) => {
                            // Forward to per-rig channel for the selected rig.
                            if let Some(rig_id) = selected_for_relay.lock().ok().and_then(|v| v.clone()) {
                                if let Ok(map) = rig_audio_rx_for_relay.read() {
                                    if let Some(tx) = map.get(&rig_id) {
                                        let _ = tx.send(data.clone());
                                    }
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                changed = relay_shutdown.changed() => {
                    if matches!(changed, Ok(()) | Err(_)) && *relay_shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    });

    // Also relay stream info changes to per-rig info channels.
    let mut info_relay_rx = global_stream_info_tx.subscribe();
    let rig_audio_info_for_relay = rig_audio_info.clone();
    let selected_for_info_relay = selected_rig_id.clone();
    let mut info_relay_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = info_relay_rx.changed() => {
                    match changed {
                        Ok(()) => {
                            let info = info_relay_rx.borrow().clone();
                            if let Some(rig_id) = selected_for_info_relay.lock().ok().and_then(|v| v.clone()) {
                                if let Ok(map) = rig_audio_info_for_relay.read() {
                                    if let Some(tx) = map.get(&rig_id) {
                                        let _ = tx.send(info);
                                    }
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                changed = info_relay_shutdown.changed() => {
                    if matches!(changed, Ok(()) | Err(_)) && *info_relay_shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    });

    let _ = relay_clone;

    // Delegate to existing single-connection audio client.
    run_audio_client(
        server_host,
        default_port,
        rig_ports,
        selected_rig_id,
        known_rigs,
        global_rx_tx,
        tx_rx,
        global_stream_info_tx,
        decode_tx,
        replay_history_sink,
        shutdown_rx,
        vchan_audio,
        vchan_cmd_rx,
        vchan_destroyed_tx,
    )
    .await;
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
