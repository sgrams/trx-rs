// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! JSON-over-TCP listener for trx-server.
//!
//! Accepts client connections speaking the `ClientEnvelope`/`ClientResponse`
//! protocol defined in `trx-protocol`.
//!
//! Multi-rig routing: `ClientEnvelope.rig_id` selects the target rig.
//! When absent the first rig in the map is used (backward compat).

use std::collections::HashMap;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch};
use tokio::time;
use tracing::{error, info, warn};

use trx_core::rig::command::RigCommand;
use trx_core::rig::request::RigRequest;
use trx_protocol::auth::{SimpleTokenValidator, TokenValidator};
use trx_protocol::codec::parse_envelope;
use trx_protocol::mapping;
use trx_protocol::types::{ClientCommand, RigEntry};
use trx_protocol::ClientResponse;

use crate::rig_handle::RigHandle;

/// Fallback I/O timeout used when no config value is provided.
const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(10);
/// Fallback request timeout used when no config value is provided.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_JSON_LINE_BYTES: usize = 256 * 1024;
/// Maximum concurrent connections allowed from a single IP address.
const MAX_CONNECTIONS_PER_IP: usize = 10;

/// Configurable timeout values for the listener, threaded from `[timeouts]`.
#[derive(Debug, Clone, Copy)]
pub struct ListenerTimeouts {
    /// Maximum time for low-level I/O operations (read/write/flush).
    pub io_timeout: Duration,
    /// Maximum time to wait for a rig command response.
    pub request_timeout: Duration,
}

impl Default for ListenerTimeouts {
    fn default() -> Self {
        Self {
            io_timeout: DEFAULT_IO_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}
/// How long to cache satellite pass predictions before recomputing.
/// SGP4 propagation for 200+ satellites is CPU-intensive; caching avoids
/// redundant recomputation when multiple clients request passes concurrently.
const SAT_PASS_CACHE_TTL: Duration = Duration::from_secs(60);

/// Cached satellite pass prediction result shared across client connections.
struct SatPassCache {
    result: trx_core::geo::PassPredictionResult,
    computed_at: Instant,
}
/// Per-IP connection tracker for rate limiting.
struct ConnectionTracker {
    counts: HashMap<std::net::IpAddr, usize>,
}

impl ConnectionTracker {
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }

    fn try_acquire(&mut self, ip: std::net::IpAddr) -> bool {
        let count = self.counts.entry(ip).or_insert(0);
        if *count >= MAX_CONNECTIONS_PER_IP {
            false
        } else {
            *count += 1;
            true
        }
    }

    fn release(&mut self, ip: std::net::IpAddr) {
        if let Some(count) = self.counts.get_mut(&ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.counts.remove(&ip);
            }
        }
    }
}

/// Shared state passed to each client handler.
struct ClientContext {
    rigs: Arc<HashMap<String, RigHandle>>,
    default_rig_id: String,
    validator: Arc<SimpleTokenValidator>,
    station_coords: Option<(f64, f64)>,
    sat_pass_cache: Arc<Mutex<Option<SatPassCache>>>,
    timeouts: ListenerTimeouts,
}

