// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

pub mod audio;
pub mod decode;
pub mod math;
pub mod radio;
pub mod rig;

pub type DynResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub use rig::AudioSource;
pub use rig::command::RigCommand;
pub use rig::request::RigRequest;
pub use rig::response::{RigError, RigResult};
pub use rig::state::{RigMode, RigSnapshot, RigState};
