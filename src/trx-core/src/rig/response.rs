// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use serde::Serialize;

/// Error type returned by rig requests.
#[derive(Debug, Clone, Serialize)]
pub struct RigError(pub String);

pub type RigResult<T> = Result<T, RigError>;

impl From<String> for RigError {
    fn from(value: String) -> Self {
        RigError(value)
    }
}

impl From<&str> for RigError {
    fn from(value: &str) -> Self {
        RigError(value.to_string())
    }
}
