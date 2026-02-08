// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::time::{self, Instant};
use tracing::{info, warn};

use trx_core::client::{ClientCommand, ClientEnvelope, ClientResponse};
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::rig::RigControl;
use trx_core::{RigError, RigResult};

pub struct RemoteClientConfig {
    pub addr: String,
    pub token: Option<String>,
    pub poll_interval: Duration,
}

pub async fn run_remote_client(
    config: RemoteClientConfig,
    mut rx: mpsc::Receiver<RigRequest>,
    state_tx: watch::Sender<RigState>,
) -> RigResult<()> {
    let mut reconnect_delay = Duration::from_secs(1);

    loop {
        info!("Remote client: connecting to {}", config.addr);
        match TcpStream::connect(&config.addr).await {
            Ok(stream) => {
                if let Err(e) = handle_connection(&config, stream, &mut rx, &state_tx).await {
                    warn!("Remote connection dropped: {}", e);
                }
            }
            Err(e) => {
                warn!("Remote connect failed: {}", e);
            }
        }

        time::sleep(reconnect_delay).await;
        reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(10));
    }
}

async fn handle_connection(
    config: &RemoteClientConfig,
    stream: TcpStream,
    rx: &mut mpsc::Receiver<RigRequest>,
    state_tx: &watch::Sender<RigState>,
) -> RigResult<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut poll_interval = time::interval(config.poll_interval);
    let mut last_poll = Instant::now();

    loop {
        tokio::select! {
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
                    let client_cmd = map_rig_command(cmd);
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
    let envelope = ClientEnvelope {
        token: config.token.clone(),
        cmd,
    };

    let payload = serde_json::to_string(&envelope)
        .map_err(|e| RigError::communication(format!("JSON serialize failed: {e}")))?;

    writer
        .write_all(format!("{}\n", payload).as_bytes())
        .await
        .map_err(|e| RigError::communication(format!("write failed: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| RigError::communication(format!("flush failed: {e}")))?;

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|e| RigError::communication(format!("read failed: {e}")))?;

    let resp: ClientResponse = serde_json::from_str(line.trim_end())
        .map_err(|e| RigError::communication(format!("invalid response: {e}")))?;

    if resp.success {
        if let Some(snapshot) = resp.state {
            let _ = state_tx.send(state_from_snapshot(snapshot.clone()));
            return Ok(snapshot);
        }
        return Err(RigError::communication("missing snapshot"));
    }

    Err(RigError::communication(
        resp.error.unwrap_or_else(|| "remote error".into()),
    ))
}

fn map_rig_command(cmd: trx_core::RigCommand) -> ClientCommand {
    match cmd {
        trx_core::RigCommand::GetSnapshot => ClientCommand::GetState,
        trx_core::RigCommand::SetFreq(freq) => ClientCommand::SetFreq { freq_hz: freq.hz },
        trx_core::RigCommand::SetMode(mode) => ClientCommand::SetMode {
            mode: mode_label(&mode),
        },
        trx_core::RigCommand::SetPtt(ptt) => ClientCommand::SetPtt { ptt },
        trx_core::RigCommand::PowerOn => ClientCommand::PowerOn,
        trx_core::RigCommand::PowerOff => ClientCommand::PowerOff,
        trx_core::RigCommand::ToggleVfo => ClientCommand::ToggleVfo,
        trx_core::RigCommand::GetTxLimit => ClientCommand::GetTxLimit,
        trx_core::RigCommand::SetTxLimit(limit) => ClientCommand::SetTxLimit { limit },
        trx_core::RigCommand::Lock => ClientCommand::Lock,
        trx_core::RigCommand::Unlock => ClientCommand::Unlock,
        trx_core::RigCommand::SetAprsDecodeEnabled(enabled) => ClientCommand::SetAprsDecodeEnabled { enabled },
        trx_core::RigCommand::SetCwDecodeEnabled(enabled) => ClientCommand::SetCwDecodeEnabled { enabled },
        trx_core::RigCommand::ResetAprsDecoder => ClientCommand::ResetAprsDecoder,
        trx_core::RigCommand::ResetCwDecoder => ClientCommand::ResetCwDecoder,
    }
}

fn mode_label(mode: &trx_core::rig::state::RigMode) -> String {
    match mode {
        trx_core::rig::state::RigMode::LSB => "LSB".to_string(),
        trx_core::rig::state::RigMode::USB => "USB".to_string(),
        trx_core::rig::state::RigMode::CW => "CW".to_string(),
        trx_core::rig::state::RigMode::CWR => "CWR".to_string(),
        trx_core::rig::state::RigMode::AM => "AM".to_string(),
        trx_core::rig::state::RigMode::WFM => "WFM".to_string(),
        trx_core::rig::state::RigMode::FM => "FM".to_string(),
        trx_core::rig::state::RigMode::DIG => "DIG".to_string(),
        trx_core::rig::state::RigMode::PKT => "PKT".to_string(),
        trx_core::rig::state::RigMode::Other(val) => val.clone(),
    }
}

pub fn state_from_snapshot(snapshot: trx_core::RigSnapshot) -> RigState {
    let status = snapshot.status;
    let lock = status.lock;
    RigState {
        rig_info: Some(snapshot.info),
        status,
        initialized: snapshot.initialized,
        control: RigControl {
            rpt_offset_hz: None,
            ctcss_hz: None,
            dcs_code: None,
            lock,
            clar_hz: None,
            clar_on: None,
            enabled: snapshot.enabled,
        },
        server_callsign: snapshot.server_callsign,
        server_version: snapshot.server_version,
        server_latitude: snapshot.server_latitude,
        server_longitude: snapshot.server_longitude,
        aprs_decode_enabled: snapshot.aprs_decode_enabled,
        cw_decode_enabled: snapshot.cw_decode_enabled,
        aprs_decode_reset_seq: 0,
        cw_decode_reset_seq: 0,
    }
}

pub fn parse_remote_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("remote url is empty".into());
    }

    let addr = trimmed
        .strip_prefix("tcp://")
        .or_else(|| trimmed.strip_prefix("http-json://"))
        .unwrap_or(trimmed);

    if !addr.contains(':') {
        return Ok(format!("{}:4532", addr));
    }

    Ok(addr.to_string())
}
