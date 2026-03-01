// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Codec utilities for parsing and formatting modes and envelopes.

use serde_json;

use crate::types::{ClientCommand, ClientEnvelope};
use trx_core::rig::state::RigMode;

/// Parse a mode string into a RigMode.
///
/// Handles LSB, USB, CW, CWR, AM, FM, WFM, DIG, DIGI, PKT, PACKET.
/// Falls back to Other(string) for unknown modes.
pub fn parse_mode(s: &str) -> RigMode {
    match s.to_uppercase().as_str() {
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

/// Convert a RigMode back to its string representation.
///
/// This is the inverse of parse_mode. Standard modes return their uppercase names,
/// and Other variants return their inner string.
pub fn mode_to_string(mode: &RigMode) -> String {
    match mode {
        RigMode::LSB => "LSB".to_string(),
        RigMode::USB => "USB".to_string(),
        RigMode::CW => "CW".to_string(),
        RigMode::CWR => "CWR".to_string(),
        RigMode::AM => "AM".to_string(),
        RigMode::FM => "FM".to_string(),
        RigMode::WFM => "WFM".to_string(),
        RigMode::DIG => "DIG".to_string(),
        RigMode::PKT => "PKT".to_string(),
        RigMode::Other(s) => s.clone(),
    }
}

/// Parse a JSON string into a ClientEnvelope.
///
/// First tries to parse as a full ClientEnvelope.
/// If that fails, tries to parse as a bare ClientCommand and wraps it with token: None.
pub fn parse_envelope(input: &str) -> Result<ClientEnvelope, serde_json::Error> {
    match serde_json::from_str::<ClientEnvelope>(input) {
        Ok(envelope) => Ok(envelope),
        Err(_) => {
            let cmd = serde_json::from_str::<ClientCommand>(input)?;
            Ok(ClientEnvelope {
                token: None,
                rig_id: None,
                cmd,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mode_standard_modes() {
        assert_eq!(parse_mode("LSB"), RigMode::LSB);
        assert_eq!(parse_mode("USB"), RigMode::USB);
        assert_eq!(parse_mode("CW"), RigMode::CW);
        assert_eq!(parse_mode("CWR"), RigMode::CWR);
        assert_eq!(parse_mode("AM"), RigMode::AM);
        assert_eq!(parse_mode("FM"), RigMode::FM);
        assert_eq!(parse_mode("WFM"), RigMode::WFM);
    }

    #[test]
    fn test_parse_mode_aliases() {
        assert_eq!(parse_mode("DIG"), RigMode::DIG);
        assert_eq!(parse_mode("DIGI"), RigMode::DIG);
        assert_eq!(parse_mode("PKT"), RigMode::PKT);
        assert_eq!(parse_mode("PACKET"), RigMode::PKT);
    }

    #[test]
    fn test_parse_mode_case_insensitive() {
        assert_eq!(parse_mode("lsb"), RigMode::LSB);
        assert_eq!(parse_mode("Usb"), RigMode::USB);
        assert_eq!(parse_mode("cw"), RigMode::CW);
    }

    #[test]
    fn test_parse_mode_unknown() {
        if let RigMode::Other(s) = parse_mode("UNKNOWN") {
            assert_eq!(s, "UNKNOWN");
        } else {
            panic!("Expected Other variant");
        }
    }

    #[test]
    fn test_parse_mode_empty() {
        if let RigMode::Other(s) = parse_mode("") {
            assert_eq!(s, "");
        } else {
            panic!("Expected Other variant");
        }
    }

    #[test]
    fn test_mode_to_string_standard_modes() {
        assert_eq!(mode_to_string(&RigMode::LSB), "LSB");
        assert_eq!(mode_to_string(&RigMode::USB), "USB");
        assert_eq!(mode_to_string(&RigMode::CW), "CW");
        assert_eq!(mode_to_string(&RigMode::CWR), "CWR");
        assert_eq!(mode_to_string(&RigMode::AM), "AM");
        assert_eq!(mode_to_string(&RigMode::FM), "FM");
        assert_eq!(mode_to_string(&RigMode::WFM), "WFM");
        assert_eq!(mode_to_string(&RigMode::DIG), "DIG");
        assert_eq!(mode_to_string(&RigMode::PKT), "PKT");
    }

    #[test]
    fn test_mode_to_string_other() {
        assert_eq!(mode_to_string(&RigMode::Other("XYZ".to_string())), "XYZ");
    }

    #[test]
    fn test_mode_round_trip() {
        let modes = vec![
            RigMode::LSB,
            RigMode::USB,
            RigMode::CW,
            RigMode::CWR,
            RigMode::AM,
            RigMode::FM,
            RigMode::WFM,
            RigMode::DIG,
            RigMode::PKT,
        ];

        for mode in modes {
            let s = mode_to_string(&mode);
            let parsed = parse_mode(&s);
            assert_eq!(parsed, mode, "Round trip failed for {:?}", mode);
        }
    }

    #[test]
    fn test_parse_envelope_full_envelope() {
        let json = r#"{"token":"abc123","cmd":"get_state"}"#;
        let envelope = parse_envelope(json).unwrap();
        assert_eq!(envelope.token, Some("abc123".to_string()));
        assert!(matches!(envelope.cmd, ClientCommand::GetState));
    }

    #[test]
    fn test_parse_envelope_bare_command() {
        let json = r#"{"cmd":"get_state"}"#;
        let envelope = parse_envelope(json).unwrap();
        assert_eq!(envelope.token, None);
        assert!(matches!(envelope.cmd, ClientCommand::GetState));
    }

    #[test]
    fn test_parse_envelope_bare_command_with_params() {
        let json = r#"{"cmd":"set_freq","freq_hz":14100000}"#;
        let envelope = parse_envelope(json).unwrap();
        assert_eq!(envelope.token, None);
        if let ClientCommand::SetFreq { freq_hz } = envelope.cmd {
            assert_eq!(freq_hz, 14100000);
        } else {
            panic!("Expected SetFreq variant");
        }
    }

    #[test]
    fn test_parse_envelope_invalid_json() {
        let json = "not valid json";
        let result = parse_envelope(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_envelope_invalid_command() {
        let json = r#"{"cmd":"invalid_command"}"#;
        let result = parse_envelope(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_envelope_with_bearer_token() {
        let json = r#"{"token":"Bearer abc123xyz","cmd":"get_state"}"#;
        let envelope = parse_envelope(json).unwrap();
        assert_eq!(envelope.token, Some("Bearer abc123xyz".to_string()));
    }

    // --- MR-09: multi-rig protocol tests ---

    #[test]
    fn test_parse_envelope_absent_rig_id_defaults_to_none() {
        let json = r#"{"cmd":"get_state"}"#;
        let envelope = parse_envelope(json).unwrap();
        assert_eq!(envelope.rig_id, None, "absent rig_id should parse as None");
    }

    #[test]
    fn test_parse_envelope_with_rig_id() {
        let json = r#"{"rig_id":"hf","cmd":"get_state"}"#;
        let envelope = parse_envelope(json).unwrap();
        assert_eq!(envelope.rig_id, Some("hf".to_string()));
        assert!(matches!(envelope.cmd, ClientCommand::GetState));
    }

    #[test]
    fn test_parse_envelope_get_rigs_command() {
        let json = r#"{"cmd":"get_rigs"}"#;
        let envelope = parse_envelope(json).unwrap();
        assert!(matches!(envelope.cmd, ClientCommand::GetRigs));
        assert_eq!(envelope.rig_id, None);
    }

    #[test]
    fn test_parse_envelope_get_rigs_with_rig_id_ignored() {
        // rig_id is parsed and available even though GetRigs is intercepted
        // before routing — the listener should ignore it for this command.
        let json = r#"{"rig_id":"sdr","cmd":"get_rigs"}"#;
        let envelope = parse_envelope(json).unwrap();
        assert!(matches!(envelope.cmd, ClientCommand::GetRigs));
        assert_eq!(envelope.rig_id, Some("sdr".to_string()));
    }

    #[test]
    fn test_client_response_rig_id_roundtrip() {
        use crate::types::ClientResponse;
        let resp = ClientResponse {
            success: true,
            rig_id: Some("hf".to_string()),
            state: None,
            rigs: None,
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""rig_id":"hf""#));
        let decoded: ClientResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.rig_id, Some("hf".to_string()));
    }

    #[test]
    fn test_client_response_omits_rig_id_when_none() {
        use crate::types::ClientResponse;
        let resp = ClientResponse {
            success: false,
            rig_id: None,
            state: None,
            rigs: None,
            error: Some("bad".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            !json.contains("rig_id"),
            "rig_id=None should be omitted from JSON"
        );
    }

    #[test]
    fn test_client_response_omits_rigs_when_none() {
        use crate::types::ClientResponse;
        let resp = ClientResponse {
            success: true,
            rig_id: Some("server".to_string()),
            state: None,
            rigs: None,
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            !json.contains("\"rigs\""),
            "rigs=None should be omitted from JSON"
        );
    }

    // --- UC-09: filter field serialization tests ---

    #[test]
    fn filter_field_included_when_some() {
        use trx_core::rig::state::RigSnapshot;
        use trx_core::RigFilterState;
        let snap_json = serde_json::to_string(&RigSnapshot {
            filter: Some(RigFilterState {
                bandwidth_hz: 3000,
                fir_taps: 64,
                cw_center_hz: 700,
                sdr_gain_db: Some(12.0),
                wfm_deemphasis_us: 75,
                wfm_stereo: true,
                wfm_stereo_detected: false,
                wfm_denoise: true,
            }),
            ..minimal_snapshot()
        })
        .unwrap();
        assert!(
            snap_json.contains("\"filter\""),
            "filter=Some should be serialized"
        );
        assert!(snap_json.contains("\"bandwidth_hz\":3000"));
        assert!(snap_json.contains("\"fir_taps\":64"));
    }

    #[test]
    fn filter_field_omitted_when_none() {
        use trx_core::rig::state::RigSnapshot;
        let snap_json = serde_json::to_string(&RigSnapshot {
            filter: None,
            ..minimal_snapshot()
        })
        .unwrap();
        assert!(
            !snap_json.contains("\"filter\""),
            "filter=None should be omitted from JSON"
        );
    }

    #[test]
    fn filter_field_roundtrips() {
        use trx_core::rig::state::RigSnapshot;
        use trx_core::RigFilterState;
        let orig = RigSnapshot {
            filter: Some(RigFilterState {
                bandwidth_hz: 12000,
                fir_taps: 128,
                cw_center_hz: 700,
                sdr_gain_db: Some(18.0),
                wfm_deemphasis_us: 50,
                wfm_stereo: true,
                wfm_stereo_detected: true,
                wfm_denoise: true,
            }),
            ..minimal_snapshot()
        };
        let json = serde_json::to_string(&orig).unwrap();
        let decoded: RigSnapshot = serde_json::from_str(&json).unwrap();
        let f = decoded.filter.expect("filter should round-trip");
        assert_eq!(f.bandwidth_hz, 12000);
        assert_eq!(f.fir_taps, 128);
        assert_eq!(f.sdr_gain_db, Some(18.0));
        assert_eq!(f.wfm_deemphasis_us, 50);
        assert!(f.wfm_stereo_detected);
    }

    fn minimal_snapshot() -> trx_core::rig::state::RigSnapshot {
        use trx_core::radio::freq::{Band, Freq};
        use trx_core::rig::state::{RigMode, RigSnapshot};
        use trx_core::rig::{RigAccessMethod, RigCapabilities, RigInfo, RigStatus};
        RigSnapshot {
            info: RigInfo {
                manufacturer: "Test".to_string(),
                model: "Mock".to_string(),
                revision: "1".to_string(),
                capabilities: RigCapabilities {
                    min_freq_step_hz: 1,
                    supported_bands: vec![Band {
                        low_hz: 14_000_000,
                        high_hz: 14_350_000,
                        tx_allowed: true,
                    }],
                    supported_modes: vec![RigMode::USB],
                    num_vfos: 1,
                    lock: false,
                    lockable: false,
                    attenuator: false,
                    preamp: false,
                    rit: false,
                    rpt: false,
                    split: false,
                    tx: false,
                    tx_limit: false,
                    vfo_switch: false,
                    filter_controls: true,
                    signal_meter: true,
                },
                access: RigAccessMethod::Tcp {
                    addr: "127.0.0.1:1234".to_string(),
                },
            },
            status: RigStatus {
                freq: Freq { hz: 14_074_000 },
                mode: RigMode::USB,
                tx_en: false,
                vfo: None,
                tx: None,
                rx: None,
                lock: None,
            },
            band: None,
            enabled: None,
            initialized: true,
            server_callsign: None,
            server_version: None,
            server_build_date: None,
            server_latitude: None,
            server_longitude: None,
            pskreporter_status: None,
            aprs_decode_enabled: false,
            cw_decode_enabled: false,
            ft8_decode_enabled: false,
            wspr_decode_enabled: false,
            cw_auto: false,
            cw_wpm: 0,
            cw_tone_hz: 0,
            filter: None,
            spectrum: None,
        }
    }
}
