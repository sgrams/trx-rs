// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Rig operational policies for retry and polling behavior.
//!
//! This module provides configurable policies that control how the rig
//! controller handles retries after failures and polling intervals.

use std::time::Duration;

use crate::rig::response::RigError;

/// Policy for retrying failed operations.
pub trait RetryPolicy: Send + Sync {
    /// Determine if the operation should be retried.
    fn should_retry(&self, attempt: u32, error: &RigError) -> bool;

    /// Get the delay before the next retry attempt.
    fn delay(&self, attempt: u32) -> Duration;

    /// Get the maximum number of attempts allowed.
    fn max_attempts(&self) -> u32;
}

/// Exponential backoff retry policy.
///
/// Delays increase exponentially with each retry attempt,
/// up to a configured maximum delay.
#[derive(Debug, Clone)]
pub struct ExponentialBackoff {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
}

impl ExponentialBackoff {
    /// Create a new exponential backoff policy.
    pub fn new(max_attempts: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_attempts,
            base_delay,
            max_delay,
        }
    }

    /// Create a policy with sensible defaults for rig communication.
    pub fn default_rig() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
        }
    }
}

impl Default for ExponentialBackoff {
    fn default() -> Self {
        Self::default_rig()
    }
}

impl RetryPolicy for ExponentialBackoff {
    fn should_retry(&self, attempt: u32, error: &RigError) -> bool {
        if attempt >= self.max_attempts {
            return false;
        }
        // Only retry transient errors
        error.is_transient()
    }

    fn delay(&self, attempt: u32) -> Duration {
        let multiplier = 2u32.saturating_pow(attempt);
        let delay = self.base_delay.saturating_mul(multiplier);
        delay.min(self.max_delay)
    }

    fn max_attempts(&self) -> u32 {
        self.max_attempts
    }
}

/// Fixed delay retry policy.
///
/// Uses a constant delay between retry attempts.
#[derive(Debug, Clone)]
pub struct FixedDelay {
    max_attempts: u32,
    delay: Duration,
}

impl FixedDelay {
    /// Create a new fixed delay policy.
    pub fn new(max_attempts: u32, delay: Duration) -> Self {
        Self {
            max_attempts,
            delay,
        }
    }
}

impl RetryPolicy for FixedDelay {
    fn should_retry(&self, attempt: u32, error: &RigError) -> bool {
        attempt < self.max_attempts && error.is_transient()
    }

    fn delay(&self, _attempt: u32) -> Duration {
        self.delay
    }

    fn max_attempts(&self) -> u32 {
        self.max_attempts
    }
}

/// No retry policy - operations fail immediately.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRetry;

impl RetryPolicy for NoRetry {
    fn should_retry(&self, _attempt: u32, _error: &RigError) -> bool {
        false
    }

    fn delay(&self, _attempt: u32) -> Duration {
        Duration::ZERO
    }

    fn max_attempts(&self) -> u32 {
        1
    }
}

/// Policy for polling the rig for status updates.
pub trait PollingPolicy: Send + Sync {
    /// Get the interval between polls.
    fn interval(&self, transmitting: bool) -> Duration;

    /// Determine if polling should occur given the current state.
    fn should_poll(&self, transmitting: bool) -> bool;
}

/// Adaptive polling policy.
///
/// Uses different intervals depending on whether the rig is transmitting.
/// Polls more frequently during TX to track power/SWR meters.
#[derive(Debug, Clone)]
pub struct AdaptivePolling {
    idle_interval: Duration,
    active_interval: Duration,
}

impl AdaptivePolling {
    /// Create a new adaptive polling policy.
    pub fn new(idle_interval: Duration, active_interval: Duration) -> Self {
        Self {
            idle_interval,
            active_interval,
        }
    }

    /// Create a policy with sensible defaults for rig polling.
    pub fn default_rig() -> Self {
        Self {
            idle_interval: Duration::from_millis(500),
            active_interval: Duration::from_millis(100),
        }
    }
}

impl Default for AdaptivePolling {
    fn default() -> Self {
        Self::default_rig()
    }
}

impl PollingPolicy for AdaptivePolling {
    fn interval(&self, transmitting: bool) -> Duration {
        if transmitting {
            self.active_interval
        } else {
            self.idle_interval
        }
    }

    fn should_poll(&self, _transmitting: bool) -> bool {
        true
    }
}

/// Fixed polling policy.
///
/// Uses a constant interval regardless of rig state.
#[derive(Debug, Clone)]
pub struct FixedPolling {
    interval: Duration,
}

impl FixedPolling {
    /// Create a new fixed polling policy.
    pub fn new(interval: Duration) -> Self {
        Self { interval }
    }
}

impl PollingPolicy for FixedPolling {
    fn interval(&self, _transmitting: bool) -> Duration {
        self.interval
    }

    fn should_poll(&self, _transmitting: bool) -> bool {
        true
    }
}

/// No polling policy - disables automatic polling.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPolling;

impl PollingPolicy for NoPolling {
    fn interval(&self, _transmitting: bool) -> Duration {
        Duration::MAX
    }

    fn should_poll(&self, _transmitting: bool) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_backoff_delays() {
        let policy = ExponentialBackoff::new(5, Duration::from_millis(100), Duration::from_secs(1));

        assert_eq!(policy.delay(0), Duration::from_millis(100));
        assert_eq!(policy.delay(1), Duration::from_millis(200));
        assert_eq!(policy.delay(2), Duration::from_millis(400));
        assert_eq!(policy.delay(3), Duration::from_millis(800));
        // Should cap at max_delay
        assert_eq!(policy.delay(4), Duration::from_secs(1));
        assert_eq!(policy.delay(5), Duration::from_secs(1));
    }

    #[test]
    fn test_exponential_backoff_should_retry() {
        let policy = ExponentialBackoff::new(3, Duration::from_millis(100), Duration::from_secs(1));

        let transient = RigError::timeout();
        let fatal = RigError::not_supported("test");

        assert!(policy.should_retry(0, &transient));
        assert!(policy.should_retry(1, &transient));
        assert!(policy.should_retry(2, &transient));
        assert!(!policy.should_retry(3, &transient)); // exceeded max attempts

        assert!(!policy.should_retry(0, &fatal)); // not transient
    }

    #[test]
    fn test_fixed_delay() {
        let policy = FixedDelay::new(3, Duration::from_millis(500));

        assert_eq!(policy.delay(0), Duration::from_millis(500));
        assert_eq!(policy.delay(1), Duration::from_millis(500));
        assert_eq!(policy.delay(5), Duration::from_millis(500));
    }

    #[test]
    fn test_adaptive_polling() {
        let policy = AdaptivePolling::new(Duration::from_millis(500), Duration::from_millis(100));

        assert_eq!(policy.interval(false), Duration::from_millis(500));
        assert_eq!(policy.interval(true), Duration::from_millis(100));
        assert!(policy.should_poll(false));
        assert!(policy.should_poll(true));
    }

    #[test]
    fn test_no_polling() {
        let policy = NoPolling;

        assert!(!policy.should_poll(false));
        assert!(!policy.should_poll(true));
    }
}
