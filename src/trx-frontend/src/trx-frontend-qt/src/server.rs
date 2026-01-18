// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::cell::RefCell;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread;

use qmetaobject::{
    qt_base_class, qt_method, qt_property, qt_signal, queued_callback, QObject, QObjectPinned,
    QString, QmlEngine,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use trx_core::rig::command::RigCommand;
use trx_core::rig::state::RigMode;
use trx_core::{RigRequest, RigState};
use trx_frontend::FrontendSpawner;

/// Qt/QML frontend (Linux-only).
pub struct QtFrontend;

impl FrontendSpawner for QtFrontend {
    fn spawn_frontend(
        state_rx: watch::Receiver<RigState>,
        rig_tx: mpsc::Sender<RigRequest>,
        _callsign: Option<String>,
        listen_addr: SocketAddr,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let (update_tx, update_rx) = oneshot::channel::<Box<dyn Fn(RigState) + Send + Sync>>();

            spawn_qt_thread(update_tx, listen_addr, rig_tx);
            spawn_state_watcher(state_rx, update_rx).await;
        })
    }
}

fn spawn_qt_thread(
    update_tx: oneshot::Sender<Box<dyn Fn(RigState) + Send + Sync>>,
    listen_addr: SocketAddr,
    rig_tx: mpsc::Sender<RigRequest>,
) {
    thread::spawn(move || {
        let model_cell = Box::leak(Box::new(RefCell::new(RigStateModel::default())));
        let model_ptr = model_cell.as_ptr();
        model_cell.borrow_mut().rig_tx = Some(rig_tx);

        let update = queued_callback(move |state: RigState| unsafe {
            // Safe as queued_callback executes on the Qt thread where the model lives.
            let model_cell = &mut *model_ptr;
            update_model(model_cell, &state);
        });

        if update_tx.send(Box::new(update)).is_err() {
            warn!("Qt frontend update channel dropped before init");
        }

        let mut engine = QmlEngine::new();
        engine.set_object_property("rig".into(), unsafe { QObjectPinned::new(model_cell) });

        let qml_path = qml_main_path();
        info!("Qt frontend loading QML from {}", qml_path.display());
        engine.load_file(QString::from(qml_path.to_string_lossy().to_string()));
        info!("Qt frontend running (addr hint: {})", listen_addr);
        engine.exec();
    });
}

async fn spawn_state_watcher(
    mut state_rx: watch::Receiver<RigState>,
    update_rx: oneshot::Receiver<Box<dyn Fn(RigState) + Send + Sync>>,
) {
    let Ok(update) = update_rx.await else {
        warn!("Qt frontend update channel closed");
        return;
    };

    update(state_rx.borrow().clone());
    while state_rx.changed().await.is_ok() {
        update(state_rx.borrow().clone());
    }
}

fn qml_main_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("qml")
        .join("Main.qml")
}

#[derive(QObject, Default)]
struct RigStateModel {
    base: qt_base_class!(trait QObject),
    rig_tx: Option<mpsc::Sender<RigRequest>>,
    freq_hz: qt_property!(u64; NOTIFY freq_hz_changed),
    freq_hz_changed: qt_signal!(),
    freq_text: qt_property!(QString; NOTIFY freq_text_changed),
    freq_text_changed: qt_signal!(),
    mode: qt_property!(QString; NOTIFY mode_changed),
    mode_changed: qt_signal!(),
    band: qt_property!(QString; NOTIFY band_changed),
    band_changed: qt_signal!(),
    tx_enabled: qt_property!(bool; NOTIFY tx_enabled_changed),
    tx_enabled_changed: qt_signal!(),
    locked: qt_property!(bool; NOTIFY locked_changed),
    locked_changed: qt_signal!(),
    powered: qt_property!(bool; NOTIFY powered_changed),
    powered_changed: qt_signal!(),
    rx_sig: qt_property!(i32; NOTIFY rx_sig_changed),
    rx_sig_changed: qt_signal!(),
    tx_power: qt_property!(i32; NOTIFY tx_power_changed),
    tx_power_changed: qt_signal!(),
    tx_limit: qt_property!(i32; NOTIFY tx_limit_changed),
    tx_limit_changed: qt_signal!(),
    tx_swr: qt_property!(f64; NOTIFY tx_swr_changed),
    tx_swr_changed: qt_signal!(),
    tx_alc: qt_property!(i32; NOTIFY tx_alc_changed),
    tx_alc_changed: qt_signal!(),
    vfo: qt_property!(QString; NOTIFY vfo_changed),
    vfo_changed: qt_signal!(),
    set_freq_hz: qt_method!(
        fn set_freq_hz(&self, hz: i64) {
            if hz <= 0 {
                return;
            }
            self.send_command(RigCommand::SetFreq(trx_core::radio::freq::Freq {
                hz: hz as u64,
            }));
        }
    ),
    set_mode: qt_method!(
        fn set_mode(&self, mode: QString) {
            let mode = parse_mode(&mode.to_string());
            self.send_command(RigCommand::SetMode(mode));
        }
    ),
    toggle_ptt: qt_method!(
        fn toggle_ptt(&self) {
            self.send_command(RigCommand::SetPtt(!self.tx_enabled));
        }
    ),
    toggle_power: qt_method!(
        fn toggle_power(&self) {
            if self.powered {
                self.send_command(RigCommand::PowerOff);
            } else {
                self.send_command(RigCommand::PowerOn);
            }
        }
    ),
    toggle_vfo: qt_method!(
        fn toggle_vfo(&self) {
            self.send_command(RigCommand::ToggleVfo);
        }
    ),
    lock_panel: qt_method!(
        fn lock_panel(&self) {
            self.send_command(RigCommand::Lock);
        }
    ),
    unlock_panel: qt_method!(
        fn unlock_panel(&self) {
            self.send_command(RigCommand::Unlock);
        }
    ),
    set_tx_limit: qt_method!(
        fn set_tx_limit(&self, limit: i32) {
            if limit < 0 {
                return;
            }
            self.send_command(RigCommand::SetTxLimit(limit as u8));
        }
    ),
}

