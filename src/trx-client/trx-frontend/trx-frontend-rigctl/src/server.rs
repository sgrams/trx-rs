// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};
use trx_protocol::{mode_to_string, parse_mode};

use trx_core::radio::freq::Freq;
use trx_core::rig::state::RigSnapshot;
use trx_core::{RigCommand, RigMode, RigRequest, RigState};
use trx_frontend::FrontendSpawner;

/// rigctl-compatible frontend.
///
/// This exposes a small subset of the rigctl/rigctld ASCII protocol to allow
/// existing tooling to drive the rig. The implementation is intentionally
/// minimal and only covers the operations supported by the core rig task.
pub struct RigctlFrontend;

impl FrontendSpawner for RigctlFrontend {
    fn spawn_frontend(
        state_rx: watch::Receiver<RigState>,
        rig_tx: mpsc::Sender<RigRequest>,
        _callsign: Option<String>,
        listen_addr: SocketAddr,
        _context: std::sync::Arc<trx_frontend::FrontendRuntimeContext>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = serve(listen_addr, state_rx, rig_tx).await {
                error!("rigctl server error: {:?}", e);
            }
        })
    }
}

async fn serve(
    listen_addr: SocketAddr,
    state_rx: watch::Receiver<RigState>,
    rig_tx: mpsc::Sender<RigRequest>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen_addr).await?;
    info!("rigctl frontend listening on {}", listen_addr);
    info!("rigctl frontend ready (rigctld-compatible)");

    loop {
        let (stream, addr) = listener.accept().await?;
        info!("rigctl client connected: {}", addr);
        let state_rx = state_rx.clone();
        let rig_tx = rig_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, addr, state_rx, rig_tx).await {
                warn!("rigctl client {} error: {:?}", addr, e);
            }
        });
    }
}

async fn handle_client(
    stream: TcpStream,
    addr: SocketAddr,
    mut state_rx: watch::Receiver<RigState>,
    rig_tx: mpsc::Sender<RigRequest>,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            debug!("rigctl client {} disconnected", addr);
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match process_command(trimmed, &mut state_rx, &rig_tx).await {
            CommandResult::Reply(resp) => writer.write_all(resp.as_bytes()).await?,
            CommandResult::Close => break,
        }
        writer.flush().await?;
    }

    Ok(())
}

enum CommandResult {
    Reply(String),
    Close,
}

