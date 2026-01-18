// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use serde::Serialize;

/// Error type returned by rig requests.
#[derive(Debug, Clone, Serialize)]
pub struct RigError {
    pub message: String,
    pub kind: RigErrorKind,
}

/// Classification of rig errors for retry decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RigErrorKind {
    /// Temporary failure that may succeed on retry (timeout, busy).
    Transient,
    /// Permanent failure that won't be fixed by retrying.
    Permanent,
}

pub type RigResult<T> = Result<T, RigError>;

impl RigError {
    /// Create a new transient error.
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: RigErrorKind::Transient,
        }
    }

    /// Create a new permanent error.
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: RigErrorKind::Permanent,
        }
    }

    /// Create a timeout error (transient).
    pub fn timeout() -> Self {
        Self::transient("operation timed out")
    }

    /// Create a not supported error (permanent).
    pub fn not_supported(operation: &str) -> Self {
        Self::permanent(format!("operation not supported: {}", operation))
    }

    /// Create a communication error (transient).
    pub fn communication(message: impl Into<String>) -> Self {
        Self::transient(message)
    }

    /// Create an invalid state error (permanent).
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::permanent(message)
    }

    /// Check if this error is transient and may succeed on retry.
    pub fn is_transient(&self) -> bool {
        self.kind == RigErrorKind::Transient
    }
}

impl From<String> for RigError {
    fn from(value: String) -> Self {
        // Default to transient for backwards compatibility
        RigError::transient(value)
    }
}

impl From<&str> for RigError {
    fn from(value: &str) -> Self {
        RigError::transient(value)
    }
}

impl std::fmt::Display for RigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RigError {}
