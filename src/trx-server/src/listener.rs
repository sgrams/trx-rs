// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! JSON-over-TCP listener for trx-server.
//!
//! Accepts client connections speaking the `ClientEnvelope`/`ClientResponse`
//! protocol defined in `trx-core::client`.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{error, info};

use trx_core::rig::command::RigCommand;
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::ClientResponse;

use trx_protocol::codec::parse_envelope;
use trx_protocol::auth::{SimpleTokenValidator, TokenValidator};
use trx_protocol::mapping;

/// Run the JSON TCP listener, accepting client connections.
pub async fn run_listener(
    addr: SocketAddr,
    rig_tx: mpsc::Sender<RigRequest>,
    auth_tokens: HashSet<String>,
    state_rx: watch::Receiver<RigState>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!("Listening on {}", addr);

    let validator = Arc::new(SimpleTokenValidator::new(auth_tokens));

    loop {
        let (socket, peer) = listener.accept().await?;
        info!("Client connected: {}", peer);

        let tx = rig_tx.clone();
        let srx = state_rx.clone();
        let validator = Arc::clone(&validator);
        tokio::spawn(async move {
            if let Err(e) = handle_client(socket, peer, tx, validator, srx).await {
                error!("Client {} error: {:?}", peer, e);
            }
        });
    }
}

async fn handle_client(
    socket: TcpStream,
    addr: SocketAddr,
    tx: mpsc::Sender<RigRequest>,
    validator: Arc<SimpleTokenValidator>,
    state_rx: watch::Receiver<RigState>,
) -> std::io::Result<()> {
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            info!("Client {} disconnected", addr);
            break;
        }

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
                let resp_line = serde_json::to_string(&resp)? + "\n";
                writer.write_all(resp_line.as_bytes()).await?;
                writer.flush().await?;
                continue;
            }
        };

        if let Err(err) = validator.as_ref().validate(&envelope.token) {
            let resp = ClientResponse {
                success: false,
                state: None,
                error: Some(err),
            };
            let resp_line = serde_json::to_string(&resp)? + "\n";
            writer.write_all(resp_line.as_bytes()).await?;
            writer.flush().await?;
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
                let resp_line = serde_json::to_string(&resp)? + "\n";
                writer.write_all(resp_line.as_bytes()).await?;
                writer.flush().await?;
                continue;
            }
        }

        let (resp_tx, resp_rx) = oneshot::channel();
        let req = RigRequest {
            cmd: rig_cmd,
            respond_to: resp_tx,
        };

        if let Err(e) = tx.send(req).await {
            error!("Failed to send request to rig_task: {:?}", e);
            let resp = ClientResponse {
                success: false,
                state: None,
                error: Some("Internal error: rig task not available".into()),
            };
            let resp_line = serde_json::to_string(&resp)? + "\n";
            writer.write_all(resp_line.as_bytes()).await?;
            writer.flush().await?;
            continue;
        }

        match resp_rx.await {
            Ok(Ok(snapshot)) => {
                let resp = ClientResponse {
                    success: true,
                    state: Some(snapshot),
                    error: None,
                };
                let resp_line = serde_json::to_string(&resp)? + "\n";
                writer.write_all(resp_line.as_bytes()).await?;
                writer.flush().await?;
            }
            Ok(Err(err)) => {
                let resp = ClientResponse {
                    success: false,
                    state: None,
                    error: Some(err.message),
                };
                let resp_line = serde_json::to_string(&resp)? + "\n";
                writer.write_all(resp_line.as_bytes()).await?;
                writer.flush().await?;
            }
            Err(e) => {
                error!("Rig response oneshot recv error: {:?}", e);
                let resp = ClientResponse {
                    success: false,
                    state: None,
                    error: Some("Internal error waiting for rig response".into()),
                };
                let resp_line = serde_json::to_string(&resp)? + "\n";
                writer.write_all(resp_line.as_bytes()).await?;
                writer.flush().await?;
            }
        }
    }

    Ok(())
}

