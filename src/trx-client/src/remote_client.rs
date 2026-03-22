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
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const SPECTRUM_IO_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_JSON_LINE_BYTES: usize = 16 * 1024;
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
    pub rig_states: Arc<RwLock<HashMap<String, watch::Sender<RigState>>>>,
    /// Per-rig spectrum watch senders, keyed by rig_id.
    pub rig_spectrums: Arc<RwLock<HashMap<String, watch::Sender<SharedSpectrum>>>>,
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
                    // No subscribers at all — clear global and skip.
                    config.spectrum.send_modify(|s| s.set(None, None));
                    continue;
                }

                // Determine the currently selected rig for backward compat.
                let selected = selected_rig_id(config);

                for rig_id in &rig_ids {
                    let envelope = ClientEnvelope {
                        token: config.token.clone(),
                        rig_id: Some(rig_id.clone()),
                        cmd: ClientCommand::GetSpectrum,
                    };
                    match send_envelope_no_state_update(&mut writer, &mut reader, envelope).await {
                        Ok(snapshot) => {
                            // Update per-rig channel.
                            if let Ok(map) = config.rig_spectrums.read() {
                                if let Some(tx) = map.get(rig_id) {
                                    tx.send_modify(|s| s.set(snapshot.spectrum.clone(), snapshot.vchan_rds.clone()));
                                }
                            }
                            // Update global channel if this is the selected rig.
                            let is_selected = selected.as_deref() == Some(rig_id.as_str());
                            if is_selected {
                                config.spectrum.send_modify(|s| s.set(snapshot.spectrum, snapshot.vchan_rds));
                            }
                        }
                        Err(e) => {
                            // A spectrum timeout desynchronises the TCP framing;
                            // return so the caller reconnects and restores sync.
                            config.spectrum.send_modify(|s| s.set(None, None));
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
            let _ = state_tx.send(RigState::from_snapshot(snapshot.clone()));
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
    ClientEnvelope {
        token: config.token.clone(),
        rig_id: rig_id_override.or_else(|| selected_rig_id(config)),
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
    cache_remote_rigs(config, &rigs);
    if rigs.is_empty() {
        return Err(RigError::communication("GetRigs returned no rigs"));
    }

    let selected = selected_rig_id(config);
    let target = selected
        .as_deref()
        .and_then(|id| rigs.iter().find(|entry| entry.rig_id == id))
        .or_else(|| choose_default_rig(rigs.as_slice()))
        .ok_or_else(|| RigError::communication("GetRigs returned no selectable rig"))?;

    if selected.as_deref() != Some(target.rig_id.as_str()) {
        set_selected_rig_id(config, Some(target.rig_id.clone()));
    }

    let new_state = RigState::from_snapshot(target.state.clone());
    // Only wake SSE subscribers when something actually changed.
    state_tx.send_if_modified(|old| {
        if *old == new_state {
            false
        } else {
            *old = new_state;
            true
        }
    });

    // Update per-rig watch channels so each SSE session can subscribe
    // to a specific rig's state independently.
    if let Ok(mut rig_map) = config.rig_states.write() {
        for entry in &rigs {
            let new_state = RigState::from_snapshot(entry.state.clone());
            if let Some(tx) = rig_map.get(&entry.rig_id) {
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
                rig_map.insert(entry.rig_id.clone(), tx);
            }
        }
        // Remove channels for rigs no longer reported by the server.
        let active_ids: std::collections::HashSet<&str> =
            rigs.iter().map(|e| e.rig_id.as_str()).collect();
        rig_map.retain(|id, _| active_ids.contains(id.as_str()));
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

fn cache_remote_rigs(config: &RemoteClientConfig, rigs: &[RigEntry]) {
    if let Ok(mut guard) = config.known_rigs.lock() {
        // Skip the Vec rebuild when the rig list is structurally unchanged.
        // We compare the fields surfaced in the UI rig picker; full state
        // changes are propagated via the watch channel, not this cache.
        let unchanged = guard.len() == rigs.len()
            && guard.iter().zip(rigs.iter()).all(|(cached, new)| {
                cached.rig_id == new.rig_id
                    && cached.display_name == new.display_name
                    && cached.state.initialized == new.state.initialized
                    && cached.audio_port == new.audio_port
            });
        if unchanged {
            return;
        }
        *guard = rigs
            .iter()
            .map(|entry| RemoteRigEntry {
                rig_id: entry.rig_id.clone(),
                display_name: entry.display_name.clone(),
                state: entry.state.clone(),
                audio_port: entry.audio_port,
            })
            .collect();
    }
}

fn selected_rig_id(config: &RemoteClientConfig) -> Option<String> {
    config.selected_rig_id.lock().ok().and_then(|g| g.clone())
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
            if tx.receiver_count() > 0 {
                ids.push(rig_id.clone());
            }
        }
    }
    // If global channel has subscribers but no per-rig subscriber covers the
    // selected rig, add the selected rig so backward compat works.
    if config.spectrum.receiver_count() > 0 {
        if let Some(selected) = selected_rig_id(config) {
            if !ids.contains(&selected) {
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
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("remote url is empty".into());
    }

    let addr = trimmed
        .strip_prefix("tcp://")
        .or_else(|| trimmed.strip_prefix("http-json://"))
        .unwrap_or(trimmed);

    parse_host_port(addr)
}

fn parse_host_port(input: &str) -> Result<RemoteEndpoint, String> {
    if let Some(rest) = input.strip_prefix('[') {
        let closing = rest
            .find(']')
            .ok_or("invalid remote url: missing closing ']' for IPv6 host")?;
        let host = &rest[..closing];
        let remainder = &rest[closing + 1..];
        if host.is_empty() {
            return Err("invalid remote url: host is empty".into());
        }
        let port = if remainder.is_empty() {
            DEFAULT_REMOTE_PORT
        } else if let Some(port_str) = remainder.strip_prefix(':') {
            parse_port(port_str)?
        } else {
            return Err("invalid remote url: expected ':<port>' after ']'".into());
        };
        return Ok(RemoteEndpoint {
            host: host.to_string(),
            port,
        });
    }

    if input.contains(':') {
        if input.matches(':').count() > 1 {
            return Err("invalid remote url: IPv6 host must be bracketed like [::1]:4532".into());
        }
        let (host, port_str) = input
            .rsplit_once(':')
            .ok_or("invalid remote url: expected host:port")?;
        if host.is_empty() {
            return Err("invalid remote url: host is empty".into());
        }
        return Ok(RemoteEndpoint {
            host: host.to_string(),
            port: parse_port(port_str)?,
        });
    }

    Ok(RemoteEndpoint {
        host: input.to_string(),
        port: DEFAULT_REMOTE_PORT,
    })
}

fn parse_port(port_str: &str) -> Result<u16, String> {
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("invalid remote port: '{port_str}'"))?;
    if port == 0 {
        return Err("invalid remote port: 0".into());
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::{parse_remote_url, RemoteClientConfig, RemoteEndpoint, SharedSpectrum};
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
                rig_states: Arc::new(RwLock::new(HashMap::new())),
                rig_spectrums: Arc::new(RwLock::new(HashMap::new())),
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
            rig_states: Arc::new(RwLock::new(HashMap::new())),
            rig_spectrums: Arc::new(RwLock::new(HashMap::new())),
        };
        let envelope = super::build_envelope(&config, trx_protocol::ClientCommand::GetState, None);
        assert_eq!(envelope.token.as_deref(), Some("secret"));
        assert_eq!(envelope.rig_id.as_deref(), Some("sdr"));
    }
}
