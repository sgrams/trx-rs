// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::net::SocketAddr;

use clap::{Parser, ValueEnum};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::signal;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{self, Duration, Instant};
use tracing::{debug, error, info, warn};

mod error;

use crate::error::is_invalid_bcd_error;
use trx_backend::{build_rig, RigAccess, RigKind};
use trx_core::radio::freq::Freq;
use trx_core::rig::command::RigCommand;
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::{RigMode, RigSnapshot, RigState};
use trx_core::rig::{RigCat, RigControl, RigRxStatus, RigStatus, RigTxStatus};
use trx_core::{ClientCommand, ClientResponse, DynResult, RigError, RigResult};
use trx_frontend::FrontendSpawner;
use trx_frontend_http::server::HttpFrontend;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum FrontendKind {
    Http,
}

const PKG_DESCRIPTION: &str = concat!(env!("CARGO_PKG_NAME"), " - ", env!("CARGO_PKG_DESCRIPTION"));
const PKG_LONG_ABOUT: &str = concat!(
    env!("CARGO_PKG_DESCRIPTION"),
    "\nHomepage: ",
    env!("CARGO_PKG_HOMEPAGE")
);

#[derive(Debug, Parser)]
#[command(
    author = env!("CARGO_PKG_AUTHORS"),
    version = env!("CARGO_PKG_VERSION"),
    about = PKG_DESCRIPTION,
    long_about = PKG_LONG_ABOUT
)]
struct Cli {
    /// Rig backend to use (e.g. ft817)
    #[arg(short = 'r', long = "rig", value_enum)]
    rig: RigKind,
    /// Access method to reach the rig CAT interface
    #[arg(short = 'a', long = "access", value_enum, default_value_t = AccessKind::Serial)]
    access: AccessKind,
    /// Frontend to expose for control/status (e.g. http)
    #[arg(short = 'f', long = "frontend", value_enum, default_value_t = FrontendKind::Http)]
    frontend: FrontendKind,
    /// Rig CAT address:
    /// when access is serial: <path> <baud>;
    /// when access is TCP: <host>:<port>
    #[arg(value_name = "RIG_ADDR")]
    rig_addr: String,
    /// Optional callsign/owner label to show in the frontend
    #[arg(short = 'c', long = "callsign")]
    callsign: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AccessKind {
    Serial,
    Tcp,
}

/// Parse a serial rig address of the form "<path> <baud>".
fn parse_serial_addr(addr: &str) -> DynResult<(String, u32)> {
    let mut parts = addr.split_whitespace();
    let path = parts
        .next()
        .ok_or("Serial rig address must be '<path> <baud>'")?;
    let baud_str = parts
        .next()
        .ok_or("Serial rig address must be '<path> <baud>'")?;
    if parts.next().is_some() {
        return Err("Serial rig address must be '<path> <baud>' (got extra data)".into());
    }
    let baud: u32 = baud_str
        .parse()
        .map_err(|e| format!("Invalid baud '{}': {}", baud_str, e))?;
    Ok((path.to_string(), baud))
}

#[tokio::main]
async fn main() -> DynResult<()> {
    init_tracing();

    let cli = Cli::parse();

    let access = match cli.access {
        AccessKind::Serial => {
            let (path, baud) = parse_serial_addr(&cli.rig_addr)?;
            info!(
                "Starting trxd (rig: {}, access: serial {} @ {} baud)",
                cli.rig, path, baud
            );
            RigAccess::Serial { path, baud }
        }
        AccessKind::Tcp => {
            info!(
                "Starting trxd (rig: {}, access: tcp {})",
                cli.rig, cli.rig_addr
            );
            RigAccess::Tcp {
                addr: cli.rig_addr.clone(),
            }
        }
    };
    // Channel used to communicate with the rig task.
    let (tx, rx) = mpsc::channel::<RigRequest>(32);
    let initial_state = RigState {
        rig_info: None,
        status: RigStatus {
            freq: Freq { hz: 144_300_000 },
            mode: RigMode::USB,
            tx_en: false,
            vfo: None,
            tx: Some(RigTxStatus {
                power: None,
                limit: None,
                swr: None,
                alc: None,
            }),
            rx: Some(RigRxStatus { sig: None }),
            lock: Some(false),
        },
        initialized: false,
        control: RigControl {
            rpt_offset_hz: None,
            ctcss_hz: None,
            dcs_code: None,
            lock: Some(false),
            clar_hz: None,
            clar_on: None,
            enabled: Some(false),
        },
    };
    let (state_tx, state_rx) = watch::channel(initial_state.clone());

    // Spawn the rig task.
    let _rig_handle = tokio::spawn(rig_task(cli.rig, access, rx, state_tx, initial_state));

    // Start TCP listener for clients.
    let listen_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(listen_addr).await?;
    let actual_addr = listener.local_addr()?;
    info!("TCP listener started on {}", actual_addr);

    // Start simple HTTP status server on 127.0.0.1:8080.
    let http_state_rx = state_rx.clone();
    if matches!(cli.frontend, FrontendKind::Http) {
        HttpFrontend::spawn_frontend(http_state_rx, tx.clone(), cli.callsign.clone());
    }

    loop {
        tokio::select! {
            res = listener.accept() => {
                let (socket, addr) = res?;
                info!("New client connected: {}", addr);

                let tx_clone = tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(socket, addr, tx_clone).await {
                        error!("Client {} error: {:?}", addr, e);
                    }
                });
            }
            _ = signal::ctrl_c() => {
                info!("Ctrl+C received, shutting down");
                break;
            }
        }
    }

    Ok(())
}

