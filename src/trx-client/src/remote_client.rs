// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;
use std::time::Duration;
use std::{sync::Arc, sync::Mutex};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::time::{self, Instant};
use tracing::{info, warn};

use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::{RigError, RigResult};
use trx_frontend::{RemoteRigEntry, SharedSpectrum};
use trx_protocol::rig_command_to_client;
use trx_protocol::types::RigEntry;
use trx_protocol::{ClientCommand, ClientEnvelope, ClientResponse};

const DEFAULT_REMOTE_PORT: u16 = 4530;
const DEFAULT_AUDIO_PORT: u16 = 4531;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const SPECTRUM_IO_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_JSON_LINE_BYTES: usize = 256 * 1024;
const MAX_CONSECUTIVE_POLL_FAILURES: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEndpoint {
    pub host: String,
    pub port: u16,
}

impl RemoteEndpoint {
    pub fn connect_addr(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

// Keep remote spectrum reasonably responsive without returning to the old
// timeout churn caused by a much tighter request cadence.
const SPECTRUM_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub struct RemoteClientConfig {
    pub addr: String,
    pub token: Option<String>,
    pub selected_rig_id: Arc<Mutex<Option<String>>>,
    pub known_rigs: Arc<Mutex<Vec<RemoteRigEntry>>>,
    pub poll_interval: Duration,
    /// Spectrum watch sender; spectrum task publishes here, SSE clients subscribe.
    pub spectrum: Arc<watch::Sender<SharedSpectrum>>,
    /// Shared flag: `true` while a TCP connection to trx-server is active.
    pub server_connected: Arc<AtomicBool>,
    /// Per-rig server connection flag.  Keyed by short name (or rig_id in legacy mode).
    /// Set to `true` once the rig appears in a successful GetRigs response, and to
    /// `false` when this config's TCP connection drops.  Allows the UI to freeze only
    /// the affected rig's view rather than all rigs.
    pub rig_server_connected: Arc<RwLock<HashMap<String, bool>>>,
    pub rig_states: Arc<RwLock<HashMap<String, watch::Sender<RigState>>>>,
    /// Per-rig spectrum watch senders, keyed by short name (or rig_id in legacy mode).
    pub rig_spectrums: Arc<RwLock<HashMap<String, watch::Sender<SharedSpectrum>>>>,
    /// Maps configured server rig_id (`Some`) or default/wildcard (`None`) to
    /// a client-side short name.  Empty in legacy single-remote mode.
    pub rig_id_to_short_name: HashMap<Option<String>, String>,
    /// Dynamically resolved reverse mapping: short_name → server rig_id.
    /// Populated during `refresh_remote_snapshot` when short-name mode is active.
    pub short_name_to_rig_id: Arc<RwLock<HashMap<String, String>>>,
    /// Cached satellite pass predictions from the server (GetSatPasses).
    pub sat_passes: Arc<RwLock<Option<trx_core::geo::PassPredictionResult>>>,
}

pub async fn run_remote_client(
    config: RemoteClientConfig,
    mut rx: mpsc::Receiver<RigRequest>,
    state_tx: watch::Sender<RigState>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> RigResult<()> {
    // Spectrum polling runs on its own dedicated TCP connection so it never
    // blocks state polls or user commands on the main connection.
    let spectrum_task = tokio::spawn(run_spectrum_connection(config.clone(), shutdown_rx.clone()));

    let mut reconnect_delay = Duration::from_secs(1);

    loop {
        if *shutdown_rx.borrow() {
            info!("Remote client shutting down");
            spectrum_task.abort();
            return Ok(());
        }

        info!("Remote client: connecting to {}", config.addr);
        match time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&config.addr)).await {
            Ok(Ok(stream)) => {
                // Reset backoff on successful TCP connect: server is reachable, so the
                // next disconnect should retry quickly rather than waiting up to 10 s.
                reconnect_delay = Duration::from_secs(1);
                // Disable Nagle's algorithm so each framed command is sent immediately
                // rather than being held for up to 40 ms waiting for ACKs.
                if let Err(e) = stream.set_nodelay(true) {
                    warn!("TCP_NODELAY failed: {}", e);
                }
                config.server_connected.store(true, Ordering::Relaxed);
                if let Err(e) =
                    handle_connection(&config, stream, &mut rx, &state_tx, &mut shutdown_rx).await
                {
                    warn!("Remote connection dropped: {}", e);
                }
                config.server_connected.store(false, Ordering::Relaxed);
                // Collect the short names owned by this config so we can mark
                // only their rigs as disconnected (other server groups unaffected).
                let owned: Vec<String> = if has_short_names(&config) {
                    config.rig_id_to_short_name.values().cloned().collect()
                } else {
                    // Legacy single-remote: every key in rig_server_connected is ours.
                    config
                        .rig_server_connected
                        .read()
                        .map(|m| m.keys().cloned().collect())
                        .unwrap_or_default()
                };
                if let Ok(mut conn_map) = config.rig_server_connected.write() {
                    for name in &owned {
                        conn_map.insert(name.clone(), false);
                    }
                }
                // Nudge each rig's watch so SSE clients see server_connected=false.
                if let Ok(rig_map) = config.rig_states.read() {
                    for name in &owned {
                        if let Some(tx) = rig_map.get(name) {
                            tx.send_modify(|_| {});
                        }
                    }
                }
                // Also nudge the global state watch for backward compat.
                state_tx.send_modify(|_| {});
            }
            Ok(Err(e)) => {
                warn!("Remote connect failed: {}", e);
            }
            Err(_) => {
                warn!("Remote connect timed out after {:?}", CONNECT_TIMEOUT);
            }
        }

        tokio::select! {
            _ = time::sleep(reconnect_delay) => {}
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => {
                        info!("Remote client shutting down");
                        spectrum_task.abort();
                        return Ok(());
                    }
                    Ok(()) => {}
                    Err(_) => {
                        spectrum_task.abort();
                        return Ok(());
                    }
                }
            }
        }
        reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(10));
    }
}

