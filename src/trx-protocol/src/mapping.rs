// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Bidirectional command mapping between ClientCommand and RigCommand.

use trx_core::radio::freq::Freq;
use trx_core::rig::command::RigCommand;

use crate::codec::{mode_to_string, parse_mode};
use crate::types::ClientCommand;

/// Convert a ClientCommand to a RigCommand.
///
/// This maps client-side commands to internal rig commands, parsing
/// mode strings into RigMode values.
pub fn client_command_to_rig(cmd: ClientCommand) -> RigCommand {
    match cmd {
        ClientCommand::GetState => RigCommand::GetSnapshot,
        ClientCommand::SetFreq { freq_hz } => RigCommand::SetFreq(Freq { hz: freq_hz }),
        ClientCommand::SetMode { mode } => RigCommand::SetMode(parse_mode(&mode)),
        ClientCommand::SetPtt { ptt } => RigCommand::SetPtt(ptt),
        ClientCommand::PowerOn => RigCommand::PowerOn,
        ClientCommand::PowerOff => RigCommand::PowerOff,
        ClientCommand::ToggleVfo => RigCommand::ToggleVfo,
        ClientCommand::Lock => RigCommand::Lock,
        ClientCommand::Unlock => RigCommand::Unlock,
        ClientCommand::GetTxLimit => RigCommand::GetTxLimit,
        ClientCommand::SetTxLimit { limit } => RigCommand::SetTxLimit(limit),
        ClientCommand::SetAprsDecodeEnabled { enabled } => RigCommand::SetAprsDecodeEnabled(enabled),
        ClientCommand::SetCwDecodeEnabled { enabled } => RigCommand::SetCwDecodeEnabled(enabled),
        ClientCommand::SetCwAuto { enabled } => RigCommand::SetCwAuto(enabled),
        ClientCommand::SetCwWpm { wpm } => RigCommand::SetCwWpm(wpm),
        ClientCommand::SetCwToneHz { tone_hz } => RigCommand::SetCwToneHz(tone_hz),
        ClientCommand::SetFt8DecodeEnabled { enabled } => RigCommand::SetFt8DecodeEnabled(enabled),
        ClientCommand::ResetAprsDecoder => RigCommand::ResetAprsDecoder,
        ClientCommand::ResetCwDecoder => RigCommand::ResetCwDecoder,
        ClientCommand::ResetFt8Decoder => RigCommand::ResetFt8Decoder,
    }
}

