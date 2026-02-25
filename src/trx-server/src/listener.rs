// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
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
use std::sync::Arc;
use std::time::Duration;

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

const IO_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_JSON_LINE_BYTES: usize = 16 * 1024;

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
    mut shutdown_rx: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!("Listening on {}", addr);

    let validator = Arc::new(SimpleTokenValidator::new(auth_tokens));

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (socket, peer) = accept?;
                info!("Client connected: {}", peer);

                let rigs = Arc::clone(&rigs);
                let default_rig_id = default_rig_id.clone();
                let validator = Arc::clone(&validator);
                let client_shutdown_rx = shutdown_rx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(socket, peer, rigs, default_rig_id, validator, client_shutdown_rx).await {
                        error!("Client {} error: {:?}", peer, e);
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
) -> std::io::Result<()> {
    let resp_line = serde_json::to_string(response).map_err(std::io::Error::other)? + "\n";
    time::timeout(IO_TIMEOUT, writer.write_all(resp_line.as_bytes()))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "response write timeout")
        })??;
    time::timeout(IO_TIMEOUT, writer.flush())
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "response flush timeout")
        })??;
    Ok(())
}

async fn handle_client(
    socket: TcpStream,
    addr: SocketAddr,
    rigs: Arc<HashMap<String, RigHandle>>,
    default_rig_id: String,
    validator: Arc<SimpleTokenValidator>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);

    loop {
        let line = tokio::select! {
            read = time::timeout(IO_TIMEOUT, read_limited_line(&mut reader, MAX_JSON_LINE_BYTES)) => {
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
                error!("Invalid JSON from {}: {} / {:?}", addr, trimmed, e);
                let resp = ClientResponse {
                    success: false,
                    rig_id: None,
                    state: None,
                    rigs: None,
                    error: Some(format!("Invalid JSON: {}", e)),
                };
                send_response(&mut writer, &resp).await?;
                continue;
            }
        };

        if let Err(err) = validator.as_ref().validate(&envelope.token) {
            let resp = ClientResponse {
                success: false,
                rig_id: None,
                state: None,
                rigs: None,
                error: Some(err),
            };
            send_response(&mut writer, &resp).await?;
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
                        state: snapshot,
                        audio_port: Some(handle.audio_port),
                    });
                }
            }
            let resp = ClientResponse {
                success: true,
                rig_id: Some("server".to_string()),
                state: None,
                rigs: Some(entries),
                error: None,
            };
            send_response(&mut writer, &resp).await?;
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
                    state: None,
                    rigs: None,
                    error: Some(format!("Unknown rig_id: {}", target_rig_id)),
                };
                send_response(&mut writer, &resp).await?;
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
                    state: Some(snapshot),
                    rigs: None,
                    error: None,
                };
                send_response(&mut writer, &resp).await?;
                continue;
            }
        }

        let (resp_tx, resp_rx) = oneshot::channel();
        let req = RigRequest {
            cmd: rig_cmd,
            respond_to: resp_tx,
        };

        match time::timeout(IO_TIMEOUT, handle.rig_tx.send(req)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                error!(
                    "Failed to send request to rig_task for '{}': {:?}",
                    target_rig_id, e
                );
                let resp = ClientResponse {
                    success: false,
                    rig_id: Some(target_rig_id.clone()),
                    state: None,
                    rigs: None,
                    error: Some("Internal error: rig task not available".into()),
                };
                send_response(&mut writer, &resp).await?;
                continue;
            }
            Err(_) => {
                let resp = ClientResponse {
                    success: false,
                    rig_id: Some(target_rig_id.clone()),
                    state: None,
                    rigs: None,
                    error: Some("Internal error: request queue timeout".into()),
                };
                send_response(&mut writer, &resp).await?;
                continue;
            }
        }

        match tokio::select! {
            result = time::timeout(REQUEST_TIMEOUT, resp_rx) => {
                match result {
                    Ok(inner) => inner,
                    Err(_) => {
                        let resp = ClientResponse {
                            success: false,
                            rig_id: Some(target_rig_id.clone()),
                            state: None,
                            rigs: None,
                            error: Some("Request timed out waiting for rig response".into()),
                        };
                        send_response(&mut writer, &resp).await?;
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
                    state: Some(snapshot),
                    rigs: None,
                    error: None,
                };
                send_response(&mut writer, &resp).await?;
            }
            Ok(Err(err)) => {
                let resp = ClientResponse {
                    success: false,
                    rig_id: Some(target_rig_id.clone()),
                    state: None,
                    rigs: None,
                    error: Some(err.message),
                };
                send_response(&mut writer, &resp).await?;
            }
            Err(e) => {
                error!("Rig response oneshot recv error: {:?}", e);
                let resp = ClientResponse {
                    success: false,
                    rig_id: Some(target_rig_id.clone()),
                    state: None,
                    rigs: None,
                    error: Some("Internal error waiting for rig response".into()),
                };
                send_response(&mut writer, &resp).await?;
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
        let handle = tokio::spawn(run_listener(addr, rigs, default_id, auth, shutdown_rx));

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
}
