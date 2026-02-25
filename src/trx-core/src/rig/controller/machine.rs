// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Rig state machine for lifecycle management.
//!
//! This module provides an explicit state machine for managing rig states,
//! making state transitions clear and preventing invalid states.

use std::fmt;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::radio::freq::Freq;
use crate::rig::state::RigMode;
use crate::rig::{RigInfo, RigRxStatus, RigStatus, RigTxStatus, RigVfo};

/// Events that can trigger state transitions in the rig state machine.
#[derive(Debug, Clone)]
pub enum RigEvent {
    /// Connection to rig established
    Connected,
    /// Rig initialization complete
    Initialized,
    /// Rig powered on
    PoweredOn,
    /// Rig powered off
    PoweredOff,
    /// PTT engaged (transmitting)
    PttOn,
    /// PTT released (receiving)
    PttOff,
    /// Error occurred
    Error(RigStateError),
    /// Recovery from error
    Recovered,
    /// Disconnect requested or detected
    Disconnected,
}

/// Error information stored in error state.
#[derive(Debug, Clone, Serialize)]
pub struct RigStateError {
    pub message: String,
    pub recoverable: bool,
    pub occurred_at: Option<u64>, // Unix timestamp, Option for serialization
}

impl RigStateError {
    pub fn new(message: impl Into<String>, recoverable: bool) -> Self {
        Self {
            message: message.into(),
            recoverable,
            occurred_at: Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            ),
        }
    }

    pub fn transient(message: impl Into<String>) -> Self {
        Self::new(message, true)
    }

    pub fn fatal(message: impl Into<String>) -> Self {
        Self::new(message, false)
    }
}

/// The current state of the rig state machine.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(tag = "state", content = "data")]
pub enum RigMachineState {
    /// Initial state, not connected to rig
    #[default]
    Disconnected,
    /// Connecting to rig backend
    Connecting { started_at: Option<u64> },
    /// Connected but not yet initialized
    Initializing { rig_info: Option<RigInfo> },
    /// Rig is powered off but connected
    PoweredOff { rig_info: RigInfo },
    /// Rig is ready and idle (receiving)
    Ready(ReadyStateData),
    /// Rig is transmitting
    Transmitting(TransmittingStateData),
    /// Error state
    Error {
        error: RigStateError,
        previous_state: Box<RigMachineState>,
    },
}

/// Data held when rig is in Ready state.
#[derive(Debug, Clone, Serialize)]
pub struct ReadyStateData {
    pub rig_info: RigInfo,
    pub freq: Freq,
    pub mode: RigMode,
    pub vfo: Option<RigVfo>,
    pub rx: Option<RigRxStatus>,
    pub tx_limit: Option<u8>,
    pub locked: bool,
}

/// Data held when rig is in Transmitting state.
#[derive(Debug, Clone, Serialize)]
pub struct TransmittingStateData {
    pub rig_info: RigInfo,
    pub freq: Freq,
    pub mode: RigMode,
    pub vfo: Option<RigVfo>,
    pub tx: Option<RigTxStatus>,
    pub locked: bool,
}

impl fmt::Display for RigMachineState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => write!(f, "Disconnected"),
            Self::Connecting { .. } => write!(f, "Connecting"),
            Self::Initializing { .. } => write!(f, "Initializing"),
            Self::PoweredOff { .. } => write!(f, "PoweredOff"),
            Self::Ready(_) => write!(f, "Ready"),
            Self::Transmitting(_) => write!(f, "Transmitting"),
            Self::Error { error, .. } => write!(f, "Error({})", error.message),
        }
    }
}

impl RigMachineState {
    /// Check if the rig is in a state where commands can be executed.
    pub fn can_execute_commands(&self) -> bool {
        matches!(self, Self::Ready(_) | Self::Transmitting(_))
    }

    /// Check if the rig is initialized.
    pub fn is_initialized(&self) -> bool {
        matches!(
            self,
            Self::Ready(_) | Self::Transmitting(_) | Self::PoweredOff { .. }
        )
    }

    /// Check if the rig is transmitting.
    pub fn is_transmitting(&self) -> bool {
        matches!(self, Self::Transmitting(_))
    }

    /// Check if the rig is in an error state.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }

    /// Check if the panel is locked.
    pub fn is_locked(&self) -> bool {
        match self {
            Self::Ready(data) => data.locked,
            Self::Transmitting(data) => data.locked,
            _ => false,
        }
    }

    /// Get the current frequency if available.
    pub fn freq(&self) -> Option<Freq> {
        match self {
            Self::Ready(data) => Some(data.freq),
            Self::Transmitting(data) => Some(data.freq),
            _ => None,
        }
    }

    /// Get the current mode if available.
    pub fn mode(&self) -> Option<&RigMode> {
        match self {
            Self::Ready(data) => Some(&data.mode),
            Self::Transmitting(data) => Some(&data.mode),
            _ => None,
        }
    }

