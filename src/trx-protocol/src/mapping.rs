// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Bidirectional command mapping between ClientCommand and RigCommand.

use trx_core::radio::freq::Freq;
use trx_core::rig::command::RigCommand;

use crate::codec::{mode_to_string, parse_mode};
use crate::types::ClientCommand;

/// Generates `client_command_to_rig` and `rig_command_to_client` from a
/// single definition table, eliminating the mechanical duplication of
/// mapping every variant by hand.
///
/// Supported row forms (each section is introduced by a keyword):
///
/// - **`client_only:`** `Name, ...;`
///   Variants that exist only in `ClientCommand` with no `RigCommand`
///   counterpart. `client_command_to_rig` panics if called with one.
///
/// - **`unit:`** `ClientName <=> RigName, ...;`
///   Unit variant on both sides, same or different names.
///
/// - **`field:`** `Name { field } <=> Name, ...;`
///   Client struct with one named field mapped to a rig tuple variant.
///
/// - **`multi:`** `Name { a, b } <=> Name, ...;`
///   Both sides use named fields with the same field names.
///
/// - **`freq:`** `Name { field } <=> Name, ...;`
///   Client `u64` field converted to/from `Freq { hz }`.
///
/// - **`mode:`** `Name { field } <=> Name, ...;`
///   Client `String` field converted to/from `RigMode` via
///   `parse_mode`/`mode_to_string`.
macro_rules! define_command_mapping {
    (
        client_only: $( $co:ident ),* ;
        unit: $( $cu:ident <=> $ru:ident ),* ;
        field: $( $cf:ident { $fld:ident } <=> $rf:ident ),* ;
        multi: $( $cs:ident { $( $sfld:ident ),+ } <=> $rs:ident ),* ;
        freq: $( $cfq:ident { $ffld:ident } <=> $rfq:ident ),* ;
        mode: $( $cm:ident { $mfld:ident } <=> $rm:ident ),* ;
    ) => {
        /// Convert a [`ClientCommand`] to a [`RigCommand`].
        ///
        /// # Panics
        ///
        /// Panics if called with a client-only command (e.g. `GetRigs`,
        /// `GetSatPasses`) that has no `RigCommand` counterpart. Those
        /// commands must be intercepted by the caller before reaching this
        /// function.
        pub fn client_command_to_rig(cmd: ClientCommand) -> RigCommand {
            match cmd {
                // Client-only variants -- no RigCommand equivalent.
                $(
                    ClientCommand::$co => {
                        panic!(
                            "{} has no RigCommand mapping; \
                             it must be handled before reaching rig_task",
                            stringify!($co),
                        );
                    }
                )*
                // Unit <=> Unit
                $( ClientCommand::$cu => RigCommand::$ru, )*
                // Single-field struct <=> tuple
                $( ClientCommand::$cf { $fld } => RigCommand::$rf($fld), )*
                // Multi-field struct passthrough
                $( ClientCommand::$cs { $( $sfld ),+ } => RigCommand::$rs { $( $sfld ),+ }, )*
                // Freq conversion (u64 => Freq)
                $( ClientCommand::$cfq { $ffld } => RigCommand::$rfq(Freq { hz: $ffld }), )*
                // Mode conversion (String => RigMode)
                $( ClientCommand::$cm { $mfld } => RigCommand::$rm(parse_mode(&$mfld)), )*
            }
        }

        /// Convert a [`RigCommand`] back to a [`ClientCommand`].
        ///
        /// This is the inverse of [`client_command_to_rig`], converting
        /// `RigMode` values back to mode strings.
        pub fn rig_command_to_client(cmd: RigCommand) -> ClientCommand {
            match cmd {
                // Unit <=> Unit
                $( RigCommand::$ru => ClientCommand::$cu, )*
                // Single-field struct <=> tuple
                $( RigCommand::$rf($fld) => ClientCommand::$cf { $fld }, )*
                // Multi-field struct passthrough
                $( RigCommand::$rs { $( $sfld ),+ } => ClientCommand::$cs { $( $sfld ),+ }, )*
                // Freq conversion (Freq => u64)
                $( RigCommand::$rfq(freq) => ClientCommand::$cfq { $ffld: freq.hz }, )*
                // Mode conversion (RigMode => String)
                $( RigCommand::$rm(mode) => ClientCommand::$cm {
                    $mfld: mode_to_string(&mode).into_owned(),
                }, )*
            }
        }
    };
}

