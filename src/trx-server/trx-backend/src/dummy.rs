// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Dummy rig backend for development and testing.
//!
//! Holds rig state in memory and responds to all commands immediately.
//! No hardware or serial port required.

use std::pin::Pin;

use trx_core::radio::freq::{Band, Freq};
use trx_core::rig::state::RigMode;
use trx_core::rig::{
    Rig, RigAccessMethod, RigCapabilities, RigCat, RigInfo, RigStatusFuture, RigVfo, RigVfoEntry,
};
use trx_core::DynResult;

pub struct DummyRig {
    info: RigInfo,
    freq: Freq,
    mode: RigMode,
    ptt: bool,
    powered: bool,
    locked: bool,
    tx_limit: u8,
    active_vfo: usize,
    vfo_b_freq: Freq,
    vfo_b_mode: RigMode,
}

impl DummyRig {
    pub fn new() -> Self {
        Self {
            info: RigInfo {
                manufacturer: "Dummy".to_string(),
                model: "dummy".to_string(),
                revision: "1.0".to_string(),
                capabilities: RigCapabilities {
                    min_freq_step_hz: 1,
                    supported_bands: vec![
                        Band {
                            low_hz: 1_800_000,
                            high_hz: 2_000_000,
                            tx_allowed: true,
                        },
                        Band {
                            low_hz: 3_500_000,
                            high_hz: 4_000_000,
                            tx_allowed: true,
                        },
                        Band {
                            low_hz: 7_000_000,
                            high_hz: 7_300_000,
                            tx_allowed: true,
                        },
                        Band {
                            low_hz: 14_000_000,
                            high_hz: 14_350_000,
                            tx_allowed: true,
                        },
                        Band {
                            low_hz: 21_000_000,
                            high_hz: 21_450_000,
                            tx_allowed: true,
                        },
                        Band {
                            low_hz: 28_000_000,
                            high_hz: 29_700_000,
                            tx_allowed: true,
                        },
                        Band {
                            low_hz: 50_000_000,
                            high_hz: 54_000_000,
                            tx_allowed: true,
                        },
                        Band {
                            low_hz: 144_000_000,
                            high_hz: 148_000_000,
                            tx_allowed: true,
                        },
                        Band {
                            low_hz: 430_000_000,
                            high_hz: 440_000_000,
                            tx_allowed: true,
                        },
                    ],
                    supported_modes: vec![
                        RigMode::LSB,
                        RigMode::USB,
                        RigMode::CW,
                        RigMode::CWR,
                        RigMode::AM,
                        RigMode::FM,
                        RigMode::WFM,
                        RigMode::DIG,
                        RigMode::PKT,
                    ],
                    num_vfos: 2,
                    lock: false,
                    lockable: true,
                    attenuator: false,
                    preamp: false,
                    rit: false,
                    rpt: false,
                    split: false,
                },
                access: RigAccessMethod::Serial {
                    path: "/dev/null".to_string(),
                    baud: 9600,
                },
            },
            freq: Freq { hz: 144_300_000 },
            mode: RigMode::USB,
            ptt: false,
            powered: true,
            locked: false,
            tx_limit: 5,
            active_vfo: 0,
            vfo_b_freq: Freq { hz: 7_100_000 },
            vfo_b_mode: RigMode::LSB,
        }
    }

    fn build_vfo(&self) -> RigVfo {
        RigVfo {
            active: Some(self.active_vfo),
            entries: vec![
                RigVfoEntry {
                    name: "A".to_string(),
                    freq: self.freq,
                    mode: Some(self.mode.clone()),
                },
                RigVfoEntry {
                    name: "B".to_string(),
                    freq: self.vfo_b_freq,
                    mode: Some(self.vfo_b_mode.clone()),
                },
            ],
        }
    }
}

impl Rig for DummyRig {
    fn info(&self) -> &RigInfo {
        &self.info
    }
}

impl RigCat for DummyRig {
    fn get_status<'a>(&'a mut self) -> RigStatusFuture<'a> {
        Box::pin(async move { Ok((self.freq, self.mode.clone(), Some(self.build_vfo()))) })
    }

    fn set_freq<'a>(
        &'a mut self,
        freq: Freq,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        self.freq = freq;
        Box::pin(async { Ok(()) })
    }

    fn set_mode<'a>(
        &'a mut self,
        mode: RigMode,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        self.mode = mode;
        Box::pin(async { Ok(()) })
    }

    fn set_ptt<'a>(
        &'a mut self,
        ptt: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        self.ptt = ptt;
        Box::pin(async { Ok(()) })
    }

    fn power_on<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        self.powered = true;
        Box::pin(async { Ok(()) })
    }

    fn power_off<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        self.powered = false;
        Box::pin(async { Ok(()) })
    }

    fn get_signal_strength<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        // Fluctuate between 2 and 8 using low-order time bits
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let val = 2 + (nanos % 7) as u8; // 2..=8
        Box::pin(async move { Ok(val) })
    }

    fn get_tx_power<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        let power = if self.ptt { 5 } else { 0 };
        Box::pin(async move { Ok(power) })
    }

    fn get_tx_limit<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        let limit = self.tx_limit;
        Box::pin(async move { Ok(limit) })
    }

    fn set_tx_limit<'a>(
        &'a mut self,
        limit: u8,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        self.tx_limit = limit;
        Box::pin(async { Ok(()) })
    }

    fn toggle_vfo<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        // Swap active VFO and swap freq/mode
        let old_freq = self.freq;
        let old_mode = self.mode.clone();
        self.freq = self.vfo_b_freq;
        self.mode = self.vfo_b_mode.clone();
        self.vfo_b_freq = old_freq;
        self.vfo_b_mode = old_mode;
        self.active_vfo = if self.active_vfo == 0 { 1 } else { 0 };
        Box::pin(async { Ok(()) })
    }

    fn lock<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        self.locked = true;
        Box::pin(async { Ok(()) })
    }

    fn unlock<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        self.locked = false;
        Box::pin(async { Ok(()) })
    }
}