    /// Get rig info if available.
    pub fn rig_info(&self) -> Option<&RigInfo> {
        match self {
            Self::Initializing { rig_info } => rig_info.as_ref(),
            Self::PoweredOff { rig_info } => Some(rig_info),
            Self::Ready(data) => Some(&data.rig_info),
            Self::Transmitting(data) => Some(&data.rig_info),
            Self::Error { previous_state, .. } => previous_state.rig_info(),
            _ => None,
        }
    }

    /// Convert to RigStatus for compatibility with existing code.
    pub fn to_rig_status(&self) -> Option<RigStatus> {
        match self {
            Self::Ready(data) => Some(RigStatus {
                freq: data.freq,
                mode: data.mode.clone(),
                tx_en: false,
                vfo: data.vfo.clone(),
                tx: Some(RigTxStatus {
                    power: Some(0),
                    limit: data.tx_limit,
                    swr: Some(0.0),
                    alc: None,
                }),
                rx: data.rx.clone(),
                lock: Some(data.locked),
            }),
            Self::Transmitting(data) => Some(RigStatus {
                freq: data.freq,
                mode: data.mode.clone(),
                tx_en: true,
                vfo: data.vfo.clone(),
                tx: data.tx.clone(),
                rx: Some(RigRxStatus { sig: Some(0) }),
                lock: Some(data.locked),
            }),
            _ => None,
        }
    }
}

/// The rig state machine that manages state transitions.
#[derive(Debug, Clone)]
pub struct RigStateMachine {
    state: RigMachineState,
    transition_count: u64,
    last_transition: Option<Instant>,
}

impl Default for RigStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl RigStateMachine {
    /// Create a new state machine in the Disconnected state.
    pub fn new() -> Self {
        Self {
            state: RigMachineState::Disconnected,
            transition_count: 0,
            last_transition: None,
        }
    }

    /// Get the current state.
    pub fn state(&self) -> &RigMachineState {
        &self.state
    }

    /// Get the number of state transitions that have occurred.
    pub fn transition_count(&self) -> u64 {
        self.transition_count
    }

    /// Get the time since the last transition.
    pub fn time_in_state(&self) -> Option<Duration> {
        self.last_transition.map(|t| t.elapsed())
    }

    /// Process an event and potentially transition to a new state.
    /// Returns true if a transition occurred.
    pub fn process_event(&mut self, event: RigEvent) -> bool {
        let new_state = self.next_state(event);
        if let Some(state) = new_state {
            self.state = state;
            self.transition_count += 1;
            self.last_transition = Some(Instant::now());
            true
        } else {
            false
        }
    }

    /// Determine the next state based on current state and event.
    fn next_state(&self, event: RigEvent) -> Option<RigMachineState> {
        match (&self.state, event) {
            // From Disconnected
            (RigMachineState::Disconnected, RigEvent::Connected) => {
                Some(RigMachineState::Connecting {
                    started_at: Some(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                    ),
                })
            }

            // From Connecting
            (RigMachineState::Connecting { .. }, RigEvent::Initialized) => {
                Some(RigMachineState::Initializing { rig_info: None })
            }

            // From Initializing
            (RigMachineState::Initializing { rig_info }, RigEvent::PoweredOn) => {
                rig_info.as_ref().map(|info| {
                    RigMachineState::Ready(ReadyStateData {
                        rig_info: info.clone(),
                        freq: Freq { hz: 0 },
                        mode: RigMode::USB,
                        vfo: None,
                        rx: None,
                        tx_limit: None,
                        locked: false,
                    })
                })
            }
            (RigMachineState::Initializing { .. }, RigEvent::PoweredOff) => {
                // Stay in initializing, rig is off
                None
            }

            // From PoweredOff
            (RigMachineState::PoweredOff { rig_info }, RigEvent::PoweredOn) => {
                Some(RigMachineState::Ready(ReadyStateData {
                    rig_info: rig_info.clone(),
                    freq: Freq { hz: 0 },
                    mode: RigMode::USB,
                    vfo: None,
                    rx: None,
                    tx_limit: None,
                    locked: false,
                }))
            }

            // From Ready
            (RigMachineState::Ready(data), RigEvent::PttOn) => {
                Some(RigMachineState::Transmitting(TransmittingStateData {
                    rig_info: data.rig_info.clone(),
                    freq: data.freq,
                    mode: data.mode.clone(),
                    vfo: data.vfo.clone(),
                    tx: Some(RigTxStatus {
                        power: None,
                        limit: data.tx_limit,
                        swr: None,
                        alc: None,
                    }),
                    locked: data.locked,
                }))
            }
            (RigMachineState::Ready(data), RigEvent::PoweredOff) => {
                Some(RigMachineState::PoweredOff {
                    rig_info: data.rig_info.clone(),
                })
            }

            // From Transmitting
            (RigMachineState::Transmitting(data), RigEvent::PttOff) => {
                Some(RigMachineState::Ready(ReadyStateData {
                    rig_info: data.rig_info.clone(),
                    freq: data.freq,
                    mode: data.mode.clone(),
                    vfo: data.vfo.clone(),
                    rx: None,
                    tx_limit: data.tx.as_ref().and_then(|t| t.limit),
                    locked: data.locked,
                }))
            }
            (RigMachineState::Transmitting(data), RigEvent::PoweredOff) => {
                Some(RigMachineState::PoweredOff {
                    rig_info: data.rig_info.clone(),
                })
            }

            // Error transitions (from any state)
            (current, RigEvent::Error(error)) => Some(RigMachineState::Error {
                error,
                previous_state: Box::new(current.clone()),
            }),

            // Recovery from error
            (
                RigMachineState::Error {
                    error,
                    previous_state,
                },
                RigEvent::Recovered,
            ) => {
                if error.recoverable {
                    Some(*previous_state.clone())
                } else {
                    Some(RigMachineState::Disconnected)
                }
            }

            // Disconnect from any state
            (_, RigEvent::Disconnected) => Some(RigMachineState::Disconnected),

            // Invalid transition - stay in current state
            _ => None,
        }
    }

