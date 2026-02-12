// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{error, info};

use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_frontend::{FrontendRuntimeContext, FrontendSpawner};
use trx_protocol::auth::{SimpleTokenValidator, TokenValidator};
use trx_protocol::codec::parse_envelope;
use trx_protocol::mapping;
use trx_protocol::ClientResponse;

const IO_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_JSON_LINE_BYTES: usize = 16 * 1024;

/// JSON-over-TCP frontend for control and status.
pub struct HttpJsonFrontend;

impl FrontendSpawner for HttpJsonFrontend {
    fn spawn_frontend(
        _state_rx: watch::Receiver<RigState>,
        rig_tx: mpsc::Sender<RigRequest>,
        _callsign: Option<String>,
        listen_addr: SocketAddr,
        context: Arc<FrontendRuntimeContext>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = serve(listen_addr, rig_tx, context).await {
                error!("json tcp server error: {:?}", e);
            }
        })
    }
}

async fn serve(
    listen_addr: SocketAddr,
    rig_tx: mpsc::Sender<RigRequest>,
    context: Arc<FrontendRuntimeContext>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen_addr).await?;
    info!("json tcp frontend listening on {}", listen_addr);

    loop {
        let (socket, addr) = listener.accept().await?;
        info!("json tcp client connected: {}", addr);

        let tx_clone = rig_tx.clone();
        let context = context.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(socket, addr, tx_clone, context).await {
                error!("json tcp client {} error: {:?}", addr, e);
            }
        });
    }
}

async fn handle_client(
    socket: TcpStream,
    addr: SocketAddr,
    tx: mpsc::Sender<RigRequest>,
    context: Arc<FrontendRuntimeContext>,
) -> std::io::Result<()> {
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);

    loop {
        let line = time::timeout(
            IO_TIMEOUT,
            read_limited_line(&mut reader, MAX_JSON_LINE_BYTES),
        )
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "read timeout waiting for client request",
            )
        })??;
        let Some(line) = line else {
            info!("json tcp client {} disconnected", addr);
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

        if let Err(err) = authorize(&envelope.token, &context) {
            let resp = ClientResponse {
                success: false,
                state: None,
                error: Some(err),
            };
            send_response(&mut writer, &resp).await?;
            continue;
        }

        let rig_cmd = mapping::client_command_to_rig(envelope.cmd);

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

        match time::timeout(REQUEST_TIMEOUT, resp_rx).await {
            Ok(Ok(Ok(snapshot))) => {
                let resp = ClientResponse {
                    success: true,
                    state: Some(snapshot),
                    error: None,
                };
                send_response(&mut writer, &resp).await?;
            }
            Ok(Ok(Err(err))) => {
                let resp = ClientResponse {
                    success: false,
                    state: None,
                    error: Some(err.message),
                };
                send_response(&mut writer, &resp).await?;
            }
            Ok(Err(e)) => {
                error!("Rig response oneshot recv error: {:?}", e);
                let resp = ClientResponse {
                    success: false,
                    state: None,
                    error: Some("Internal error waiting for rig response".into()),
                };
                send_response(&mut writer, &resp).await?;
            }
            Err(_) => {
                let resp = ClientResponse {
                    success: false,
                    state: None,
                    error: Some("Request timed out waiting for rig response".into()),
                };
                send_response(&mut writer, &resp).await?;
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

fn authorize(token: &Option<String>, context: &FrontendRuntimeContext) -> Result<(), String> {
    let validator = SimpleTokenValidator::new(context.auth_tokens.clone());
    validator.validate(token)
}
