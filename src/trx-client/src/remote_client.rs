// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::time::{self, Instant};
use tracing::{info, warn};

use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::{RigError, RigResult};
use trx_protocol::rig_command_to_client;
use trx_protocol::{ClientCommand, ClientEnvelope, ClientResponse};

const DEFAULT_REMOTE_PORT: u16 = 4530;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_JSON_LINE_BYTES: usize = 16 * 1024;

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

pub struct RemoteClientConfig {
    pub addr: String,
    pub token: Option<String>,
    pub rig_id: Option<String>,
    pub poll_interval: Duration,
}

pub async fn run_remote_client(
    config: RemoteClientConfig,
    mut rx: mpsc::Receiver<RigRequest>,
    state_tx: watch::Sender<RigState>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> RigResult<()> {
    let mut reconnect_delay = Duration::from_secs(1);

    loop {
        if *shutdown_rx.borrow() {
            info!("Remote client shutting down");
            return Ok(());
        }

        info!("Remote client: connecting to {}", config.addr);
        match time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&config.addr)).await {
            Ok(Ok(stream)) => {
                if let Err(e) =
                    handle_connection(&config, stream, &mut rx, &state_tx, &mut shutdown_rx).await
                {
                    warn!("Remote connection dropped: {}", e);
                }
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
                        return Ok(());
                    }
                    Ok(()) => {}
                    Err(_) => return Ok(()),
                }
            }
        }
        reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(10));
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
                if let Err(e) = send_command(config, &mut writer, &mut reader, ClientCommand::GetState, state_tx).await {
                    warn!("Remote poll failed: {}", e);
                }
            }
            req = rx.recv() => {
                let Some(req) = req else {
                    return Ok(());
                };
                let cmd = req.cmd;
                let result = {
                    let client_cmd = rig_command_to_client(cmd);
                    send_command(config, &mut writer, &mut reader, client_cmd, state_tx).await
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
    state_tx: &watch::Sender<RigState>,
) -> RigResult<trx_core::RigSnapshot> {
    let envelope = build_envelope(config, cmd);

    let payload = serde_json::to_string(&envelope)
        .map_err(|e| RigError::communication(format!("JSON serialize failed: {e}")))?;

    time::timeout(
        IO_TIMEOUT,
        writer.write_all(format!("{}\n", payload).as_bytes()),
    )
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

fn build_envelope(config: &RemoteClientConfig, cmd: ClientCommand) -> ClientEnvelope {
    ClientEnvelope {
        token: config.token.clone(),
        rig_id: config.rig_id.clone(),
        cmd,
    }
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
    use super::{parse_remote_url, RemoteClientConfig, RemoteEndpoint};
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::sync::{mpsc, watch};

    use trx_core::radio::freq::{Band, Freq};
    use trx_core::rig::state::RigSnapshot;
    use trx_core::rig::{RigAccessMethod, RigCapabilities, RigInfo, RigStatus, RigTxStatus};
    use trx_core::{RigMode, RigState};
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
            aprs_decode_enabled: false,
            cw_decode_enabled: false,
            ft8_decode_enabled: false,
            wspr_decode_enabled: false,
            cw_auto: true,
            cw_wpm: 15,
            cw_tone_hz: 700,
            filter: None,
        }
    }

    #[tokio::test]
    #[ignore = "requires TCP bind permissions"]
    async fn reconnects_and_updates_state_after_drop() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let response = serde_json::to_string(&ClientResponse {
            success: true,
            rig_id: None,
            state: Some(sample_snapshot()),
            rigs: None,
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

        let client = tokio::spawn(super::run_remote_client(
            RemoteClientConfig {
                addr: addr.to_string(),
                token: None,
                rig_id: None,
                poll_interval: Duration::from_millis(100),
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
        let config = RemoteClientConfig {
            addr: "127.0.0.1:4530".to_string(),
            token: Some("secret".to_string()),
            rig_id: Some("sdr".to_string()),
            poll_interval: Duration::from_millis(500),
        };
        let envelope = super::build_envelope(&config, trx_protocol::ClientCommand::GetState);
        assert_eq!(envelope.token.as_deref(), Some("secret"));
        assert_eq!(envelope.rig_id.as_deref(), Some("sdr"));
    }
}
