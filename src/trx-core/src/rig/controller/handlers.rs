// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Command handlers for rig operations.
//!
//! This module provides a trait-based command system where each command
//! is encapsulated in its own struct with validation and execution logic.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use crate::radio::freq::Freq;
use crate::rig::state::RigMode;
use crate::DynResult;

use super::machine::RigMachineState;

/// Result of command validation.
#[derive(Debug, Clone)]
pub enum ValidationResult {
    /// Command can be executed.
    Ok,
    /// Command cannot be executed due to current state.
    InvalidState(String),
    /// Command parameters are invalid.
    InvalidParams(String),
    /// Panel is locked.
    Locked,
}

impl ValidationResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Context provided to commands for execution.
/// This allows commands to access rig state without owning it.
pub trait CommandContext: Send {
    /// Get the current state machine state.
    fn state(&self) -> &RigMachineState;

    /// Check if the panel is locked.
    fn is_locked(&self) -> bool {
        self.state().is_locked()
    }

    /// Check if the rig is initialized.
    fn is_initialized(&self) -> bool {
        self.state().is_initialized()
    }

    /// Check if the rig is transmitting.
    fn is_transmitting(&self) -> bool {
        self.state().is_transmitting()
    }
}

/// Trait for rig commands following the Command Pattern.
///
/// Each command encapsulates:
/// - Validation logic (`can_execute`)
/// - Execution logic (`execute`)
/// - Optional description for logging
pub trait RigCommandHandler: Debug + Send + Sync {
    /// Human-readable name of the command.
    fn name(&self) -> &'static str;

    /// Validate if the command can be executed in the current context.
    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult;

    /// Execute the command. Returns the result of the operation.
    /// The actual rig interaction is done via the executor passed to the pipeline.
    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>>;
}

/// Executor interface for commands to interact with the rig.
/// This abstracts the actual rig communication from the command logic.
pub trait CommandExecutor: Send {
    fn set_freq<'a>(
        &'a mut self,
        freq: Freq,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn set_mode<'a>(
        &'a mut self,
        mode: RigMode,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn set_ptt<'a>(
        &'a mut self,
        ptt: bool,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn power_on<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn power_off<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn toggle_vfo<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn lock<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn unlock<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn get_tx_limit<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<u8>> + Send + 'a>>;

    fn set_tx_limit<'a>(
        &'a mut self,
        limit: u8,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn refresh_state<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;
}

/// Result of command execution containing any state updates.
#[derive(Debug, Clone)]
pub enum CommandResult {
    /// Command executed successfully with no state change needed.
    Ok,
    /// Command executed and frequency was updated.
    FreqUpdated(Freq),
    /// Command executed and mode was updated.
    ModeUpdated(RigMode),
    /// Command executed and PTT state was updated.
    PttUpdated(bool),
    /// Command executed and power state was updated.
    PowerUpdated(bool),
    /// Command executed and lock state was updated.
    LockUpdated(bool),
    /// Command executed and TX limit was updated.
    TxLimitUpdated(u8),
    /// Command requires state refresh from rig.
    RefreshRequired,
}

// ============================================================================
// Concrete Command Implementations
// ============================================================================

/// Command to set the rig frequency.
#[derive(Debug, Clone)]
pub struct SetFreqCommand {
    pub freq: Freq,
}

impl SetFreqCommand {
    pub fn new(freq: Freq) -> Self {
        Self { freq }
    }
}

impl RigCommandHandler for SetFreqCommand {
    fn name(&self) -> &'static str {
        "SetFreq"
    }

    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult {
        if !ctx.is_initialized() {
            return ValidationResult::InvalidState("Rig not initialized".into());
        }
        if ctx.is_locked() {
            return ValidationResult::Locked;
        }
        if self.freq.hz == 0 {
            return ValidationResult::InvalidParams("Frequency cannot be 0 Hz".into());
        }
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        Box::pin(async move {
            executor.set_freq(self.freq).await?;
            Ok(CommandResult::FreqUpdated(self.freq))
        })
    }
}

/// Command to set the rig mode.
#[derive(Debug, Clone)]
pub struct SetModeCommand {
    pub mode: RigMode,
}

impl SetModeCommand {
    pub fn new(mode: RigMode) -> Self {
        Self { mode }
    }
}

impl RigCommandHandler for SetModeCommand {
    fn name(&self) -> &'static str {
        "SetMode"
    }

    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult {
        if !ctx.is_initialized() {
            return ValidationResult::InvalidState("Rig not initialized".into());
        }
        if ctx.is_locked() {
            return ValidationResult::Locked;
        }
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        let mode = self.mode.clone();
        Box::pin(async move {
            executor.set_mode(mode.clone()).await?;
            Ok(CommandResult::ModeUpdated(mode))
        })
    }
}

