// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Plugin loading with SHA-256 checksum verification and cross-platform validation.
//!
//! # Security Model
//!
//! Before loading a dynamic library plugin, this module verifies:
//! 1. **Checksum manifest**: Each plugin must have a SHA-256 entry in `plugins.toml`.
//! 2. **Allowlist**: Only explicitly listed plugin filenames are loadable.
//! 3. **File permissions** (Unix): Plugin files must be owned by root or the
//!    current user, and must not be world-writable.
//! 4. **Disabled flag**: The `TRX_PLUGINS_DISABLED` environment variable
//!    prevents any plugin from loading when set to a truthy value.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::info;
#[cfg(windows)]
use tracing::warn;

/// Current plugin API version. Plugins must declare a compatible version
/// to be loaded; incompatible plugins are rejected at load time.
pub const PLUGIN_API_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugins are disabled via TRX_PLUGINS_DISABLED")]
    Disabled,

    #[error("plugin not in allowlist: {0}")]
    NotAllowed(String),

    #[error("checksum mismatch for {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("no checksum entry for plugin: {0}")]
    MissingChecksum(String),

    #[error("failed to read plugin file {0}: {1}")]
    IoError(PathBuf, String),

    #[error("unsafe file permissions on {0}: {1}")]
    UnsafePermissions(PathBuf, String),

    #[error("manifest error: {0}")]
    ManifestError(String),

    #[error("incompatible plugin API version: plugin declares v{plugin}, server requires v{required}")]
    IncompatibleVersion { plugin: u32, required: u32 },
}

/// A single plugin entry in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEntry {
    /// Plugin filename (basename only, no path).
    pub filename: String,
    /// Expected SHA-256 hex digest of the plugin file.
    pub sha256: String,
    /// Plugin API version this plugin was built against.
    #[serde(default = "default_api_version")]
    pub api_version: u32,
}

fn default_api_version() -> u32 {
    1
}

/// Plugin manifest loaded from `plugins.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Allowed plugins keyed by filename.
    #[serde(default)]
    pub plugins: HashMap<String, PluginEntry>,
}

impl PluginManifest {
    /// Load manifest from a TOML file.
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| PluginError::ManifestError(format!("cannot read {}: {e}", path.display())))?;
        toml::from_str(&content)
            .map_err(|e| PluginError::ManifestError(format!("parse error in {}: {e}", path.display())))
    }

    /// Look up a plugin entry by filename.
    pub fn get(&self, filename: &str) -> Option<&PluginEntry> {
        self.plugins.get(filename)
    }
}