/// Convert a RigCommand back to a ClientCommand.
///
/// This is the inverse of client_command_to_rig, converting RigMode
/// values back to mode strings.
pub fn rig_command_to_client(cmd: RigCommand) -> ClientCommand {
    match cmd {
        RigCommand::GetSnapshot => ClientCommand::GetState,
        RigCommand::SetFreq(freq) => ClientCommand::SetFreq { freq_hz: freq.hz },
        RigCommand::SetMode(mode) => ClientCommand::SetMode {
            mode: mode_to_string(&mode),
        },
        RigCommand::SetPtt(ptt) => ClientCommand::SetPtt { ptt },
        RigCommand::PowerOn => ClientCommand::PowerOn,
        RigCommand::PowerOff => ClientCommand::PowerOff,
        RigCommand::ToggleVfo => ClientCommand::ToggleVfo,
        RigCommand::Lock => ClientCommand::Lock,
        RigCommand::Unlock => ClientCommand::Unlock,
        RigCommand::GetTxLimit => ClientCommand::GetTxLimit,
        RigCommand::SetTxLimit(limit) => ClientCommand::SetTxLimit { limit },
        RigCommand::SetAprsDecodeEnabled(enabled) => ClientCommand::SetAprsDecodeEnabled { enabled },
        RigCommand::SetCwDecodeEnabled(enabled) => ClientCommand::SetCwDecodeEnabled { enabled },
        RigCommand::SetCwAuto(enabled) => ClientCommand::SetCwAuto { enabled },
        RigCommand::SetCwWpm(wpm) => ClientCommand::SetCwWpm { wpm },
        RigCommand::SetCwToneHz(tone_hz) => ClientCommand::SetCwToneHz { tone_hz },
        RigCommand::SetFt8DecodeEnabled(enabled) => ClientCommand::SetFt8DecodeEnabled { enabled },
        RigCommand::ResetAprsDecoder => ClientCommand::ResetAprsDecoder,
        RigCommand::ResetCwDecoder => ClientCommand::ResetCwDecoder,
        RigCommand::ResetFt8Decoder => ClientCommand::ResetFt8Decoder,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trx_core::rig::state::RigMode;

    #[test]
    fn test_client_command_to_rig_get_state() {
        let cmd = ClientCommand::GetState;
        if let RigCommand::GetSnapshot = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected GetSnapshot");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_freq() {
        let cmd = ClientCommand::SetFreq { freq_hz: 14100000 };
        if let RigCommand::SetFreq(freq) = client_command_to_rig(cmd) {
            assert_eq!(freq.hz, 14100000);
        } else {
            panic!("Expected SetFreq");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_mode_lsb() {
        let cmd = ClientCommand::SetMode {
            mode: "LSB".to_string(),
        };
        if let RigCommand::SetMode(mode) = client_command_to_rig(cmd) {
            assert_eq!(mode, RigMode::LSB);
        } else {
            panic!("Expected SetMode");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_mode_unknown() {
        let cmd = ClientCommand::SetMode {
            mode: "UNKNOWN".to_string(),
        };
        if let RigCommand::SetMode(RigMode::Other(s)) = client_command_to_rig(cmd) {
            assert_eq!(s, "UNKNOWN");
        } else {
            panic!("Expected SetMode with Other");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_ptt() {
        let cmd = ClientCommand::SetPtt { ptt: true };
        if let RigCommand::SetPtt(ptt) = client_command_to_rig(cmd) {
            assert!(ptt);
        } else {
            panic!("Expected SetPtt");
        }
    }

    #[test]
    fn test_client_command_to_rig_power_on() {
        let cmd = ClientCommand::PowerOn;
        if let RigCommand::PowerOn = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected PowerOn");
        }
    }

    #[test]
    fn test_client_command_to_rig_power_off() {
        let cmd = ClientCommand::PowerOff;
        if let RigCommand::PowerOff = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected PowerOff");
        }
    }

    #[test]
    fn test_client_command_to_rig_toggle_vfo() {
        let cmd = ClientCommand::ToggleVfo;
        if let RigCommand::ToggleVfo = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected ToggleVfo");
        }
    }

    #[test]
    fn test_client_command_to_rig_lock() {
        let cmd = ClientCommand::Lock;
        if let RigCommand::Lock = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected Lock");
        }
    }

    #[test]
    fn test_client_command_to_rig_unlock() {
        let cmd = ClientCommand::Unlock;
        if let RigCommand::Unlock = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected Unlock");
        }
    }

    #[test]
    fn test_client_command_to_rig_get_tx_limit() {
        let cmd = ClientCommand::GetTxLimit;
        if let RigCommand::GetTxLimit = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected GetTxLimit");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_tx_limit() {
        let cmd = ClientCommand::SetTxLimit { limit: 50 };
        if let RigCommand::SetTxLimit(limit) = client_command_to_rig(cmd) {
            assert_eq!(limit, 50);
        } else {
            panic!("Expected SetTxLimit");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_aprs_decode_enabled() {
        let cmd = ClientCommand::SetAprsDecodeEnabled { enabled: true };
        if let RigCommand::SetAprsDecodeEnabled(enabled) = client_command_to_rig(cmd) {
            assert!(enabled);
        } else {
            panic!("Expected SetAprsDecodeEnabled");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_cw_decode_enabled() {
        let cmd = ClientCommand::SetCwDecodeEnabled { enabled: false };
        if let RigCommand::SetCwDecodeEnabled(enabled) = client_command_to_rig(cmd) {
            assert!(!enabled);
        } else {
            panic!("Expected SetCwDecodeEnabled");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_cw_auto() {
        let cmd = ClientCommand::SetCwAuto { enabled: true };
        if let RigCommand::SetCwAuto(enabled) = client_command_to_rig(cmd) {
            assert!(enabled);
        } else {
            panic!("Expected SetCwAuto");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_cw_wpm() {
        let cmd = ClientCommand::SetCwWpm { wpm: 25 };
        if let RigCommand::SetCwWpm(wpm) = client_command_to_rig(cmd) {
            assert_eq!(wpm, 25);
        } else {
            panic!("Expected SetCwWpm");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_cw_tone_hz() {
        let cmd = ClientCommand::SetCwToneHz { tone_hz: 800 };
        if let RigCommand::SetCwToneHz(tone_hz) = client_command_to_rig(cmd) {
            assert_eq!(tone_hz, 800);
        } else {
            panic!("Expected SetCwToneHz");
        }
    }

    #[test]
    fn test_client_command_to_rig_set_ft8_decode_enabled() {
        let cmd = ClientCommand::SetFt8DecodeEnabled { enabled: true };
        if let RigCommand::SetFt8DecodeEnabled(enabled) = client_command_to_rig(cmd) {
            assert!(enabled);
        } else {
            panic!("Expected SetFt8DecodeEnabled");
        }
    }

    #[test]
    fn test_client_command_to_rig_reset_aprs_decoder() {
        let cmd = ClientCommand::ResetAprsDecoder;
        if let RigCommand::ResetAprsDecoder = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected ResetAprsDecoder");
        }
    }

    #[test]
    fn test_client_command_to_rig_reset_cw_decoder() {
        let cmd = ClientCommand::ResetCwDecoder;
        if let RigCommand::ResetCwDecoder = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected ResetCwDecoder");
        }
    }

    #[test]
    fn test_client_command_to_rig_reset_ft8_decoder() {
        let cmd = ClientCommand::ResetFt8Decoder;
        if let RigCommand::ResetFt8Decoder = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected ResetFt8Decoder");
        }
    }

    #[test]
    fn test_rig_command_to_client_get_snapshot() {
        let cmd = RigCommand::GetSnapshot;
        if let ClientCommand::GetState = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected GetState");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_freq() {
        let cmd = RigCommand::SetFreq(Freq { hz: 14100000 });
        if let ClientCommand::SetFreq { freq_hz } = rig_command_to_client(cmd) {
            assert_eq!(freq_hz, 14100000);
        } else {
            panic!("Expected SetFreq");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_mode_lsb() {
        let cmd = RigCommand::SetMode(RigMode::LSB);
        if let ClientCommand::SetMode { mode } = rig_command_to_client(cmd) {
            assert_eq!(mode, "LSB");
        } else {
            panic!("Expected SetMode");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_mode_other() {
        let cmd = RigCommand::SetMode(RigMode::Other("CUSTOM".to_string()));
        if let ClientCommand::SetMode { mode } = rig_command_to_client(cmd) {
            assert_eq!(mode, "CUSTOM");
        } else {
            panic!("Expected SetMode");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_ptt() {
        let cmd = RigCommand::SetPtt(true);
        if let ClientCommand::SetPtt { ptt } = rig_command_to_client(cmd) {
            assert!(ptt);
        } else {
            panic!("Expected SetPtt");
        }
    }

    #[test]
    fn test_rig_command_to_client_power_on() {
        let cmd = RigCommand::PowerOn;
        if let ClientCommand::PowerOn = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected PowerOn");
        }
    }

    #[test]
    fn test_rig_command_to_client_power_off() {
        let cmd = RigCommand::PowerOff;
        if let ClientCommand::PowerOff = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected PowerOff");
        }
    }

    #[test]
    fn test_rig_command_to_client_toggle_vfo() {
        let cmd = RigCommand::ToggleVfo;
        if let ClientCommand::ToggleVfo = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected ToggleVfo");
        }
    }

    #[test]
    fn test_rig_command_to_client_lock() {
        let cmd = RigCommand::Lock;
        if let ClientCommand::Lock = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected Lock");
        }
    }

    #[test]
    fn test_rig_command_to_client_unlock() {
        let cmd = RigCommand::Unlock;
        if let ClientCommand::Unlock = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected Unlock");
        }
    }

    #[test]
    fn test_rig_command_to_client_get_tx_limit() {
        let cmd = RigCommand::GetTxLimit;
        if let ClientCommand::GetTxLimit = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected GetTxLimit");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_tx_limit() {
        let cmd = RigCommand::SetTxLimit(50);
        if let ClientCommand::SetTxLimit { limit } = rig_command_to_client(cmd) {
            assert_eq!(limit, 50);
        } else {
            panic!("Expected SetTxLimit");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_aprs_decode_enabled() {
        let cmd = RigCommand::SetAprsDecodeEnabled(true);
        if let ClientCommand::SetAprsDecodeEnabled { enabled } = rig_command_to_client(cmd) {
            assert!(enabled);
        } else {
            panic!("Expected SetAprsDecodeEnabled");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_cw_decode_enabled() {
        let cmd = RigCommand::SetCwDecodeEnabled(false);
        if let ClientCommand::SetCwDecodeEnabled { enabled } = rig_command_to_client(cmd) {
            assert!(!enabled);
        } else {
            panic!("Expected SetCwDecodeEnabled");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_cw_auto() {
        let cmd = RigCommand::SetCwAuto(true);
        if let ClientCommand::SetCwAuto { enabled } = rig_command_to_client(cmd) {
            assert!(enabled);
        } else {
            panic!("Expected SetCwAuto");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_cw_wpm() {
        let cmd = RigCommand::SetCwWpm(25);
        if let ClientCommand::SetCwWpm { wpm } = rig_command_to_client(cmd) {
            assert_eq!(wpm, 25);
        } else {
            panic!("Expected SetCwWpm");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_cw_tone_hz() {
        let cmd = RigCommand::SetCwToneHz(800);
        if let ClientCommand::SetCwToneHz { tone_hz } = rig_command_to_client(cmd) {
            assert_eq!(tone_hz, 800);
        } else {
            panic!("Expected SetCwToneHz");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_ft8_decode_enabled() {
        let cmd = RigCommand::SetFt8DecodeEnabled(true);
        if let ClientCommand::SetFt8DecodeEnabled { enabled } = rig_command_to_client(cmd) {
            assert!(enabled);
        } else {
            panic!("Expected SetFt8DecodeEnabled");
        }
    }

    #[test]
    fn test_rig_command_to_client_reset_aprs_decoder() {
        let cmd = RigCommand::ResetAprsDecoder;
        if let ClientCommand::ResetAprsDecoder = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected ResetAprsDecoder");
        }
    }

    #[test]
    fn test_rig_command_to_client_reset_cw_decoder() {
        let cmd = RigCommand::ResetCwDecoder;
        if let ClientCommand::ResetCwDecoder = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected ResetCwDecoder");
        }
    }

    #[test]
    fn test_rig_command_to_client_reset_ft8_decoder() {
        let cmd = RigCommand::ResetFt8Decoder;
        if let ClientCommand::ResetFt8Decoder = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected ResetFt8Decoder");
        }
    }

    #[test]
    fn test_round_trip_set_freq() {
        let original = ClientCommand::SetFreq { freq_hz: 7050000 };
        let rig_cmd = client_command_to_rig(original);
        let client_cmd = rig_command_to_client(rig_cmd);

        if let ClientCommand::SetFreq { freq_hz } = client_cmd {
            assert_eq!(freq_hz, 7050000);
        } else {
            panic!("Round trip failed");
        }
    }

    #[test]
    fn test_round_trip_set_mode_standard() {
        let original = ClientCommand::SetMode {
            mode: "USB".to_string(),
        };
        let rig_cmd = client_command_to_rig(original);
        let client_cmd = rig_command_to_client(rig_cmd);

        if let ClientCommand::SetMode { mode } = client_cmd {
            assert_eq!(mode, "USB");
        } else {
            panic!("Round trip failed");
        }
    }

    #[test]
    fn test_round_trip_set_ptt() {
        let original = ClientCommand::SetPtt { ptt: false };
        let rig_cmd = client_command_to_rig(original);
        let client_cmd = rig_command_to_client(rig_cmd);

        if let ClientCommand::SetPtt { ptt } = client_cmd {
            assert!(!ptt);
        } else {
            panic!("Round trip failed");
        }
    }
}