    /// Force set the state (for initialization or recovery).
    pub fn set_state(&mut self, state: RigMachineState) {
        self.state = state;
        self.transition_count += 1;
        self.last_transition = Some(Instant::now());
    }

    /// Update Ready state data in place.
    pub fn update_ready_data<F>(&mut self, f: F)
    where
        F: FnOnce(&mut ReadyStateData),
    {
        if let RigMachineState::Ready(ref mut data) = self.state {
            f(data);
        }
    }

    /// Update Transmitting state data in place.
    pub fn update_transmitting_data<F>(&mut self, f: F)
    where
        F: FnOnce(&mut TransmittingStateData),
    {
        if let RigMachineState::Transmitting(ref mut data) = self.state {
            f(data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_rig_info() -> RigInfo {
        use crate::rig::{RigAccessMethod, RigCapabilities};

        RigInfo {
            manufacturer: "Test".to_string(),
            model: "Mock".to_string(),
            revision: "1.0".to_string(),
            capabilities: RigCapabilities {
                min_freq_step_hz: 1,
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
                tx: true,
                tx_limit: true,
                vfo_switch: true,
                filter_controls: false,
                signal_meter: true,
            },
            access: RigAccessMethod::Serial {
                path: "/dev/test".to_string(),
                baud: 9600,
            },
        }
    }

    #[test]
    fn test_initial_state() {
        let sm = RigStateMachine::new();
        assert!(matches!(sm.state(), RigMachineState::Disconnected));
    }

    #[test]
    fn test_connect_transition() {
        let mut sm = RigStateMachine::new();
        assert!(sm.process_event(RigEvent::Connected));
        assert!(matches!(sm.state(), RigMachineState::Connecting { .. }));
    }

    #[test]
    fn test_full_lifecycle() {
        let mut sm = RigStateMachine::new();

        // Connect
        sm.process_event(RigEvent::Connected);
        assert!(matches!(sm.state(), RigMachineState::Connecting { .. }));

        // Initialize
        sm.process_event(RigEvent::Initialized);
        assert!(matches!(sm.state(), RigMachineState::Initializing { .. }));

        // Set rig info and power on
        sm.set_state(RigMachineState::Initializing {
            rig_info: Some(mock_rig_info()),
        });
        sm.process_event(RigEvent::PoweredOn);
        assert!(matches!(sm.state(), RigMachineState::Ready(_)));

        // Transmit
        sm.process_event(RigEvent::PttOn);
        assert!(matches!(sm.state(), RigMachineState::Transmitting(_)));
        assert!(sm.state().is_transmitting());

        // Back to ready
        sm.process_event(RigEvent::PttOff);
        assert!(matches!(sm.state(), RigMachineState::Ready(_)));

        // Power off
        sm.process_event(RigEvent::PoweredOff);
        assert!(matches!(sm.state(), RigMachineState::PoweredOff { .. }));
    }

    #[test]
    fn test_error_and_recovery() {
        let mut sm = RigStateMachine::new();
        sm.process_event(RigEvent::Connected);
        sm.process_event(RigEvent::Initialized);
        sm.set_state(RigMachineState::Initializing {
            rig_info: Some(mock_rig_info()),
        });
        sm.process_event(RigEvent::PoweredOn);

        // Trigger error
        sm.process_event(RigEvent::Error(RigStateError::transient("Test error")));
        assert!(sm.state().is_error());

        // Recover
        sm.process_event(RigEvent::Recovered);
        assert!(matches!(sm.state(), RigMachineState::Ready(_)));
    }

    #[test]
    fn test_invalid_transition() {
        let mut sm = RigStateMachine::new();

        // Can't transmit from disconnected
        let transitioned = sm.process_event(RigEvent::PttOn);
        assert!(!transitioned);
        assert!(matches!(sm.state(), RigMachineState::Disconnected));
    }
}
