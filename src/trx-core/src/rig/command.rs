// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use crate::radio::freq::Freq;
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
    SetCwDecodeEnabled(bool),
    SetCwAuto(bool),
    SetCwWpm(u32),
    SetCwToneHz(u32),
    SetFt8DecodeEnabled(bool),
    SetWsprDecodeEnabled(bool),
    ResetAprsDecoder,
    ResetCwDecoder,
    ResetFt8Decoder,
    ResetWsprDecoder,
    SetBandwidth(u32),
    SetFirTaps(u32),
    SetSdrGain(f64),
    SetWfmDeemphasis(u32),
    SetWfmStereo(bool),
    SetWfmDenoise(bool),
    GetSpectrum,
}
