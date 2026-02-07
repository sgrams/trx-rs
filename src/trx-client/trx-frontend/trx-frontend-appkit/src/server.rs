// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! AppKit frontend spawner.
//!
//! Spawns a dedicated thread for the NSApplication run loop and an async
//! task that watches for rig state changes and pushes them to the UI
//! thread via a std::sync::mpsc channel.

use std::net::SocketAddr;

use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use trx_core::rig::command::RigCommand;
use trx_core::{RigRequest, RigState};
use trx_frontend::FrontendSpawner;

use crate::model::RigStateModel;
use crate::ui::{self, ButtonAction, UiElements};

/// AppKit frontend implementation.
pub struct AppKitFrontend;

impl FrontendSpawner for AppKitFrontend {
    fn spawn_frontend(
        state_rx: watch::Receiver<RigState>,
        _rig_tx: mpsc::Sender<RigRequest>,
        _callsign: Option<String>,
        listen_addr: SocketAddr,
    ) -> JoinHandle<()> {
        let (state_update_tx, _state_update_rx) = std::sync::mpsc::channel::<RigState>();

        // Spawn async state watcher that forwards state changes.
        // The actual AppKit event loop is driven by `run_appkit_main_thread`
        // called from main() on the process main thread.
        tokio::spawn(async move {
            info!("AppKit frontend starting (addr hint: {})", listen_addr);
            run_state_watcher(state_rx, state_update_tx).await;
        })
    }
}

/// Run the AppKit event loop on the calling thread (must be the process main
/// thread, i.e. thread 0). This function **blocks forever**.
///
/// It creates the NSApplication, builds the UI window, and enters a polling
/// loop that drains AppKit events, applies rig state updates, and dispatches
/// button actions.
pub fn run_appkit_main_thread(
    state_rx: watch::Receiver<RigState>,
    rig_tx: mpsc::Sender<RigRequest>,
) {
    // Channel for state updates: async watcher -> main thread.
    let (state_update_tx, state_update_rx) = std::sync::mpsc::channel::<RigState>();

    // Channel for button actions: UI buttons -> main thread loop.
    let (action_tx, action_rx) = std::sync::mpsc::channel::<ButtonAction>();

    // Spawn async state watcher onto the tokio runtime (running on a
    // background thread).
    tokio::spawn(async move {
        run_state_watcher(state_rx, state_update_tx).await;
    });

    let mtm = MainThreadMarker::new()
        .expect("run_appkit_main_thread must be called from the process main thread");

    let app = NSApplication::sharedApplication(mtm);

    let (window, ui_elements) = ui::build_window(mtm, action_tx);

    // Keep window alive for the process lifetime.
    std::mem::forget(window);

    let mut model = RigStateModel::default();

    info!("AppKit frontend: entering main run loop");

    // Run a polling loop instead of NSApplication::run() so we can
    // process state updates and button actions between event cycles.
    loop {
        // Process pending AppKit events.
        drain_appkit_events(&app);

        // Process state updates from the async watcher.
        while let Ok(state) = state_update_rx.try_recv() {
            if model.update(&state) {
                ui_elements.refresh(&model);
            }
        }

        // Process button actions.
        while let Ok(action) = action_rx.try_recv() {
            handle_action(action, &ui_elements, &rig_tx, &model);
        }

        // Sleep briefly to avoid busy-waiting.
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

fn drain_appkit_events(app: &NSApplication) {
    use objc2_app_kit::NSEventMask;
    use objc2_foundation::NSDate;

    loop {
        let event = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&NSDate::distantPast()),
                objc2_foundation::NSDefaultRunLoopMode,
                true,
            )
        };
        match event {
            Some(event) => {
                app.sendEvent(&event);
            }
            None => break,
        }
    }
}

fn handle_action(
    action: ButtonAction,
    ui: &UiElements,
    rig_tx: &mpsc::Sender<RigRequest>,
    model: &RigStateModel,
) {
    match action {
        ButtonAction::TogglePtt => {
            send_command(rig_tx, RigCommand::SetPtt(!model.tx_enabled));
        }
        ButtonAction::TogglePower => {
            if model.powered {
                send_command(rig_tx, RigCommand::PowerOff);
            } else {
                send_command(rig_tx, RigCommand::PowerOn);
            }
        }
        ButtonAction::ToggleVfo => {
            send_command(rig_tx, RigCommand::ToggleVfo);
        }
        ButtonAction::ToggleLock => {
            if model.locked {
                send_command(rig_tx, RigCommand::Unlock);
            } else {
                send_command(rig_tx, RigCommand::Lock);
            }
        }
        ButtonAction::SetFreq => {
            ui.handle_set_freq(rig_tx);
        }
        ButtonAction::SetMode => {
            ui.handle_set_mode(rig_tx);
        }
        ButtonAction::SetTxLimit => {
            ui.handle_set_tx_limit(rig_tx);
        }
    }
}

fn send_command(tx: &mpsc::Sender<RigRequest>, cmd: RigCommand) {
    let (resp_tx, _resp_rx) = tokio::sync::oneshot::channel();
    if tx
        .blocking_send(RigRequest {
            cmd,
            respond_to: resp_tx,
        })
        .is_err()
    {
        warn!("AppKit frontend: rig command send failed");
    }
}

async fn run_state_watcher(
    mut state_rx: watch::Receiver<RigState>,
    state_update_tx: std::sync::mpsc::Sender<RigState>,
) {
    // Send initial state.
    let _ = state_update_tx.send(state_rx.borrow().clone());

    while state_rx.changed().await.is_ok() {
        let state = state_rx.borrow().clone();
        if state_update_tx.send(state).is_err() {
            warn!("AppKit frontend: state update channel closed");
            break;
        }
    }
}