impl RigStateModel {
    fn send_command(&self, cmd: RigCommand) {
        let Some(tx) = self.rig_tx.as_ref() else {
            warn!("Qt frontend: rig command dropped (channel not set)");
            return;
        };

        let (resp_tx, _resp_rx) = oneshot::channel();
        if tx
            .blocking_send(RigRequest {
                cmd,
                respond_to: resp_tx,
            })
            .is_err()
        {
            warn!("Qt frontend: rig command send failed");
        }
    }
}

fn update_model(model: &mut RigStateModel, state: &RigState) {
    let freq_hz = state.status.freq.hz;
    if model.freq_hz != freq_hz {
        model.freq_hz = freq_hz;
        model.freq_hz_changed();
    }

    let freq_text = QString::from(format_freq(freq_hz));
    if model.freq_text != freq_text {
        model.freq_text = freq_text;
        model.freq_text_changed();
    }

    let mode = QString::from(mode_label(&state.status.mode));
    if model.mode != mode {
        model.mode = mode;
        model.mode_changed();
    }

    let band = QString::from(state.band_name().unwrap_or_else(|| "--".to_string()));
    if model.band != band {
        model.band = band;
        model.band_changed();
    }

    if model.tx_enabled != state.status.tx_en {
        model.tx_enabled = state.status.tx_en;
        model.tx_enabled_changed();
    }

    let locked = state.status.lock.unwrap_or(false);
    if model.locked != locked {
        model.locked = locked;
        model.locked_changed();
    }

    let powered = state.control.enabled.unwrap_or(false);
    if model.powered != powered {
        model.powered = powered;
        model.powered_changed();
    }

    let rx_sig = state.status.rx.as_ref().and_then(|rx| rx.sig).unwrap_or(0);
    if model.rx_sig != rx_sig {
        model.rx_sig = rx_sig;
        model.rx_sig_changed();
    }

    let tx_power = state
        .status
        .tx
        .as_ref()
        .and_then(|tx| tx.power)
        .map(i32::from)
        .unwrap_or(0);
    if model.tx_power != tx_power {
        model.tx_power = tx_power;
        model.tx_power_changed();
    }

    let tx_limit = state
        .status
        .tx
        .as_ref()
        .and_then(|tx| tx.limit)
        .map(i32::from)
        .unwrap_or(0);
    if model.tx_limit != tx_limit {
        model.tx_limit = tx_limit;
        model.tx_limit_changed();
    }

    let tx_swr = state
        .status
        .tx
        .as_ref()
        .and_then(|tx| tx.swr)
        .unwrap_or(0.0) as f64;
    if (model.tx_swr - tx_swr).abs() > f64::EPSILON {
        model.tx_swr = tx_swr;
        model.tx_swr_changed();
    }

    let tx_alc = state
        .status
        .tx
        .as_ref()
        .and_then(|tx| tx.alc)
        .map(i32::from)
        .unwrap_or(0);
    if model.tx_alc != tx_alc {
        model.tx_alc = tx_alc;
        model.tx_alc_changed();
    }

    let vfo = QString::from(vfo_label(state));
    if model.vfo != vfo {
        model.vfo = vfo;
        model.vfo_changed();
    }
}

fn format_freq(hz: u64) -> String {
    if hz >= 1_000_000_000 {
        format!("{:.3} GHz", hz as f64 / 1_000_000_000.0)
    } else if hz >= 10_000_000 {
        format!("{:.3} MHz", hz as f64 / 1_000_000.0)
    } else if hz >= 1_000 {
        format!("{:.1} kHz", hz as f64 / 1_000.0)
    } else {
        format!("{hz} Hz")
    }
}

fn mode_label(mode: &RigMode) -> String {
    match mode {
        RigMode::LSB => "LSB".to_string(),
        RigMode::USB => "USB".to_string(),
        RigMode::CW => "CW".to_string(),
        RigMode::CWR => "CWR".to_string(),
        RigMode::AM => "AM".to_string(),
        RigMode::WFM => "WFM".to_string(),
        RigMode::FM => "FM".to_string(),
        RigMode::DIG => "DIG".to_string(),
        RigMode::PKT => "PKT".to_string(),
        RigMode::Other(val) => val.clone(),
    }
}

fn parse_mode(value: &str) -> RigMode {
    match value.trim().to_uppercase().as_str() {
        "LSB" => RigMode::LSB,
        "USB" => RigMode::USB,
        "CW" => RigMode::CW,
        "CWR" => RigMode::CWR,
        "AM" => RigMode::AM,
        "FM" => RigMode::FM,
        "WFM" => RigMode::WFM,
        "DIG" | "DIGI" => RigMode::DIG,
        "PKT" | "PACKET" => RigMode::PKT,
        other => RigMode::Other(other.to_string()),
    }
}

fn vfo_label(state: &RigState) -> String {
    let Some(vfo) = state.status.vfo.as_ref() else {
        return "--".to_string();
    };

    let mut lines = Vec::new();
    for (idx, entry) in vfo.entries.iter().enumerate() {
        let marker = if vfo.active == Some(idx) { "*" } else { " " };
        let freq = format_freq(entry.freq.hz);
        let mode = entry
            .mode
            .as_ref()
            .map(mode_label)
            .unwrap_or_else(|| "--".to_string());
        lines.push(format!("{marker} {}: {} {}", entry.name, freq, mode));
    }
    lines.join("\\n")
}
