// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! JSON-over-TCP listener for trx-server.
//!
//! Accepts client connections speaking the `ClientEnvelope`/`ClientResponse`
//! protocol defined in `trx-core::client`.

use std::collections::HashSet;
use std::net::SocketAddr;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

use trx_core::client::ClientEnvelope;
use trx_core::radio::freq::Freq;
use trx_core::rig::command::RigCommand;
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigMode;
use trx_core::{ClientCommand, ClientResponse};

/// Run the JSON TCP listener, accepting client connections.
pub async fn run_listener(
    addr: SocketAddr,
    rig_tx: mpsc::Sender<RigRequest>,
    auth_tokens: HashSet<String>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!("Listening on {}", addr);

    loop {
        let (socket, peer) = listener.accept().await?;
        info!("Client connected: {}", peer);

        let tx = rig_tx.clone();
        let tokens = auth_tokens.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(socket, peer, tx, &tokens).await {
                error!("Client {} error: {:?}", peer, e);
            }
        });
    }
}

async fn handle_client(
    socket: TcpStream,
    addr: SocketAddr,
    tx: mpsc::Sender<RigRequest>,
    auth_tokens: &HashSet<String>,
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

        if let Err(err) = authorize(&envelope.token, auth_tokens) {
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

        let rig_cmd = map_command(envelope.cmd);

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

fn map_command(cmd: ClientCommand) -> RigCommand {
    match cmd {
        ClientCommand::GetState => RigCommand::GetSnapshot,
        ClientCommand::SetFreq { freq_hz } => RigCommand::SetFreq(Freq { hz: freq_hz }),
        ClientCommand::SetMode { mode } => RigCommand::SetMode(parse_mode(&mode)),
        ClientCommand::SetPtt { ptt } => RigCommand::SetPtt(ptt),
        ClientCommand::PowerOn => RigCommand::PowerOn,
        ClientCommand::PowerOff => RigCommand::PowerOff,
        ClientCommand::ToggleVfo => RigCommand::ToggleVfo,
        ClientCommand::Lock => RigCommand::Lock,
        ClientCommand::Unlock => RigCommand::Unlock,
        ClientCommand::GetTxLimit => RigCommand::GetTxLimit,
        ClientCommand::SetTxLimit { limit } => RigCommand::SetTxLimit(limit),
    }
}

fn parse_mode(s: &str) -> RigMode {
    match s.to_uppercase().as_str() {
        "LSB" => RigMode::LSB,
        "USB" => RigMode::USB,
        "CW" => RigMode::CW,
        "CWR" => RigMode::CWR,
        "AM" => RigMode::AM,
        "FM" => RigMode::FM,
        "DIG" | "DIGI" => RigMode::DIG,
        "PKT" | "PACKET" => RigMode::PKT,
        other => RigMode::Other(other.to_string()),
    }
}

fn parse_envelope(input: &str) -> Result<ClientEnvelope, serde_json::Error> {
    match serde_json::from_str::<ClientEnvelope>(input) {
        Ok(envelope) => Ok(envelope),
        Err(_) => {
            let cmd = serde_json::from_str::<ClientCommand>(input)?;
            Ok(ClientEnvelope { token: None, cmd })
        }
    }
}

fn authorize(token: &Option<String>, valid_tokens: &HashSet<String>) -> Result<(), String> {
    if valid_tokens.is_empty() {
        return Ok(());
    }

    let Some(token) = token.as_ref() else {
        return Err("missing authorization token".into());
    };

    let candidate = strip_bearer(token);
    if valid_tokens.contains(candidate) {
        return Ok(());
    }

    Err("invalid authorization token".into())
}

fn strip_bearer(value: &str) -> &str {
    let trimmed = value.trim();
    let prefix = "bearer ";
    if trimmed.len() >= prefix.len() && trimmed[..prefix.len()].eq_ignore_ascii_case(prefix) {
        &trimmed[prefix.len()..]
    } else {
        trimmed
    }
}
