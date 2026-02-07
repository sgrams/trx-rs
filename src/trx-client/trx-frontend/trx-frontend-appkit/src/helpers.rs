// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use trx_core::rig::state::{RigMode, RigState};

pub fn format_freq(hz: u64) -> String {
    if hz >= 1_000_000_000 {
        format!("{:.3} GHz", hz as f64 / 1_000_000_000.0)
    } else if hz >= 10_000_000 {
        format!("{:.3} MHz", hz as f64 / 1_000_000.0)
    } else if hz >= 1_000 {
        format!("{:.1} kHz", hz as f64 / 1_000.0)
    } else {
        format!("{hz} Hz")
    }
}

pub fn mode_label(mode: &RigMode) -> String {
    match mode {
        RigMode::LSB => "LSB".to_string(),
        RigMode::USB => "USB".to_string(),
        RigMode::CW => "CW".to_string(),
        RigMode::CWR => "CWR".to_string(),
        RigMode::AM => "AM".to_string(),
        RigMode::WFM => "WFM".to_string(),
        RigMode::FM => "FM".to_string(),
        RigMode::DIG => "DIG".to_string(),
        RigMode::PKT => "PKT".to_string(),
        RigMode::Other(val) => val.clone(),
    }
}

pub fn parse_mode(value: &str) -> RigMode {
    match value.trim().to_uppercase().as_str() {
        "LSB" => RigMode::LSB,
        "USB" => RigMode::USB,
        "CW" => RigMode::CW,
        "CWR" => RigMode::CWR,
        "AM" => RigMode::AM,
        "FM" => RigMode::FM,
        "WFM" => RigMode::WFM,
        "DIG" | "DIGI" => RigMode::DIG,
        "PKT" | "PACKET" => RigMode::PKT,
        other => RigMode::Other(other.to_string()),
    }
}

pub fn vfo_label(state: &RigState) -> String {
    let Some(vfo) = state.status.vfo.as_ref() else {
        return "--".to_string();
    };

    let mut lines = Vec::new();
    for (idx, entry) in vfo.entries.iter().enumerate() {
        let marker = if vfo.active == Some(idx) { "*" } else { " " };
        let freq = format_freq(entry.freq.hz);
        let mode = entry
            .mode
            .as_ref()
            .map(mode_label)
            .unwrap_or_else(|| "--".to_string());
        lines.push(format!("{marker} {}: {} {}", entry.name, freq, mode));
    }
    lines.join("\n")
}
