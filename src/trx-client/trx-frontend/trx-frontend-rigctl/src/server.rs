// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use std::sync::atomic::Ordering;
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
        context: Arc<trx_frontend::FrontendRuntimeContext>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = serve(listen_addr, state_rx, rig_tx, context).await {
                error!("rigctl server error: {:?}", e);
            }
        })
    }
}

async fn serve(
    listen_addr: SocketAddr,
    state_rx: watch::Receiver<RigState>,
    rig_tx: mpsc::Sender<RigRequest>,
    context: Arc<trx_frontend::FrontendRuntimeContext>,
) -> std::io::Result<()> {
    if let Ok(mut slot) = context.rigctl_listen_addr.lock() {
        *slot = Some(listen_addr);
    }
    let listener = TcpListener::bind(listen_addr).await?;
    info!("rigctl frontend listening on {}", listen_addr);
    info!("rigctl frontend ready (rigctld-compatible)");

    loop {
        let (stream, addr) = listener.accept().await?;
        info!("rigctl client connected: {}", addr);
        let state_rx = state_rx.clone();
        let rig_tx = rig_tx.clone();
        let context = context.clone();
        context.rigctl_clients.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, addr, state_rx, rig_tx).await {
                warn!("rigctl client {} error: {:?}", addr, e);
            }
            context.rigctl_clients.fetch_sub(1, Ordering::Relaxed);
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
    debug!("rigctl command: {}", cmd_line);
    let mut parts = cmd_line.split_whitespace();
    let Some(raw_op) = parts.next() else {
        return CommandResult::Reply(err_response("empty command"));
    };
    let extended = raw_op.starts_with('+');
    let op = raw_op.trim_start_matches('+').trim_end_matches(':');

    let resp = match op {
        "q" | "Q" | "\\q" | "\\quit" => return CommandResult::Close,
        "f" | "\\get_freq" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => ok_response(op, extended, [snapshot.status.freq.hz.to_string()]),
            Err(e) => err_response(&e),
        },
        "F" | "\\set_freq" => match parts.next().and_then(parse_freq_hz_arg) {
            Some(freq) => {
                match send_set_freq_with_compat_retry(rig_tx, freq).await {
                    Ok(_) => ok_only(op, extended),
                    Err(e) => err_response(&e),
                }
            }
            None => err_response("expected frequency in Hz"),
        },
        "l" | "\\get_level" => {
            // Hamlib may probe optional levels during open (e.g. KEYSPD).
            // Return a benign default to keep client compatibility.
            let _level_name = parts.next();
            ok_response(op, extended, ["0"])
        }
        "m" | "\\get_mode" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => {
                let mode = rig_mode_to_str(&snapshot.status.mode);
                ok_response(op, extended, [mode, "0".to_string()])
            }
            Err(e) => err_response(&e),
        },
        "M" | "\\set_mode" => {
            let Some(mode_str) = parts.next() else {
                return CommandResult::Reply(err_response("expected mode"));
            };
            let mode = parse_mode(mode_str);
            match send_rig_command(rig_tx, RigCommand::SetMode(mode)).await {
                Ok(_) => ok_only(op, extended),
                Err(e) => err_response(&e),
            }
        }
        "t" | "\\get_ptt" | "get_ptt" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => {
                ok_response(
                    op,
                    extended,
                    [if snapshot.status.tx_en { "1" } else { "0" }.to_string()],
                )
            }
            Err(e) => err_response(&e),
        },
        "T" | "\\set_ptt" | "set_ptt" => match parse_ptt_tokens(parts.collect()) {
            Some(v) => {
                let snapshot = match current_snapshot(state_rx) {
                    Some(s) => s,
                    None => match request_snapshot(rig_tx).await {
                        Ok(s) => s,
                        Err(e) => return CommandResult::Reply(err_response(&e)),
                    },
                };
                if !rig_supports_ptt(&snapshot) {
                    return CommandResult::Reply(err_response("PTT not supported"));
                }

                match parse_ptt_arg(&v) {
                    Some(ptt) => {
                        debug!("rigctl ptt request: cmd='{}' parsed_ptt={}", cmd_line, ptt);
                        match send_rig_command(rig_tx, RigCommand::SetPtt(ptt)).await {
                            Ok(_) => ok_only(op, extended),
                            Err(e) => err_response(&e),
                        }
                    }
                    None => err_response("expected PTT state (0/1)"),
                }
            }
            _ => err_response("expected PTT state (0/1)"),
        },
        "v" | "\\get_vfo" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => ok_response(op, extended, [active_vfo_label(&snapshot)]),
            Err(e) => err_response(&e),
        },
        "V" | "\\set_vfo" => {
            let Some(target) = parts.next() else {
                return CommandResult::Reply(err_response("expected VFO (VFOA/VFOB)"));
            };
            match set_vfo_target(target, rig_tx).await {
                Ok(()) => ok_only(op, extended),
                Err(e) => err_response(&e),
            }
        }
        "s" | "\\get_split_vfo" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => {
                // split state, tx vfo
                ok_response(op, extended, ["0".to_string(), active_vfo_label(&snapshot)])
            }
            Err(e) => err_response(&e),
        },
        "S" | "\\set_split_vfo" => match parts.next() {
            Some(v) if is_false(v) => ok_only(op, extended),
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
            ok_response(op, extended, [info])
        }
        "\\get_powerstat" | "get_powerstat" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => {
                let val = snapshot.enabled.unwrap_or(false);
                ok_response(op, extended, [if val { "1" } else { "0" }.to_string()])
            }
            Err(e) => err_response(&e),
        },
        "\\chk_vfo" | "chk_vfo" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => ok_response(op, extended, [active_vfo_label(&snapshot)]),
            Err(e) => err_response(&e),
        },
        "\\dump_state" | "dump_state" => match request_snapshot(rig_tx).await {
            Ok(snapshot) => ok_response(op, extended, dump_state_lines(&snapshot)),
            Err(e) => err_response(&e),
        },
        "1" | "\\dump_caps" | "dump_caps" | "\\dumpcaps" | "dumpcaps" => {
            match request_snapshot(rig_tx).await {
                Ok(snapshot) => dump_caps_response(op, extended, &snapshot),
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
            ok_response(op, extended, [info_line])
        }
        _ => {
            warn!("rigctl unsupported command: {}", cmd_line);
            err_response("unsupported command")
        }
    };

    CommandResult::Reply(resp)
}

