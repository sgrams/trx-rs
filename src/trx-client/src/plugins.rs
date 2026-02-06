// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};
use tracing::{info, warn};

const PLUGIN_ENV: &str = "TRX_PLUGIN_DIRS";
const PLUGIN_ENTRYPOINT: &str = "trx_register";

#[cfg(windows)]
const PATH_SEPARATOR: char = ';';
#[cfg(not(windows))]
const PATH_SEPARATOR: char = ':';

#[cfg(windows)]
const PLUGIN_EXTENSIONS: &[&str] = &["dll"];
#[cfg(target_os = "macos")]
const PLUGIN_EXTENSIONS: &[&str] = &["dylib"];
#[cfg(all(unix, not(target_os = "macos")))]
const PLUGIN_EXTENSIONS: &[&str] = &["so"];

pub fn load_plugins() -> Vec<Library> {
    let mut libraries = Vec::new();
    let search_paths = plugin_search_paths();

    if search_paths.is_empty() {
        return libraries;
    }

    info!("Plugin search paths: {:?}", search_paths);

    for path in search_paths {
        if let Err(err) = load_plugins_from_dir(&path, &mut libraries) {
            warn!("Plugin scan failed for {:?}: {}", path, err);
        }
    }

    libraries
}

fn load_plugins_from_dir(path: &Path, libraries: &mut Vec<Library>) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !is_plugin_file(&path) {
            continue;
        }

        unsafe {
            match Library::new(&path) {
                Ok(lib) => {
                    if let Err(err) = register_library(&lib, &path) {
                        warn!("Plugin {:?} failed to register: {}", path, err);
                        continue;
                    }
                    info!("Loaded plugin {:?}", path);
                    libraries.push(lib);
                }
                Err(err) => {
                    warn!("Failed to load plugin {:?}: {}", path, err);
                }
            }
        }
    }

    Ok(())
}

unsafe fn register_library(lib: &Library, path: &Path) -> Result<(), String> {
    let entry: Symbol<unsafe extern "C" fn()> = lib
        .get(PLUGIN_ENTRYPOINT.as_bytes())
        .map_err(|e| format!("missing entrypoint {}: {}", PLUGIN_ENTRYPOINT, e))?;
    entry();
    info!("Registered plugin {:?}", path);
    Ok(())
}

fn plugin_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(env_paths) = std::env::var(PLUGIN_ENV) {
        for raw in env_paths.split(PATH_SEPARATOR) {
            if raw.trim().is_empty() {
                continue;
            }
            paths.push(PathBuf::from(raw));
        }
    }

    paths.push(PathBuf::from("plugins"));

    if let Some(config_dir) = dirs::config_dir() {
        paths.push(config_dir.join("trx-rs").join("plugins"));
    }

    paths
}

fn is_plugin_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| PLUGIN_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}