/// Initialize logging/tracing.
fn init_tracing() {
    // Uses default formatting and RUST_LOG if available.
    tracing_subscriber::fmt().with_target(false).init();
}

/// Task that owns the TRX state and talks to the serial port.
async fn rig_task(
    rig_kind: RigKind,
    access: RigAccess,
    mut rx: mpsc::Receiver<RigRequest>,
    state_tx: watch::Sender<RigState>,
    mut state: RigState,
) -> DynResult<()> {
    info!("Opening rig backend {}", rig_kind);
    match &access {
        RigAccess::Serial { path, baud } => info!("Serial: {} @ {} baud", path, baud),
        RigAccess::Tcp { addr } => info!("TCP CAT: {}", addr),
    }

    let mut rig: Box<dyn RigCat> = build_rig(rig_kind, access)?;
    info!("Rig backend ready");

    let mut poll = time::interval(Duration::from_millis(250));
    let mut poll_pause_until: Option<Instant> = None;
    let mut last_power_on: Option<Instant> = None;

    // Initial bring-up and VFO priming.
    let rig_info = rig.info().clone();
    state.rig_info = Some(rig_info);
    if let Some(info) = state.rig_info.as_ref() {
        info!(
            "Rig info: {} {} {}",
            info.manufacturer, info.model, info.revision
        );
    }
    let _ = state_tx.send(state.clone());
    if !state.control.enabled.unwrap_or(false) {
        info!("Sending initial PowerOn to wake rig");
        match rig.power_on().await {
            Ok(()) => {
                state.control.enabled = Some(true);
                time::sleep(Duration::from_secs(3)).await;
                if let Err(e) = refresh_state_with_retry(&mut rig, &mut state, 2).await {
                    warn!(
                        "Initial PowerOn refresh failed: {:?}; retrying once after short delay",
                        e
                    );
                    time::sleep(Duration::from_millis(500)).await;
                    if let Err(e2) = refresh_state_with_retry(&mut rig, &mut state, 1).await {
                        warn!(
                            "Initial PowerOn second refresh failed (continuing): {:?}",
                            e2
                        );
                    }
                }
                info!("Rig initialized after power on sequence");
            }
            Err(e) => warn!("Initial PowerOn failed (continuing): {:?}", e),
        }
    }
    if let Err(e) = prime_vfo_state(&mut rig, &mut state).await {
        warn!("VFO priming failed: {:?}", e);
    }
    state.initialized = true;
    let _ = state_tx.send(state.clone());

    // Single-task loop: handle commands and periodic polling.
    loop {
        tokio::select! {
            _ = poll.tick() => {
                if let Some(until) = poll_pause_until {
                    if Instant::now() < until {
                        continue;
                    } else {
                        poll_pause_until = None;
                    }
                }
                if matches!(state.control.enabled, Some(false)) {
                    continue;
                }
                match refresh_state_with_retry(&mut rig, &mut state, 2).await {
                    Ok(()) => { let _ = state_tx.send(state.clone()); }
                    Err(e) => {
                        error!("CAT polling error: {:?}", e);
                        if let Some(last_on) = last_power_on {
                            if Instant::now().duration_since(last_on) < Duration::from_secs(5) {
                                poll_pause_until = Some(Instant::now() + Duration::from_millis(800));
                                continue;
                            }
                        }
                    }
                }
            },
            maybe_req = rx.recv() => {
                let Some(first_req) = maybe_req else { break; };
                let mut batch = vec![first_req];
                while let Ok(next) = rx.try_recv() {
                    batch.push(next);
                }
                while let Some(RigRequest { cmd, respond_to }) = batch.pop() {
                    let responders = vec![respond_to];
                    let cmd_label = format!("{:?}", cmd);
                    let started = Instant::now();

                    let result: RigResult<RigSnapshot> = {
                        let not_ready = !state.initialized
                            && !matches!(cmd, RigCommand::PowerOn | RigCommand::GetSnapshot);
                        if not_ready {
                            Err(RigError("rig not initialized yet".into()))
                        } else {
                            match cmd {
                                RigCommand::GetSnapshot => match refresh_state_with_retry(&mut rig, &mut state, 2).await {
                                    Ok(()) => {
                                        let _ = state_tx.send(state.clone());
                                        snapshot_from(&state)
                                    }
                                    Err(e) => {
                                        error!("Failed to read CAT status: {:?}", e);
                                        Err(RigError(format!("CAT error: {}", e)))
                                    }
                                },
                                RigCommand::SetFreq(freq) => {
                                    info!("SetFreq requested: {} Hz", freq.hz);
                                    if state.control.lock.unwrap_or(false) {
                                        warn!("SetFreq blocked: panel lock is active");
                                        Err(RigError("panel is locked".into()))
                                    } else {
                                        let res = time::timeout(Duration::from_secs(1), rig.set_freq(freq)).await;
                                        match res {
                                            Ok(Ok(())) => {
                                                state.apply_freq(freq);
                                                poll_pause_until = Some(Instant::now() + Duration::from_millis(200));
                                                let _ = state_tx.send(state.clone());
                                                snapshot_from(&state)
                                            }
                                            Ok(Err(e)) => {
                                                error!("Failed to send CAT SetFreq: {:?}", e);
                                                Err(RigError(format!("CAT error: {}", e)))
                                            }
                                            Err(elapsed) => {
                                                warn!("CAT SetFreq timed out ({:?}) but proceeding with state update", elapsed);
                                                state.apply_freq(freq);
                                                poll_pause_until = Some(Instant::now() + Duration::from_millis(200));
                                                let _ = state_tx.send(state.clone());
                                                snapshot_from(&state)
                                            }
                                        }
                                    }
                                }
                                RigCommand::SetMode(mode) => {
                                    info!("SetMode requested: {:?}", mode);
                                    if state.control.lock.unwrap_or(false) {
                                        warn!("SetMode blocked: panel lock is active");
                                        Err(RigError("panel is locked".into()))
                                    } else {
                                        let res = time::timeout(Duration::from_secs(1), rig.set_mode(mode.clone())).await;
                                        match res {
                                            Ok(Ok(())) => {
                                                state.apply_mode(mode.clone());
                                                poll_pause_until = Some(Instant::now() + Duration::from_millis(200));
                                                let _ = state_tx.send(state.clone());
                                                snapshot_from(&state)
                                            }
                                            Ok(Err(e)) => {
                                                error!("Failed to send CAT SetMode: {:?}", e);
                                                Err(RigError(format!("CAT error: {}", e)))
                                            }
                                            Err(elapsed) => {
                                                warn!("CAT SetMode timed out ({:?}) but proceeding with state update", elapsed);
                                                state.apply_mode(mode.clone());
                                                poll_pause_until = Some(Instant::now() + Duration::from_millis(200));
                                                let _ = state_tx.send(state.clone());
                                                snapshot_from(&state)
                                            }
                                        }
                                    }
                                }
                                RigCommand::SetPtt(ptt) => {
                                    info!("SetPtt requested: {}", ptt);
                                    if let Err(e) = rig.set_ptt(ptt).await {
                                        error!("Failed to send CAT SetPtt: {:?}", e);
                                        Err(RigError(format!("CAT error: {}", e)))
                                    } else {
                                        state.status.tx_en = ptt;
                                        if !ptt {
                                            if let Some(tx) = state.status.tx.as_mut() {
                                                tx.power = Some(0);
                                                tx.swr = Some(0.0);
                                            }
                                        }
                                        state.status.lock = state.control.lock;
                                        let _ = state_tx.send(state.clone());
                                        snapshot_from(&state)
                                    }
                                }
                                RigCommand::PowerOn => {
                                    info!("PowerOn requested");
                                    if let Err(e) = rig.power_on().await {
                                        error!("Failed to send CAT PowerOn: {:?}", e);
                                        Err(RigError(format!("CAT error: {}", e)))
                                    } else {
                                        state.control.enabled = Some(true);
                                        time::sleep(Duration::from_secs(3)).await;
                                        let now = Instant::now();
                                        poll_pause_until = Some(now + Duration::from_secs(3));
                                        last_power_on = Some(now);
                                        match refresh_state_with_retry(&mut rig, &mut state, 2).await {
                                            Ok(()) => {
                                                let _ = state_tx.send(state.clone());
                                                snapshot_from(&state)
                                            }
                                            Err(e) => {
                                                if is_invalid_bcd_error(e.as_ref()) {
                                                    warn!("Transient CAT decode after PowerOn (ignored): {:?}", e);
                                                    poll_pause_until = Some(Instant::now() + Duration::from_millis(1500));
                                                    let _ = state_tx.send(state.clone());
                                                    snapshot_from(&state)
                                                } else {
                                                    error!("Failed to refresh after PowerOn: {:?}", e);
                                                    Err(RigError(format!("CAT error: {}", e)))
                                                }
                                            }
                                        }
                                    }
                                }
                                RigCommand::PowerOff => {
                                    info!("PowerOff requested");
                                    if let Err(e) = rig.power_off().await {
                                        error!("Failed to send CAT PowerOff: {:?}", e);
                                        Err(RigError(format!("CAT error: {}", e)))
                                    } else {
                                        state.control.enabled = Some(false);
                                        state.status.tx_en = false;
                                        let _ = state_tx.send(state.clone());
                                        snapshot_from(&state)
                                    }
                                }
                                RigCommand::ToggleVfo => {
                                    info!("Toggle VFO requested");
                                    if state.control.lock.unwrap_or(false) {
                                        warn!("ToggleVfo blocked: panel lock is active");
                                        Err(RigError("panel is locked".into()))
                                    } else if let Err(e) = rig.toggle_vfo().await {
                                        error!("Failed to send CAT ToggleVfo: {:?}", e);
                                        Err(RigError(format!("CAT error: {}", e)))
                                    } else {
                                        time::sleep(Duration::from_millis(150)).await;
                                        poll_pause_until = Some(Instant::now() + Duration::from_millis(300));
                                        match refresh_state_with_retry(&mut rig, &mut state, 2).await {
                                            Ok(()) => {
                                                let _ = state_tx.send(state.clone());
                                                snapshot_from(&state)
                                            }
                                            Err(e) => {
                                                error!("Failed to refresh after ToggleVfo: {:?}", e);
                                                Err(RigError(format!("CAT error: {}", e)))
                                            }
                                        }
                                    }
                                }
                                RigCommand::GetTxLimit => match rig.get_tx_limit().await {
                                    Ok(limit) => {
                                        state
                                            .status
                                            .tx
                                            .get_or_insert(RigTxStatus { power: None, limit: None, swr: None, alc: None })
                                            .limit = Some(limit);
                                        let _ = state_tx.send(state.clone());
                                        snapshot_from(&state)
                                    }
                                    Err(e) => {
                                        error!("Failed to read TX limit: {:?}", e);
                                        Err(RigError(format!("CAT error: {}", e)))
                                    }
                                },
                                RigCommand::SetTxLimit(limit) => match rig.set_tx_limit(limit).await {
                                    Ok(()) => {
                                        state
                                            .status
                                            .tx
                                            .get_or_insert(RigTxStatus { power: None, limit: None, swr: None, alc: None })
                                            .limit = Some(limit);
                                        let _ = state_tx.send(state.clone());
                                        snapshot_from(&state)
                                    }
                                    Err(e) => {
                                        error!("Failed to set TX limit: {:?}", e);
                                        Err(RigError(format!("CAT error: {}", e)))
                                    }
                                }
                                RigCommand::Lock => {
                                    info!("Lock requested");
                                    match rig.lock().await {
                                        Ok(()) => {
                                            state.control.lock = Some(true);
                                            state.status.lock = Some(true);
                                            let _ = state_tx.send(state.clone());
                                            snapshot_from(&state)
                                        }
                                        Err(e) => {
                                            error!("Failed to send CAT Lock: {:?}", e);
                                            Err(RigError(format!("CAT error: {}", e)))
                                        }
                                    }
                                }
                                RigCommand::Unlock => {
                                    info!("Unlock requested");
                                    match rig.unlock().await {
                                        Ok(()) => {
                                            state.control.lock = Some(false);
                                            state.status.lock = Some(false);
                                            let _ = state_tx.send(state.clone());
                                            snapshot_from(&state)
                                        }
                                        Err(e) => {
                                            error!("Failed to send CAT Unlock: {:?}", e);
                                            Err(RigError(format!("CAT error: {}", e)))
                                        }
                                    }
                                }
                            }
                        }
                    };

                    for tx in responders {
                        let _ = tx.send(result.clone());
                    }
                    let elapsed = started.elapsed();
                    if elapsed > Duration::from_millis(500) {
                        warn!("Rig command {} took {:?}", cmd_label, elapsed);
                    } else {
                        debug!("Rig command {} completed in {:?}", cmd_label, elapsed);
                    }
                }
            },
        }
    }

    info!("rig_task shutting down (channel closed)");
    Ok(())
}