fn ok_response<I, S>(op: &str, extended: bool, lines: I) -> String
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    if extended {
        let mut resp = String::new();
        for line in lines {
            resp.push_str(op);
            resp.push_str(": ");
            resp.push_str(&line.into());
            resp.push('\n');
        }
        resp.push_str("RPRT 0\n");
        resp
    } else {
        let mut resp = String::new();
        for line in lines {
            let line = line.into();
            if !line.is_empty() {
                resp.push_str(&line);
                resp.push('\n');
            }
        }
        resp
    }
}

fn ok_only(op: &str, extended: bool) -> String {
    if extended {
        format!("{op}:\nRPRT 0\n")
    } else {
        "RPRT 0\n".to_string()
    }
}

fn err_response(msg: &str) -> String {
    warn!("rigctl command error: {}", msg);
    "RPRT -1\n".to_string()
}

fn rig_supports_ptt(snapshot: &RigSnapshot) -> bool {
    snapshot.status.tx.is_some()
        || snapshot
            .info
            .capabilities
            .supported_bands
            .iter()
            .any(|b| b.tx_allowed)
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

async fn send_set_freq_with_compat_retry(
    rig_tx: &mpsc::Sender<RigRequest>,
    freq_hz: u64,
) -> Result<RigSnapshot, String> {
    match send_rig_command(rig_tx, RigCommand::SetFreq(Freq { hz: freq_hz })).await {
        Ok(snapshot) => Ok(snapshot),
        Err(e) => {
            // FT-817 backend requires 10 Hz alignment; some hamlib clients submit
            // values with 1 Hz granularity.
            if e.contains("multiple of 10 Hz") {
                let rounded = ((freq_hz + 5) / 10) * 10;
                if rounded != freq_hz {
                    return send_rig_command(rig_tx, RigCommand::SetFreq(Freq { hz: rounded }))
                        .await;
                }
            }
            Err(e)
        }
    }
}

fn current_snapshot(state_rx: &watch::Receiver<RigState>) -> Option<RigSnapshot> {
    state_rx.borrow().snapshot()
}

fn rig_mode_to_str(mode: &RigMode) -> String {
    mode_to_string(mode)
}

fn dump_state_lines(snapshot: &RigSnapshot) -> Vec<String> {
    // Hamlib expects a long, fixed sequence of bare values.
    // To maximize compatibility, mirror the ordering produced by hamlib's dummy backend.
    // Some Hamlib/netrigctl versions expect a trailing `done` sentinel.
    let mut lines = vec![
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
        "0xffffffffffffffff".to_string(),
        if rig_supports_ptt(snapshot) {
            "0xffffffffffffffff".to_string()
        } else {
            "0x0".to_string()
        },
        "0xffffffffffffffff".to_string(),
        "0xffffffffffffffff".to_string(),
    ];
    lines.push("done".to_string());
    lines
}

fn dump_caps_response(op: &str, extended: bool, snapshot: &RigSnapshot) -> String {
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
        if rig_supports_ptt(snapshot) {
            "1".to_string()
        } else {
            "0".to_string()
        },
    );
    resp.push_str("done\n");
    if extended {
        ok_response(op, true, resp.lines().map(|s| s.to_string()).collect::<Vec<_>>())
    } else {
        resp
    }
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

fn parse_ptt_arg(s: &str) -> Option<bool> {
    let normalized = s.trim().trim_end_matches(';').trim_end_matches(',');
    if is_true(normalized) {
        return Some(true);
    }
    if is_false(normalized) {
        return Some(false);
    }

    // Hamlib may send enum-like numeric values where non-zero means ON.
    if let Ok(v) = normalized.parse::<i64>() {
        return Some(v != 0);
    }

    match normalized.to_ascii_uppercase().as_str() {
        "ON_DATA" | "DATA" | "MIC" | "ON_MIC" => Some(true),
        _ => None,
    }
}

fn parse_ptt_tokens(tokens: Vec<&str>) -> Option<String> {
    match tokens.as_slice() {
        [] => None,
        [only] => Some((*only).to_string()),
        [first, second, ..] if normalize_vfo_name(first).is_some() => Some((*second).to_string()),
        _ => tokens
            .iter()
            .rev()
            .find(|t| parse_ptt_arg(t).is_some())
            .copied()
            .map(str::to_string)
            .or_else(|| tokens.last().map(|s| (*s).to_string())),
    }
}

fn parse_freq_hz_arg(s: &str) -> Option<u64> {
    if let Ok(hz) = s.parse::<u64>() {
        return Some(hz);
    }

    let mut hz = s.parse::<f64>().ok()?;
    if !hz.is_finite() || hz <= 0.0 {
        return None;
    }

    // Some rigctl clients send MHz as a decimal float (e.g. "7.100000").
    // Heuristic: if decimal value is below 1 MHz, interpret as MHz.
    if s.contains('.') && hz < 1_000_000.0 {
        hz *= 1_000_000.0;
    }

    if hz > (u64::MAX as f64) {
        return None;
    }
    Some(hz.round() as u64)
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
            server_build_date: None,
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
        let response = dump_caps_response("dump_caps", false, &test_snapshot());
        let lines: Vec<&str> = response.lines().collect();
        assert!(lines.iter().all(|line| *line == "done" || line.contains('=')));
        assert_eq!(lines.last(), Some(&"done"));
        assert!(response.contains("model_name=Virtual\n"));
        assert!(response.contains("mfg_name=TRX\n"));
    }

    #[test]
    fn ok_response_does_not_append_rprt_status() {
        let response = ok_response("f", false, ["7100000"]);
        assert_eq!(response, "7100000\n");
    }

    #[test]
    fn ok_response_extended_includes_command_prefix_and_status() {
        let response = ok_response("\\get_freq", true, ["7100000"]);
        assert_eq!(response, "\\get_freq: 7100000\nRPRT 0\n");
    }

    #[test]
    fn parse_freq_hz_arg_accepts_integer_and_decimal() {
        assert_eq!(parse_freq_hz_arg("7100000"), Some(7_100_000));
        assert_eq!(parse_freq_hz_arg("7100000.000000"), Some(7_100_000));
        assert_eq!(parse_freq_hz_arg("7.100000"), Some(7_100_000));
    }

    #[test]
    fn parse_ptt_arg_accepts_common_hamlib_values() {
        assert_eq!(parse_ptt_arg("0"), Some(false));
        assert_eq!(parse_ptt_arg("1"), Some(true));
        assert_eq!(parse_ptt_arg("2"), Some(true));
        assert_eq!(parse_ptt_arg("OFF"), Some(false));
        assert_eq!(parse_ptt_arg("ON"), Some(true));
        assert_eq!(parse_ptt_arg("DATA"), Some(true));
    }

    #[test]
    fn parse_ptt_tokens_accepts_optional_vfo_prefix() {
        assert_eq!(parse_ptt_tokens(vec!["1"]), Some("1".to_string()));
        assert_eq!(
            parse_ptt_tokens(vec!["VFOA", "1"]),
            Some("1".to_string())
        );
        assert_eq!(
            parse_ptt_tokens(vec!["VFOB", "ON_DATA"]),
            Some("ON_DATA".to_string())
        );
    }
}
