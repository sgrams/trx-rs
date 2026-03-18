// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use crate::radio::freq::Freq;
use crate::rig::state::WfmDenoiseLevel;
use crate::RigMode;

/// Internal command handled by the rig task.
#[derive(Debug, Clone)]
pub enum RigCommand {
    GetSnapshot,
    SetFreq(Freq),
    SetCenterFreq(Freq),
    SetMode(RigMode),
    SetPtt(bool),
    PowerOn,
    PowerOff,
    ToggleVfo,
    GetTxLimit,
    SetTxLimit(u8),
    Lock,
    Unlock,
    SetAprsDecodeEnabled(bool),
    SetHfAprsDecodeEnabled(bool),
    SetCwDecodeEnabled(bool),
    SetCwAuto(bool),
    SetCwWpm(u32),
    SetCwToneHz(u32),
    SetFt8DecodeEnabled(bool),
    SetFt4DecodeEnabled(bool),
    SetFt2DecodeEnabled(bool),
    SetWsprDecodeEnabled(bool),
    ResetAprsDecoder,
    ResetHfAprsDecoder,
    ResetCwDecoder,
    ResetFt8Decoder,
    ResetFt4Decoder,
    ResetFt2Decoder,
    ResetWsprDecoder,
    SetBandwidth(u32),
    SetSdrGain(f64),
    SetSdrLnaGain(f64),
    SetSdrAgc(bool),
    SetSdrSquelch { enabled: bool, threshold_db: f64 },
    SetWfmDeemphasis(u32),
    SetWfmStereo(bool),
    SetWfmDenoise(WfmDenoiseLevel),
    GetSpectrum,
}
