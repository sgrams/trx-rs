// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use libloading::{Library, Symbol};
use tracing::{info, warn};

const PLUGIN_ENV: &str = "TRX_PLUGIN_DIRS";
const BACKEND_ENTRYPOINT: &str = "trx_register_backend";
const FRONTEND_ENTRYPOINT: &str = "trx_register_frontend";

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

pub fn load_backend_plugins(context: NonNull<std::ffi::c_void>) -> Vec<Library> {
    load_plugins_for_entrypoint(BACKEND_ENTRYPOINT, context)
}

pub fn load_frontend_plugins(context: NonNull<std::ffi::c_void>) -> Vec<Library> {
    load_plugins_for_entrypoint(FRONTEND_ENTRYPOINT, context)
}

fn load_plugins_for_entrypoint(
    entrypoint: &str,
    context: NonNull<std::ffi::c_void>,
) -> Vec<Library> {
    let mut libraries = Vec::new();
    let search_paths = plugin_search_paths();

    if search_paths.is_empty() {
        return libraries;
    }

    info!("Plugin search paths: {:?}", search_paths);

    for path in search_paths {
        if let Err(err) = load_plugins_from_dir(&path, entrypoint, context, &mut libraries) {
            warn!("Plugin scan failed for {:?}: {}", path, err);
        }
    }

    libraries
}

fn load_plugins_from_dir(
    path: &Path,
    entrypoint: &str,
    context: NonNull<std::ffi::c_void>,
    libraries: &mut Vec<Library>,
) -> std::io::Result<()> {
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
                    if let Err(err) = register_library(&lib, &path, entrypoint, context) {
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

unsafe fn register_library(
    lib: &Library,
    path: &Path,
    entrypoint: &str,
    context: NonNull<std::ffi::c_void>,
) -> Result<(), String> {
    let entry: Symbol<unsafe extern "C" fn(*mut std::ffi::c_void)> = lib
        .get(entrypoint.as_bytes())
        .map_err(|e| format!("missing entrypoint {}: {}", entrypoint, e))?;
    entry(context.as_ptr());
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
        .map(|ext| {
            PLUGIN_EXTENSIONS
                .iter()
                .any(|e| ext.eq_ignore_ascii_case(e))
        })
        .unwrap_or(false)
}