/// Spectrum polling runs on a dedicated TCP connection so it never blocks
/// state polls or user commands on the main connection.  Reconnects
/// independently with a short fixed delay.
async fn run_spectrum_connection(
    config: RemoteClientConfig,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        match time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&config.addr)).await {
            Ok(Ok(stream)) => {
                if let Err(e) = stream.set_nodelay(true) {
                    warn!("Spectrum TCP_NODELAY failed: {}", e);
                }
                if let Err(e) = handle_spectrum_connection(&config, stream, &mut shutdown_rx).await
                {
                    warn!("Spectrum connection dropped: {}", e);
                }
                // Mark spectrum unavailable while reconnecting.
                config.spectrum.send_modify(|s| s.set(None, None));
            }
            Ok(Err(e)) => warn!("Spectrum connect failed: {}", e),
            Err(_) => warn!("Spectrum connect timed out"),
        }

        tokio::select! {
            _ = time::sleep(Duration::from_secs(1)) => {}
            changed = shutdown_rx.changed() => {
                if matches!(changed, Ok(()) | Err(_)) && *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }
}

async fn handle_spectrum_connection(
    config: &RemoteClientConfig,
    stream: TcpStream,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> RigResult<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut interval = time::interval(SPECTRUM_POLL_INTERVAL);

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => return Ok(()),
                    Ok(()) => {}
                    Err(_) => return Ok(()),
                }
            }
            _ = interval.tick() => {
                // Collect rig IDs that have active spectrum subscribers.
                let rig_ids = active_spectrum_rig_ids(config);

                if rig_ids.is_empty() {
                    // This server currently has no owned spectrum subscribers.
                    continue;
                }

                // Determine the currently selected rig for backward compat.
                let selected = selected_rig_id(config);

                for short_name in &rig_ids {
                    // Resolve the server rig_id for the wire envelope.
                    let wire_rig_id = if has_short_names(config) {
                        resolve_server_rig_id(config, short_name)
                    } else {
                        Some(short_name.clone())
                    };
                    let envelope = ClientEnvelope {
                        token: config.token.clone(),
                        rig_id: wire_rig_id,
                        cmd: ClientCommand::GetSpectrum,
                    };
                    match send_envelope_no_state_update(&mut writer, &mut reader, envelope).await {
                        Ok(snapshot) => {
                            // Update per-rig channel (keyed by short name).
                            if let Ok(map) = config.rig_spectrums.read() {
                                if let Some(tx) = map.get(short_name) {
                                    tx.send_modify(|s| s.set(snapshot.spectrum.clone(), snapshot.vchan_rds.clone()));
                                }
                            }
                            // Update global channel if this is the selected rig.
                            let is_selected = selected.as_deref() == Some(short_name.as_str());
                            if is_selected {
                                config.spectrum.send_modify(|s| s.set(snapshot.spectrum, snapshot.vchan_rds));
                            }
                        }
                        Err(e) => {
                            if selected
                                .as_deref()
                                .is_some_and(|rid| rid == short_name && config_owns_short_name(config, rid))
                            {
                                config.spectrum.send_modify(|s| s.set(None, None));
                            }
                            return Err(e);
                        }
                    }
                }
            }
        }
    }
}

async fn handle_connection(
    config: &RemoteClientConfig,
    stream: TcpStream,
    rx: &mut mpsc::Receiver<RigRequest>,
    state_tx: &watch::Sender<RigState>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> RigResult<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut poll_interval = time::interval(config.poll_interval);
    let mut last_poll = Instant::now();
    let mut poll_failure_streak: u32 = 0;

    // Prime rig list/state immediately after connect so frontends can render
    // rig selectors without waiting for the first poll interval.
    if let Err(e) = refresh_remote_snapshot(config, &mut writer, &mut reader, state_tx).await {
        warn!("Initial remote snapshot refresh failed: {}", e);
    }

    // Fetch satellite passes immediately and then every 5 minutes.
    let sat_pass_interval = Duration::from_secs(5 * 60);
    let mut last_sat_pass_refresh = Instant::now();
    match send_get_sat_passes(config, &mut writer, &mut reader).await {
        Ok(result) => {
            if let Ok(mut guard) = config.sat_passes.write() {
                *guard = Some(result);
            }
        }
        Err(e) => warn!("Initial sat passes fetch failed: {}", e),
    }

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => return Ok(()),
                    Ok(()) => {}
                    Err(_) => return Ok(()),
                }
            }
            _ = poll_interval.tick() => {
                if last_poll.elapsed() < config.poll_interval {
                    continue;
                }
                last_poll = Instant::now();
                if let Err(e) =
                    refresh_remote_snapshot(config, &mut writer, &mut reader, state_tx).await
                {
                    poll_failure_streak = poll_failure_streak.saturating_add(1);
                    warn!(
                        "Remote poll failed (streak={}): {}",
                        poll_failure_streak, e
                    );

                    let timeout_or_disconnect =
                        e.message.contains("timed out")
                            || e.message.contains("connection closed");
                    if timeout_or_disconnect {
                        return Err(e);
                    }
                    if poll_failure_streak >= MAX_CONSECUTIVE_POLL_FAILURES {
                        return Err(RigError::communication(format!(
                            "remote poll failed {} consecutive times: {}",
                            poll_failure_streak, e
                        )));
                    }
                } else {
                    poll_failure_streak = 0;
                }

                // Refresh satellite passes periodically (every 5 minutes).
                if last_sat_pass_refresh.elapsed() >= sat_pass_interval {
                    last_sat_pass_refresh = Instant::now();
                    match send_get_sat_passes(config, &mut writer, &mut reader).await {
                        Ok(result) => {
                            if let Ok(mut guard) = config.sat_passes.write() {
                                *guard = Some(result);
                            }
                        }
                        Err(e) => warn!("Sat passes refresh failed: {}", e),
                    }
                }
            }
            req = rx.recv() => {
                let Some(req) = req else {
                    return Ok(());
                };
                let rig_id_override = req.rig_id_override;
                let cmd = req.cmd;
                let result = {
                    let client_cmd = rig_command_to_client(cmd);
                    send_command(config, &mut writer, &mut reader, client_cmd, rig_id_override, state_tx).await
                };

                let _ = req.respond_to.send(result);
            }
        }
    }
}

