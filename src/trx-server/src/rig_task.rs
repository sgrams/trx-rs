// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Rig task implementation using controller components.

use std::time::Duration;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio::time::{self, Instant};
use tracing::{debug, error, info, warn};

use trx_backend::{RegistrationContext, RigAccess};
use trx_core::radio::freq::Freq;
use trx_core::rig::command::RigCommand;
use trx_core::rig::controller::{
    command_from_rig_command, AdaptivePolling, CommandContext, CommandResult, ExponentialBackoff,
    PollingPolicy, ReadyStateData, RetryPolicy, RigCatExecutor, RigEventEmitter, RigMachineState,
    RigStateMachine, TransmittingStateData, ValidationResult,
};
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::{RigMode, RigSnapshot, RigState};
use trx_core::rig::{RigCat, RigRxStatus, RigTxStatus};
use trx_core::{DynResult, RigError, RigResult};

use crate::audio;
use crate::error::is_invalid_bcd_error;

/// Configuration for the rig task.
pub struct RigTaskConfig {
    pub registry: Arc<RegistrationContext>,
    pub rig_model: String,
    pub access: RigAccess,
    pub polling: AdaptivePolling,
    pub retry: ExponentialBackoff,
    pub initial_freq_hz: u64,
    pub initial_mode: RigMode,
    pub server_callsign: Option<String>,
    pub server_version: Option<String>,
    pub server_latitude: Option<f64>,
    pub server_longitude: Option<f64>,
}

impl Default for RigTaskConfig {
    fn default() -> Self {
        let mut registry = RegistrationContext::new();
        trx_backend::register_builtin_backends_on(&mut registry);
        Self {
            registry: Arc::new(registry),
            rig_model: "ft817".to_string(),
            access: RigAccess::Serial {
                path: "/dev/ttyUSB0".to_string(),
                baud: 9600,
            },
            polling: AdaptivePolling::default(),
            retry: ExponentialBackoff::default(),
            initial_freq_hz: 144_300_000,
            initial_mode: RigMode::USB,
            server_callsign: None,
            server_version: None,
            server_latitude: None,
            server_longitude: None,
        }
    }
}

/// Command context implementation for validation.
struct TaskCommandContext<'a> {
    machine: &'a RigStateMachine,
}

impl<'a> CommandContext for TaskCommandContext<'a> {
    fn state(&self) -> &RigMachineState {
        self.machine.state()
    }
}