/// Command to set PTT state.
#[derive(Debug, Clone)]
pub struct SetPttCommand {
    pub ptt: bool,
}

impl SetPttCommand {
    pub fn new(ptt: bool) -> Self {
        Self { ptt }
    }
}

impl RigCommandHandler for SetPttCommand {
    fn name(&self) -> &'static str {
        "SetPtt"
    }

    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult {
        if !ctx.is_initialized() {
            return ValidationResult::InvalidState("Rig not initialized".into());
        }
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        let ptt = self.ptt;
        Box::pin(async move {
            executor.set_ptt(ptt).await?;
            Ok(CommandResult::PttUpdated(ptt))
        })
    }
}

/// Command to power on the rig.
#[derive(Debug, Clone)]
pub struct PowerOnCommand;

impl RigCommandHandler for PowerOnCommand {
    fn name(&self) -> &'static str {
        "PowerOn"
    }

    fn can_execute(&self, _ctx: &dyn CommandContext) -> ValidationResult {
        // Power on can always be attempted
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        Box::pin(async move {
            executor.power_on().await?;
            Ok(CommandResult::PowerUpdated(true))
        })
    }
}

/// Command to power off the rig.
#[derive(Debug, Clone)]
pub struct PowerOffCommand;

impl RigCommandHandler for PowerOffCommand {
    fn name(&self) -> &'static str {
        "PowerOff"
    }

    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult {
        if ctx.is_transmitting() {
            return ValidationResult::InvalidState("Cannot power off while transmitting".into());
        }
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        Box::pin(async move {
            executor.power_off().await?;
            Ok(CommandResult::PowerUpdated(false))
        })
    }
}

/// Command to toggle VFO.
#[derive(Debug, Clone)]
pub struct ToggleVfoCommand;

impl RigCommandHandler for ToggleVfoCommand {
    fn name(&self) -> &'static str {
        "ToggleVfo"
    }

    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult {
        if !ctx.is_initialized() {
            return ValidationResult::InvalidState("Rig not initialized".into());
        }
        if ctx.is_locked() {
            return ValidationResult::Locked;
        }
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        Box::pin(async move {
            executor.toggle_vfo().await?;
            Ok(CommandResult::RefreshRequired)
        })
    }
}

/// Command to lock the panel.
#[derive(Debug, Clone)]
pub struct LockCommand;

impl RigCommandHandler for LockCommand {
    fn name(&self) -> &'static str {
        "Lock"
    }

    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult {
        if !ctx.is_initialized() {
            return ValidationResult::InvalidState("Rig not initialized".into());
        }
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        Box::pin(async move {
            executor.lock().await?;
            Ok(CommandResult::LockUpdated(true))
        })
    }
}

/// Command to unlock the panel.
#[derive(Debug, Clone)]
pub struct UnlockCommand;

impl RigCommandHandler for UnlockCommand {
    fn name(&self) -> &'static str {
        "Unlock"
    }

    fn can_execute(&self, _ctx: &dyn CommandContext) -> ValidationResult {
        // Unlock can always be attempted
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        Box::pin(async move {
            executor.unlock().await?;
            Ok(CommandResult::LockUpdated(false))
        })
    }
}

/// Command to get TX limit.
#[derive(Debug, Clone)]
pub struct GetTxLimitCommand;

impl RigCommandHandler for GetTxLimitCommand {
    fn name(&self) -> &'static str {
        "GetTxLimit"
    }

    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult {
        if !ctx.is_initialized() {
            return ValidationResult::InvalidState("Rig not initialized".into());
        }
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        Box::pin(async move {
            let limit = executor.get_tx_limit().await?;
            Ok(CommandResult::TxLimitUpdated(limit))
        })
    }
}

/// Command to set TX limit.
#[derive(Debug, Clone)]
pub struct SetTxLimitCommand {
    pub limit: u8,
}

impl SetTxLimitCommand {
    pub fn new(limit: u8) -> Self {
        Self { limit }
    }
}

impl RigCommandHandler for SetTxLimitCommand {
    fn name(&self) -> &'static str {
        "SetTxLimit"
    }

    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult {
        if !ctx.is_initialized() {
            return ValidationResult::InvalidState("Rig not initialized".into());
        }
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        let limit = self.limit;
        Box::pin(async move {
            executor.set_tx_limit(limit).await?;
            Ok(CommandResult::TxLimitUpdated(limit))
        })
    }
}

/// Command to get current state snapshot.
#[derive(Debug, Clone)]
pub struct GetSnapshotCommand;

impl RigCommandHandler for GetSnapshotCommand {
    fn name(&self) -> &'static str {
        "GetSnapshot"
    }

    fn can_execute(&self, _ctx: &dyn CommandContext) -> ValidationResult {
        // Getting snapshot can always be attempted
        ValidationResult::Ok
    }

    fn execute<'a>(
        &'a self,
        executor: &'a mut dyn CommandExecutor,
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>> {
        Box::pin(async move {
            executor.refresh_state().await?;
            Ok(CommandResult::RefreshRequired)
        })
    }
}