async fn send_command(
    config: &RemoteClientConfig,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    cmd: ClientCommand,
    rig_id_override: Option<String>,
    state_tx: &watch::Sender<RigState>,
) -> RigResult<trx_core::RigSnapshot> {
    // Keep the original short name for per-rig channel update after response.
    let channel_key_override = rig_id_override.clone();
    let envelope = build_envelope(config, cmd, rig_id_override);

    let mut payload = serde_json::to_string(&envelope)
        .map_err(|e| RigError::communication(format!("JSON serialize failed: {e}")))?;
    payload.push('\n');

    time::timeout(IO_TIMEOUT, writer.write_all(payload.as_bytes()))
        .await
        .map_err(|_| RigError::communication(format!("write timed out after {:?}", IO_TIMEOUT)))?
        .map_err(|e| RigError::communication(format!("write failed: {e}")))?;
    time::timeout(IO_TIMEOUT, writer.flush())
        .await
        .map_err(|_| RigError::communication(format!("flush timed out after {:?}", IO_TIMEOUT)))?
        .map_err(|e| RigError::communication(format!("flush failed: {e}")))?;

    let line = time::timeout(IO_TIMEOUT, read_limited_line(reader, MAX_JSON_LINE_BYTES))
        .await
        .map_err(|_| RigError::communication(format!("read timed out after {:?}", IO_TIMEOUT)))?
        .map_err(|e| RigError::communication(format!("read failed: {e}")))?;
    let line = line.ok_or_else(|| RigError::communication("connection closed by remote"))?;

    let resp: ClientResponse = serde_json::from_str(line.trim_end())
        .map_err(|e| RigError::communication(format!("invalid response: {e}")))?;

    if resp.success {
        if let Some(snapshot) = resp.state {
            let new_state = RigState::from_snapshot(snapshot.clone());
            let _ = state_tx.send(new_state.clone());
            // Also update the per-rig watch channel so SSE sessions
            // subscribed to a specific rig see the change immediately
            // instead of waiting for the next poll cycle.
            // The rig_id_override is a short name in multi-server mode;
            // resolve accordingly for the per-rig channel key.
            let channel_key = channel_key_override
                .as_deref()
                .map(String::from)
                .or_else(|| selected_rig_id(config));
            if let Some(key) = channel_key {
                if let Ok(map) = config.rig_states.read() {
                    if let Some(tx) = map.get(&key) {
                        tx.send_if_modified(|old| {
                            if *old == new_state {
                                false
                            } else {
                                *old = new_state.clone();
                                true
                            }
                        });
                    }
                }
            }
            return Ok(snapshot);
        }
        return Err(RigError::communication("missing snapshot"));
    }

    Err(RigError::communication(
        resp.error.unwrap_or_else(|| "remote error".into()),
    ))
}

fn build_envelope(
    config: &RemoteClientConfig,
    cmd: ClientCommand,
    rig_id_override: Option<String>,
) -> ClientEnvelope {
    let rig_id = rig_id_override.or_else(|| selected_rig_id(config));
    // In multi-server mode, the rig_id is actually a short name that needs to
    // be translated back to the server-side rig_id for the wire envelope.
    let wire_rig_id = if has_short_names(config) {
        rig_id.and_then(|name| resolve_server_rig_id(config, &name))
    } else {
        rig_id
    };
    ClientEnvelope {
        token: config.token.clone(),
        rig_id: wire_rig_id,
        cmd,
    }
}

async fn refresh_remote_snapshot(
    config: &RemoteClientConfig,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    state_tx: &watch::Sender<RigState>,
) -> RigResult<()> {
    let rigs = send_get_rigs(config, writer, reader).await?;

    // In multi-server mode, filter rigs to only those that have a short name
    // mapping, and populate the reverse mapping (short_name → server rig_id).
    let mapped_rigs: Vec<(String, &RigEntry)> = if has_short_names(config) {
        let mut mapped = Vec::new();
        // Track which wildcard (None-key) entry we've already resolved.
        let mut wildcard_resolved = false;
        for entry in &rigs {
            if let Some(short_name) = config.rig_id_to_short_name.get(&Some(entry.rig_id.clone())) {
                // Update reverse map.
                if let Ok(mut rev) = config.short_name_to_rig_id.write() {
                    rev.insert(short_name.clone(), entry.rig_id.clone());
                }
                mapped.push((short_name.clone(), entry));
            } else if !wildcard_resolved {
                if let Some(short_name) = config.rig_id_to_short_name.get(&None) {
                    // Wildcard: first unmatched rig gets the default short name.
                    // Prefer an initialized, TX-capable rig when possible.
                    let candidate = choose_default_rig(&rigs)
                        .filter(|r| {
                            !config
                                .rig_id_to_short_name
                                .contains_key(&Some(r.rig_id.clone()))
                        })
                        .unwrap_or(entry);
                    if let Ok(mut rev) = config.short_name_to_rig_id.write() {
                        rev.insert(short_name.clone(), candidate.rig_id.clone());
                    }
                    mapped.push((short_name.clone(), candidate));
                    wildcard_resolved = true;
                }
            }
        }
        mapped
    } else {
        rigs.iter().map(|e| (e.rig_id.clone(), e)).collect()
    };

    cache_remote_rigs(config, &rigs, &mapped_rigs);

    if mapped_rigs.is_empty() {
        return Err(RigError::communication("GetRigs returned no mapped rigs"));
    }

    // Determine target for global state_tx (backward compat).
    let selected = selected_rig_id(config);
    let target_key = global_target_for_snapshot(selected.as_deref(), &mapped_rigs);

    if let Some((key, entry)) = target_key {
        if selected.is_none() {
            set_selected_rig_id(config, Some(key.clone()));
        }
        let new_state = RigState::from_snapshot(entry.state.clone());
        state_tx.send_if_modified(|old| {
            if *old == new_state {
                false
            } else {
                *old = new_state;
                true
            }
        });
    }

    // Update per-rig watch channels keyed by short name (or rig_id in legacy mode).
    if let Ok(mut rig_map) = config.rig_states.write() {
        for (key, entry) in &mapped_rigs {
            let new_state = RigState::from_snapshot(entry.state.clone());
            if let Some(tx) = rig_map.get(key) {
                tx.send_if_modified(|old| {
                    if *old == new_state {
                        false
                    } else {
                        *old = new_state;
                        true
                    }
                });
            } else {
                let (tx, _rx) = watch::channel(new_state);
                rig_map.insert(key.clone(), tx);
            }
        }
        // Remove channels for keys no longer present on this server while
        // keeping watch channels owned by other server groups intact.
        let active_keys: std::collections::HashSet<&str> =
            mapped_rigs.iter().map(|(k, _)| k.as_str()).collect();
        rig_map.retain(|id, _| {
            !config_owns_short_name(config, id) || active_keys.contains(id.as_str())
        });
    }
    // Mark all mapped rigs as connected now that we have a live snapshot.
    if let Ok(mut conn_map) = config.rig_server_connected.write() {
        for (key, _) in &mapped_rigs {
            conn_map.insert(key.clone(), true);
        }
    }
    Ok(())
}

