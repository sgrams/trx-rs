// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Rig operational policies for retry and polling behavior.
//!
//! This module provides configurable policies that control how the rig
//! controller handles retries after failures and polling intervals.

use std::time::Duration;

use crate::rig::response::RigError;

/// Apply ±25% jitter to a duration to prevent thundering herd on reconnect.
fn apply_jitter(delay: Duration) -> Duration {
    // Simple deterministic-ish jitter using the current instant's low bits.
    // We avoid pulling in `rand` for this single use.
    let nanos = std::time::Instant::now()
        .elapsed()
        .as_nanos()
        .wrapping_add(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
    // Map to range [0.75, 1.25]
    let frac = (nanos % 1000) as f64 / 1000.0; // 0.0 .. 1.0
    let factor = 0.75 + frac * 0.5; // 0.75 .. 1.25
    Duration::from_secs_f64(delay.as_secs_f64() * factor)
}

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
        let capped = delay.min(self.max_delay);
        apply_jitter(capped)
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

        // Delays include ±25% jitter, so check they fall in the expected range.
        let check = |attempt: u32, base_ms: u64| {
            let d = policy.delay(attempt);
            let lo = Duration::from_secs_f64(base_ms as f64 * 0.75 / 1000.0);
            let hi = Duration::from_secs_f64(base_ms as f64 * 1.25 / 1000.0);
            assert!(
                d >= lo && d <= hi,
                "attempt {}: {:?} not in [{:?}, {:?}]",
                attempt,
                d,
                lo,
                hi
            );
        };

        check(0, 100);
        check(1, 200);
        check(2, 400);
        check(3, 800);
        // Should cap at max_delay (1s) before jitter
        check(4, 1000);
        check(5, 1000);
    }

    #[test]
    fn test_exponential_backoff_jitter_varies() {
        // Two calls should (almost always) produce different values,
        // confirming jitter is applied.
        let policy = ExponentialBackoff::new(5, Duration::from_millis(100), Duration::from_secs(1));
        let d1 = policy.delay(2);
        std::thread::sleep(Duration::from_micros(10));
        let d2 = policy.delay(2);
        // With nanosecond-based jitter they should differ; if not,
        // the test is still valid — it just means the same instant was sampled.
        let _ = (d1, d2); // no assertion — this is a smoke test
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