// ============================================================================
// Command Factory
// ============================================================================

use crate::rig::command::RigCommand;

/// Convert from the existing RigCommand enum to a command handler.
pub fn command_from_rig_command(cmd: RigCommand) -> Box<dyn RigCommandHandler> {
    match cmd {
        RigCommand::GetSnapshot => Box::new(GetSnapshotCommand),
        RigCommand::SetFreq(freq) => Box::new(SetFreqCommand::new(freq)),
        RigCommand::SetMode(mode) => Box::new(SetModeCommand::new(mode)),
        RigCommand::SetPtt(ptt) => Box::new(SetPttCommand::new(ptt)),
        RigCommand::PowerOn => Box::new(PowerOnCommand),
        RigCommand::PowerOff => Box::new(PowerOffCommand),
        RigCommand::ToggleVfo => Box::new(ToggleVfoCommand),
        RigCommand::GetTxLimit => Box::new(GetTxLimitCommand),
        RigCommand::SetTxLimit(limit) => Box::new(SetTxLimitCommand::new(limit)),
        RigCommand::Lock => Box::new(LockCommand),
        RigCommand::Unlock => Box::new(UnlockCommand),
        // Decoder commands are handled before reaching this function;
        // map to GetSnapshot as a safe fallback.
        RigCommand::SetAprsDecodeEnabled(_)
        | RigCommand::SetCwDecodeEnabled(_)
        | RigCommand::SetCwAuto(_)
        | RigCommand::SetCwWpm(_)
        | RigCommand::SetCwToneHz(_)
        | RigCommand::ResetAprsDecoder
        | RigCommand::ResetCwDecoder => Box::new(GetSnapshotCommand),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockContext {
        state: RigMachineState,
    }

    impl CommandContext for MockContext {
        fn state(&self) -> &RigMachineState {
            &self.state
        }
    }

    #[test]
    fn test_set_freq_validation_locked() {
        use crate::rig::controller::machine::ReadyStateData;
        use crate::rig::{RigAccessMethod, RigCapabilities, RigInfo};

        let ctx = MockContext {
            state: RigMachineState::Ready(ReadyStateData {
                rig_info: RigInfo {
                    manufacturer: "Test".to_string(),
                    model: "Mock".to_string(),
                    revision: "1.0".to_string(),
                    capabilities: RigCapabilities {
                        supported_bands: vec![],
                        supported_modes: vec![],
                        num_vfos: 2,
                        lock: false,
                        lockable: true,
                        attenuator: false,
                        preamp: false,
                        rit: false,
                        rpt: false,
                        split: false,
                    },
                    access: RigAccessMethod::Serial {
                        path: "/dev/test".to_string(),
                        baud: 9600,
                    },
                },
                freq: Freq { hz: 14_200_000 },
                mode: RigMode::USB,
                vfo: None,
                rx: None,
                tx_limit: None,
                locked: true, // Panel is locked
            }),
        };

        let cmd = SetFreqCommand::new(Freq { hz: 14_300_000 });
        let result = cmd.can_execute(&ctx);
        assert!(matches!(result, ValidationResult::Locked));
    }

    #[test]
    fn test_set_freq_validation_not_initialized() {
        let ctx = MockContext {
            state: RigMachineState::Disconnected,
        };

        let cmd = SetFreqCommand::new(Freq { hz: 14_300_000 });
        let result = cmd.can_execute(&ctx);
        assert!(matches!(result, ValidationResult::InvalidState(_)));
    }

    #[test]
    fn test_power_off_while_transmitting() {
        use crate::rig::controller::machine::TransmittingStateData;
        use crate::rig::{RigAccessMethod, RigCapabilities, RigInfo};

        let ctx = MockContext {
            state: RigMachineState::Transmitting(TransmittingStateData {
                rig_info: RigInfo {
                    manufacturer: "Test".to_string(),
                    model: "Mock".to_string(),
                    revision: "1.0".to_string(),
                    capabilities: RigCapabilities {
                        supported_bands: vec![],
                        supported_modes: vec![],
                        num_vfos: 2,
                        lock: false,
                        lockable: true,
                        attenuator: false,
                        preamp: false,
                        rit: false,
                        rpt: false,
                        split: false,
                    },
                    access: RigAccessMethod::Serial {
                        path: "/dev/test".to_string(),
                        baud: 9600,
                    },
                },
                freq: Freq { hz: 14_200_000 },
                mode: RigMode::USB,
                vfo: None,
                tx: None,
                locked: false,
            }),
        };

        let cmd = PowerOffCommand;
        let result = cmd.can_execute(&ctx);
        assert!(matches!(result, ValidationResult::InvalidState(_)));
    }
}