async fn send_get_rigs(
    config: &RemoteClientConfig,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> RigResult<Vec<RigEntry>> {
    let envelope = build_envelope(config, ClientCommand::GetRigs, None);
    let mut payload = serde_json::to_string(&envelope)
        .map_err(|e| RigError::communication(format!("JSON serialize failed: {e}")))?;
    payload.push('\n');

    time::timeout(IO_TIMEOUT, writer.write_all(payload.as_bytes()))
        .await
        .map_err(|_| RigError::communication(format!("write timed out after {:?}", IO_TIMEOUT)))?
        .map_err(|e| RigError::communication(format!("write failed: {e}")))?;
    time::timeout(IO_TIMEOUT, writer.flush())
        .await
        .map_err(|_| RigError::communication(format!("flush timed out after {:?}", IO_TIMEOUT)))?
        .map_err(|e| RigError::communication(format!("flush failed: {e}")))?;

    let line = time::timeout(IO_TIMEOUT, read_limited_line(reader, MAX_JSON_LINE_BYTES))
        .await
        .map_err(|_| RigError::communication(format!("read timed out after {:?}", IO_TIMEOUT)))?
        .map_err(|e| RigError::communication(format!("read failed: {e}")))?;
    let line = line.ok_or_else(|| RigError::communication("connection closed by remote"))?;

    let resp: ClientResponse = serde_json::from_str(line.trim_end())
        .map_err(|e| RigError::communication(format!("invalid response: {e}")))?;
    if resp.success {
        return resp
            .rigs
            .ok_or_else(|| RigError::communication("missing rigs list in GetRigs response"));
    }

    Err(RigError::communication(
        resp.error.unwrap_or_else(|| "remote error".into()),
    ))
}

async fn send_get_sat_passes(
    config: &RemoteClientConfig,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> RigResult<trx_core::geo::PassPredictionResult> {
    let envelope = build_envelope(config, ClientCommand::GetSatPasses, None);
    let mut payload = serde_json::to_string(&envelope)
        .map_err(|e| RigError::communication(format!("JSON serialize failed: {e}")))?;
    payload.push('\n');

    time::timeout(IO_TIMEOUT, writer.write_all(payload.as_bytes()))
        .await
        .map_err(|_| RigError::communication(format!("write timed out after {:?}", IO_TIMEOUT)))?
        .map_err(|e| RigError::communication(format!("write failed: {e}")))?;
    time::timeout(IO_TIMEOUT, writer.flush())
        .await
        .map_err(|_| RigError::communication(format!("flush timed out after {:?}", IO_TIMEOUT)))?
        .map_err(|e| RigError::communication(format!("flush failed: {e}")))?;

    let line = time::timeout(IO_TIMEOUT, read_limited_line(reader, MAX_JSON_LINE_BYTES))
        .await
        .map_err(|_| RigError::communication(format!("read timed out after {:?}", IO_TIMEOUT)))?
        .map_err(|e| RigError::communication(format!("read failed: {e}")))?;
    let line = line.ok_or_else(|| RigError::communication("connection closed by remote"))?;

    let resp: ClientResponse = serde_json::from_str(line.trim_end())
        .map_err(|e| RigError::communication(format!("invalid response: {e}")))?;
    if resp.success {
        return resp
            .sat_passes
            .ok_or_else(|| RigError::communication("missing sat_passes in GetSatPasses response"));
    }

    Err(RigError::communication(
        resp.error.unwrap_or_else(|| "remote error".into()),
    ))
}

fn cache_remote_rigs(
    config: &RemoteClientConfig,
    _raw_rigs: &[RigEntry],
    mapped_rigs: &[(String, &RigEntry)],
) {
    if let Ok(mut guard) = config.known_rigs.lock() {
        let next = if has_short_names(config) {
            merge_known_rigs(config, &guard, mapped_rigs)
        } else {
            mapped_rigs
                .iter()
                .map(|(key, entry)| remote_rig_entry_from_snapshot(key, entry))
                .collect()
        };

        // Skip the Vec rebuild when the rig list is structurally unchanged.
        let unchanged = guard.len() == next.len()
            && guard
                .iter()
                .zip(next.iter())
                .all(|(cached, new)| same_remote_rig_entry(cached, new));
        if unchanged {
            return;
        }
        *guard = next;
    }
}

fn merge_known_rigs(
    config: &RemoteClientConfig,
    current: &[RemoteRigEntry],
    mapped_rigs: &[(String, &RigEntry)],
) -> Vec<RemoteRigEntry> {
    let owned_keys: std::collections::HashSet<String> =
        config.rig_id_to_short_name.values().cloned().collect();
    let mapped_by_key: std::collections::HashMap<&str, &RigEntry> = mapped_rigs
        .iter()
        .map(|(key, entry)| (key.as_str(), *entry))
        .collect();
    let mut merged = Vec::with_capacity(current.len().max(mapped_rigs.len()));
    let mut inserted: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for existing in current {
        if owned_keys.contains(&existing.rig_id) {
            if let Some(entry) = mapped_by_key.get(existing.rig_id.as_str()) {
                merged.push(remote_rig_entry_from_snapshot(&existing.rig_id, entry));
                inserted.insert(existing.rig_id.as_str());
            }
        } else {
            merged.push(existing.clone());
        }
    }

    for (key, entry) in mapped_rigs {
        if !inserted.contains(key.as_str()) {
            merged.push(remote_rig_entry_from_snapshot(key, entry));
        }
    }

    merged
}

fn remote_rig_entry_from_snapshot(key: &str, entry: &RigEntry) -> RemoteRigEntry {
    RemoteRigEntry {
        rig_id: key.to_string(),
        display_name: entry.display_name.clone(),
        state: entry.state.clone(),
        audio_port: entry.audio_port,
    }
}

fn same_remote_rig_entry(left: &RemoteRigEntry, right: &RemoteRigEntry) -> bool {
    left.rig_id == right.rig_id
        && left.display_name == right.display_name
        && left.state.initialized == right.state.initialized
        && left.audio_port == right.audio_port
}

fn selected_rig_id(config: &RemoteClientConfig) -> Option<String> {
    config.selected_rig_id.lock().ok().and_then(|g| g.clone())
}

fn global_target_for_snapshot<'a>(
    selected: Option<&str>,
    mapped_rigs: &'a [(String, &RigEntry)],
) -> Option<&'a (String, &'a RigEntry)> {
    selected
        .and_then(|id| mapped_rigs.iter().find(|(key, _)| key == id))
        .or_else(|| {
            if selected.is_none() {
                mapped_rigs.first()
            } else {
                None
            }
        })
}

