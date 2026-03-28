// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Shared configuration validation helpers used by both `trx-server` and
//! `trx-client`.
//!
//! # Non-shared structs
//!
//! `GeneralConfig` is defined separately in each binary because the fields
//! differ:
//!
//! - **Server** `GeneralConfig`: `callsign`, `log_level`, `latitude`,
//!   `longitude`
//! - **Client** `GeneralConfig`: `callsign`, `log_level`, `website_url`,
//!   `website_name`, `ais_vessel_url_base`
//!
//! Only `callsign` and `log_level` overlap.  Merging into a single struct
//! would either bloat both binaries with unused fields or require a trait
//! abstraction that adds complexity without clear benefit.

/// Validate that a log level string is one of the accepted values.
///
/// Returns `Ok(())` when `level` is `None` (defaulting is handled elsewhere)
/// or a recognised level name.
pub fn validate_log_level(level: Option<&str>) -> Result<(), String> {
    if let Some(level) = level {
        match level {
            "trace" | "debug" | "info" | "warn" | "error" => {}
            _ => {
                return Err(format!(
                    "[general].log_level '{}' is invalid (expected one of: trace, debug, info, warn, error)",
                    level
                ))
            }
        }
    }
    Ok(())
}

/// Validate that a list of authentication tokens contains no empty entries.
///
/// `path` is a human-readable config path prefix used in the error message
/// (e.g. `"[listen.auth].tokens"`).
pub fn validate_tokens(path: &str, tokens: &[String]) -> Result<(), String> {
    if tokens.iter().any(|t| t.trim().is_empty()) {
        return Err(format!("{path} must not contain empty tokens"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_log_level_none() {
        assert!(validate_log_level(None).is_ok());
    }

    #[test]
    fn test_validate_log_level_valid() {
        for level in &["trace", "debug", "info", "warn", "error"] {
            assert!(validate_log_level(Some(level)).is_ok());
        }
    }

    #[test]
    fn test_validate_log_level_invalid() {
        assert!(validate_log_level(Some("verbose")).is_err());
    }

    #[test]
    fn test_validate_tokens_empty_list() {
        assert!(validate_tokens("[auth].tokens", &[]).is_ok());
    }

    #[test]
    fn test_validate_tokens_valid() {
        let tokens = vec!["abc".to_string(), "def".to_string()];
        assert!(validate_tokens("[auth].tokens", &tokens).is_ok());
    }

    #[test]
    fn test_validate_tokens_rejects_empty() {
        let tokens = vec!["abc".to_string(), "".to_string()];
        assert!(validate_tokens("[auth].tokens", &tokens).is_err());
    }

    #[test]
    fn test_validate_tokens_rejects_whitespace_only() {
        let tokens = vec!["  ".to_string()];
        assert!(validate_tokens("[auth].tokens", &tokens).is_err());
    }
}