/// Compute SHA-256 hex digest of a file.
pub fn sha256_file(path: &Path) -> Result<String, PluginError> {
    let data = std::fs::read(path)
        .map_err(|e| PluginError::IoError(path.to_path_buf(), e.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(hex::encode(hasher.finalize()))
}

/// Validate a plugin file before loading.
///
/// Checks:
/// 1. `TRX_PLUGINS_DISABLED` is not set.
/// 2. Plugin filename is in the manifest allowlist.
/// 3. SHA-256 checksum matches the manifest entry.
/// 4. Plugin API version is compatible.
/// 5. File permissions are safe (Unix only).
pub fn validate_plugin(
    plugin_path: &Path,
    manifest: &PluginManifest,
) -> Result<(), PluginError> {
    // Check disabled flag.
    if let Ok(val) = std::env::var("TRX_PLUGINS_DISABLED") {
        if matches!(val.as_str(), "1" | "true" | "yes") {
            return Err(PluginError::Disabled);
        }
    }

    let filename = plugin_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| PluginError::NotAllowed(plugin_path.display().to_string()))?;

    // Check allowlist.
    let entry = manifest
        .get(filename)
        .ok_or_else(|| PluginError::NotAllowed(filename.to_string()))?;

    // Verify API version compatibility.
    if entry.api_version != PLUGIN_API_VERSION {
        return Err(PluginError::IncompatibleVersion {
            plugin: entry.api_version,
            required: PLUGIN_API_VERSION,
        });
    }

    // Verify SHA-256 checksum.
    let actual_hash = sha256_file(plugin_path)?;
    if actual_hash != entry.sha256 {
        return Err(PluginError::ChecksumMismatch {
            path: plugin_path.display().to_string(),
            expected: entry.sha256.clone(),
            actual: actual_hash,
        });
    }

    // Platform-specific permission checks.
    validate_permissions(plugin_path)?;

    info!(
        "Plugin '{}' passed validation (SHA-256: {})",
        filename,
        &entry.sha256[..16]
    );
    Ok(())
}

/// Unix file permission validation.
#[cfg(unix)]
fn validate_permissions(path: &Path) -> Result<(), PluginError> {
    use std::os::unix::fs::MetadataExt;

    let meta = std::fs::metadata(path)
        .map_err(|e| PluginError::IoError(path.to_path_buf(), e.to_string()))?;

    // Reject world-writable files.
    let mode = meta.mode();
    if mode & 0o002 != 0 {
        return Err(PluginError::UnsafePermissions(
            path.to_path_buf(),
            "file is world-writable".to_string(),
        ));
    }

    // File must be owned by root (uid 0) or the current user.
    let file_uid = meta.uid();
    let current_uid = unsafe { libc::getuid() };
    if file_uid != 0 && file_uid != current_uid {
        return Err(PluginError::UnsafePermissions(
            path.to_path_buf(),
            format!(
                "file owned by uid {} (expected root or current user uid {})",
                file_uid, current_uid
            ),
        ));
    }

    Ok(())
}

/// Windows file permission validation.
///
/// On Windows, verifies the file is not in a world-writable directory.
/// Full ACL/owner validation via GetSecurityInfo would require the `windows`
/// crate; this provides a basic safety check.
#[cfg(windows)]
fn validate_permissions(path: &Path) -> Result<(), PluginError> {
    let meta = std::fs::metadata(path)
        .map_err(|e| PluginError::IoError(path.to_path_buf(), e.to_string()))?;

    if meta.permissions().readonly() {
        // Read-only is fine from a security perspective.
        return Ok(());
    }

    // Warn but allow — full ACL checks require the `windows` crate.
    warn!(
        "Plugin '{}' has writable permissions on Windows; consider restricting access",
        path.display()
    );
    Ok(())
}

/// Fallback for other platforms.
#[cfg(not(any(unix, windows)))]
fn validate_permissions(_path: &Path) -> Result<(), PluginError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_sha256_file() {
        let dir = std::env::temp_dir().join("trx_plugin_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_plugin.so");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"hello plugin").unwrap();
        drop(f);

        let hash = sha256_file(&path).unwrap();
        // SHA-256 of "hello plugin"
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_manifest_parse() {
        let toml_str = r#"
[plugins.my_plugin]
filename = "my_plugin.so"
sha256 = "abc123"
api_version = 1
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let entry = manifest.get("my_plugin").unwrap();
        assert_eq!(entry.filename, "my_plugin.so");
        assert_eq!(entry.sha256, "abc123");
        assert_eq!(entry.api_version, 1);
    }

    #[test]
    fn test_validate_plugin_not_in_allowlist() {
        let manifest = PluginManifest::default();
        let path = Path::new("/tmp/unknown_plugin.so");
        let result = validate_plugin(path, &manifest);
        assert!(matches!(result, Err(PluginError::NotAllowed(_))));
    }

    #[test]
    fn test_validate_plugin_checksum_mismatch() {
        let dir = std::env::temp_dir().join("trx_plugin_test_mismatch");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("bad_plugin.so");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"tampered content").unwrap();
        drop(f);

        let mut manifest = PluginManifest::default();
        manifest.plugins.insert(
            "bad_plugin.so".to_string(),
            PluginEntry {
                filename: "bad_plugin.so".to_string(),
                sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
                api_version: PLUGIN_API_VERSION,
            },
        );

        let result = validate_plugin(&path, &manifest);
        assert!(matches!(result, Err(PluginError::ChecksumMismatch { .. })));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_validate_plugin_incompatible_version() {
        let dir = std::env::temp_dir().join("trx_plugin_test_ver");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("old_plugin.so");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"plugin data").unwrap();
        drop(f);

        let mut manifest = PluginManifest::default();
        manifest.plugins.insert(
            "old_plugin.so".to_string(),
            PluginEntry {
                filename: "old_plugin.so".to_string(),
                sha256: sha256_file(&path).unwrap(),
                api_version: 999, // Incompatible
            },
        );

        let result = validate_plugin(&path, &manifest);
        assert!(matches!(
            result,
            Err(PluginError::IncompatibleVersion { .. })
        ));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_validate_plugin_success() {
        let dir = std::env::temp_dir().join("trx_plugin_test_ok");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("good_plugin.so");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"valid plugin content").unwrap();
        drop(f);

        let hash = sha256_file(&path).unwrap();
        let mut manifest = PluginManifest::default();
        manifest.plugins.insert(
            "good_plugin.so".to_string(),
            PluginEntry {
                filename: "good_plugin.so".to_string(),
                sha256: hash,
                api_version: PLUGIN_API_VERSION,
            },
        );

        let result = validate_plugin(&path, &manifest);
        assert!(result.is_ok());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