fn config_owns_short_name(config: &RemoteClientConfig, short_name: &str) -> bool {
    if !has_short_names(config) {
        return true;
    }
    config
        .rig_id_to_short_name
        .values()
        .any(|name| name == short_name)
}

/// Returns `true` when the config has short-name mappings (multi-server mode).
fn has_short_names(config: &RemoteClientConfig) -> bool {
    !config.rig_id_to_short_name.is_empty()
}

/// Resolve a server rig_id to the client-side short name.
/// In legacy mode (no mappings), returns the rig_id unchanged.
#[cfg(test)]
fn resolve_short_name(config: &RemoteClientConfig, server_rig_id: &str) -> Option<String> {
    if !has_short_names(config) {
        return Some(server_rig_id.to_string());
    }
    // Try explicit rig_id mapping first.
    if let Some(name) = config
        .rig_id_to_short_name
        .get(&Some(server_rig_id.to_string()))
    {
        return Some(name.clone());
    }
    // Try wildcard (None key = "default rig on this server").
    config.rig_id_to_short_name.get(&None).cloned()
}

/// Resolve a client-side short name back to a server rig_id for building envelopes.
fn resolve_server_rig_id(config: &RemoteClientConfig, short_name: &str) -> Option<String> {
    if !has_short_names(config) {
        return Some(short_name.to_string());
    }
    config
        .short_name_to_rig_id
        .read()
        .ok()
        .and_then(|map| map.get(short_name).cloned())
}

fn set_selected_rig_id(config: &RemoteClientConfig, value: Option<String>) {
    if let Ok(mut guard) = config.selected_rig_id.lock() {
        *guard = value;
    }
}

/// Send a pre-built envelope and return the snapshot without updating state.
async fn send_envelope_no_state_update(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    envelope: ClientEnvelope,
) -> RigResult<trx_core::RigSnapshot> {
    let mut payload = serde_json::to_string(&envelope)
        .map_err(|e| RigError::communication(format!("JSON serialize failed: {e}")))?;
    payload.push('\n');
    time::timeout(SPECTRUM_IO_TIMEOUT, writer.write_all(payload.as_bytes()))
        .await
        .map_err(|_| {
            RigError::communication(format!("write timed out after {:?}", SPECTRUM_IO_TIMEOUT))
        })?
        .map_err(|e| RigError::communication(format!("write failed: {e}")))?;
    time::timeout(SPECTRUM_IO_TIMEOUT, writer.flush())
        .await
        .map_err(|_| {
            RigError::communication(format!("flush timed out after {:?}", SPECTRUM_IO_TIMEOUT))
        })?
        .map_err(|e| RigError::communication(format!("flush failed: {e}")))?;
    let line = time::timeout(
        SPECTRUM_IO_TIMEOUT,
        read_limited_line(reader, MAX_JSON_LINE_BYTES),
    )
    .await
    .map_err(|_| {
        RigError::communication(format!("read timed out after {:?}", SPECTRUM_IO_TIMEOUT))
    })?
    .map_err(|e| RigError::communication(format!("read failed: {e}")))?;
    let line = line.ok_or_else(|| RigError::communication("connection closed by remote"))?;
    let resp: ClientResponse = serde_json::from_str(line.trim_end())
        .map_err(|e| RigError::communication(format!("invalid response: {e}")))?;
    if resp.success {
        if let Some(snapshot) = resp.state {
            return Ok(snapshot);
        }
        return Err(RigError::communication("missing snapshot"));
    }
    Err(RigError::communication(
        resp.error.unwrap_or_else(|| "remote error".into()),
    ))
}

/// Collect rig IDs that have active per-rig spectrum subscribers or fall back
/// to the selected rig when only the global channel has subscribers.
fn active_spectrum_rig_ids(config: &RemoteClientConfig) -> Vec<String> {
    let mut ids = Vec::new();
    // Collect per-rig channels with active subscribers.
    if let Ok(map) = config.rig_spectrums.read() {
        for (rig_id, tx) in map.iter() {
            if tx.receiver_count() > 0 && config_owns_short_name(config, rig_id) {
                ids.push(rig_id.clone());
            }
        }
    }
    // If global channel has subscribers but no per-rig subscriber covers the
    // selected rig, add the selected rig so backward compat works.
    if config.spectrum.receiver_count() > 0 {
        if let Some(selected) = selected_rig_id(config) {
            if config_owns_short_name(config, &selected) && !ids.contains(&selected) {
                // Only add if the rig is initialized.
                let initialized = config
                    .known_rigs
                    .lock()
                    .ok()
                    .and_then(|entries| entries.iter().find(|e| e.rig_id == selected).cloned())
                    .map(|e| e.state.initialized)
                    .unwrap_or(true);
                if initialized {
                    ids.push(selected);
                }
            }
        }
    }
    // Filter to only initialized rigs.
    if let Ok(entries) = config.known_rigs.lock() {
        ids.retain(|id| {
            entries
                .iter()
                .find(|e| &e.rig_id == id)
                .map(|e| e.state.initialized)
                .unwrap_or(true)
        });
    }
    ids
}

fn choose_default_rig(rigs: &[RigEntry]) -> Option<&RigEntry> {
    rigs.iter().max_by_key(|entry| {
        let tx_capable = entry.state.info.capabilities.tx;
        let initialized = entry.state.initialized;
        // Prefer initialized TX-capable rigs; tie-break by rig_id for deterministic choice.
        (tx_capable, initialized, entry.rig_id.as_str())
    })
}

async fn read_limited_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    let mut line = Vec::with_capacity(256);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            let text = String::from_utf8(line).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("line is not valid UTF-8: {e}"),
                )
            })?;
            return Ok(Some(text));
        }

        if let Some(pos) = available.iter().position(|b| *b == b'\n') {
            let chunk = &available[..=pos];
            if line.len() + chunk.len() > max_bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("line exceeds maximum size of {max_bytes} bytes"),
                ));
            }
            line.extend_from_slice(chunk);
            reader.consume(pos + 1);
            let text = String::from_utf8(line).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("line is not valid UTF-8: {e}"),
                )
            })?;
            return Ok(Some(text));
        }

        if line.len() + available.len() > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("line exceeds maximum size of {max_bytes} bytes"),
            ));
        }

        line.extend_from_slice(available);
        let consumed = available.len();
        reader.consume(consumed);
    }
}