/// Run the JSON TCP listener, accepting client connections.
///
/// `rigs` is a shared map from rig_id → `RigHandle`.  The first entry (by
/// insertion order — deterministic after MR-07 iterates `resolved_rigs()` in
/// order) is the default rig for backward-compat clients that omit `rig_id`.
pub async fn run_listener(
    addr: SocketAddr,
    rigs: Arc<HashMap<String, RigHandle>>,
    default_rig_id: String,
    auth_tokens: HashSet<String>,
    station_coords: Option<(f64, f64)>,
    timeouts: ListenerTimeouts,
    mut shutdown_rx: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!("Listening on {}", addr);
    let validator = Arc::new(SimpleTokenValidator::new(auth_tokens));
    let sat_pass_cache: Arc<Mutex<Option<SatPassCache>>> = Arc::new(Mutex::new(None));
    let conn_tracker = Arc::new(Mutex::new(ConnectionTracker::new()));

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (socket, peer) = accept?;

                // Per-IP connection rate limiting.
                let peer_ip = peer.ip();
                {
                    let mut tracker = conn_tracker.lock().unwrap_or_else(|e| e.into_inner());
                    if !tracker.try_acquire(peer_ip) {
                        warn!("Rejecting connection from {} (per-IP limit reached)", peer);
                        drop(socket);
                        continue;
                    }
                }

                info!("Client connected: {}", peer);

                let ctx = ClientContext {
                    rigs: Arc::clone(&rigs),
                    default_rig_id: default_rig_id.clone(),
                    validator: Arc::clone(&validator),
                    station_coords,
                    sat_pass_cache: Arc::clone(&sat_pass_cache),
                    timeouts,
                };
                let client_shutdown_rx = shutdown_rx.clone();
                let tracker_clone = Arc::clone(&conn_tracker);
                tokio::spawn(async move {
                    if let Err(e) = handle_client(socket, peer, ctx, client_shutdown_rx).await {
                        error!("Client {} error: {:?}", peer, e);
                    }
                    // Release connection slot when client disconnects.
                    if let Ok(mut tracker) = tracker_clone.lock() {
                        tracker.release(peer_ip);
                    }
                });
            }
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => {
                        info!("Listener shutting down");
                        break;
                    }
                    Ok(()) => {}
                    Err(_) => break,
                }
            }
        }
    }
    Ok(())
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

async fn send_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    response: &ClientResponse,
    io_timeout: Duration,
) -> std::io::Result<()> {
    let resp_line = serde_json::to_string(response).map_err(std::io::Error::other)? + "\n";
    time::timeout(io_timeout, writer.write_all(resp_line.as_bytes()))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "response write timeout")
        })??;
    time::timeout(io_timeout, writer.flush())
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "response flush timeout")
        })??;
    Ok(())
}

