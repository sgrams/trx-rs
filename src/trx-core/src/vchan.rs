// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Virtual channel management trait and shared types.
//!
//! A *virtual channel* is an independent DSP slice within the capture bandwidth
//! of an SDR rig.  Each has its own frequency offset, demodulation mode, and
//! PCM audio broadcast.  Traditional (non-SDR) rigs do not support virtual
//! channels; their `RigHandle::vchan_manager` field will be `None`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::rig::state::RigMode;

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Snapshot of one virtual channel's state (HTTP-serialisable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VChannelInfo {
    /// Stable UUID identifier.
    pub id: Uuid,
    /// Display index in the ordered channel list (0 = primary).
    pub index: usize,
    /// Dial frequency in Hz.
    pub freq_hz: u64,
    /// Demodulation mode name (e.g. "USB", "FM").
    pub mode: String,
    /// `true` for the primary channel (index 0), which cannot be removed.
    pub permanent: bool,
}

/// Errors returned by virtual channel management operations.
#[derive(Debug, Clone)]
pub enum VChanError {
    /// The configured channel cap would be exceeded.
    CapReached { max: usize },
    /// The requested frequency lies outside the current SDR capture bandwidth.
    OutOfBandwidth { half_span_hz: i64 },
    /// No channel with the given UUID exists.
    NotFound,
    /// Attempted to remove the permanent primary channel.
    Permanent,
}

impl std::fmt::Display for VChanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VChanError::CapReached { max } => {
                write!(f, "virtual channel cap reached (max {})", max)
            }
            VChanError::OutOfBandwidth { half_span_hz } => write!(
                f,
                "frequency outside SDR capture bandwidth (±{} Hz)",
                half_span_hz
            ),
            VChanError::NotFound => write!(f, "virtual channel not found"),
            VChanError::Permanent => write!(f, "cannot remove the primary channel"),
        }
    }
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Manages virtual DSP channels for an SDR rig.
///
/// Implementations are `Send + Sync` so the manager can be shared across
/// tokio tasks and actix-web handlers.
pub trait VirtualChannelManager: Send + Sync {
    /// Add a new virtual channel tuned to `freq_hz` with `mode`.
    ///
    /// Returns the new channel UUID and a PCM broadcast receiver that delivers
    /// decoded audio frames for this channel.
    fn add_channel(
        &self,
        freq_hz: u64,
        mode: &RigMode,
    ) -> Result<(Uuid, broadcast::Receiver<Vec<f32>>), VChanError>;

    /// Remove a virtual channel by UUID.  The primary channel (index 0) cannot
    /// be removed and returns `VChanError::Permanent`.
    fn remove_channel(&self, id: Uuid) -> Result<(), VChanError>;

    /// Update the dial frequency of an existing channel.
    fn set_channel_freq(&self, id: Uuid, freq_hz: u64) -> Result<(), VChanError>;

    /// Update the demodulation mode of an existing channel.
    fn set_channel_mode(&self, id: Uuid, mode: &RigMode) -> Result<(), VChanError>;

    /// Subscribe to decoded PCM audio from a channel.
    /// Returns `None` if the channel UUID does not exist.
    fn subscribe_pcm(&self, id: Uuid) -> Option<broadcast::Receiver<Vec<f32>>>;

    /// Return a PCM receiver for an existing channel, or create a new channel
    /// with the given `id`, `freq_hz`, and `mode` and subscribe to it.
    ///
    /// Used by the audio-TCP server path where the client provides a stable UUID
    /// (generated on the client side) so that both sides use the same identifier
    /// without a separate round-trip to allocate a server UUID.
    fn ensure_channel_pcm(
        &self,
        id: Uuid,
        freq_hz: u64,
        mode: &RigMode,
    ) -> Result<broadcast::Receiver<Vec<f32>>, VChanError>;

    /// Return a snapshot of all channels in display order.
    fn channels(&self) -> Vec<VChannelInfo>;

    /// Maximum number of channels (including the primary channel).
    fn max_channels(&self) -> usize;
}

/// Convenience alias used in `RigHandle`.
pub type SharedVChanManager = Arc<dyn VirtualChannelManager>;
