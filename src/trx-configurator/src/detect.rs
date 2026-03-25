// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

/// Detect available serial ports on the system.
/// Returns a list of (path, description) pairs.
pub fn detect_serial_ports() -> Vec<(String, String)> {
    // TODO: use serialport::available_ports() for real detection
    Vec::new()
}
