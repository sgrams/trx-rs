// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct WsprdWrapper {
    binary: String,
}

impl WsprdWrapper {
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub fn default_binary() -> Self {
        Self::new("wsprd")
    }

    pub fn is_available(&self) -> bool {
        Command::new(&self.binary).output().is_ok()
    }

    pub fn decode_wav(&self, wav_path: &Path) -> Result<String, String> {
        let output = Command::new(&self.binary)
            .arg(wav_path)
            .output()
            .map_err(|e| format!("failed to run {}: {}", self.binary, e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "{} failed with status {}: {}",
                self.binary,
                output.status,
                stderr.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}