async fn process_command(
    cmd_line: &str,
    state_rx: &mut watch::Receiver<RigState>,
    rig_tx: &mpsc::Sender<RigRequest>,
) -> CommandResult {
    let mut parts = cmd_line.split_whitespace();
    let Some(raw_op) = parts.next() else {
        return CommandResult::Reply(err_response("empty command"));
    };
    let op = raw_op.trim_start_matches('+');

    let resp = match op {
        "q" | "Q" | "\\q" | "\\quit" => return CommandResult::Close,
        "f" | "\\get_freq" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => ok_response([snapshot.status.freq.hz.to_string()]),
            Err(e) => err_response(&e),
        },
        "F" | "\\set_freq" => match parts.next().and_then(|s| s.parse::<u64>().ok()) {
            Some(freq) => {
                match send_rig_command(rig_tx, RigCommand::SetFreq(Freq { hz: freq })).await {
                    Ok(_) => ok_only(),
                    Err(e) => err_response(&e),
                }
            }
            None => err_response("expected frequency in Hz"),
        },
        "m" | "\\get_mode" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => {
                let mode = rig_mode_to_str(&snapshot.status.mode);
                ok_response([mode, "0".to_string()])
            }
            Err(e) => err_response(&e),
        },
        "M" | "\\set_mode" => {
            let Some(mode_str) = parts.next() else {
                return CommandResult::Reply(err_response("expected mode"));
            };
            let mode = parse_mode(mode_str);
            match send_rig_command(rig_tx, RigCommand::SetMode(mode)).await {
                Ok(_) => ok_only(),
                Err(e) => err_response(&e),
            }
        }
        "t" | "\\get_ptt" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => {
                ok_response([if snapshot.status.tx_en { "1" } else { "0" }.to_string()])
            }
            Err(e) => err_response(&e),
        },
        "T" | "\\set_ptt" => match parts.next() {
            Some(v) if is_true(v) => match send_rig_command(rig_tx, RigCommand::SetPtt(true)).await
            {
                Ok(_) => ok_only(),
                Err(e) => err_response(&e),
            },
            Some(v) if is_false(v) => {
                match send_rig_command(rig_tx, RigCommand::SetPtt(false)).await {
                    Ok(_) => ok_only(),
                    Err(e) => err_response(&e),
                }
            }
            _ => err_response("expected PTT state (0/1)"),
        },
        "v" | "\\get_vfo" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => ok_response([active_vfo_label(&snapshot)]),
            Err(e) => err_response(&e),
        },
        "V" | "\\set_vfo" => {
            let Some(target) = parts.next() else {
                return CommandResult::Reply(err_response("expected VFO (VFOA/VFOB)"));
            };
            match set_vfo_target(target, rig_tx).await {
                Ok(()) => ok_only(),
                Err(e) => err_response(&e),
            }
        }
        "s" | "\\get_split_vfo" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => {
                // split state, tx vfo
                ok_response(["0".to_string(), active_vfo_label(&snapshot)])
            }
            Err(e) => err_response(&e),
        },
        "S" | "\\set_split_vfo" => match parts.next() {
            Some(v) if is_false(v) => ok_only(),
            Some(v) if is_true(v) => err_response("split mode not supported"),
            _ => err_response("expected split state (0/1)"),
        },
        "\\get_info" => {
            let snapshot = match current_snapshot(state_rx) {
                Some(s) => s,
                None => match request_snapshot(rig_tx).await {
                    Ok(s) => s,
                    Err(e) => return CommandResult::Reply(err_response(&e)),
                },
            };
            let info = format!(
                "Model: {} {}; Version: {}",
                snapshot.info.manufacturer, snapshot.info.model, snapshot.info.revision
            );
            ok_response([info])
        }
        "\\get_powerstat" | "get_powerstat" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => {
                let val = snapshot.enabled.unwrap_or(false);
                ok_response([if val { "1" } else { "0" }.to_string()])
            }
            Err(e) => err_response(&e),
        },
        "\\chk_vfo" | "chk_vfo" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => ok_response([active_vfo_label(&snapshot)]),
            Err(e) => err_response(&e),
        },
        "\\dump_state" | "dump_state" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => ok_response(dump_state_lines(&snapshot)),
            Err(e) => err_response(&e),
        },
        "1" | "\\dump_caps" | "dump_caps" | "\\dumpcaps" | "dumpcaps" => {
            match request_snapshot(rig_tx).await {
                Ok(snapshot) => dump_caps_response(&snapshot),
                Err(e) => err_response(&e),
            }
        }
        "i" | "I" => {
            let snapshot = match current_snapshot(state_rx) {
                Some(s) => s,
                None => match request_snapshot(rig_tx).await {
                    Ok(s) => s,
                    Err(e) => return CommandResult::Reply(err_response(&e)),
                },
            };
            let info_line = format!("{} {}", snapshot.info.manufacturer, snapshot.info.model);
            ok_response([info_line])
        }
        _ => {
            warn!("rigctl unsupported command: {}", cmd_line);
            err_response("unsupported command")
        }
    };

    CommandResult::Reply(resp)
}

fn ok_response<I, S>(lines: I) -> String
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut resp = String::new();
    for line in lines {
        let line = line.into();
        if !line.is_empty() {
            resp.push_str(&line);
            resp.push('\n');
        }
    }
    resp.push_str("RPRT 0\n");
    resp
}

fn ok_only() -> String {
    "RPRT 0\n".to_string()
}

fn err_response(msg: &str) -> String {
    warn!("rigctl command error: {}", msg);
    "RPRT -1\n".to_string()
}

async fn request_snapshot(rig_tx: &mpsc::Sender<RigRequest>) -> Result<RigSnapshot, String> {
    send_rig_command(rig_tx, RigCommand::GetSnapshot).await
}

