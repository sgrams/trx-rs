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

/// Trait for loading configuration files with default paths.
pub trait ConfigFile: Sized + Default + DeserializeOwned {
    /// Config filename (e.g., "server.toml" or "client.toml")
    fn config_filename() -> &'static str;

    /// Load config from specific path
    fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::ReadError(path.to_path_buf(), e.to_string()))?;

        toml::from_str(&content)
            .map_err(|e| ConfigError::ParseError(path.to_path_buf(), e.to_string()))
    }

    /// Search default paths and load first found config.
    /// Returns (config, path_where_found) or (Default::default(), None) if not found.
    fn load_from_default_paths() -> Result<(Self, Option<PathBuf>), ConfigError> {
        for path in Self::default_search_paths() {
            if path.exists() {
                let cfg = Self::load_from_file(&path)?;
                return Ok((cfg, Some(path)));
            }
        }
        Ok((Self::default(), None))
    }

    /// Default search paths (current dir → XDG → /etc)
    fn default_search_paths() -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from(Self::config_filename())];

        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("trx-rs").join(Self::config_filename()));
        }

        paths.push(PathBuf::from("/etc/trx-rs").join(Self::config_filename()));
        paths
    }
}
