// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::net::SocketAddr;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{error, info};

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use trx_core::client::ClientEnvelope;
use trx_core::radio::freq::Freq;
use trx_core::rig::command::RigCommand;
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::{RigMode, RigState};
use trx_core::{ClientCommand, ClientResponse};
use trx_frontend::FrontendSpawner;

/// JSON-over-TCP frontend for control and status.
pub struct HttpJsonFrontend;

struct AuthConfig {
    tokens: HashSet<String>,
}

fn auth_registry() -> &'static Mutex<AuthConfig> {
    static REGISTRY: OnceLock<Mutex<AuthConfig>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        Mutex::new(AuthConfig {
            tokens: HashSet::new(),
        })
    })
}

pub fn set_auth_tokens(tokens: Vec<String>) {
    let mut reg = auth_registry()
        .lock()
        .expect("http-json auth mutex poisoned");
    reg.tokens = tokens.into_iter().filter(|t| !t.is_empty()).collect();
}

impl FrontendSpawner for HttpJsonFrontend {
    fn spawn_frontend(
        _state_rx: watch::Receiver<RigState>,
        rig_tx: mpsc::Sender<RigRequest>,
        _callsign: Option<String>,
        listen_addr: SocketAddr,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = serve(listen_addr, rig_tx).await {
                error!("json tcp server error: {:?}", e);
            }
        })
    }
}

async fn serve(listen_addr: SocketAddr, rig_tx: mpsc::Sender<RigRequest>) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen_addr).await?;
    info!("json tcp frontend listening on {}", listen_addr);

    loop {
        let (socket, addr) = listener.accept().await?;
        info!("json tcp client connected: {}", addr);

        let tx_clone = rig_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(socket, addr, tx_clone).await {
                error!("json tcp client {} error: {:?}", addr, e);
            }
        });
    }
}

async fn handle_client(
    socket: TcpStream,
    addr: SocketAddr,
    tx: mpsc::Sender<RigRequest>,
) -> std::io::Result<()> {
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            info!("json tcp client {} disconnected", addr);
            break;
        }

        // Simple protocol: one line = one JSON command.
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

        if let Err(err) = authorize(&envelope.token) {
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

        // Map ClientCommand -> RigCommand.
        let rig_cmd = match envelope.cmd {
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
            ClientCommand::SetAprsDecodeEnabled { enabled } => RigCommand::SetAprsDecodeEnabled(enabled),
            ClientCommand::SetCwDecodeEnabled { enabled } => RigCommand::SetCwDecodeEnabled(enabled),
            ClientCommand::SetCwAuto { enabled } => RigCommand::SetCwAuto(enabled),
            ClientCommand::SetCwWpm { wpm } => RigCommand::SetCwWpm(wpm),
            ClientCommand::SetCwToneHz { tone_hz } => RigCommand::SetCwToneHz(tone_hz),
            ClientCommand::ResetAprsDecoder => RigCommand::ResetAprsDecoder,
            ClientCommand::ResetCwDecoder => RigCommand::ResetCwDecoder,
        };

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

fn authorize(token: &Option<String>) -> Result<(), String> {
    let reg = auth_registry()
        .lock()
        .expect("http-json auth mutex poisoned");
    if reg.tokens.is_empty() {
        return Ok(());
    }

    let Some(token) = token.as_ref() else {
        return Err("missing authorization token".into());
    };

    let candidate = strip_bearer(token);
    if reg.tokens.contains(candidate) {
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