async fn send_rig_command(
    rig_tx: &mpsc::Sender<RigRequest>,
    cmd: RigCommand,
) -> Result<RigSnapshot, String> {
    let (resp_tx, resp_rx) = oneshot::channel();
    rig_tx
        .send(RigRequest {
            cmd,
            respond_to: resp_tx,
        })
        .await
        .map_err(|e| format!("failed to send to rig: {e:?}"))?;

    match timeout(Duration::from_secs(5), resp_rx).await {
        Ok(Ok(Ok(snapshot))) => Ok(snapshot),
        Ok(Ok(Err(err))) => Err(err.message),
        Ok(Err(e)) => Err(format!("rig response error: {e:?}")),
        Err(_) => Err("rig response timeout".into()),
    }
}

fn current_snapshot(state_rx: &watch::Receiver<RigState>) -> Option<RigSnapshot> {
    state_rx.borrow().snapshot()
}

fn rig_mode_to_str(mode: &RigMode) -> String {
    mode_to_string(mode)
}

fn dump_state_lines(_snapshot: &RigSnapshot) -> Vec<String> {
    // Hamlib expects a long, fixed sequence of bare values.
    // To maximize compatibility, mirror the ordering produced by hamlib's dummy backend.
    vec![
        "1".to_string(),
        "1".to_string(),
        "0".to_string(),
        "150000.000000 1500000000.000000 0x1ff -1 -1 0x17e00007 0xf".to_string(),
        "0 0 0 0 0 0 0".to_string(),
        "150000.000000 1500000000.000000 0x1ff 5000 100000 0x17e00007 0xf".to_string(),
        "0 0 0 0 0 0 0".to_string(),
        "0x1ff 1".to_string(),
        "0x1ff 0".to_string(),
        "0 0".to_string(),
        "0xc 2400".to_string(),
        "0xc 1800".to_string(),
        "0xc 3000".to_string(),
        "0xc 0".to_string(),
        "0x2 500".to_string(),
        "0x2 2400".to_string(),
        "0x2 50".to_string(),
        "0x2 0".to_string(),
        "0x10 300".to_string(),
        "0x10 2400".to_string(),
        "0x10 50".to_string(),
        "0x10 0".to_string(),
        "0x1 8000".to_string(),
        "0x1 2400".to_string(),
        "0x1 10000".to_string(),
        "0x20 15000".to_string(),
        "0x20 8000".to_string(),
        "0x40 230000".to_string(),
        "0 0".to_string(),
        "9990".to_string(),
        "9990".to_string(),
        "10000".to_string(),
        "0".to_string(),
        "10 ".to_string(),
        "10 20 30 ".to_string(),
        "0xffffffffffffffff".to_string(),
        "0xffffffffffffffff".to_string(),
        "0xfffffffff7ffffff".to_string(),
        "0xfffeff7083ffffff".to_string(),
        "0xffffffffffffffff".to_string(),
        "0xffffffffffffffbf".to_string(),
    ]
}

fn dump_caps_response(snapshot: &RigSnapshot) -> String {
    // netrigctl_open expects `setting=value` lines terminated by `done`.
    // Unknown keys are tolerated by Hamlib, but malformed lines are not.
    let mut resp = String::new();
    let push = |buf: &mut String, key: &str, val: String| {
        buf.push_str(key);
        buf.push('=');
        buf.push_str(&val);
        buf.push('\n');
    };

    push(&mut resp, "protocol_version", "1".to_string());
    push(&mut resp, "rig_model", "2".to_string());
    push(&mut resp, "model_name", snapshot.info.model.clone());
    push(
        &mut resp,
        "mfg_name",
        snapshot.info.manufacturer.clone(),
    );
    push(
        &mut resp,
        "backend_version",
        snapshot.info.revision.clone(),
    );
    push(
        &mut resp,
        "vfo_count",
        snapshot.info.capabilities.num_vfos.to_string(),
    );
    push(
        &mut resp,
        "has_vfo_b",
        if snapshot.info.capabilities.num_vfos >= 2 {
            "1".to_string()
        } else {
            "0".to_string()
        },
    );
    push(
        &mut resp,
        "can_ptt",
        if snapshot.status.tx.is_some() { "1" } else { "0" }.to_string(),
    );
    resp.push_str("done\n");
    resp
}

