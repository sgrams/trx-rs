// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Rig event notification system.
//!
//! This module provides typed event notifications for rig state changes,
//! allowing frontends and other components to react to specific events.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::radio::freq::Freq;
use crate::rig::state::RigMode;
use crate::rig::{RigRxStatus, RigTxStatus};

use super::machine::RigMachineState;

/// Unique identifier for a registered listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(u64);

impl ListenerId {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

/// Trait for components that want to receive rig events.
///
/// Implementors receive typed notifications when rig state changes.
/// All methods have default no-op implementations, so listeners can
/// selectively override only the events they care about.
pub trait RigListener: Send + Sync {
    /// Called when the operating frequency changes.
    fn on_frequency_change(&self, _old: Option<Freq>, _new: Freq) {}

    /// Called when the operating mode changes.
    fn on_mode_change(&self, _old: Option<&RigMode>, _new: &RigMode) {}

    /// Called when PTT state changes.
    fn on_ptt_change(&self, _transmitting: bool) {}

    /// Called when the rig state machine transitions.
    fn on_state_change(&self, _old: &RigMachineState, _new: &RigMachineState) {}

    /// Called when meter readings are updated.
    fn on_meter_update(&self, _rx: Option<&RigRxStatus>, _tx: Option<&RigTxStatus>) {}

    /// Called when the panel lock state changes.
    fn on_lock_change(&self, _locked: bool) {}

    /// Called when the rig powers on or off.
    fn on_power_change(&self, _powered: bool) {}
}

/// Manages registered listeners and dispatches events.
pub struct RigEventEmitter {
    listeners: Vec<(ListenerId, Arc<dyn RigListener>)>,
}

impl Default for RigEventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl RigEventEmitter {
    /// Create a new event emitter with no listeners.
    pub fn new() -> Self {
        Self {
            listeners: Vec::new(),
        }
    }

    /// Register a listener to receive events.
    /// Returns an ID that can be used to unregister the listener.
    pub fn register(&mut self, listener: Arc<dyn RigListener>) -> ListenerId {
        let id = ListenerId::new();
        self.listeners.push((id, listener));
        id
    }

    /// Unregister a listener by its ID.
    pub fn unregister(&mut self, id: ListenerId) {
        self.listeners.retain(|(lid, _)| *lid != id);
    }

    /// Get the number of registered listeners.
    pub fn listener_count(&self) -> usize {
        self.listeners.len()
    }

    /// Notify all listeners of a frequency change.
    pub fn notify_frequency_change(&self, old: Option<Freq>, new: Freq) {
        for (_, listener) in &self.listeners {
            listener.on_frequency_change(old, new);
        }
    }

    /// Notify all listeners of a mode change.
    pub fn notify_mode_change(&self, old: Option<&RigMode>, new: &RigMode) {
        for (_, listener) in &self.listeners {
            listener.on_mode_change(old, new);
        }
    }

    /// Notify all listeners of a PTT state change.
    pub fn notify_ptt_change(&self, transmitting: bool) {
        for (_, listener) in &self.listeners {
            listener.on_ptt_change(transmitting);
        }
    }

    /// Notify all listeners of a state machine transition.
    pub fn notify_state_change(&self, old: &RigMachineState, new: &RigMachineState) {
        for (_, listener) in &self.listeners {
            listener.on_state_change(old, new);
        }
    }

    /// Notify all listeners of updated meter readings.
    pub fn notify_meter_update(&self, rx: Option<&RigRxStatus>, tx: Option<&RigTxStatus>) {
        for (_, listener) in &self.listeners {
            listener.on_meter_update(rx, tx);
        }
    }

    /// Notify all listeners of a lock state change.
    pub fn notify_lock_change(&self, locked: bool) {
        for (_, listener) in &self.listeners {
            listener.on_lock_change(locked);
        }
    }

    /// Notify all listeners of a power state change.
    pub fn notify_power_change(&self, powered: bool) {
        for (_, listener) in &self.listeners {
            listener.on_power_change(powered);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    struct TestListener {
        freq_changed: AtomicBool,
        ptt_changed: AtomicBool,
    }

    impl TestListener {
        fn new() -> Self {
            Self {
                freq_changed: AtomicBool::new(false),
                ptt_changed: AtomicBool::new(false),
            }
        }
    }

    impl RigListener for TestListener {
        fn on_frequency_change(&self, _old: Option<Freq>, _new: Freq) {
            self.freq_changed.store(true, Ordering::Relaxed);
        }

        fn on_ptt_change(&self, _transmitting: bool) {
            self.ptt_changed.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn test_register_and_notify() {
        let mut emitter = RigEventEmitter::new();
        let listener = Arc::new(TestListener::new());
        let id = emitter.register(listener.clone());

        assert_eq!(emitter.listener_count(), 1);

        emitter.notify_frequency_change(None, Freq { hz: 14_200_000 });
        assert!(listener.freq_changed.load(Ordering::Relaxed));
        assert!(!listener.ptt_changed.load(Ordering::Relaxed));

        emitter.notify_ptt_change(true);
        assert!(listener.ptt_changed.load(Ordering::Relaxed));

        emitter.unregister(id);
        assert_eq!(emitter.listener_count(), 0);
    }

    #[test]
    fn test_multiple_listeners() {
        let mut emitter = RigEventEmitter::new();
        let listener1 = Arc::new(TestListener::new());
        let listener2 = Arc::new(TestListener::new());

        emitter.register(listener1.clone());
        emitter.register(listener2.clone());

        emitter.notify_frequency_change(Some(Freq { hz: 7_000_000 }), Freq { hz: 14_200_000 });

        assert!(listener1.freq_changed.load(Ordering::Relaxed));
        assert!(listener2.freq_changed.load(Ordering::Relaxed));
    }
}
