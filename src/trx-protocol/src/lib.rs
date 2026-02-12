// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Protocol conversion utilities for trx-rs.
//!
//! This crate provides centralized utilities for converting between client and rig protocols,
//! handling authentication tokens, and parsing mode strings.

pub mod auth;
pub mod codec;
pub mod mapping;
pub mod types;

// Re-export commonly used items
pub use auth::{NoAuthValidator, SimpleTokenValidator, TokenValidator};
pub use codec::{mode_to_string, parse_envelope, parse_mode};
pub use mapping::{client_command_to_rig, rig_command_to_client};
pub use types::{ClientCommand, ClientEnvelope, ClientResponse};