async fn handle_client(
    socket: TcpStream,
    addr: SocketAddr,
    ctx: ClientContext,
    mut shutdown_rx: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let ClientContext {
        rigs,
        default_rig_id,
        validator,
        station_coords,
        sat_pass_cache,
        timeouts,
    } = ctx;
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);

    loop {
        let line = tokio::select! {
            read = time::timeout(timeouts.io_timeout, read_limited_line(&mut reader, MAX_JSON_LINE_BYTES)) => {
                match read {
                    Ok(Ok(line)) => line,
                    Ok(Err(e)) => return Err(e),
                    Err(_) => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "read timeout waiting for client request",
                        ));
                    }
                }
            }
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => {
                        info!("Client {} closing due to shutdown", addr);
                        break;
                    }
                    Ok(()) => continue,
                    Err(_) => break,
                }
            }
        };
        let Some(line) = line else {
            info!("Client {} disconnected", addr);
            break;
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let envelope = match parse_envelope(trimmed) {
            Ok(envelope) => envelope,
            Err(e) => {
                // Truncate raw input in logs to prevent information disclosure.
                let preview = if trimmed.len() > 128 {
                    format!("{}...", &trimmed[..128])
                } else {
                    trimmed.to_string()
                };
                error!("Invalid JSON from {}: {} / {:?}", addr, preview, e);
                let resp = ClientResponse {
                    success: false,
                    rig_id: None,
                    protocol_version: None,
                    state: None,
                    rigs: None,
                    sat_passes: None,
                    error: Some(format!("Invalid JSON: {}", e)),
                };
                send_response(&mut writer, &resp, timeouts.io_timeout).await?;
                continue;
            }
        };

        if let Err(err) = validator.as_ref().validate(&envelope.token) {
            let resp = ClientResponse {
                success: false,
                rig_id: None,
                protocol_version: None,
                state: None,
                rigs: None,
                sat_passes: None,
                error: Some(err),
            };
            send_response(&mut writer, &resp, timeouts.io_timeout).await?;
            continue;
        }

        // Resolve rig_id from the envelope (absent = default).
        let target_rig_id = envelope
            .rig_id
            .as_deref()
            .unwrap_or(&default_rig_id)
            .to_string();

        // GetRigs: aggregate all rig states and return without hitting any task.
        if matches!(envelope.cmd, ClientCommand::GetRigs) {
            let mut entries: Vec<RigEntry> = Vec::new();
            for handle in rigs.values() {
                let state = handle.state_rx.borrow().clone();
                if let Some(snapshot) = state.snapshot() {
                    entries.push(RigEntry {
                        rig_id: handle.rig_id.clone(),
                        display_name: Some(handle.display_name.clone()),
                        state: snapshot,
                        audio_port: Some(handle.audio_port),
                    });
                }
            }
            let resp = ClientResponse {
                success: true,
                rig_id: Some("server".to_string()),
                protocol_version: None,
                state: None,
                rigs: Some(entries),
                sat_passes: None,
                error: None,
            };
            send_response(&mut writer, &resp, timeouts.io_timeout).await?;
            continue;
        }

        // GetSatPasses: compute satellite passes from the server-side TLE store.
        // Results are cached for SAT_PASS_CACHE_TTL to avoid redundant CPU-heavy
        // SGP4 propagation when multiple clients request passes concurrently.
        if matches!(envelope.cmd, ClientCommand::GetSatPasses) {
            // Check cache first.
            let cached = sat_pass_cache.lock().ok().and_then(|guard| {
                guard.as_ref().and_then(|c| {
                    if c.computed_at.elapsed() < SAT_PASS_CACHE_TTL {
                        Some(c.result.clone())
                    } else {
                        None
                    }
                })
            });

            let result = if let Some(cached_result) = cached {
                cached_result
            } else if let Some((lat, lon)) = station_coords {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                let window_ms = 24 * 3600 * 1000; // 24 hours
                let fresh = match time::timeout(
                    Duration::from_secs(30),
                    tokio::task::spawn_blocking(move || {
                        trx_core::geo::compute_upcoming_passes(lat, lon, now_ms, window_ms)
                    }),
                )
                .await
                {
                    Ok(Ok(result)) => result,
                    Ok(Err(e)) => {
                        warn!("Satellite pass computation panicked: {:?}", e);
                        trx_core::geo::PassPredictionResult {
                            passes: vec![],
                            satellite_count: 0,
                            tle_source: trx_core::geo::TleSource::Unavailable,
                        }
                    }
                    Err(_) => {
                        warn!("Satellite pass computation timed out after 30s");
                        trx_core::geo::PassPredictionResult {
                            passes: vec![],
                            satellite_count: 0,
                            tle_source: trx_core::geo::TleSource::Unavailable,
                        }
                    }
                };
                // Update cache.
                if let Ok(mut guard) = sat_pass_cache.lock() {
                    *guard = Some(SatPassCache {
                        result: fresh.clone(),
                        computed_at: Instant::now(),
                    });
                }
                fresh
            } else {
                trx_core::geo::PassPredictionResult {
                    passes: vec![],
                    satellite_count: 0,
                    tle_source: trx_core::geo::TleSource::Unavailable,
                }
            };
            let resp = ClientResponse {
                success: true,
                rig_id: Some("server".to_string()),
                protocol_version: None,
                state: None,
                rigs: None,
                sat_passes: Some(result),
                error: None,
            };
            send_response(&mut writer, &resp, timeouts.io_timeout).await?;
            continue;
        }

        // Look up the target rig handle.
        let handle = match rigs.get(&target_rig_id) {
            Some(h) => h,
            None => {
                warn!("Unknown rig_id '{}' from {}", target_rig_id, addr);
                let resp = ClientResponse {
                    success: false,
                    rig_id: Some(target_rig_id.clone()),
                    protocol_version: None,
                    state: None,
                    rigs: None,
                    sat_passes: None,
                    error: Some(format!("Unknown rig_id: {}", target_rig_id)),
                };
                send_response(&mut writer, &resp, timeouts.io_timeout).await?;
                continue;
            }
        };

        let rig_cmd = mapping::client_command_to_rig(envelope.cmd);
        // Fast path: serve GetSnapshot directly from the watch channel
        // so clients get a response even while the rig task is initializing.
        if matches!(rig_cmd, RigCommand::GetSnapshot) {
            let state = handle.state_rx.borrow().clone();
            if let Some(snapshot) = state.snapshot() {
                let resp = ClientResponse {
                    success: true,
                    rig_id: Some(target_rig_id.clone()),
                    protocol_version: None,
                    state: Some(snapshot),
                    rigs: None,
                    sat_passes: None,
                    error: None,
                };
                send_response(&mut writer, &resp, timeouts.io_timeout).await?;
                continue;
            }
        }

        let (resp_tx, resp_rx) = oneshot::channel();
        let req = RigRequest {
            cmd: rig_cmd,
            respond_to: resp_tx,
            rig_id_override: None,
        };

        match time::timeout(timeouts.io_timeout, handle.rig_tx.send(req)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                error!(
                    "Failed to send request to rig_task for '{}': {:?}",
                    target_rig_id, e
                );
                let resp = ClientResponse {
                    success: false,
                    rig_id: Some(target_rig_id.clone()),
                    protocol_version: None,
                    state: None,
                    rigs: None,
                    sat_passes: None,
                    error: Some("Internal error: rig task not available".into()),
                };
                send_response(&mut writer, &resp, timeouts.io_timeout).await?;
                continue;
            }
            Err(_) => {
                let resp = ClientResponse {
                    success: false,
                    rig_id: Some(target_rig_id.clone()),
                    protocol_version: None,
                    state: None,
                    rigs: None,
                    sat_passes: None,
                    error: Some("Internal error: request queue timeout".into()),
                };
                send_response(&mut writer, &resp, timeouts.io_timeout).await?;
                continue;
            }
        }

        match tokio::select! {
            result = time::timeout(timeouts.request_timeout, resp_rx) => {
                match result {
                    Ok(inner) => inner,
                    Err(_) => {
                        let resp = ClientResponse {
                            success: false,
                            rig_id: Some(target_rig_id.clone()),
                            protocol_version: None,
                            state: None,
                            rigs: None,
                            sat_passes: None,
                            error: Some("Request timed out waiting for rig response".into()),
                        };
                        send_response(&mut writer, &resp, timeouts.io_timeout).await?;
                        continue;
                    }
                }
            }
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => {
                        info!("Client {} request canceled due to shutdown", addr);
                        break;
                    }
                    Ok(()) => continue,
                    Err(_) => break,
                }
            }
        } {
            Ok(Ok(snapshot)) => {
                let resp = ClientResponse {
                    success: true,
                    rig_id: Some(target_rig_id.clone()),
                    protocol_version: None,
                    state: Some(snapshot),
                    rigs: None,
                    sat_passes: None,
                    error: None,
                };
                send_response(&mut writer, &resp, timeouts.io_timeout).await?;
            }
            Ok(Err(err)) => {
                let resp = ClientResponse {
                    success: false,
                    rig_id: Some(target_rig_id.clone()),
                    protocol_version: None,
                    state: None,
                    rigs: None,
                    sat_passes: None,
                    error: Some(err.message),
                };
                send_response(&mut writer, &resp, timeouts.io_timeout).await?;
            }
            Err(e) => {
                error!("Rig response oneshot recv error: {:?}", e);
                let resp = ClientResponse {
                    success: false,
                    rig_id: Some(target_rig_id.clone()),
                    protocol_version: None,
                    state: None,
                    rigs: None,
                    sat_passes: None,
                    error: Some("Internal error waiting for rig response".into()),
                };
                send_response(&mut writer, &resp, timeouts.io_timeout).await?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::net::{Ipv4Addr, SocketAddr};

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;
    use tokio::sync::{mpsc, watch};

    use trx_core::radio::freq::Band;
    use trx_core::rig::request::RigRequest;
    use trx_core::rig::state::RigState;
    use trx_core::rig::{RigAccessMethod, RigCapabilities, RigInfo};

    fn loopback_addr() -> SocketAddr {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener);
        addr
    }

    fn sample_state() -> RigState {
        let mut state = RigState::new_uninitialized();
        state.initialized = true;
        state.rig_info = Some(RigInfo {
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
                supported_modes: vec![trx_core::RigMode::USB],
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
        });
        state
    }

    fn make_rigs(state: RigState) -> (Arc<HashMap<String, RigHandle>>, String) {
        let (rig_tx, _rig_rx) = mpsc::channel::<RigRequest>(8);
        let (state_tx, state_rx) = watch::channel(state);
        let _state_tx = state_tx;
        let handle = RigHandle {
            rig_id: "default".to_string(),
            display_name: "Default Rig".to_string(),
            rig_tx,
            state_rx,
            audio_port: 4531,
        };
        let mut map = HashMap::new();
        map.insert("default".to_string(), handle);
        (Arc::new(map), "default".to_string())
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn listener_rejects_missing_token() {
        let addr = loopback_addr();
        let (rigs, default_id) = make_rigs(sample_state());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let mut auth = HashSet::new();
        auth.insert("secret".to_string());
        let handle = tokio::spawn(run_listener(
            addr,
            rigs,
            default_id,
            auth,
            None,
            ListenerTimeouts::default(),
            shutdown_rx,
        ));

        let stream = TcpStream::connect(addr).await.expect("connect");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        writer
            .write_all(br#"{"cmd":"get_state"}"#)
            .await
            .expect("write");
        writer.write_all(b"\n").await.expect("newline");
        writer.flush().await.expect("flush");

        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read");
        let resp: ClientResponse = serde_json::from_str(line.trim_end()).expect("response json");
        assert!(!resp.success);
        assert_eq!(resp.error.as_deref(), Some("missing authorization token"));

        let _ = shutdown_tx.send(true);
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn listener_serves_get_state_snapshot() {
        let addr = loopback_addr();
        let (rigs, default_id) = make_rigs(sample_state());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(run_listener(
            addr,
            rigs,
            default_id,
            HashSet::new(),
            None,
            ListenerTimeouts::default(),
            shutdown_rx,
        ));

        let stream = TcpStream::connect(addr).await.expect("connect");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        writer
            .write_all(br#"{"cmd":"get_state"}"#)
            .await
            .expect("write");
        writer.write_all(b"\n").await.expect("newline");
        writer.flush().await.expect("flush");

        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read");
        let resp: ClientResponse = serde_json::from_str(line.trim_end()).expect("response json");
        assert!(resp.success);
        let snapshot = resp.state.expect("snapshot");
        assert_eq!(snapshot.info.model, "Dummy");
        assert_eq!(snapshot.status.freq.hz, 144_300_000);
        // rig_id should be set in the response
        assert_eq!(resp.rig_id.as_deref(), Some("default"));

        let _ = shutdown_tx.send(true);
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn listener_routes_unknown_rig_id() {
        let addr = loopback_addr();
        let (rigs, default_id) = make_rigs(sample_state());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(run_listener(
            addr,
            rigs,
            default_id,
            HashSet::new(),
            None,
            ListenerTimeouts::default(),
            shutdown_rx,
        ));

        let stream = TcpStream::connect(addr).await.expect("connect");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        writer
            .write_all(br#"{"rig_id":"nonexistent","cmd":"get_state"}"#)
            .await
            .expect("write");
        writer.write_all(b"\n").await.expect("newline");
        writer.flush().await.expect("flush");

        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read");
        let resp: ClientResponse = serde_json::from_str(line.trim_end()).expect("response json");
        assert!(!resp.success);
        assert!(resp
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Unknown rig_id"));

        let _ = shutdown_tx.send(true);
        handle.abort();
        let _ = handle.await;
    }

    // ========================================================================
    // Multi-rig integration tests
    // ========================================================================

    /// Create a sample state with custom model name, frequency, and mode.
    fn sample_state_custom(model: &str, freq_hz: u64, mode: trx_core::RigMode) -> RigState {
        let mut state = RigState::new_uninitialized();
        state.initialized = true;
        state.status.freq = trx_core::radio::freq::Freq { hz: freq_hz };
        state.status.mode = mode;
        state.rig_info = Some(RigInfo {
            manufacturer: "Test".to_string(),
            model: model.to_string(),
            revision: "1".to_string(),
            capabilities: RigCapabilities {
                min_freq_step_hz: 1,
                supported_bands: vec![Band {
                    low_hz: 1_800_000,
                    high_hz: 440_000_000,
                    tx_allowed: true,
                }],
                supported_modes: vec![
                    trx_core::RigMode::USB,
                    trx_core::RigMode::LSB,
                    trx_core::RigMode::FM,
                ],
                num_vfos: 2,
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
                addr: "127.0.0.1:0".to_string(),
            },
        });
        state
    }

    /// Build a multi-rig HashMap with two rigs having independent state and
    /// command channels. Returns the map, default rig id, and the mpsc
    /// receivers for each rig so tests can inspect routed commands.
    fn make_two_rigs(
        state_a: RigState,
        state_b: RigState,
    ) -> (
        Arc<HashMap<String, RigHandle>>,
        String,
        mpsc::Receiver<RigRequest>,
        mpsc::Receiver<RigRequest>,
    ) {
        let (tx_a, rx_a) = mpsc::channel::<RigRequest>(8);
        let (_state_tx_a, state_rx_a) = watch::channel(state_a);
        let handle_a = RigHandle {
            rig_id: "rig_hf".to_string(),
            display_name: "HF Rig".to_string(),
            rig_tx: tx_a,
            state_rx: state_rx_a,
            audio_port: 4531,
        };

        let (tx_b, rx_b) = mpsc::channel::<RigRequest>(8);
        let (_state_tx_b, state_rx_b) = watch::channel(state_b);
        let handle_b = RigHandle {
            rig_id: "rig_vhf".to_string(),
            display_name: "VHF Rig".to_string(),
            rig_tx: tx_b,
            state_rx: state_rx_b,
            audio_port: 4532,
        };

        let mut map = HashMap::new();
        map.insert("rig_hf".to_string(), handle_a);
        map.insert("rig_vhf".to_string(), handle_b);
        (Arc::new(map), "rig_hf".to_string(), rx_a, rx_b)
    }

    /// Helper: send a JSON line and read one response line from the stream.
    async fn send_and_recv(
        writer: &mut tokio::net::tcp::OwnedWriteHalf,
        reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
        json: &[u8],
    ) -> ClientResponse {
        writer.write_all(json).await.expect("write");
        writer.write_all(b"\n").await.expect("newline");
        writer.flush().await.expect("flush");
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read");
        serde_json::from_str(line.trim_end()).expect("response json")
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn multi_rig_state_isolation() {
        // Two rigs with different frequencies and modes.
        let state_hf =
            sample_state_custom("HF-Dummy", 14_200_000, trx_core::RigMode::USB);
        let state_vhf =
            sample_state_custom("VHF-Dummy", 145_500_000, trx_core::RigMode::FM);

        let (rigs, default_id, _rx_a, _rx_b) = make_two_rigs(state_hf, state_vhf);
        let addr = loopback_addr();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(run_listener(
            addr,
            rigs,
            default_id,
            HashSet::new(),
            None,
            ListenerTimeouts::default(),
            shutdown_rx,
        ));

        // Allow listener to bind.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let stream = TcpStream::connect(addr).await.expect("connect");
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // Query rig_hf — should return HF state.
        let resp = send_and_recv(
            &mut writer,
            &mut reader,
            br#"{"rig_id":"rig_hf","cmd":"get_state"}"#,
        )
        .await;
        assert!(resp.success, "rig_hf get_state should succeed");
        assert_eq!(resp.rig_id.as_deref(), Some("rig_hf"));
        let snap_hf = resp.state.expect("rig_hf snapshot");
        assert_eq!(snap_hf.info.model, "HF-Dummy");
        assert_eq!(snap_hf.status.freq.hz, 14_200_000);

        // Query rig_vhf — should return VHF state.
        let resp = send_and_recv(
            &mut writer,
            &mut reader,
            br#"{"rig_id":"rig_vhf","cmd":"get_state"}"#,
        )
        .await;
        assert!(resp.success, "rig_vhf get_state should succeed");
        assert_eq!(resp.rig_id.as_deref(), Some("rig_vhf"));
        let snap_vhf = resp.state.expect("rig_vhf snapshot");
        assert_eq!(snap_vhf.info.model, "VHF-Dummy");
        assert_eq!(snap_vhf.status.freq.hz, 145_500_000);

        // Verify the two snapshots have different modes.
        assert_ne!(
            snap_hf.status.mode, snap_vhf.status.mode,
            "Rig states should be independent"
        );

        let _ = shutdown_tx.send(true);
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn multi_rig_default_fallback() {
        // When rig_id is omitted, the default rig (rig_hf) should be used.
        let state_hf =
            sample_state_custom("HF-Dummy", 14_200_000, trx_core::RigMode::USB);
        let state_vhf =
            sample_state_custom("VHF-Dummy", 145_500_000, trx_core::RigMode::FM);

        let (rigs, default_id, _rx_a, _rx_b) = make_two_rigs(state_hf, state_vhf);
        let addr = loopback_addr();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(run_listener(
            addr,
            rigs,
            default_id,
            HashSet::new(),
            None,
            ListenerTimeouts::default(),
            shutdown_rx,
        ));

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let stream = TcpStream::connect(addr).await.expect("connect");
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // No rig_id — should resolve to default (rig_hf).
        let resp = send_and_recv(
            &mut writer,
            &mut reader,
            br#"{"cmd":"get_state"}"#,
        )
        .await;
        assert!(resp.success, "default get_state should succeed");
        assert_eq!(resp.rig_id.as_deref(), Some("rig_hf"));
        let snap = resp.state.expect("default snapshot");
        assert_eq!(snap.info.model, "HF-Dummy");

        let _ = shutdown_tx.send(true);
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn multi_rig_get_rigs_returns_all() {
        let state_hf =
            sample_state_custom("HF-Dummy", 14_200_000, trx_core::RigMode::USB);
        let state_vhf =
            sample_state_custom("VHF-Dummy", 145_500_000, trx_core::RigMode::FM);

        let (rigs, default_id, _rx_a, _rx_b) = make_two_rigs(state_hf, state_vhf);
        let addr = loopback_addr();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(run_listener(
            addr,
            rigs,
            default_id,
            HashSet::new(),
            None,
            ListenerTimeouts::default(),
            shutdown_rx,
        ));

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let stream = TcpStream::connect(addr).await.expect("connect");
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        let resp = send_and_recv(
            &mut writer,
            &mut reader,
            br#"{"cmd":"get_rigs"}"#,
        )
        .await;
        assert!(resp.success, "get_rigs should succeed");
        let entries = resp.rigs.expect("rigs list");
        assert_eq!(entries.len(), 2, "should return both rigs");

        // Collect rig_ids from the entries.
        let ids: HashSet<String> = entries.iter().map(|e| e.rig_id.clone()).collect();
        assert!(ids.contains("rig_hf"), "should contain rig_hf");
        assert!(ids.contains("rig_vhf"), "should contain rig_vhf");

        // Verify each entry has the correct frequency.
        for entry in &entries {
            match entry.rig_id.as_str() {
                "rig_hf" => {
                    assert_eq!(entry.state.status.freq.hz, 14_200_000);
                    assert_eq!(entry.state.info.model, "HF-Dummy");
                    assert_eq!(entry.audio_port, Some(4531));
                }
                "rig_vhf" => {
                    assert_eq!(entry.state.status.freq.hz, 145_500_000);
                    assert_eq!(entry.state.info.model, "VHF-Dummy");
                    assert_eq!(entry.audio_port, Some(4532));
                }
                other => panic!("Unexpected rig_id: {}", other),
            }
        }

        let _ = shutdown_tx.send(true);
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn multi_rig_command_routing() {
        // Verify that a set_freq command targeting rig_vhf is delivered to the
        // VHF rig's mpsc channel and not to the HF rig's channel.
        let state_hf =
            sample_state_custom("HF-Dummy", 14_200_000, trx_core::RigMode::USB);
        let state_vhf =
            sample_state_custom("VHF-Dummy", 145_500_000, trx_core::RigMode::FM);

        let (rigs, default_id, mut rx_hf, mut rx_vhf) =
            make_two_rigs(state_hf, state_vhf);
        let addr = loopback_addr();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(run_listener(
            addr,
            rigs,
            default_id,
            HashSet::new(),
            None,
            ListenerTimeouts::default(),
            shutdown_rx,
        ));

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let stream = TcpStream::connect(addr).await.expect("connect");
        let (_read_half, mut writer) = stream.into_split();

        // Send set_freq targeting rig_vhf. The listener will forward the
        // command to the VHF rig's mpsc channel.
        writer
            .write_all(br#"{"rig_id":"rig_vhf","cmd":"set_freq","freq_hz":146000000}"#)
            .await
            .expect("write");
        writer.write_all(b"\n").await.expect("newline");
        writer.flush().await.expect("flush");

        // The VHF channel should receive the command.
        let req = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            rx_vhf.recv(),
        )
        .await
        .expect("timeout waiting for VHF command")
        .expect("VHF channel closed");
        assert!(
            matches!(req.cmd, trx_core::rig::command::RigCommand::SetFreq(f) if f.hz == 146_000_000),
            "VHF rig should receive SetFreq(146 MHz), got {:?}",
            req.cmd
        );

        // The HF channel should NOT have received anything.
        assert!(
            rx_hf.try_recv().is_err(),
            "HF rig should not receive commands targeting VHF"
        );

        let _ = shutdown_tx.send(true);
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn multi_rig_command_routing_to_default() {
        // When rig_id is omitted, commands should go to the default rig (HF).
        let state_hf =
            sample_state_custom("HF-Dummy", 14_200_000, trx_core::RigMode::USB);
        let state_vhf =
            sample_state_custom("VHF-Dummy", 145_500_000, trx_core::RigMode::FM);

        let (rigs, default_id, mut rx_hf, mut rx_vhf) =
            make_two_rigs(state_hf, state_vhf);
        let addr = loopback_addr();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(run_listener(
            addr,
            rigs,
            default_id,
            HashSet::new(),
            None,
            ListenerTimeouts::default(),
            shutdown_rx,
        ));

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let stream = TcpStream::connect(addr).await.expect("connect");
        let (_read_half, mut writer) = stream.into_split();

        // No rig_id — should route to default (rig_hf).
        writer
            .write_all(br#"{"cmd":"set_freq","freq_hz":7100000}"#)
            .await
            .expect("write");
        writer.write_all(b"\n").await.expect("newline");
        writer.flush().await.expect("flush");

        // The HF channel should receive the command.
        let req = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            rx_hf.recv(),
        )
        .await
        .expect("timeout waiting for HF command")
        .expect("HF channel closed");
        assert!(
            matches!(req.cmd, trx_core::rig::command::RigCommand::SetFreq(f) if f.hz == 7_100_000),
            "HF rig should receive SetFreq(7.1 MHz), got {:?}",
            req.cmd
        );

        // VHF should not receive anything.
        assert!(
            rx_vhf.try_recv().is_err(),
            "VHF rig should not receive commands with no rig_id"
        );

        let _ = shutdown_tx.send(true);
        handle.abort();
        let _ = handle.await;
    }
}
