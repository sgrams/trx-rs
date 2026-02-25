// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Thin handle giving the listener access to one rig's task and state.

use tokio::sync::{mpsc, watch};

use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;

/// A handle to a single running rig backend.
///
/// One `RigHandle` is created per rig in `main.rs` and stored in the shared
/// `Arc<HashMap<String, RigHandle>>` passed to the listener.
pub struct RigHandle {
    /// Stable rig identifier, matches the key in the HashMap.
    pub rig_id: String,
    /// Send commands to the rig task.
    pub rig_tx: mpsc::Sender<RigRequest>,
    /// Watch the latest rig state for fast GetState/GetRigs responses.
    pub state_rx: watch::Receiver<RigState>,
}