async fn refresh_state_from_cat(trx: &mut Box<dyn RigCat>, state: &mut RigState) -> DynResult<()> {
    let (freq, mode, vfo) = trx.get_status().await?;
    state.control.enabled = Some(true);
    state.apply_freq(freq);
    state.apply_mode(mode);
    state.status.vfo = vfo.clone();

    if state.status.tx_en {
        state.status.rx.get_or_insert(RigRxStatus { sig: None }).sig = Some(0);
    } else if let Ok(meter) = trx.get_signal_strength().await {
        let sig = map_signal_strength(&state.status.mode, meter);
        state.status.rx.get_or_insert(RigRxStatus { sig: None }).sig = Some(sig);
    }
    if let Ok(limit) = trx.get_tx_limit().await {
        state
            .status
            .tx
            .get_or_insert(RigTxStatus {
                power: None,
                limit: None,
                swr: None,
                alc: None,
            })
            .limit = Some(limit);
    }
    if state.status.tx_en {
        if let Ok(power) = trx.get_tx_power().await {
            state
                .status
                .tx
                .get_or_insert(RigTxStatus {
                    power: None,
                    limit: None,
                    swr: None,
                    alc: None,
                })
                .power = Some(power);
        }
    }
    state.status.lock = Some(state.control.lock.unwrap_or(false));
    Ok(())
}

