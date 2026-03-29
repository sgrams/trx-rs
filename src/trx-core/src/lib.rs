// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

pub mod audio;
pub mod decode;
pub mod geo;
pub mod math;
pub mod radio;
pub mod rig;
pub mod vchan;

pub type DynResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub use rig::command::RigCommand;
pub use rig::request::RigRequest;
pub use rig::response::{RigError, RigResult};
pub use rig::state::{
    DecoderConfig, DecoderResetSeqs, RdsData, RigFilterState, RigMode, RigSnapshot, RigState,
    WfmDenoiseLevel,
};
pub use rig::AudioSource;
