// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Centralised decoder registry.
//!
//! Every decoder supported by trx-rs is described exactly once here.
//! Backend, frontend, scheduler, and background-decode code all derive
//! their decoder knowledge from [`DECODER_REGISTRY`].

use serde::Serialize;

// ============================================================================
// Types
// ============================================================================

/// How a decoder is activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecoderActivation {
    /// Automatically active when the rig mode matches.
    ModeBound,
    /// User-controlled toggle; only runs in `active_modes`.
    Toggle,
}

/// Static descriptor for a single decoder.
#[derive(Debug, Clone, Serialize)]
pub struct DecoderDescriptor {
    /// Machine identifier, e.g. `"ft8"`, `"aprs"`.
    pub id: &'static str,
    /// Human-readable label, e.g. `"FT8"`, `"APRS"`.
    pub label: &'static str,
    /// How the decoder is activated.
    pub activation: DecoderActivation,
    /// Rig modes where this decoder operates (upper-case).
    pub active_modes: &'static [&'static str],
    /// Whether the decoder can run on SDR virtual channels
    /// (background-decode / scheduler).
    pub background_decode: bool,
    /// Whether this decoder should appear in bookmark forms.
    pub bookmark_selectable: bool,
}

// ============================================================================
// Registry
// ============================================================================

pub const DECODER_REGISTRY: &[DecoderDescriptor] = &[
    // -- Mode-bound decoders (auto-active when mode matches) -----------------
    DecoderDescriptor {
        id: "ais",
        label: "AIS",
        activation: DecoderActivation::ModeBound,
        active_modes: &["AIS"],
        background_decode: true,
        bookmark_selectable: true,
    },
    DecoderDescriptor {
        id: "aprs",
        label: "APRS",
        activation: DecoderActivation::ModeBound,
        active_modes: &["PKT"],
        background_decode: true,
        bookmark_selectable: true,
    },
    DecoderDescriptor {
        id: "vdes",
        label: "VDES",
        activation: DecoderActivation::ModeBound,
        active_modes: &["VDES"],
        background_decode: false,
        bookmark_selectable: false,
    },
    DecoderDescriptor {
        id: "cw",
        label: "CW",
        activation: DecoderActivation::ModeBound,
        active_modes: &["CW", "CWR"],
        background_decode: false,
        bookmark_selectable: false,
    },
    // -- Toggle-gated decoders (user enables/disables) -----------------------
    DecoderDescriptor {
        id: "ft8",
        label: "FT8",
        activation: DecoderActivation::Toggle,
        active_modes: &["DIG", "USB"],
        background_decode: true,
        bookmark_selectable: true,
    },
    DecoderDescriptor {
        id: "ft4",
        label: "FT4",
        activation: DecoderActivation::Toggle,
        active_modes: &["DIG", "USB"],
        background_decode: true,
        bookmark_selectable: true,
    },
    DecoderDescriptor {
        id: "ft2",
        label: "FT2",
        activation: DecoderActivation::Toggle,
        active_modes: &["DIG", "USB"],
        background_decode: true,
        bookmark_selectable: true,
    },
    DecoderDescriptor {
        id: "wspr",
        label: "WSPR",
        activation: DecoderActivation::Toggle,
        active_modes: &["DIG", "USB"],
        background_decode: true,
        bookmark_selectable: true,
    },
    DecoderDescriptor {
        id: "hf-aprs",
        label: "HF APRS",
        activation: DecoderActivation::Toggle,
        active_modes: &["DIG", "USB"],
        background_decode: true,
        bookmark_selectable: true,
    },
    DecoderDescriptor {
        id: "lrpt",
        label: "Meteor LRPT",
        activation: DecoderActivation::Toggle,
        active_modes: &["DIG", "USB"],
        background_decode: false,
        bookmark_selectable: true,
    },
];

// ============================================================================
// Helpers
// ============================================================================

/// Resolve a bookmark's effective decoder kinds.
///
/// If `explicit_decoders` is non-empty, filters them to known IDs (optionally
/// restricting to background-capable decoders).  Otherwise infers decoders
/// from the bookmark `mode` using mode-bound entries in the registry.
pub fn resolve_bookmark_decoders(
    explicit_decoders: &[String],
    mode: &str,
    background_only: bool,
) -> Vec<String> {
    let from_explicit: Vec<String> = explicit_decoders
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| {
            DECODER_REGISTRY
                .iter()
                .any(|d| d.id == s.as_str() && (!background_only || d.background_decode))
        })
        .fold(Vec::new(), |mut acc, s| {
            if !acc.contains(&s) {
                acc.push(s);
            }
            acc
        });

    if !from_explicit.is_empty() {
        return from_explicit;
    }

    // Fall back: infer from mode via mode-bound decoders.
    let mode_upper = mode.trim().to_ascii_uppercase();
    DECODER_REGISTRY
        .iter()
        .filter(|d| {
            d.activation == DecoderActivation::ModeBound
                && (!background_only || d.background_decode)
                && d.active_modes.contains(&mode_upper.as_str())
        })
        .map(|d| d.id.to_string())
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_decoders_filtered() {
        let result =
            resolve_bookmark_decoders(&["ft8".into(), "bogus".into(), "ft4".into()], "USB", false);
        assert_eq!(result, vec!["ft8", "ft4"]);
    }

    #[test]
    fn explicit_decoders_deduped() {
        let result = resolve_bookmark_decoders(&["ft8".into(), "FT8".into()], "USB", false);
        assert_eq!(result, vec!["ft8"]);
    }

    #[test]
    fn mode_fallback_ais() {
        let result = resolve_bookmark_decoders(&[], "AIS", false);
        assert_eq!(result, vec!["ais"]);
    }

    #[test]
    fn mode_fallback_pkt() {
        let result = resolve_bookmark_decoders(&[], "PKT", false);
        assert_eq!(result, vec!["aprs"]);
    }

    #[test]
    fn mode_fallback_unknown() {
        let result = resolve_bookmark_decoders(&[], "USB", false);
        assert!(result.is_empty());
    }

    #[test]
    fn background_only_filters_lrpt() {
        let result = resolve_bookmark_decoders(&["lrpt".into(), "ft8".into()], "DIG", true);
        assert_eq!(result, vec!["ft8"]);
    }

    #[test]
    fn background_only_mode_fallback_excludes_cw() {
        // CW is mode-bound but not background_decode capable.
        let result = resolve_bookmark_decoders(&[], "CW", true);
        assert!(result.is_empty());
    }

    #[test]
    fn registry_ids_unique() {
        let mut seen = std::collections::HashSet::new();
        for d in DECODER_REGISTRY {
            assert!(seen.insert(d.id), "duplicate decoder id: {}", d.id);
        }
    }
}