async fn refresh_state_with_retry(
    trx: &mut Box<dyn RigCat>,
    state: &mut RigState,
    attempts: usize,
) -> DynResult<()> {
    let mut last_err: Option<Box<dyn std::error::Error + Send + Sync>> = None;
    for i in 0..attempts {
        match refresh_state_from_cat(trx, state).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                let should_retry = is_invalid_bcd_error(e.as_ref());
                last_err = Some(e);
                if should_retry && i + 1 < attempts {
                    warn!(
                        "Retrying CAT state read after invalid BCD (attempt {} of {})",
                        i + 1,
                        attempts
                    );
                    time::sleep(Duration::from_millis(300)).await;
                    continue;
                } else {
                    break;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| "Unknown CAT error".into()))
}

async fn prime_vfo_state(trx: &mut Box<dyn RigCat>, state: &mut RigState) -> DynResult<()> {
    // Ensure panel is unlocked so we can CAT-control safely.
    let _ = trx.unlock().await;
    time::sleep(Duration::from_millis(100)).await;

    refresh_state_with_retry(trx, state, 2).await?;
    time::sleep(Duration::from_millis(150)).await;

    trx.toggle_vfo().await?;
    time::sleep(Duration::from_millis(150)).await;
    refresh_state_with_retry(trx, state, 2).await?;

    trx.toggle_vfo().await?;
    time::sleep(Duration::from_millis(150)).await;
    refresh_state_with_retry(trx, state, 2).await?;

    Ok(())
}