define_command_mapping! {
    // ── Client-only variants (no RigCommand counterpart) ─────────────
    client_only: GetRigs, GetSatPasses;

    // ── Unit variants (no payload) ───────────────────────────────────
    unit:
        GetState             <=> GetSnapshot,
        PowerOn              <=> PowerOn,
        PowerOff             <=> PowerOff,
        ToggleVfo            <=> ToggleVfo,
        Lock                 <=> Lock,
        Unlock               <=> Unlock,
        GetTxLimit           <=> GetTxLimit,
        GetSpectrum          <=> GetSpectrum,
        ResetAprsDecoder     <=> ResetAprsDecoder,
        ResetHfAprsDecoder   <=> ResetHfAprsDecoder,
        ResetCwDecoder       <=> ResetCwDecoder,
        ResetFt8Decoder      <=> ResetFt8Decoder,
        ResetFt4Decoder      <=> ResetFt4Decoder,
        ResetFt2Decoder      <=> ResetFt2Decoder,
        ResetWsprDecoder     <=> ResetWsprDecoder,
        ResetLrptDecoder     <=> ResetLrptDecoder,
        ResetWefaxDecoder    <=> ResetWefaxDecoder;

    // ── Single-field struct <=> tuple ────────────────────────────────
    field:
        SetPtt                { ptt }            <=> SetPtt,
        SetTxLimit            { limit }          <=> SetTxLimit,
        SetAprsDecodeEnabled  { enabled }        <=> SetAprsDecodeEnabled,
        SetHfAprsDecodeEnabled { enabled }       <=> SetHfAprsDecodeEnabled,
        SetCwDecodeEnabled    { enabled }        <=> SetCwDecodeEnabled,
        SetCwAuto             { enabled }        <=> SetCwAuto,
        SetCwWpm              { wpm }            <=> SetCwWpm,
        SetCwToneHz           { tone_hz }        <=> SetCwToneHz,
        SetFt8DecodeEnabled   { enabled }        <=> SetFt8DecodeEnabled,
        SetFt4DecodeEnabled   { enabled }        <=> SetFt4DecodeEnabled,
        SetFt2DecodeEnabled   { enabled }        <=> SetFt2DecodeEnabled,
        SetWsprDecodeEnabled  { enabled }        <=> SetWsprDecodeEnabled,
        SetLrptDecodeEnabled  { enabled }        <=> SetLrptDecodeEnabled,
        SetWefaxDecodeEnabled { enabled }        <=> SetWefaxDecodeEnabled,
        SetBandwidth          { bandwidth_hz }   <=> SetBandwidth,
        SetSdrGain            { gain_db }        <=> SetSdrGain,
        SetSdrLnaGain         { gain_db }        <=> SetSdrLnaGain,
        SetSdrAgc             { enabled }        <=> SetSdrAgc,
        SetWfmDeemphasis      { deemphasis_us }  <=> SetWfmDeemphasis,
        SetWfmStereo          { enabled }        <=> SetWfmStereo,
        SetWfmDenoise         { level }          <=> SetWfmDenoise,
        SetSamStereoWidth     { width }          <=> SetSamStereoWidth,
        SetSamCarrierSync     { enabled }        <=> SetSamCarrierSync,
        SetRecorderEnabled    { enabled }        <=> SetRecorderEnabled;

    // ── Multi-field struct passthrough ───────────────────────────────
    multi:
        SetSdrSquelch      { enabled, threshold_db } <=> SetSdrSquelch,
        SetSdrNoiseBlanker { enabled, threshold }    <=> SetSdrNoiseBlanker;

    // ── Freq conversions (u64 <=> Freq) ──────────────────────────────
    freq:
        SetFreq       { freq_hz } <=> SetFreq,
        SetCenterFreq { freq_hz } <=> SetCenterFreq;

    // ── Mode conversion (String <=> RigMode) ─────────────────────────
    mode:
        SetMode { mode } <=> SetMode;
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
    fn test_client_command_to_rig_set_wspr_decode_enabled() {
        let cmd = ClientCommand::SetWsprDecodeEnabled { enabled: true };
        if let RigCommand::SetWsprDecodeEnabled(enabled) = client_command_to_rig(cmd) {
            assert!(enabled);
        } else {
            panic!("Expected SetWsprDecodeEnabled");
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
    fn test_client_command_to_rig_reset_wspr_decoder() {
        let cmd = ClientCommand::ResetWsprDecoder;
        if let RigCommand::ResetWsprDecoder = client_command_to_rig(cmd) {
            // Success
        } else {
            panic!("Expected ResetWsprDecoder");
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
    fn test_rig_command_to_client_set_wspr_decode_enabled() {
        let cmd = RigCommand::SetWsprDecodeEnabled(true);
        if let ClientCommand::SetWsprDecodeEnabled { enabled } = rig_command_to_client(cmd) {
            assert!(enabled);
        } else {
            panic!("Expected SetWsprDecodeEnabled");
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
    fn test_rig_command_to_client_reset_wspr_decoder() {
        let cmd = RigCommand::ResetWsprDecoder;
        if let ClientCommand::ResetWsprDecoder = rig_command_to_client(cmd) {
            // Success
        } else {
            panic!("Expected ResetWsprDecoder");
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

    #[test]
    fn test_client_command_to_rig_set_recorder_enabled() {
        let cmd = ClientCommand::SetRecorderEnabled { enabled: true };
        if let RigCommand::SetRecorderEnabled(enabled) = client_command_to_rig(cmd) {
            assert!(enabled);
        } else {
            panic!("Expected SetRecorderEnabled");
        }
    }

    #[test]
    fn test_rig_command_to_client_set_recorder_enabled() {
        let cmd = RigCommand::SetRecorderEnabled(true);
        if let ClientCommand::SetRecorderEnabled { enabled } = rig_command_to_client(cmd) {
            assert!(enabled);
        } else {
            panic!("Expected SetRecorderEnabled");
        }
    }

    #[test]
    fn test_round_trip_set_recorder_enabled() {
        let original = ClientCommand::SetRecorderEnabled { enabled: false };
        let rig_cmd = client_command_to_rig(original);
        let client_cmd = rig_command_to_client(rig_cmd);

        if let ClientCommand::SetRecorderEnabled { enabled } = client_cmd {
            assert!(!enabled);
        } else {
            panic!("Round trip failed");
        }
    }
}