pub fn parse_remote_url(url: &str) -> Result<RemoteEndpoint, String> {
    parse_endpoint_url(url, DEFAULT_REMOTE_PORT, "remote")
}

pub fn parse_audio_url(url: &str) -> Result<RemoteEndpoint, String> {
    parse_endpoint_url(url, DEFAULT_AUDIO_PORT, "audio")
}

fn parse_endpoint_url(url: &str, default_port: u16, kind: &str) -> Result<RemoteEndpoint, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(format!("{kind} url is empty"));
    }

    let addr = trimmed
        .strip_prefix("tcp://")
        .or_else(|| trimmed.strip_prefix("http-json://"))
        .or_else(|| trimmed.strip_prefix("audio://"))
        .unwrap_or(trimmed);

    parse_host_port(addr, default_port, kind)
}

fn parse_host_port(input: &str, default_port: u16, kind: &str) -> Result<RemoteEndpoint, String> {
    if let Some(rest) = input.strip_prefix('[') {
        let closing = rest
            .find(']')
            .ok_or_else(|| format!("invalid {kind} url: missing closing ']' for IPv6 host"))?;
        let host = &rest[..closing];
        let remainder = &rest[closing + 1..];
        if host.is_empty() {
            return Err(format!("invalid {kind} url: host is empty"));
        }
        let port = if remainder.is_empty() {
            default_port
        } else if let Some(port_str) = remainder.strip_prefix(':') {
            parse_port(port_str, kind)?
        } else {
            return Err(format!("invalid {kind} url: expected ':<port>' after ']'"));
        };
        return Ok(RemoteEndpoint {
            host: host.to_string(),
            port,
        });
    }

    if input.contains(':') {
        if input.matches(':').count() > 1 {
            return Err(format!(
                "invalid {kind} url: IPv6 host must be bracketed like [::1]:4532"
            ));
        }
        let (host, port_str) = input
            .rsplit_once(':')
            .ok_or_else(|| format!("invalid {kind} url: expected host:port"))?;
        if host.is_empty() {
            return Err(format!("invalid {kind} url: host is empty"));
        }
        return Ok(RemoteEndpoint {
            host: host.to_string(),
            port: parse_port(port_str, kind)?,
        });
    }

    Ok(RemoteEndpoint {
        host: input.to_string(),
        port: default_port,
    })
}