/// Handle a single TCP client.
async fn handle_client(
    socket: TcpStream,
    addr: SocketAddr,
    tx: mpsc::Sender<RigRequest>,
) -> DynResult<()> {
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

        // Simple protocol: one line = one JSON command.
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let cmd: ClientCommand = match serde_json::from_str(trimmed) {
            Ok(c) => c,
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

        // Map ClientCommand -> RigCommand.
        let rig_cmd = match cmd {
            ClientCommand::GetState => RigCommand::GetSnapshot,
            ClientCommand::SetFreq { freq_hz } => RigCommand::SetFreq(Freq { hz: freq_hz }),
            ClientCommand::SetMode { mode } => RigCommand::SetMode(parse_mode(&mode)),
            ClientCommand::SetPtt { ptt } => RigCommand::SetPtt(ptt),
            ClientCommand::PowerOn => RigCommand::PowerOn,
            ClientCommand::PowerOff => RigCommand::PowerOff,
            ClientCommand::ToggleVfo => RigCommand::ToggleVfo,
            ClientCommand::GetTxLimit => RigCommand::GetTxLimit,
            ClientCommand::SetTxLimit { limit } => RigCommand::SetTxLimit(limit),
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
                    error: Some(err.0),
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

fn map_signal_strength(mode: &RigMode, raw: u8) -> i32 {
    let val = raw as i32;
    match mode {
        RigMode::FM | RigMode::WFM => val.saturating_sub(128),
        _ => val,
    }
}

/// Parse mode string coming from the client into RigMode.
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

fn snapshot_from(state: &RigState) -> RigResult<RigSnapshot> {
    state
        .snapshot()
        .ok_or_else(|| RigError("Rig info unavailable".into()))
}
