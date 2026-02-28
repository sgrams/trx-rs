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

/// Returns search paths for the combined `trx-rs.toml` config file
/// (current directory → XDG config → /etc).
pub fn combined_config_paths() -> Vec<PathBuf> {
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

/// Trait for loading configuration files with default paths.
pub trait ConfigFile: Sized + Default + DeserializeOwned {
    /// Config filename (e.g., "server.toml" or "client.toml")
    fn config_filename() -> &'static str;

    /// Section key inside a combined `trx-rs.toml` file, e.g. `"trx-server"`.
    /// Return `None` (the default) to disable combined-file support.
    fn combined_key() -> Option<&'static str> {
        None
    }

    /// Load config from a specific file path.
    ///
    /// If `combined_key()` is set and the file contains that section header,
    /// only that section is deserialized.  Otherwise the whole file is used,
    /// preserving full backward compatibility with per-binary config files.
    fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        if let Some(key) = Self::combined_key() {
            // Peek at the file: if it contains our section, use that section.
            if let Ok(Some(cfg)) = load_section_from_file::<Self>(path, key) {
                return Ok(cfg);
            }
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::ReadError(path.to_path_buf(), e.to_string()))?;
        toml::from_str(&content)
            .map_err(|e| ConfigError::ParseError(path.to_path_buf(), e.to_string()))
    }

    /// Search default paths and load first found config.
    ///
    /// Search order (for each location tier — CWD, XDG, /etc):
    ///   1. `trx-rs.toml`  with our section header  (combined file)
    ///   2. per-binary flat file (e.g. `trx-server.toml`)
    ///
    /// Returns `(config, path_where_found)` or `(Default::default(), None)`.
    fn load_from_default_paths() -> Result<(Self, Option<PathBuf>), ConfigError> {
        let combined = combined_config_paths();
        let flat = Self::default_search_paths();

        // Build interleaved list: (combined_path, flat_path) per tier.
        let tiers = combined.len().max(flat.len());
        for i in 0..tiers {
            // Combined file at this tier
            if let Some(key) = Self::combined_key() {
                if let Some(path) = combined.get(i) {
                    if path.exists() {
                        if let Some(cfg) = load_section_from_file::<Self>(path, key)? {
                            return Ok((cfg, Some(path.clone())));
                        }
                        // Combined file present but our section absent → skip to flat.
                    }
                }
            }
            // Flat file at this tier
            if let Some(path) = flat.get(i) {
                if path.exists() {
                    let cfg = Self::load_from_file(path)?;
                    return Ok((cfg, Some(path.clone())));
                }
            }
        }
        Ok((Self::default(), None))
    }

    /// Default search paths for the per-binary flat config file
    /// (current dir → XDG → /etc).
    fn default_search_paths() -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from(Self::config_filename())];

        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("trx-rs").join(Self::config_filename()));
        }

        paths.push(PathBuf::from("/etc/trx-rs").join(Self::config_filename()));
        paths
    }
}