/// Run the rig task with the new controller-based implementation.
pub async fn run_rig_task(
    config: RigTaskConfig,
    mut rx: mpsc::Receiver<RigRequest>,
    state_tx: watch::Sender<RigState>,
) -> DynResult<()> {
    info!("Opening rig backend {}", config.rig_model);
    match &config.access {
        RigAccess::Serial { path, baud } => info!("Serial: {} @ {} baud", path, baud),
        RigAccess::Tcp { addr } => info!("TCP CAT: {}", addr),
    }

    let mut rig: Box<dyn RigCat> = config.registry.build_rig(&config.rig_model, config.access)?;
    info!("Rig backend ready");

    // Initialize state machine and state
    let mut machine = RigStateMachine::new();
    let emitter = RigEventEmitter::new();
    let mut state = RigState::new_with_metadata(
        config.server_callsign.clone(),
        config.server_version.clone(),
        config.server_latitude,
        config.server_longitude,
        config.initial_freq_hz,
        config.initial_mode.clone(),
    );

    // Polling configuration
    let polling = &config.polling;
    let retry = &config.retry;
    let mut poll_pause_until: Option<Instant> = None;
    let mut last_power_on: Option<Instant> = None;
    let mut initial_status_read = false;

    // Initial setup: get rig info
    let rig_info = rig.info().clone();
    state.rig_info = Some(rig_info);
    if let Some(info) = state.rig_info.as_ref() {
        info!(
            "Rig info: {} {} {}",
            info.manufacturer, info.model, info.revision
        );
    }
    let old_machine_state = machine.state().clone();
    sync_machine_state(&mut machine, &state);
    let new_machine_state = machine.state().clone();
    emit_state_changes(
        &emitter,
        &state,
        &state,
        &old_machine_state,
        &new_machine_state,
    );
    let _ = state_tx.send(state.clone());

    // Initial power-on sequence
    if !state.control.enabled.unwrap_or(false) {
        info!("Sending initial PowerOn to wake rig");
        match rig.power_on().await {
            Ok(()) => {
                state.control.enabled = Some(true);
                time::sleep(Duration::from_secs(3)).await;
                if let Err(e) = refresh_state_with_retry(&mut rig, &mut state, retry).await {
                    warn!(
                        "Initial PowerOn refresh failed: {:?}; retrying once after short delay",
                        e
                    );
                    time::sleep(Duration::from_millis(500)).await;
                    if let Err(e2) = refresh_state_with_retry(&mut rig, &mut state, retry).await {
                        warn!(
                            "Initial PowerOn second refresh failed (continuing): {:?}",
                            e2
                        );
                    }
                } else {
                    initial_status_read = true;
                }
                info!("Rig initialized after power on sequence");
            }
            Err(e) => warn!("Initial PowerOn failed (continuing): {:?}", e),
        }
    }

    // Prime VFO state
    if let Err(e) = prime_vfo_state(&mut rig, &mut state, retry).await {
        warn!("VFO priming failed: {:?}", e);
    } else {
        initial_status_read = true;
    }

    if initial_status_read {
        let old_state = state.clone();
        if let Err(e) = apply_initial_tune(
            &mut rig,
            &mut state,
            retry,
            config.initial_freq_hz,
            &config.initial_mode,
        )
        .await
        {
            warn!("Initial tune failed (continuing): {:?}", e);
        } else {
            let old_machine_state = machine.state().clone();
            sync_machine_state(&mut machine, &state);
            let new_machine_state = machine.state().clone();
            emit_state_changes(
                &emitter,
                &old_state,
                &state,
                &old_machine_state,
                &new_machine_state,
            );
        }
    }

    state.initialized = true;
    let old_machine_state = machine.state().clone();
    sync_machine_state(&mut machine, &state);
    let new_machine_state = machine.state().clone();
    emit_state_changes(
        &emitter,
        &state,
        &state,
        &old_machine_state,
        &new_machine_state,
    );
    let _ = state_tx.send(state.clone());

    // Main task loop
    let mut current_poll_duration = polling.interval(state.status.tx_en);
    let mut poll_sleep: std::pin::Pin<Box<tokio::time::Sleep>> =
        Box::pin(tokio::time::sleep(current_poll_duration));
    loop {
        // Update sleep duration if tx_en state changed
        let new_duration = polling.interval(state.status.tx_en);
        if new_duration != current_poll_duration {
            current_poll_duration = new_duration;
            poll_sleep = Box::pin(tokio::time::sleep(current_poll_duration));
        }

        tokio::select! {
            _ = &mut poll_sleep => {
                poll_sleep = Box::pin(tokio::time::sleep(current_poll_duration));
                // Check if polling is paused
                if let Some(until) = poll_pause_until {
                    if Instant::now() < until {
                        continue;
                    } else {
                        poll_pause_until = None;
                    }
                }

                // Skip polling if rig is powered off
                if matches!(state.control.enabled, Some(false)) {
                    continue;
                }

                // Poll rig state
                let old_state = state.clone();
                match refresh_state_with_retry(&mut rig, &mut state, retry).await {
                    Ok(()) => {
                        let old_machine_state = machine.state().clone();
                        sync_machine_state(&mut machine, &state);
                        let new_machine_state = machine.state().clone();
                        emit_state_changes(
                            &emitter,
                            &old_state,
                            &state,
                            &old_machine_state,
                            &new_machine_state,
                        );
                        let _ = state_tx.send(state.clone());
                    }
                    Err(e) => {
                        error!("CAT polling error: {:?}", e);
                        // Grace period after power on
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

                // Batch up any pending requests
                let mut batch = vec![first_req];
                while let Ok(next) = rx.try_recv() {
                    batch.push(next);
                }

                // Process each request
                while let Some(RigRequest { cmd, respond_to }) = batch.pop() {
                    let cmd_label = format!("{:?}", cmd);
                    let started = Instant::now();

                    let mut cmd_ctx = CommandExecContext {
                        rig: &mut rig,
                        state: &mut state,
                        machine: &mut machine,
                        emitter: &emitter,
                        poll_pause_until: &mut poll_pause_until,
                        last_power_on: &mut last_power_on,
                        state_tx: &state_tx,
                        retry,
                    };
                    let result = process_command(cmd, &mut cmd_ctx).await;

                    let _ = respond_to.send(result);

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

/// Process a single rig command using command handlers.
struct CommandExecContext<'a> {
    rig: &'a mut Box<dyn RigCat>,
    state: &'a mut RigState,
    machine: &'a mut RigStateMachine,
    emitter: &'a RigEventEmitter,
    poll_pause_until: &'a mut Option<Instant>,
    last_power_on: &'a mut Option<Instant>,
    state_tx: &'a watch::Sender<RigState>,
    retry: &'a ExponentialBackoff,
}

async fn process_command(
    cmd: RigCommand,
    ctx: &mut CommandExecContext<'_>,
) -> RigResult<RigSnapshot> {
    // Handle decoder commands early — they don't touch the rig CAT.
    match cmd {
        RigCommand::SetAprsDecodeEnabled(en) => {
            ctx.state.aprs_decode_enabled = en;
            let _ = ctx.state_tx.send(ctx.state.clone());
            return snapshot_from(ctx.state);
        }
        RigCommand::SetCwDecodeEnabled(en) => {
            ctx.state.cw_decode_enabled = en;
            let _ = ctx.state_tx.send(ctx.state.clone());
            return snapshot_from(ctx.state);
        }
        RigCommand::SetCwAuto(en) => {
            ctx.state.cw_auto = en;
            let _ = ctx.state_tx.send(ctx.state.clone());
            return snapshot_from(ctx.state);
        }
        RigCommand::SetCwWpm(wpm) => {
            ctx.state.cw_wpm = wpm.clamp(5, 40);
            let _ = ctx.state_tx.send(ctx.state.clone());
            return snapshot_from(ctx.state);
        }
        RigCommand::SetCwToneHz(tone_hz) => {
            ctx.state.cw_tone_hz = tone_hz.clamp(300, 1200);
            let _ = ctx.state_tx.send(ctx.state.clone());
            return snapshot_from(ctx.state);
        }
        RigCommand::SetFt8DecodeEnabled(en) => {
            ctx.state.ft8_decode_enabled = en;
            info!("FT8 decode {}", if en { "enabled" } else { "disabled" });
            let _ = ctx.state_tx.send(ctx.state.clone());
            return snapshot_from(ctx.state);
        }
        RigCommand::ResetAprsDecoder => {
            audio::clear_aprs_history();
            ctx.state.aprs_decode_reset_seq += 1;
            let _ = ctx.state_tx.send(ctx.state.clone());
            return snapshot_from(ctx.state);
        }
        RigCommand::ResetCwDecoder => {
            ctx.state.cw_decode_reset_seq += 1;
            let _ = ctx.state_tx.send(ctx.state.clone());
            return snapshot_from(ctx.state);
        }
        RigCommand::ResetFt8Decoder => {
            audio::clear_ft8_history();
            ctx.state.ft8_decode_reset_seq += 1;
            let _ = ctx.state_tx.send(ctx.state.clone());
            return snapshot_from(ctx.state);
        }
        _ => {} // fall through to normal rig handler
    }

    sync_machine_state(ctx.machine, ctx.state);

    // Check if rig is ready for commands
    let not_ready =
        !ctx.state.initialized && !matches!(cmd, RigCommand::PowerOn | RigCommand::GetSnapshot);

    if not_ready {
        return Err(RigError::invalid_state("rig not initialized yet"));
    }

    // Get command handler and validate
    let handler = command_from_rig_command(cmd.clone());
    let ctx_view = TaskCommandContext {
        machine: ctx.machine,
    };

    match handler.can_execute(&ctx_view) {
        ValidationResult::Ok => {}
        ValidationResult::Locked => {
            warn!("{} blocked: panel lock is active", handler.name());
            return Err(RigError::invalid_state("panel is locked"));
        }
        ValidationResult::InvalidState(msg) => {
            warn!("{} blocked: {}", handler.name(), msg);
            return Err(RigError::invalid_state(msg));
        }
        ValidationResult::InvalidParams(msg) => {
            warn!("{} invalid params: {}", handler.name(), msg);
            return Err(RigError::invalid_state(msg));
        }
    }

    // Execute command
    let old_state = ctx.state.clone();
    let mut executor = RigCatExecutor::new(ctx.rig.as_mut());
    let result = handler.execute(&mut executor).await;

    match result {
        Ok(cmd_result) => {
            // Apply state updates based on command result
            match cmd_result {
                CommandResult::FreqUpdated(freq) => {
                    ctx.state.apply_freq(freq);
                    *ctx.poll_pause_until = Some(Instant::now() + Duration::from_millis(200));
                }
                CommandResult::ModeUpdated(mode) => {
                    ctx.state.apply_mode(mode);
                    *ctx.poll_pause_until = Some(Instant::now() + Duration::from_millis(200));
                }
                CommandResult::PttUpdated(ptt) => {
                    ctx.state.apply_ptt(ptt);
                }
                CommandResult::PowerUpdated(on) => {
                    ctx.state.control.enabled = Some(on);
                    if on {
                        time::sleep(Duration::from_secs(3)).await;
                        let now = Instant::now();
                        *ctx.poll_pause_until = Some(now + Duration::from_secs(3));
                        *ctx.last_power_on = Some(now);
                        // Refresh state after power on
                        if let Err(e) =
                            refresh_state_with_retry(ctx.rig, ctx.state, ctx.retry).await
                        {
                            if is_invalid_bcd_error(e.as_ref()) {
                                warn!("Transient CAT decode after PowerOn (ignored): {:?}", e);
                                *ctx.poll_pause_until =
                                    Some(Instant::now() + Duration::from_millis(1500));
                            } else {
                                error!("Failed to refresh after PowerOn: {:?}", e);
                                return Err(RigError::communication(format!("CAT error: {}", e)));
                            }
                        }
                    } else {
                        ctx.state.status.tx_en = false;
                    }
                }
                CommandResult::LockUpdated(locked) => {
                    ctx.state.control.lock = Some(locked);
                    ctx.state.status.lock = Some(locked);
                }
                CommandResult::TxLimitUpdated(limit) => {
                    ctx.state
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
                CommandResult::RefreshRequired => {
                    // For commands like ToggleVfo, GetSnapshot
                    if matches!(cmd, RigCommand::ToggleVfo) {
                        time::sleep(Duration::from_millis(150)).await;
                        *ctx.poll_pause_until = Some(Instant::now() + Duration::from_millis(300));
                    }
                    if let Err(e) = refresh_state_with_retry(ctx.rig, ctx.state, ctx.retry).await {
                        error!("Failed to refresh state: {:?}", e);
                        return Err(RigError::communication(format!("CAT error: {}", e)));
                    }
                }
                CommandResult::Ok => {}
            }

            let old_machine_state = ctx.machine.state().clone();
            sync_machine_state(ctx.machine, ctx.state);
            let new_machine_state = ctx.machine.state().clone();
            emit_state_changes(
                ctx.emitter,
                &old_state,
                ctx.state,
                &old_machine_state,
                &new_machine_state,
            );
            let _ = ctx.state_tx.send(ctx.state.clone());
            snapshot_from(ctx.state)
        }
        Err(e) => {
            error!("Command {} failed: {:?}", handler.name(), e);
            Err(RigError::communication(format!("CAT error: {}", e)))
        }
    }
}

/// Refresh state from CAT with retry logic using the retry policy.
async fn refresh_state_with_retry(
    rig: &mut Box<dyn RigCat>,
    state: &mut RigState,
    retry: &ExponentialBackoff,
) -> DynResult<()> {
    let mut last_err: Option<Box<dyn std::error::Error + Send + Sync>> = None;
    let max = retry.max_attempts() as usize;

    for attempt in 0..max {
        match refresh_state_from_cat(rig, state).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                let rig_err = RigError::communication(e.to_string());
                if retry.should_retry(attempt as u32, &rig_err) && attempt + 1 < max {
                    let delay = retry.delay(attempt as u32);
                    warn!(
                        "Retrying CAT state read (attempt {} of {}, delay {:?})",
                        attempt + 1,
                        max,
                        delay
                    );
                    time::sleep(delay).await;
                    last_err = Some(e);
                    continue;
                } else {
                    return Err(e);
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| "Unknown CAT error".into()))
}

/// Read current state from the rig via CAT.
async fn refresh_state_from_cat(rig: &mut Box<dyn RigCat>, state: &mut RigState) -> DynResult<()> {
    let (freq, mode, vfo) = rig.get_status().await?;
    state.control.enabled = Some(true);
    state.apply_freq(freq);
    state.apply_mode(mode);
    state.status.vfo = vfo;

    if state.status.tx_en {
        state.status.rx.get_or_insert(RigRxStatus { sig: None }).sig = Some(0);
    } else if let Ok(meter) = rig.get_signal_strength().await {
        let sig = map_signal_strength(&state.status.mode, meter);
        state.status.rx.get_or_insert(RigRxStatus { sig: None }).sig = Some(sig);
    }

    if let Ok(limit) = rig.get_tx_limit().await {
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
        if let Ok(power) = rig.get_tx_power().await {
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

/// Apply initial mode/frequency after a successful CAT status read.
async fn apply_initial_tune(
    rig: &mut Box<dyn RigCat>,
    state: &mut RigState,
    retry: &ExponentialBackoff,
    initial_freq_hz: u64,
    initial_mode: &RigMode,
) -> DynResult<()> {
    let needs_freq = state.status.freq.hz != initial_freq_hz;
    let needs_mode = &state.status.mode != initial_mode;

    if !needs_freq && !needs_mode {
        return Ok(());
    }

    if needs_mode {
        rig.set_mode(initial_mode.clone()).await?;
    }
    if needs_freq {
        rig.set_freq(Freq {
            hz: initial_freq_hz,
        })
        .await?;
    }

    refresh_state_with_retry(rig, state, retry).await
}

/// Prime VFO state by toggling and reading both VFOs.
async fn prime_vfo_state(
    rig: &mut Box<dyn RigCat>,
    state: &mut RigState,
    retry: &ExponentialBackoff,
) -> DynResult<()> {
    // Ensure panel is unlocked
    let _ = rig.unlock().await;
    time::sleep(Duration::from_millis(100)).await;

    refresh_state_with_retry(rig, state, retry).await?;
    time::sleep(Duration::from_millis(150)).await;

    rig.toggle_vfo().await?;
    time::sleep(Duration::from_millis(150)).await;
    refresh_state_with_retry(rig, state, retry).await?;

    rig.toggle_vfo().await?;
    time::sleep(Duration::from_millis(150)).await;
    refresh_state_with_retry(rig, state, retry).await?;

    Ok(())
}

/// Map raw signal strength to S-meter value based on mode.
fn map_signal_strength(mode: &RigMode, raw: u8) -> i32 {
    // FT-817 returns 0-15 for signal strength
    // Map to approximate dBm / S-units
    match mode {
        RigMode::FM | RigMode::WFM => -120 + (raw as i32 * 6),
        _ => -127 + (raw as i32 * 6),
    }
}

/// Create a snapshot from current state.
fn snapshot_from(state: &RigState) -> RigResult<RigSnapshot> {
    state
        .snapshot()
        .ok_or_else(|| RigError::invalid_state("Rig info unavailable"))
}

fn sync_machine_state(machine: &mut RigStateMachine, state: &RigState) {
    let desired = desired_machine_state(state);
    match (machine.state().clone(), &desired) {
        (RigMachineState::Ready(_), RigMachineState::Ready(new_data)) => {
            machine.update_ready_data(|data| {
                *data = new_data.clone();
            });
        }
        (RigMachineState::Transmitting(_), RigMachineState::Transmitting(new_data)) => {
            machine.update_transmitting_data(|data| {
                *data = new_data.clone();
            });
        }
        _ => {
            machine.set_state(desired);
        }
    }
}

fn desired_machine_state(state: &RigState) -> RigMachineState {
    let rig_info = state.rig_info.clone();
    if !state.initialized {
        return rig_info
            .map(|info| RigMachineState::Initializing {
                rig_info: Some(info),
            })
            .unwrap_or(RigMachineState::Disconnected);
    }

    let Some(info) = rig_info else {
        return RigMachineState::Disconnected;
    };

    if matches!(state.control.enabled, Some(false)) {
        return RigMachineState::PoweredOff { rig_info: info };
    }

    if state.status.tx_en {
        RigMachineState::Transmitting(transmitting_data_from_state(state, info))
    } else {
        RigMachineState::Ready(ready_data_from_state(state, info))
    }
}

fn ready_data_from_state(state: &RigState, rig_info: trx_core::rig::RigInfo) -> ReadyStateData {
    ReadyStateData {
        rig_info,
        freq: state.status.freq,
        mode: state.status.mode.clone(),
        vfo: state.status.vfo.clone(),
        rx: state.status.rx.clone(),
        tx_limit: state.status.tx.as_ref().and_then(|tx| tx.limit),
        locked: lock_state_from(state),
    }
}

fn transmitting_data_from_state(
    state: &RigState,
    rig_info: trx_core::rig::RigInfo,
) -> TransmittingStateData {
    TransmittingStateData {
        rig_info,
        freq: state.status.freq,
        mode: state.status.mode.clone(),
        vfo: state.status.vfo.clone(),
        tx: state.status.tx.clone(),
        locked: lock_state_from(state),
    }
}

fn lock_state_from(state: &RigState) -> bool {
    state.control.lock.or(state.status.lock).unwrap_or(false)
}

fn emit_state_changes(
    emitter: &RigEventEmitter,
    old_state: &RigState,
    new_state: &RigState,
    old_machine_state: &RigMachineState,
    new_machine_state: &RigMachineState,
) {
    if old_state.status.freq.hz != new_state.status.freq.hz {
        emitter.notify_frequency_change(Some(old_state.status.freq), new_state.status.freq);
    }

    if old_state.status.mode != new_state.status.mode {
        emitter.notify_mode_change(Some(&old_state.status.mode), &new_state.status.mode);
    }

    if old_state.status.tx_en != new_state.status.tx_en {
        emitter.notify_ptt_change(new_state.status.tx_en);
    }

    if lock_state_from(old_state) != lock_state_from(new_state) {
        emitter.notify_lock_change(lock_state_from(new_state));
    }

    if old_state.control.enabled.unwrap_or(false) != new_state.control.enabled.unwrap_or(false) {
        emitter.notify_power_change(new_state.control.enabled.unwrap_or(false));
    }

    if meters_changed(old_state, new_state) {
        emitter.notify_meter_update(new_state.status.rx.as_ref(), new_state.status.tx.as_ref());
    }

    if std::mem::discriminant(old_machine_state) != std::mem::discriminant(new_machine_state) {
        emitter.notify_state_change(old_machine_state, new_machine_state);
    }
}

fn meters_changed(old_state: &RigState, new_state: &RigState) -> bool {
    let old_rx_sig = old_state.status.rx.as_ref().and_then(|rx| rx.sig);
    let new_rx_sig = new_state.status.rx.as_ref().and_then(|rx| rx.sig);
    if old_rx_sig != new_rx_sig {
        return true;
    }

    let (old_tx_power, old_tx_limit, old_tx_swr, old_tx_alc) =
        tx_meter_parts(old_state.status.tx.as_ref());
    let (new_tx_power, new_tx_limit, new_tx_swr, new_tx_alc) =
        tx_meter_parts(new_state.status.tx.as_ref());

    old_tx_power != new_tx_power
        || old_tx_limit != new_tx_limit
        || old_tx_swr != new_tx_swr
        || old_tx_alc != new_tx_alc
}

fn tx_meter_parts(tx: Option<&RigTxStatus>) -> (Option<u8>, Option<u8>, Option<f32>, Option<u8>) {
    tx.map(|tx| (tx.power, tx.limit, tx.swr, tx.alc))
        .unwrap_or((None, None, None, None))
}