fn active_vfo_label(snapshot: &RigSnapshot) -> String {
    // Normalize to VFOA/VFOB/... for hamlib compatibility.
    snapshot
        .status
        .vfo
        .as_ref()
        .and_then(|vfo| vfo.active)
        .map(|idx| {
            let letter = (b'A' + (idx as u8)) as char;
            format!("VFO{}", letter)
        })
        .unwrap_or_else(|| "VFOA".to_string())
}

async fn set_vfo_target(target: &str, rig_tx: &mpsc::Sender<RigRequest>) -> Result<(), String> {
    let desired = normalize_vfo_name(target).ok_or_else(|| "expected VFOA or VFOB".to_string())?;
    let snapshot = request_snapshot(rig_tx).await?;
    let current = active_vfo_label(&snapshot);
    if current == desired {
        return Ok(());
    }

    let supports_toggle = snapshot
        .info
        .capabilities
        .num_vfos
        >= 2
        && snapshot
            .status
            .vfo
            .as_ref()
            .is_some_and(|v| v.entries.len() >= 2);
    if !supports_toggle {
        return Err("VFO selection not supported".to_string());
    }

    send_rig_command(rig_tx, RigCommand::ToggleVfo).await?;
    let after = request_snapshot(rig_tx).await?;
    if active_vfo_label(&after) == desired {
        Ok(())
    } else {
        Err("failed to switch VFO".to_string())
    }
}

fn normalize_vfo_name(v: &str) -> Option<String> {
    match v.trim().to_ascii_uppercase().as_str() {
        "VFOA" | "A" => Some("VFOA".to_string()),
        "VFOB" | "B" => Some("VFOB".to_string()),
        _ => None,
    }
}

fn is_true(s: &str) -> bool {
    matches!(s, "1" | "on" | "ON" | "true" | "True" | "TRUE")
}

fn is_false(s: &str) -> bool {
    matches!(s, "0" | "off" | "OFF" | "false" | "False" | "FALSE")
}

#[cfg(test)]
mod tests {
    use super::*;
    use trx_core::rig::{RigAccessMethod, RigCapabilities, RigInfo, RigStatus, RigTxStatus};

    fn test_snapshot() -> RigSnapshot {
        RigSnapshot {
            info: RigInfo {
                manufacturer: "TRX".to_string(),
                model: "Virtual".to_string(),
                revision: "0.1.0".to_string(),
                capabilities: RigCapabilities {
                    min_freq_step_hz: 1,
                    supported_bands: vec![],
                    supported_modes: vec![RigMode::USB],
                    num_vfos: 2,
                    lock: false,
                    lockable: false,
                    attenuator: false,
                    preamp: false,
                    rit: false,
                    rpt: false,
                    split: false,
                },
                access: RigAccessMethod::Tcp {
                    addr: "127.0.0.1:4532".to_string(),
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
                lock: None,
            },
            band: None,
            enabled: Some(true),
            initialized: true,
            server_callsign: None,
            server_version: None,
            server_latitude: None,
            server_longitude: None,
            pskreporter_status: None,
            aprs_decode_enabled: false,
            cw_decode_enabled: false,
            ft8_decode_enabled: false,
            wspr_decode_enabled: false,
            cw_auto: false,
            cw_wpm: 0,
            cw_tone_hz: 0,
        }
    }

    #[test]
    fn dump_caps_is_setting_value_and_ends_with_done() {
        let response = dump_caps_response(&test_snapshot());
        let lines: Vec<&str> = response.lines().collect();
        assert!(lines.iter().all(|line| *line == "done" || line.contains('=')));
        assert_eq!(lines.last(), Some(&"done"));
        assert!(response.contains("model_name=Virtual\n"));
        assert!(response.contains("mfg_name=TRX\n"));
    }
}
