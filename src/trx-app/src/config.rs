// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to read config file {0}: {1}")]
    ReadError(PathBuf, String),

    #[error("Failed to parse config file {0}: {1}")]
    ParseError(PathBuf, String),
}

/// Returns the default search paths for `trx-rs.toml`
/// (current directory → XDG config → /etc).
fn config_search_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("trx-rs.toml")];
    if let Some(config_dir) = dirs::config_dir() {
        paths.push(config_dir.join("trx-rs").join("trx-rs.toml"));
    }
    paths.push(PathBuf::from("/etc/trx-rs/trx-rs.toml"));
    paths
}

/// Extract and deserialize a named section from a TOML file.
///
/// Returns `Ok(Some(cfg))` when the section is present and parses cleanly,
/// `Ok(None)` when the section is absent, or `Err` on I/O / parse failure.
fn load_section_from_file<T: DeserializeOwned>(
    path: &Path,
    key: &str,
) -> Result<Option<T>, ConfigError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::ReadError(path.to_path_buf(), e.to_string()))?;

    let table: toml::Table = toml::from_str(&content)
        .map_err(|e| ConfigError::ParseError(path.to_path_buf(), e.to_string()))?;

    let Some(section) = table.get(key) else {
        return Ok(None);
    };

    // Re-serialize the section then parse as T so all serde defaults apply.
    let section_toml = toml::to_string(section)
        .map_err(|e| ConfigError::ParseError(path.to_path_buf(), e.to_string()))?;
    let cfg = toml::from_str::<T>(&section_toml)
        .map_err(|e| ConfigError::ParseError(path.to_path_buf(), e.to_string()))?;
    Ok(Some(cfg))
}

/// Trait for loading configuration from a `trx-rs.toml` section.
pub trait ConfigFile: Sized + Default + DeserializeOwned {
    /// Section key in `trx-rs.toml` (e.g. `"trx-server"` or `"trx-client"`).
    fn section_key() -> &'static str;

    /// Load the section from a specific file path.
    ///
    /// Returns an error if the file cannot be read, is not valid TOML, or
    /// does not contain the expected `[<section_key>]` header.
    fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        load_section_from_file::<Self>(path, Self::section_key())?.ok_or_else(|| {
            ConfigError::ParseError(
                path.to_path_buf(),
                format!("missing [{}] section", Self::section_key()),
            )
        })
    }

    /// Search default paths (`trx-rs.toml` in CWD → XDG → /etc) and load
    /// the first file that contains the expected section.
    ///
    /// Returns `(config, path_where_found)` or `(Default::default(), None)`
    /// when no config file is found.
    fn load_from_default_paths() -> Result<(Self, Option<PathBuf>), ConfigError> {
        for path in config_search_paths() {
            if path.exists() {
                if let Some(cfg) = load_section_from_file::<Self>(&path, Self::section_key())? {
                    return Ok((cfg, Some(path)));
                }
            }
        }
        Ok((Self::default(), None))
    }
}
