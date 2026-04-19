// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Thin handle giving the listener access to one rig's task and state.

use tokio::sync::{broadcast, mpsc, watch};

use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_protocol::MeterUpdate;

/// Bounded broadcast capacity for the meter stream.  Keeps ~0.5 s of buffered
/// samples at 30 Hz — more than enough slack to tolerate a scheduling blip
/// without forcing the producer to block or drop silently.
pub const METER_BROADCAST_CAPACITY: usize = 16;

/// A handle to a single running rig backend.
///
/// One `RigHandle` is created per rig in `main.rs` and stored in the shared
/// `Arc<HashMap<String, RigHandle>>` passed to the listener.
pub struct RigHandle {
    /// Stable rig identifier, matches the key in the HashMap.
    pub rig_id: String,
    /// Display name for the rig (from config, or rig_id if not set).
    pub display_name: String,
    /// Send commands to the rig task.
    pub rig_tx: mpsc::Sender<RigRequest>,
    /// Watch the latest rig state for fast GetState/GetRigs responses.
    pub state_rx: watch::Receiver<RigState>,
    /// Per-rig audio listener TCP port.
    pub audio_port: u16,
    /// Fast per-rig meter samples published by `rig_task` at ~30 Hz (SDR) or
    /// ~6–7 Hz (CAT).  Consumed by `SubscribeMeter` clients; independent of
    /// the slower `state_rx` snapshot path.
    pub meter_tx: broadcast::Sender<MeterUpdate>,
}
