// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Command executor implementation that bridges to RigCat.

use std::future::Future;
use std::pin::Pin;

use crate::radio::freq::Freq;
use crate::rig::state::RigMode;
use crate::rig::RigCat;
use crate::DynResult;

use super::handlers::CommandExecutor;

/// Executor that delegates to a RigCat implementation.
pub struct RigCatExecutor<'a> {
    rig: &'a mut dyn RigCat,
}

impl<'a> RigCatExecutor<'a> {
    pub fn new(rig: &'a mut dyn RigCat) -> Self {
        Self { rig }
    }
}

impl<'a> CommandExecutor for RigCatExecutor<'a> {
    fn set_freq<'b>(
        &'b mut self,
        freq: Freq,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'b>> {
        self.rig.set_freq(freq)
    }

    fn set_mode<'b>(
        &'b mut self,
        mode: RigMode,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'b>> {
        self.rig.set_mode(mode)
    }

    fn set_ptt<'b>(
        &'b mut self,
        ptt: bool,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'b>> {
        self.rig.set_ptt(ptt)
    }

    fn power_on<'b>(&'b mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'b>> {
        self.rig.power_on()
    }

    fn power_off<'b>(&'b mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'b>> {
        self.rig.power_off()
    }

    fn toggle_vfo<'b>(&'b mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'b>> {
        self.rig.toggle_vfo()
    }

    fn lock<'b>(&'b mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'b>> {
        self.rig.lock()
    }

    fn unlock<'b>(&'b mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'b>> {
        self.rig.unlock()
    }

    fn get_tx_limit<'b>(&'b mut self) -> Pin<Box<dyn Future<Output = DynResult<u8>> + Send + 'b>> {
        self.rig.get_tx_limit()
    }

    fn set_tx_limit<'b>(
        &'b mut self,
        limit: u8,
    ) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'b>> {
        self.rig.set_tx_limit(limit)
    }

    fn refresh_state<'b>(&'b mut self) -> Pin<Box<dyn Future<Output = DynResult<()>> + Send + 'b>> {
        // This is a no-op for the executor - the controller handles state refresh
        Box::pin(async { Ok(()) })
    }
}