fn parse_port(port_str: &str, kind: &str) -> Result<u16, String> {
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("invalid {kind} port: '{port_str}'"))?;
    if port == 0 {
        return Err(format!("invalid {kind} port: 0"));
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::{has_short_names, resolve_server_rig_id, resolve_short_name};
    use super::{
        parse_audio_url, parse_remote_url, RemoteClientConfig, RemoteEndpoint, SharedSpectrum,
    };
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::sync::{mpsc, watch};

    use trx_core::radio::freq::{Band, Freq};
    use trx_core::rig::state::RigSnapshot;
    use trx_core::rig::{RigAccessMethod, RigCapabilities, RigInfo, RigStatus, RigTxStatus};
    use trx_core::{RigMode, RigState};
    use trx_protocol::types::RigEntry;
    use trx_protocol::ClientResponse;

    #[test]
    fn parse_host_default_port() {
        let parsed = parse_remote_url("example.local").expect("must parse");
        assert_eq!(
            parsed,
            RemoteEndpoint {
                host: "example.local".to_string(),
                port: 4530
            }
        );
    }

    #[test]
    fn parse_audio_host_default_port() {
        let parsed = parse_audio_url("audio.example.local").expect("must parse");
        assert_eq!(
            parsed,
            RemoteEndpoint {
                host: "audio.example.local".to_string(),
                port: 4531
            }
        );
    }

    #[test]
    fn parse_ipv4_with_port() {
        let parsed = parse_remote_url("tcp://127.0.0.1:9000").expect("must parse");
        assert_eq!(
            parsed,
            RemoteEndpoint {
                host: "127.0.0.1".to_string(),
                port: 9000
            }
        );
    }

    #[test]
    fn parse_bracketed_ipv6() {
        let parsed = parse_remote_url("http-json://[::1]:7000").expect("must parse");
        assert_eq!(
            parsed,
            RemoteEndpoint {
                host: "::1".to_string(),
                port: 7000
            }
        );
    }

    #[test]
    fn reject_unbracketed_ipv6() {
        let err = parse_remote_url("::1:7000").expect_err("must fail");
        assert!(err.contains("must be bracketed"));
    }

    fn sample_snapshot() -> RigSnapshot {
        RigSnapshot {
            info: RigInfo {
                manufacturer: "Test".to_string(),
                model: "Dummy".to_string(),
                revision: "1".to_string(),
                capabilities: RigCapabilities {
                    min_freq_step_hz: 1,
                    supported_bands: vec![Band {
                        low_hz: 7_000_000,
                        high_hz: 7_200_000,
                        tx_allowed: true,
                    }],
                    supported_modes: vec![RigMode::USB],
                    num_vfos: 1,
                    lock: false,
                    lockable: true,
                    attenuator: false,
                    preamp: false,
                    rit: false,
                    rpt: false,
                    split: false,
                    tx: true,
                    tx_limit: true,
                    vfo_switch: true,
                    filter_controls: false,
                    signal_meter: true,
                },
                access: RigAccessMethod::Tcp {
                    addr: "127.0.0.1:1234".to_string(),
                },
            },
            status: RigStatus {
                freq: Freq { hz: 7_100_000 },
                mode: RigMode::USB,
                tx_en: false,
                vfo: None,
                tx: Some(RigTxStatus {
                    power: None,
                    limit: None,
                    swr: None,
                    alc: None,
                }),
                rx: None,
                lock: Some(false),
            },
            band: None,
            enabled: Some(true),
            initialized: true,
            server_callsign: Some("N0CALL".to_string()),
            server_version: Some("test".to_string()),
            server_build_date: Some("2026-01-01".to_string()),
            server_latitude: None,
            server_longitude: None,
            pskreporter_status: Some("Disabled".to_string()),
            aprs_is_status: Some("Disabled".to_string()),
            aprs_decode_enabled: false,
            hf_aprs_decode_enabled: false,
            cw_decode_enabled: false,
            ft8_decode_enabled: false,
            ft4_decode_enabled: false,
            ft2_decode_enabled: false,
            wspr_decode_enabled: false,
            cw_auto: true,
            cw_wpm: 15,
            cw_tone_hz: 700,
            wxsat_decode_enabled: false,
            lrpt_decode_enabled: false,
            filter: None,
            spectrum: None,
            vchan_rds: None,
        }
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn reconnects_and_updates_state_after_drop() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let snapshot = sample_snapshot();
        let response = serde_json::to_string(&ClientResponse {
            success: true,
            rig_id: Some("server".to_string()),
            state: None,
            rigs: Some(vec![RigEntry {
                rig_id: "default".to_string(),
                display_name: None,
                state: snapshot.clone(),
                audio_port: Some(4531),
            }]),
            sat_passes: None,
            error: None,
        })
        .expect("serialize response")
            + "\n";

        let server = tokio::spawn(async move {
            let (first, _) = listener.accept().await.expect("accept first");
            let (first_reader, _) = first.into_split();
            let mut first_reader = BufReader::new(first_reader);
            let mut buf = String::new();
            let _ = first_reader.read_line(&mut buf).await.expect("read first");

            let (second, _) = listener.accept().await.expect("accept second");
            let (second_reader, mut second_writer) = second.into_split();
            let mut second_reader = BufReader::new(second_reader);
            buf.clear();
            let _ = second_reader
                .read_line(&mut buf)
                .await
                .expect("read second");
            second_writer
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            second_writer.flush().await.expect("flush");
        });

        let (_req_tx, req_rx) = mpsc::channel(8);
        let (state_tx, mut state_rx) = watch::channel(RigState::new_uninitialized());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (spectrum_tx, _spectrum_rx) = watch::channel(SharedSpectrum::default());

        let client = tokio::spawn(super::run_remote_client(
            RemoteClientConfig {
                addr: addr.to_string(),
                token: None,
                selected_rig_id: Arc::new(Mutex::new(None)),
                known_rigs: Arc::new(Mutex::new(Vec::new())),
                poll_interval: Duration::from_millis(100),
                spectrum: Arc::new(spectrum_tx),
                server_connected: Arc::new(AtomicBool::new(false)),
                rig_server_connected: Arc::new(RwLock::new(HashMap::new())),
                rig_states: Arc::new(RwLock::new(HashMap::new())),
                rig_spectrums: Arc::new(RwLock::new(HashMap::new())),
                rig_id_to_short_name: HashMap::new(),
                short_name_to_rig_id: Arc::new(RwLock::new(HashMap::new())),
                sat_passes: Arc::new(RwLock::new(None)),
            },
            req_rx,
            state_tx,
            shutdown_rx,
        ));

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state_rx.borrow().initialized {
                    break;
                }
                state_rx.changed().await.expect("state channel");
            }
        })
        .await
        .expect("state update timeout");
        assert_eq!(state_rx.borrow().status.freq.hz, 7_100_000);

        let _ = shutdown_tx.send(true);
        tokio::time::timeout(Duration::from_secs(2), async {
            let _ = client.await;
        })
        .await
        .expect("client shutdown timeout");
        let _ = server.await;
    }

    #[test]
    fn build_envelope_includes_rig_id() {
        let (spectrum_tx, _spectrum_rx) = watch::channel(SharedSpectrum::default());
        let config = RemoteClientConfig {
            addr: "127.0.0.1:4530".to_string(),
            token: Some("secret".to_string()),
            selected_rig_id: Arc::new(Mutex::new(Some("sdr".to_string()))),
            known_rigs: Arc::new(Mutex::new(Vec::new())),
            poll_interval: Duration::from_millis(500),
            spectrum: Arc::new(spectrum_tx),
            server_connected: Arc::new(AtomicBool::new(false)),
            rig_server_connected: Arc::new(RwLock::new(HashMap::new())),
            rig_states: Arc::new(RwLock::new(HashMap::new())),
            rig_spectrums: Arc::new(RwLock::new(HashMap::new())),
            rig_id_to_short_name: HashMap::new(),
            short_name_to_rig_id: Arc::new(RwLock::new(HashMap::new())),
            sat_passes: Arc::new(RwLock::new(None)),
        };
        let envelope = super::build_envelope(&config, trx_protocol::ClientCommand::GetState, None);
        assert_eq!(envelope.token.as_deref(), Some("secret"));
        assert_eq!(envelope.rig_id.as_deref(), Some("sdr"));
    }

    #[test]
    fn build_envelope_translates_short_name_to_server_rig_id() {
        let (spectrum_tx, _spectrum_rx) = watch::channel(SharedSpectrum::default());
        let short_name_to_rig_id = Arc::new(RwLock::new(HashMap::from([(
            "home-hf".to_string(),
            "hf".to_string(),
        )])));
        let config = RemoteClientConfig {
            addr: "127.0.0.1:4530".to_string(),
            token: None,
            selected_rig_id: Arc::new(Mutex::new(Some("home-hf".to_string()))),
            known_rigs: Arc::new(Mutex::new(Vec::new())),
            poll_interval: Duration::from_millis(500),
            spectrum: Arc::new(spectrum_tx),
            server_connected: Arc::new(AtomicBool::new(false)),
            rig_server_connected: Arc::new(RwLock::new(HashMap::new())),
            rig_states: Arc::new(RwLock::new(HashMap::new())),
            rig_spectrums: Arc::new(RwLock::new(HashMap::new())),
            rig_id_to_short_name: HashMap::from([(Some("hf".to_string()), "home-hf".to_string())]),
            short_name_to_rig_id,
            sat_passes: Arc::new(RwLock::new(None)),
        };
        // selected_rig_id is "home-hf" (short name), envelope should translate to "hf"
        let envelope = super::build_envelope(&config, trx_protocol::ClientCommand::GetState, None);
        assert_eq!(envelope.rig_id.as_deref(), Some("hf"));

        // Override with short name should also translate
        let envelope = super::build_envelope(
            &config,
            trx_protocol::ClientCommand::GetState,
            Some("home-hf".to_string()),
        );
        assert_eq!(envelope.rig_id.as_deref(), Some("hf"));
    }

    #[test]
    fn resolve_short_name_legacy_passthrough() {
        let (spectrum_tx, _spectrum_rx) = watch::channel(SharedSpectrum::default());
        let config = RemoteClientConfig {
            addr: "127.0.0.1:4530".to_string(),
            token: None,
            selected_rig_id: Arc::new(Mutex::new(None)),
            known_rigs: Arc::new(Mutex::new(Vec::new())),
            poll_interval: Duration::from_millis(500),
            spectrum: Arc::new(spectrum_tx),
            server_connected: Arc::new(AtomicBool::new(false)),
            rig_server_connected: Arc::new(RwLock::new(HashMap::new())),
            rig_states: Arc::new(RwLock::new(HashMap::new())),
            rig_spectrums: Arc::new(RwLock::new(HashMap::new())),
            rig_id_to_short_name: HashMap::new(),
            short_name_to_rig_id: Arc::new(RwLock::new(HashMap::new())),
            sat_passes: Arc::new(RwLock::new(None)),
        };
        // Legacy mode: rig_id passes through unchanged
        assert!(!has_short_names(&config));
        assert_eq!(resolve_short_name(&config, "hf"), Some("hf".to_string()));
    }

    #[test]
    fn resolve_short_name_with_mapping() {
        let (spectrum_tx, _spectrum_rx) = watch::channel(SharedSpectrum::default());
        let config = RemoteClientConfig {
            addr: "127.0.0.1:4530".to_string(),
            token: None,
            selected_rig_id: Arc::new(Mutex::new(None)),
            known_rigs: Arc::new(Mutex::new(Vec::new())),
            poll_interval: Duration::from_millis(500),
            spectrum: Arc::new(spectrum_tx),
            server_connected: Arc::new(AtomicBool::new(false)),
            rig_server_connected: Arc::new(RwLock::new(HashMap::new())),
            rig_states: Arc::new(RwLock::new(HashMap::new())),
            rig_spectrums: Arc::new(RwLock::new(HashMap::new())),
            rig_id_to_short_name: HashMap::from([
                (Some("hf".to_string()), "home-hf".to_string()),
                (None, "default-rig".to_string()),
            ]),
            short_name_to_rig_id: Arc::new(RwLock::new(HashMap::new())),
            sat_passes: Arc::new(RwLock::new(None)),
        };
        assert!(has_short_names(&config));
        assert_eq!(
            resolve_short_name(&config, "hf"),
            Some("home-hf".to_string())
        );
        // Unknown rig_id falls through to wildcard
        assert_eq!(
            resolve_short_name(&config, "unknown"),
            Some("default-rig".to_string())
        );
    }

    #[test]
    fn cache_remote_rigs_keeps_other_server_entries() {
        let (spectrum_tx, _spectrum_rx) = watch::channel(SharedSpectrum::default());
        let known_rigs = Arc::new(Mutex::new(vec![trx_frontend::RemoteRigEntry {
            rig_id: "lidzbark-vhf".to_string(),
            display_name: Some("Lidzbark VHF".to_string()),
            state: sample_snapshot(),
            audio_port: Some(4531),
        }]));
        let config = RemoteClientConfig {
            addr: "127.0.0.1:4530".to_string(),
            token: None,
            selected_rig_id: Arc::new(Mutex::new(None)),
            known_rigs: known_rigs.clone(),
            poll_interval: Duration::from_millis(500),
            spectrum: Arc::new(spectrum_tx),
            server_connected: Arc::new(AtomicBool::new(false)),
            rig_server_connected: Arc::new(RwLock::new(HashMap::new())),
            rig_states: Arc::new(RwLock::new(HashMap::new())),
            rig_spectrums: Arc::new(RwLock::new(HashMap::new())),
            rig_id_to_short_name: HashMap::from([(Some("hf".to_string()), "gdansk".to_string())]),
            short_name_to_rig_id: Arc::new(RwLock::new(HashMap::new())),
            sat_passes: Arc::new(RwLock::new(None)),
        };
        let snapshot = sample_snapshot();
        let rigs = vec![RigEntry {
            rig_id: "hf".to_string(),
            display_name: Some("Gdansk HF".to_string()),
            state: snapshot,
            audio_port: Some(4532),
        }];
        let mapped = vec![("gdansk".to_string(), &rigs[0])];

        super::cache_remote_rigs(&config, &rigs, &mapped);

        let cached = known_rigs.lock().expect("known rigs lock");
        assert_eq!(cached.len(), 2);
        assert!(cached.iter().any(|entry| entry.rig_id == "lidzbark-vhf"));
        assert!(cached.iter().any(|entry| {
            entry.rig_id == "gdansk"
                && entry.display_name.as_deref() == Some("Gdansk HF")
                && entry.audio_port == Some(4532)
        }));
    }

    #[test]
    fn global_target_for_snapshot_skips_other_server_selection() {
        let snapshot = sample_snapshot();
        let rigs = vec![RigEntry {
            rig_id: "hf".to_string(),
            display_name: Some("Gdansk HF".to_string()),
            state: snapshot,
            audio_port: Some(4532),
        }];
        let mapped = vec![("gdansk".to_string(), &rigs[0])];

        let target = super::global_target_for_snapshot(Some("lidzbark-vhf"), &mapped);
        assert!(target.is_none());

        let target = super::global_target_for_snapshot(None, &mapped);
        assert_eq!(target.map(|(key, _)| key.as_str()), Some("gdansk"));
    }

    #[test]
    fn active_spectrum_rig_ids_only_returns_owned_selected_rig() {
        let (spectrum_tx, _spectrum_rx) = watch::channel(SharedSpectrum::default());
        let selected_rig_id = Arc::new(Mutex::new(Some("lidzbark-vhf".to_string())));
        let known_rigs = Arc::new(Mutex::new(vec![
            trx_frontend::RemoteRigEntry {
                rig_id: "gdansk".to_string(),
                display_name: Some("Gdansk".to_string()),
                state: sample_snapshot(),
                audio_port: Some(4532),
            },
            trx_frontend::RemoteRigEntry {
                rig_id: "lidzbark-vhf".to_string(),
                display_name: Some("Lidzbark VHF".to_string()),
                state: sample_snapshot(),
                audio_port: Some(4531),
            },
        ]));
        let config = RemoteClientConfig {
            addr: "127.0.0.1:4530".to_string(),
            token: None,
            selected_rig_id,
            known_rigs,
            poll_interval: Duration::from_millis(500),
            spectrum: Arc::new(spectrum_tx),
            server_connected: Arc::new(AtomicBool::new(false)),
            rig_server_connected: Arc::new(RwLock::new(HashMap::new())),
            rig_states: Arc::new(RwLock::new(HashMap::new())),
            rig_spectrums: Arc::new(RwLock::new(HashMap::new())),
            rig_id_to_short_name: HashMap::from([(Some("hf".to_string()), "gdansk".to_string())]),
            short_name_to_rig_id: Arc::new(RwLock::new(HashMap::new())),
            sat_passes: Arc::new(RwLock::new(None)),
        };

        let ids = super::active_spectrum_rig_ids(&config);
        assert!(ids.is_empty());
    }
}
