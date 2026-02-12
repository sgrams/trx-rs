// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! JSON-over-TCP listener for trx-server.
//!
//! Accepts client connections speaking the `ClientEnvelope`/`ClientResponse`
//! protocol defined in `trx-protocol`.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time;
use tracing::{error, info};

use trx_core::rig::command::RigCommand;
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_protocol::auth::{SimpleTokenValidator, TokenValidator};
use trx_protocol::codec::parse_envelope;
use trx_protocol::mapping;
use trx_protocol::ClientResponse;

const IO_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_JSON_LINE_BYTES: usize = 16 * 1024;

/// Run the JSON TCP listener, accepting client connections.
pub async fn run_listener(
    addr: SocketAddr,
    rig_tx: mpsc::Sender<RigRequest>,
    auth_tokens: HashSet<String>,
    state_rx: watch::Receiver<RigState>,
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

                let tx = rig_tx.clone();
                let srx = state_rx.clone();
                let validator = Arc::clone(&validator);
                let client_shutdown_rx = shutdown_rx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(socket, peer, tx, validator, srx, client_shutdown_rx).await {
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
    tx: mpsc::Sender<RigRequest>,
    validator: Arc<SimpleTokenValidator>,
    state_rx: watch::Receiver<RigState>,
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
                    state: None,
                    error: Some(format!("Invalid JSON: {}", e)),
                };
                send_response(&mut writer, &resp).await?;
                continue;
            }
        };

        if let Err(err) = validator.as_ref().validate(&envelope.token) {
            let resp = ClientResponse {
                success: false,
                state: None,
                error: Some(err),
            };
            send_response(&mut writer, &resp).await?;
            continue;
        }

        let rig_cmd = mapping::client_command_to_rig(envelope.cmd);

        // Fast path: serve GetSnapshot directly from the watch channel
        // so clients get a response even while the rig task is initializing.
        if matches!(rig_cmd, RigCommand::GetSnapshot) {
            let state = state_rx.borrow().clone();
            if let Some(snapshot) = state.snapshot() {
                let resp = ClientResponse {
                    success: true,
                    state: Some(snapshot),
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

        match time::timeout(IO_TIMEOUT, tx.send(req)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                error!("Failed to send request to rig_task: {:?}", e);
                let resp = ClientResponse {
                    success: false,
                    state: None,
                    error: Some("Internal error: rig task not available".into()),
                };
                send_response(&mut writer, &resp).await?;
                continue;
            }
            Err(_) => {
                let resp = ClientResponse {
                    success: false,
                    state: None,
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
                            state: None,
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
                    state: Some(snapshot),
                    error: None,
                };
                send_response(&mut writer, &resp).await?;
            }
            Ok(Err(err)) => {
                let resp = ClientResponse {
                    success: false,
                    state: None,
                    error: Some(err.message),
                };
                send_response(&mut writer, &resp).await?;
            }
            Err(e) => {
                error!("Rig response oneshot recv error: {:?}", e);
                let resp = ClientResponse {
                    success: false,
                    state: None,
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

    use trx_core::radio::freq::Band;
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
            },
            access: RigAccessMethod::Tcp {
                addr: "127.0.0.1:1234".to_string(),
            },
        });
        state
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn listener_rejects_missing_token() {
        let addr = loopback_addr();
        let (rig_tx, _rig_rx) = mpsc::channel::<RigRequest>(8);
        let (state_tx, state_rx) = watch::channel(sample_state());
        let _state_tx = state_tx;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let mut auth = HashSet::new();
        auth.insert("secret".to_string());
        let handle = tokio::spawn(run_listener(addr, rig_tx, auth, state_rx, shutdown_rx));

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
        let (rig_tx, _rig_rx) = mpsc::channel::<RigRequest>(8);
        let (state_tx, state_rx) = watch::channel(sample_state());
        let _state_tx = state_tx;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(run_listener(
            addr,
            rig_tx,
            HashSet::new(),
            state_rx,
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

        let _ = shutdown_tx.send(true);
        handle.abort();
        let _ = handle.await;
    }
}
