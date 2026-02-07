// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Platform-agnostic rig state model.
//!
//! This struct holds the display-ready state derived from `RigState`.
//! The UI layer reads from this model; command sending is handled
//! separately via an `mpsc::Sender<RigRequest>`.

use trx_core::rig::state::RigState;

use crate::helpers::{format_freq, mode_label, vfo_label};

/// Display-ready rig state. Updated from `RigState` on each change.
#[derive(Debug, Clone)]
pub struct RigStateModel {
    pub freq_hz: u64,
    pub freq_text: String,
    pub mode: String,
    pub band: String,
    pub tx_enabled: bool,
    pub locked: bool,
    pub powered: bool,
    pub rx_sig: i32,
    pub tx_power: i32,
    pub tx_limit: i32,
    pub tx_swr: f64,
    pub tx_alc: i32,
    pub vfo: String,
}

impl Default for RigStateModel {
    fn default() -> Self {
        Self {
            freq_hz: 0,
            freq_text: "-- Hz".to_string(),
            mode: "--".to_string(),
            band: "--".to_string(),
            tx_enabled: false,
            locked: false,
            powered: false,
            rx_sig: 0,
            tx_power: 0,
            tx_limit: 0,
            tx_swr: 0.0,
            tx_alc: 0,
            vfo: "--".to_string(),
        }
    }
}

impl RigStateModel {
    /// Update all fields from a `RigState` snapshot. Returns `true` if anything changed.
    pub fn update(&mut self, state: &RigState) -> bool {
        let mut changed = false;

        let freq_hz = state.status.freq.hz;
        if self.freq_hz != freq_hz {
            self.freq_hz = freq_hz;
            self.freq_text = format_freq(freq_hz);
            changed = true;
        }

        let mode = mode_label(&state.status.mode);
        if self.mode != mode {
            self.mode = mode;
            changed = true;
        }

        let band = state.band_name().unwrap_or_else(|| "--".to_string());
        if self.band != band {
            self.band = band;
            changed = true;
        }

        if self.tx_enabled != state.status.tx_en {
            self.tx_enabled = state.status.tx_en;
            changed = true;
        }

        let locked = state.status.lock.unwrap_or(false);
        if self.locked != locked {
            self.locked = locked;
            changed = true;
        }

        let powered = state.control.enabled.unwrap_or(false);
        if self.powered != powered {
            self.powered = powered;
            changed = true;
        }

        let rx_sig = state
            .status
            .rx
            .as_ref()
            .and_then(|rx| rx.sig)
            .unwrap_or(0);
        if self.rx_sig != rx_sig {
            self.rx_sig = rx_sig;
            changed = true;
        }

        let tx_power = state
            .status
            .tx
            .as_ref()
            .and_then(|tx| tx.power)
            .map(i32::from)
            .unwrap_or(0);
        if self.tx_power != tx_power {
            self.tx_power = tx_power;
            changed = true;
        }

        let tx_limit = state
            .status
            .tx
            .as_ref()
            .and_then(|tx| tx.limit)
            .map(i32::from)
            .unwrap_or(0);
        if self.tx_limit != tx_limit {
            self.tx_limit = tx_limit;
            changed = true;
        }

        let tx_swr = state
            .status
            .tx
            .as_ref()
            .and_then(|tx| tx.swr)
            .unwrap_or(0.0) as f64;
        if (self.tx_swr - tx_swr).abs() > f64::EPSILON {
            self.tx_swr = tx_swr;
            changed = true;
        }

        let tx_alc = state
            .status
            .tx
            .as_ref()
            .and_then(|tx| tx.alc)
            .map(i32::from)
            .unwrap_or(0);
        if self.tx_alc != tx_alc {
            self.tx_alc = tx_alc;
            changed = true;
        }

        let vfo = vfo_label(state);
        if self.vfo != vfo {
            self.vfo = vfo;
            changed = true;
        }

        changed
    }
}
