// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

pub mod demod;
pub mod dsp;

use std::pin::Pin;

use trx_core::radio::freq::{Band, Freq};
use trx_core::rig::{
    Rig, RigAccessMethod, RigCapabilities, RigCat, RigInfo, RigStatusFuture,
};
use trx_core::rig::response::RigError;
use trx_core::{DynResult, RigMode};

/// RX-only backend for any SoapySDR-compatible device.
pub struct SoapySdrRig {
    info: RigInfo,
    freq: Freq,
    mode: RigMode,
}

impl SoapySdrRig {
    /// Construct a new `SoapySdrRig` from a SoapySDR device args string.
    ///
    /// The `args` value follows SoapySDR's key=value comma-separated convention
    /// (e.g. `"driver=rtlsdr"` or `"driver=airspy,serial=00000001"`).
    pub fn new(args: &str) -> DynResult<Self> {
        tracing::info!("initialising SoapySDR backend (args={:?})", args);

        let info = RigInfo {
            manufacturer: "SoapySDR".to_string(),
            model: "Generic SDR".to_string(),
            revision: "".to_string(),
            capabilities: RigCapabilities {
                min_freq_step_hz: 1,
                // Broad RX-only coverage: DC through 6 GHz as a single band.
                supported_bands: vec![Band {
                    low_hz: 0,
                    high_hz: 6_000_000_000,
                    tx_allowed: false,
                }],
                supported_modes: vec![
                    RigMode::LSB,
                    RigMode::USB,
                    RigMode::CW,
                    RigMode::CWR,
                    RigMode::AM,
                    RigMode::WFM,
                    RigMode::FM,
                    RigMode::DIG,
                    RigMode::PKT,
                ],
                num_vfos: 1,
                lockable: false,
                attenuator: false,
                preamp: false,
                rit: false,
                rpt: false,
                split: false,
                lock: false,
            },
            // There is no serial/TCP access for SDR devices; use a dummy TCP
            // placeholder so `RigAccessMethod` (which has no SDR variant) can
            // still carry the args string in a human-readable form.
            access: RigAccessMethod::Tcp {
                addr: format!("soapysdr:{}", args),
            },
        };

        Ok(Self {
            info,
            freq: Freq { hz: 14_074_000 },
            mode: RigMode::USB,
        })
    }
}

// ---------------------------------------------------------------------------
// Rig
// ---------------------------------------------------------------------------

impl Rig for SoapySdrRig {
    fn info(&self) -> &RigInfo {
        &self.info
    }
}

// ---------------------------------------------------------------------------
// RigCat
// ---------------------------------------------------------------------------

impl RigCat for SoapySdrRig {
    // -- Supported RX methods -----------------------------------------------

    fn get_status<'a>(&'a mut self) -> RigStatusFuture<'a> {
        Box::pin(async move { Ok((self.freq, self.mode.clone(), None)) })
    }

    fn set_freq<'a>(
        &'a mut self,
        freq: Freq,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!("SoapySdrRig: set_freq -> {} Hz", freq.hz);
            self.freq = freq;
            Ok(())
        })
    }

    fn set_mode<'a>(
        &'a mut self,
        mode: RigMode,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!("SoapySdrRig: set_mode -> {:?}", mode);
            self.mode = mode;
            Ok(())
        })
    }

    fn get_signal_strength<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        // RSSI mapping will be implemented in SDR-07; return 0 for now.
        Box::pin(async move { Ok(0) })
    }

    // -- TX / unsupported methods -------------------------------------------

    fn set_ptt<'a>(
        &'a mut self,
        _ptt: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("set_ptt")) as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn power_on<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("power_on")) as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn power_off<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("power_off")) as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn get_tx_power<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("get_tx_power")) as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn get_tx_limit<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("get_tx_limit")) as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn set_tx_limit<'a>(
        &'a mut self,
        _limit: u8,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("set_tx_limit")) as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn toggle_vfo<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("toggle_vfo")) as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn lock<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("lock")) as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    fn unlock<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(Box::new(RigError::not_supported("unlock")) as Box<dyn std::error::Error + Send + Sync>)
        })
    }

    /// Returns `None` for now; will be overridden with `Some(self)` in SDR-07
    /// once the IQ DSP pipeline is in place.
    fn as_audio_source(&self) -> Option<&dyn trx_core::rig::AudioSource> {
        None
    }
}
