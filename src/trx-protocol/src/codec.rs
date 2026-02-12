// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Codec utilities for parsing and formatting modes and envelopes.

use serde_json;

use trx_core::client::{ClientCommand, ClientEnvelope};
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
            Ok(ClientEnvelope { token: None, cmd })
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
}
