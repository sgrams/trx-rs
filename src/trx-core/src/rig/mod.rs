// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::radio::freq::{Band, Freq};
use crate::{DynResult, RigMode};

/// Alias to reduce type complexity in RigCat.
pub type RigStatusFuture<'a> =
    Pin<Box<dyn Future<Output = DynResult<(Freq, RigMode, Option<RigVfo>)>> + Send + 'a>>;

pub mod command;
pub mod controller;
pub mod request;
pub mod response;
pub mod state;

/// How this backend communicates with the rig.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RigAccessMethod {
    Serial { path: String, baud: u32 },
    Tcp { addr: String },
}

/// Static info describing a rig backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigInfo {
    pub manufacturer: String,
    pub model: String,
    pub revision: String,
    pub capabilities: RigCapabilities,
    pub access: RigAccessMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigCapabilities {
    #[serde(default = "default_min_freq_step_hz")]
    pub min_freq_step_hz: u64,
    pub supported_bands: Vec<Band>,
    pub supported_modes: Vec<RigMode>,
    pub num_vfos: usize,
    pub lock: bool,
    pub lockable: bool,
    pub attenuator: bool,
    pub preamp: bool,
    pub rit: bool,
    pub rpt: bool,
    pub split: bool,
    /// Backend supports transmit: PTT, power on/off, TX meters, TX audio.
    pub tx: bool,
    /// Backend supports get_tx_limit / set_tx_limit.
    pub tx_limit: bool,
    /// Backend supports toggle_vfo.
    pub vfo_switch: bool,
    /// Backend supports runtime filter adjustment (bandwidth, FIR taps).
    pub filter_controls: bool,
    /// Backend returns a meaningful RX signal strength value.
    pub signal_meter: bool,
}

fn default_min_freq_step_hz() -> u64 {
    1
}

/// Trait for rigs that can provide demodulated PCM audio.
pub trait AudioSource: Send + Sync {
    /// Subscribe to demodulated PCM audio from the primary channel.
    /// Returns a broadcast receiver that yields 20ms frames of mono f32 PCM.
    fn subscribe_pcm(&self) -> tokio::sync::broadcast::Receiver<Vec<f32>>;
}

/// Common interface for rig backends.
pub trait Rig {
    fn info(&self) -> &RigInfo;
}

/// Common CAT control operations any rig backend should implement.
pub trait RigCat: Rig + Send {
    fn get_status<'a>(&'a mut self) -> RigStatusFuture<'a>;

    fn set_freq<'a>(
        &'a mut self,
        freq: Freq,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn set_center_freq<'a>(
        &'a mut self,
        _freq: Freq,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(std::future::ready(Err(
            Box::new(response::RigError::not_supported("set_center_freq"))
                as Box<dyn std::error::Error + Send + Sync>,
        )))
    }

    fn set_mode<'a>(
        &'a mut self,
        mode: RigMode,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn set_ptt<'a>(
        &'a mut self,
        ptt: bool,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn power_on<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn power_off<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn get_signal_strength<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = DynResult<u8>> + Send + 'a>>;

    fn get_tx_power<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<u8>> + Send + 'a>>;

    fn get_tx_limit<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<u8>> + Send + 'a>>;

    fn set_tx_limit<'a>(
        &'a mut self,
        limit: u8,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn toggle_vfo<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn lock<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn unlock<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>>;

    fn as_audio_source(&self) -> Option<&dyn AudioSource> {
        None
    }

    fn set_bandwidth<'a>(
        &'a mut self,
        _bandwidth_hz: u32,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(std::future::ready(Err(
            Box::new(response::RigError::not_supported("set_bandwidth"))
                as Box<dyn std::error::Error + Send + Sync>,
        )))
    }

    fn set_fir_taps<'a>(
        &'a mut self,
        _taps: u32,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(std::future::ready(Err(
            Box::new(response::RigError::not_supported("set_fir_taps"))
                as Box<dyn std::error::Error + Send + Sync>,
        )))
    }

    fn set_wfm_deemphasis<'a>(
        &'a mut self,
        _deemphasis_us: u32,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(std::future::ready(Err(
            Box::new(response::RigError::not_supported("set_wfm_deemphasis"))
                as Box<dyn std::error::Error + Send + Sync>,
        )))
    }

    fn set_wfm_denoise<'a>(
        &'a mut self,
        _enabled: bool,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(std::future::ready(Err(
            Box::new(response::RigError::not_supported("set_wfm_denoise"))
                as Box<dyn std::error::Error + Send + Sync>,
        )))
    }

    fn set_wfm_stereo<'a>(
        &'a mut self,
        _enabled: bool,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(std::future::ready(Err(
            Box::new(response::RigError::not_supported("set_wfm_stereo"))
                as Box<dyn std::error::Error + Send + Sync>,
        )))
    }

    /// Return the current filter state if this backend supports filter controls.
    fn filter_state(&self) -> Option<state::RigFilterState> {
        None
    }

    /// Return the latest spectrum frame if this backend supports spectrum output.
    fn get_spectrum(&self) -> Option<state::SpectrumData> {
        None
    }
}

/// Snapshot of a rig's status that every backend can expose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigStatus {
    pub freq: Freq,
    pub mode: RigMode,
    pub tx_en: bool,
    pub vfo: Option<RigVfo>,
    pub tx: Option<RigTxStatus>,
    pub rx: Option<RigRxStatus>,
    pub lock: Option<bool>,
}

/// Trait for presenting rig status in a backend-agnostic way.
pub trait RigStatusProvider {
    fn status(&self) -> RigStatus;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigVfo {
    pub entries: Vec<RigVfoEntry>,
    /// Index into `entries` for the active VFO, if known.
    pub active: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigVfoEntry {
    pub name: String,
    pub freq: Freq,
    pub mode: Option<RigMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigTxStatus {
    pub power: Option<u8>,
    pub limit: Option<u8>,
    pub swr: Option<f32>,
    pub alc: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigRxStatus {
    pub sig: Option<i32>,
}

/// Configurable control settings that can be pushed to the rig.
#[derive(Debug, Clone, Serialize)]
pub struct RigControl {
    pub enabled: Option<bool>,
    pub lock: Option<bool>,
    pub clar_hz: Option<i32>,
    pub clar_on: Option<bool>,
    pub rpt_offset_hz: Option<i32>,
    pub ctcss_hz: Option<f32>,
    pub dcs_code: Option<u16>,
}
