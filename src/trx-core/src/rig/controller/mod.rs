// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Rig controller components.
//!
//! This module contains the core control logic for managing rig state,
//! handling commands, emitting events, and configuring operational policies.

pub mod events;
pub mod executor;
pub mod handlers;
pub mod machine;
pub mod policies;

pub use events::{ListenerId, RigEventEmitter, RigListener};
pub use executor::RigCatExecutor;
pub use handlers::{
    command_from_rig_command, CommandContext, CommandExecutor, CommandResult, RigCommandHandler,
    ValidationResult,
};
pub use machine::{
    ReadyStateData, RigEvent, RigMachineState, RigStateError, RigStateMachine,
    TransmittingStateData,
};
pub use policies::{AdaptivePolling, ExponentialBackoff, PollingPolicy, RetryPolicy};
