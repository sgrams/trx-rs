// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Authorization and token handling utilities.

use std::collections::HashSet;

/// Strip the "Bearer " prefix from a token string (case-insensitive).
///
/// If the string starts with "Bearer " (ignoring case), returns the remainder.
/// Otherwise returns the original trimmed string.
pub fn strip_bearer(value: &str) -> &str {
    let trimmed = value.trim();
    let prefix = "bearer ";
    if trimmed.len() >= prefix.len() && trimmed[..prefix.len()].eq_ignore_ascii_case(prefix) {
        &trimmed[prefix.len()..]
    } else {
        trimmed
    }
}

/// Trait for validating authorization tokens.
pub trait TokenValidator {
    /// Validate a token. Returns Ok(()) if valid, Err(String) with error message if invalid.
    fn validate(&self, token: &Option<String>) -> Result<(), String>;
}

/// Simple token validator using a HashSet of valid tokens.
pub struct SimpleTokenValidator {
    tokens: HashSet<String>,
}

impl SimpleTokenValidator {
    /// Create a new SimpleTokenValidator with a set of valid tokens.
    pub fn new(tokens: HashSet<String>) -> Self {
        SimpleTokenValidator { tokens }
    }

    /// Create a new SimpleTokenValidator from a vector of tokens.
    pub fn from_vec(tokens: Vec<String>) -> Self {
        SimpleTokenValidator {
            tokens: tokens.into_iter().collect(),
        }
    }

    /// Check if the validator has any tokens configured.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

impl TokenValidator for SimpleTokenValidator {
    fn validate(&self, token: &Option<String>) -> Result<(), String> {
        // No auth required if no tokens configured
        if self.tokens.is_empty() {
            return Ok(());
        }

        let Some(token) = token.as_ref() else {
            return Err("missing authorization token".into());
        };

        let candidate = strip_bearer(token);
        if self.tokens.contains(candidate) {
            return Ok(());
        }

        Err("invalid authorization token".into())
    }
}

/// No-op token validator that always accepts all tokens.
///
/// Use this when authentication is disabled.
pub struct NoAuthValidator;

impl TokenValidator for NoAuthValidator {
    fn validate(&self, _token: &Option<String>) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_bearer_with_prefix() {
        assert_eq!(strip_bearer("Bearer abc123"), "abc123");
    }

    #[test]
    fn test_strip_bearer_lowercase() {
        assert_eq!(strip_bearer("bearer xyz789"), "xyz789");
    }

    #[test]
    fn test_strip_bearer_mixed_case() {
        assert_eq!(strip_bearer("BeArEr test123"), "test123");
    }

    #[test]
    fn test_strip_bearer_without_prefix() {
        assert_eq!(strip_bearer("abc123"), "abc123");
    }

    #[test]
    fn test_strip_bearer_with_whitespace() {
        assert_eq!(strip_bearer("  Bearer token  "), "token");
    }

    #[test]
    fn test_strip_bearer_empty() {
        assert_eq!(strip_bearer(""), "");
    }

    #[test]
    fn test_strip_bearer_only_prefix() {
        // "bearer " is exactly the prefix with nothing after it
        // trim() preserves it as "bearer " (7 chars including space)
        // After stripping "bearer " (7 chars), nothing is left
        // But trim also removes the trailing space, so we get "bearer"
        // which is 6 chars, less than the 7-char prefix, so it doesn't strip
        assert_eq!(strip_bearer("bearer "), "bearer");
    }

    #[test]
    fn test_simple_token_validator_with_valid_token() {
        let mut tokens = HashSet::new();
        tokens.insert("token123".to_string());
        let validator = SimpleTokenValidator::new(tokens);

        let result = validator.validate(&Some("token123".to_string()));
        assert!(result.is_ok());
    }

    #[test]
    fn test_simple_token_validator_with_bearer_prefix() {
        let mut tokens = HashSet::new();
        tokens.insert("token123".to_string());
        let validator = SimpleTokenValidator::new(tokens);

        let result = validator.validate(&Some("Bearer token123".to_string()));
        assert!(result.is_ok());
    }

    #[test]
    fn test_simple_token_validator_with_invalid_token() {
        let mut tokens = HashSet::new();
        tokens.insert("token123".to_string());
        let validator = SimpleTokenValidator::new(tokens);

        let result = validator.validate(&Some("wrongtoken".to_string()));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "invalid authorization token");
    }

    #[test]
    fn test_simple_token_validator_with_missing_token() {
        let mut tokens = HashSet::new();
        tokens.insert("token123".to_string());
        let validator = SimpleTokenValidator::new(tokens);

        let result = validator.validate(&None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "missing authorization token");
    }

    #[test]
    fn test_simple_token_validator_no_auth_required() {
        let tokens = HashSet::new();
        let validator = SimpleTokenValidator::new(tokens);

        // No token required when validator is empty
        let result = validator.validate(&None);
        assert!(result.is_ok());

        let result = validator.validate(&Some("anytoken".to_string()));
        assert!(result.is_ok());
    }

    #[test]
    fn test_simple_token_validator_from_vec() {
        let tokens = vec!["token1".to_string(), "token2".to_string()];
        let validator = SimpleTokenValidator::from_vec(tokens);

        assert!(validator.validate(&Some("token1".to_string())).is_ok());
        assert!(validator.validate(&Some("token2".to_string())).is_ok());
        assert!(validator.validate(&Some("token3".to_string())).is_err());
    }

    #[test]
    fn test_simple_token_validator_is_empty() {
        let empty = SimpleTokenValidator::new(HashSet::new());
        assert!(empty.is_empty());

        let mut tokens = HashSet::new();
        tokens.insert("token".to_string());
        let not_empty = SimpleTokenValidator::new(tokens);
        assert!(!not_empty.is_empty());
    }

    #[test]
    fn test_no_auth_validator_with_no_token() {
        let validator = NoAuthValidator;
        assert!(validator.validate(&None).is_ok());
    }

    #[test]
    fn test_no_auth_validator_with_token() {
        let validator = NoAuthValidator;
        assert!(validator.validate(&Some("anytoken".to_string())).is_ok());
    }

    #[test]
    fn test_no_auth_validator_with_bearer_token() {
        let validator = NoAuthValidator;
        assert!(validator
            .validate(&Some("Bearer secret123".to_string()))
            .is_ok());
    }
}
