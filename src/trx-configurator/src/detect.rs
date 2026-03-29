// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

/// Detect available serial ports on the system.
/// Returns a list of (path, description) pairs.
pub fn detect_serial_ports() -> Vec<(String, String)> {
    match tokio_serial::available_ports() {
        Ok(ports) => ports
            .into_iter()
            .map(|p| {
                let desc = match &p.port_type {
                    tokio_serial::SerialPortType::UsbPort(usb) => {
                        let mut parts = Vec::new();
                        if let Some(m) = &usb.manufacturer {
                            parts.push(m.clone());
                        }
                        if let Some(prod) = &usb.product {
                            parts.push(prod.clone());
                        }
                        if parts.is_empty() {
                            format!("USB (VID:{:04X} PID:{:04X})", usb.vid, usb.pid)
                        } else {
                            parts.join(" ")
                        }
                    }
                    tokio_serial::SerialPortType::BluetoothPort => "Bluetooth".to_string(),
                    tokio_serial::SerialPortType::PciPort => "PCI".to_string(),
                    tokio_serial::SerialPortType::Unknown => "Unknown".to_string(),
                };
                (p.port_name, desc)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_vec() {
        // Just verify it doesn't panic; actual ports depend on hardware.
        let ports = detect_serial_ports();
        // Result is a Vec, might be empty on CI.
        assert!(ports.len() >= 0);
    }
}
