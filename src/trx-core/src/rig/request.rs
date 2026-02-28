// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use tokio::sync::oneshot;

use crate::{RigCommand, RigResult, RigSnapshot};

/// Request sent to the rig task.
#[derive(Debug)]
pub struct RigRequest {
    pub cmd: RigCommand,
    pub respond_to: oneshot::Sender<RigResult<RigSnapshot>>,
    /// When set, the remote client routes this request to the specified rig
    /// instead of the globally selected rig. Used for per-rig rigctl listeners.
    pub rig_id_override: Option<String>,
}
