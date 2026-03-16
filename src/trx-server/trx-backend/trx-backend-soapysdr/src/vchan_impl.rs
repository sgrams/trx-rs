// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Concrete `VirtualChannelManager` implementation for SoapySDR rigs.
//!
//! `SdrVirtualChannelManager` wraps an `Arc<SdrPipeline>` and maintains a list
//! of managed channel entries.  The primary channel (pipeline slot 0) is always
//! present and marked permanent; additional virtual channels are appended
//! dynamically.
//!
//! ## Slot stability
//!
//! Virtual channels occupy pipeline slots `fixed_slot_count..`.  When a channel
//! at slot *K* is removed, `Vec::remove(K)` shifts all entries K+1..end left by
//! one; the manager updates every surviving entry's `pipeline_slot` accordingly.
//!
//! ## Center-frequency updates
//!
//! When the hardware retunes (changing `center_hz`), all channel IF offsets must
//! be recomputed. The rig calls `update_center_hz()` after every retune; this
//! updates every `ChannelDsp` in place and pauses out-of-span channels instead
//! of destroying them.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

use num_complex::Complex;
use tokio::sync::broadcast;
use trx_core::rig::state::{RigMode, VchanRdsEntry};
use uuid::Uuid;

use crate::dsp::SdrPipeline;
#[cfg(test)]
use crate::dsp::VirtualSquelchConfig;
use trx_core::vchan::{VChanError, VChannelInfo, VirtualChannelManager};

// ---------------------------------------------------------------------------
// Default DSP parameters for virtual channels
// ---------------------------------------------------------------------------

fn default_bandwidth_hz(mode: &RigMode) -> u32 {
    match mode {
        RigMode::CW | RigMode::CWR => 500,
        RigMode::LSB | RigMode::USB | RigMode::DIG => 3_000,
        RigMode::AM => 9_000,
        RigMode::FM => 12_500,
        RigMode::WFM => 180_000,
        RigMode::PKT | RigMode::AIS => 25_000,
        RigMode::VDES => 100_000,
        RigMode::Other(_) => 3_000,
    }
}

// ---------------------------------------------------------------------------
// Internal channel record
// ---------------------------------------------------------------------------

struct ManagedChannel {
    id: Uuid,
    freq_hz: u64,
    mode: RigMode,
    /// `broadcast::Sender` kept alive so new subscribers can join at any time.
    pcm_tx: broadcast::Sender<Vec<f32>>,
    /// IQ tap sender (kept alive; external consumers may subscribe).
    #[allow(dead_code)]
    iq_tx: broadcast::Sender<Vec<Complex<f32>>>,
    /// Index of this channel in `pipeline.channel_dsps`.
    pipeline_slot: usize,
    /// True only for the primary channel; prevents removal.
    permanent: bool,
    /// Hidden background-decode channels are omitted from the normal virtual
    /// channel listing and do not count against the visible channel cap.
    hidden: bool,
}

// ---------------------------------------------------------------------------
// SdrVirtualChannelManager
// ---------------------------------------------------------------------------

pub struct SdrVirtualChannelManager {
    pipeline: Arc<SdrPipeline>,
    /// Current SDR hardware center frequency, updated on every retune.
    center_hz: Arc<AtomicI64>,
    /// Pipeline slots 0..fixed_slot_count are reserved (primary + AIS).
    /// Virtual channels occupy slots fixed_slot_count and above.
    #[allow(dead_code)]
    fixed_slot_count: usize,
    /// Maximum total channels including the primary (enforced on `add_channel`).
    max_total: usize,
    channels: RwLock<Vec<ManagedChannel>>,
    /// Fires whenever a channel is explicitly destroyed.
    destroyed_tx: broadcast::Sender<Uuid>,
}

impl SdrVirtualChannelManager {
    /// Create a new manager.
    ///
    /// - `pipeline`: shared reference to the running `SdrPipeline`.
    /// - `fixed_slot_count`: number of fixed pipeline slots (primary + AIS),
    ///   i.e. the index of the first slot available for virtual channels.
    /// - `max_total`: maximum total channels including primary (e.g. 4).
    pub fn new(
        pipeline: Arc<SdrPipeline>,
        fixed_slot_count: usize,
        max_total: usize,
    ) -> Self {
        // Seed the channel list with a synthetic primary-channel entry.
        // We use the first PCM sender from the pipeline (index 0).
        let primary_pcm_tx = pipeline
            .pcm_senders
            .first()
            .cloned()
            .unwrap_or_else(|| broadcast::channel::<Vec<f32>>(1).0);
        let primary_iq_tx = pipeline
            .iq_senders
            .first()
            .cloned()
            .unwrap_or_else(|| broadcast::channel::<Vec<Complex<f32>>>(1).0);

        let primary = ManagedChannel {
            id: Uuid::new_v4(),
            freq_hz: 0, // actual freq kept by SoapySdrRig; manager treats ch-0 as opaque
            mode: RigMode::USB,
            pcm_tx: primary_pcm_tx,
            iq_tx: primary_iq_tx,
            pipeline_slot: 0,
            permanent: true,
            hidden: false,
        };

        let (destroyed_tx, _) = broadcast::channel::<Uuid>(16);

        Self {
            center_hz: pipeline.shared_center_hz.clone(),
            pipeline,
            fixed_slot_count,
            max_total: max_total.max(1),
            channels: RwLock::new(vec![primary]),
            destroyed_tx,
        }
    }

    pub fn destroyed_sender(&self) -> broadcast::Sender<Uuid> {
        self.destroyed_tx.clone()
    }

    fn half_span_hz(&self) -> i64 {
        i64::from(self.pipeline.sdr_sample_rate) / 2
    }

    fn visible_channel_count(channels: &[ManagedChannel]) -> usize {
        channels.iter().filter(|ch| !ch.hidden).count()
    }

    fn create_channel(
        &self,
        channels: &mut Vec<ManagedChannel>,
        id: Uuid,
        freq_hz: u64,
        mode: &RigMode,
        hidden: bool,
    ) -> Result<broadcast::Receiver<Vec<f32>>, VChanError> {
        let half_span = self.half_span_hz();
        let center = self.center_hz.load(Ordering::Relaxed);
        let if_hz = freq_hz as i64 - center;
        if if_hz.unsigned_abs() as i64 > half_span {
            return Err(VChanError::OutOfBandwidth {
                half_span_hz: half_span,
            });
        }

        if !hidden && Self::visible_channel_count(channels) >= self.max_total {
            return Err(VChanError::CapReached {
                max: self.max_total,
            });
        }

        let bandwidth_hz = default_bandwidth_hz(mode);
        let (pcm_tx, iq_tx) =
            self.pipeline
                .add_virtual_channel(if_hz as f64, mode, bandwidth_hz);

        let pipeline_slot = self
            .pipeline
            .channel_dsps
            .read()
            .unwrap()
            .len()
            .saturating_sub(1);

        let pcm_rx = pcm_tx.subscribe();
        channels.push(ManagedChannel {
            id,
            freq_hz,
            mode: mode.clone(),
            pcm_tx,
            iq_tx,
            pipeline_slot,
            permanent: false,
            hidden,
        });

        if hidden {
            let dsps = self.pipeline.channel_dsps.read().unwrap();
            if let Some(dsp_arc) = dsps.get(pipeline_slot) {
                dsp_arc.lock().unwrap().set_force_mono_pcm(true);
            }
        }

        Ok(pcm_rx)
    }

    /// Called by `SoapySdrRig` whenever the hardware center frequency changes.
    /// Recomputes the IF offset for every virtual channel and pauses any
    /// channel that is temporarily outside the current SDR span.
    pub fn update_center_hz(&self, new_center_hz: i64) {
        self.center_hz.store(new_center_hz, Ordering::Relaxed);
        let half_span = self.half_span_hz();

        let channels = self.channels.read().unwrap();
        let dsps = self.pipeline.channel_dsps.read().unwrap();
        for ch in channels.iter().filter(|c| !c.permanent) {
            let new_if_hz = ch.freq_hz as i64 - new_center_hz;
            let in_span = new_if_hz.unsigned_abs() as i64 <= half_span;
            if let Some(dsp_arc) = dsps.get(ch.pipeline_slot) {
                let mut dsp = dsp_arc.lock().unwrap();
                if in_span {
                    dsp.set_channel_if_hz(new_if_hz as f64);
                }
                dsp.set_processing_enabled(in_span);
            }
        }
    }

    /// Update the primary channel's freq/mode metadata (called by SoapySdrRig
    /// on SetFreq/SetMode so channel-0 info stays current for API consumers).
    pub fn update_primary_meta(&self, freq_hz: u64, mode: &RigMode) {
        let mut channels = self.channels.write().unwrap();
        if let Some(ch) = channels.first_mut() {
            ch.freq_hz = freq_hz;
            ch.mode = mode.clone();
        }
    }

    /// Snapshot RDS data for each WFM virtual channel (including primary).
    pub fn rds_snapshots(&self) -> Vec<VchanRdsEntry> {
        let channels = self.channels.read().unwrap();
        let dsps = self.pipeline.channel_dsps.read().unwrap();
        channels
            .iter()
            .filter(|ch| matches!(ch.mode, RigMode::WFM))
            .map(|ch| {
                let rds = dsps
                    .get(ch.pipeline_slot)
                    .and_then(|dsp| dsp.lock().ok().and_then(|d| d.rds_data()));
                VchanRdsEntry { id: ch.id, rds }
            })
            .collect()
    }
}

impl VirtualChannelManager for SdrVirtualChannelManager {
    fn add_channel(
        &self,
        freq_hz: u64,
        mode: &RigMode,
    ) -> Result<(Uuid, broadcast::Receiver<Vec<f32>>), VChanError> {
        let mut channels = self.channels.write().unwrap();
        let id = Uuid::new_v4();
        let pcm_rx = self.create_channel(&mut channels, id, freq_hz, mode, false)?;
        Ok((id, pcm_rx))
    }

    fn remove_channel(&self, id: Uuid) -> Result<(), VChanError> {
        let mut channels = self.channels.write().unwrap();
        let pos = channels
            .iter()
            .position(|c| c.id == id)
            .ok_or(VChanError::NotFound)?;

        if channels[pos].permanent {
            return Err(VChanError::Permanent);
        }

        let slot = channels[pos].pipeline_slot;
        self.pipeline.remove_virtual_channel(slot);
        channels.remove(pos);

        // Shift pipeline_slot for all channels that were after the removed one.
        for ch in channels.iter_mut().filter(|c| c.pipeline_slot > slot) {
            ch.pipeline_slot -= 1;
        }
        Ok(())
    }

    fn set_channel_freq(&self, id: Uuid, freq_hz: u64) -> Result<(), VChanError> {
        let half_span = self.half_span_hz();
        let center = self.center_hz.load(Ordering::Relaxed);
        let if_hz = freq_hz as i64 - center;
        if if_hz.unsigned_abs() as i64 > half_span {
            return Err(VChanError::OutOfBandwidth {
                half_span_hz: half_span,
            });
        }

        let mut channels = self.channels.write().unwrap();
        let ch = channels
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or(VChanError::NotFound)?;

        ch.freq_hz = freq_hz;
        let dsps = self.pipeline.channel_dsps.read().unwrap();
        if let Some(dsp_arc) = dsps.get(ch.pipeline_slot) {
            dsp_arc.lock().unwrap().set_channel_if_hz(if_hz as f64);
        }
        Ok(())
    }

    fn set_channel_mode(&self, id: Uuid, mode: &RigMode) -> Result<(), VChanError> {
        let mut channels = self.channels.write().unwrap();
        let ch = channels
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or(VChanError::NotFound)?;

        ch.mode = mode.clone();
        let dsps = self.pipeline.channel_dsps.read().unwrap();
        if let Some(dsp_arc) = dsps.get(ch.pipeline_slot) {
            dsp_arc.lock().unwrap().set_mode(mode);
        }
        Ok(())
    }

    fn set_channel_bandwidth(&self, id: Uuid, bandwidth_hz: u32) -> Result<(), VChanError> {
        let channels = self.channels.read().unwrap();
        let ch = channels
            .iter()
            .find(|c| c.id == id)
            .ok_or(VChanError::NotFound)?;

        let dsps = self.pipeline.channel_dsps.read().unwrap();
        if let Some(dsp_arc) = dsps.get(ch.pipeline_slot) {
            dsp_arc.lock().unwrap().set_filter(bandwidth_hz);
        }
        Ok(())
    }

    fn subscribe_pcm(&self, id: Uuid) -> Option<broadcast::Receiver<Vec<f32>>> {
        let channels = self.channels.read().unwrap();
        channels
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.pcm_tx.subscribe())
    }

    fn channels(&self) -> Vec<VChannelInfo> {
        let channels = self.channels.read().unwrap();
        channels
            .iter()
            .filter(|ch| !ch.hidden)
            .enumerate()
            .map(|(idx, ch)| VChannelInfo {
                id: ch.id,
                index: idx,
                freq_hz: ch.freq_hz,
                mode: format!("{:?}", ch.mode),
                permanent: ch.permanent,
            })
            .collect()
    }

    fn max_channels(&self) -> usize {
        self.max_total
    }

    fn subscribe_destroyed(&self) -> broadcast::Receiver<Uuid> {
        self.destroyed_tx.subscribe()
    }

    fn ensure_channel_pcm(
        &self,
        id: Uuid,
        freq_hz: u64,
        mode: &RigMode,
    ) -> Result<broadcast::Receiver<Vec<f32>>, VChanError> {
        // Fast path: channel already exists.
        {
            let channels = self.channels.read().unwrap();
            if let Some(ch) = channels.iter().find(|c| c.id == id) {
                return Ok(ch.pcm_tx.subscribe());
            }
        }

        let mut channels = self.channels.write().unwrap();
        self.create_channel(&mut channels, id, freq_hz, mode, false)
    }

    fn ensure_background_channel_pcm(
        &self,
        id: Uuid,
        freq_hz: u64,
        mode: &RigMode,
    ) -> Result<broadcast::Receiver<Vec<f32>>, VChanError> {
        {
            let channels = self.channels.read().unwrap();
            if let Some(ch) = channels.iter().find(|c| c.id == id) {
                return Ok(ch.pcm_tx.subscribe());
            }
        }

        let mut channels = self.channels.write().unwrap();
        self.create_channel(&mut channels, id, freq_hz, mode, true)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{MockIqSource, SdrPipeline};

    fn make_pipeline() -> Arc<SdrPipeline> {
        Arc::new(SdrPipeline::start(
            Box::new(MockIqSource),
            1_920_000,
            48_000,
            1,
            20,
            75,
            true,
            VirtualSquelchConfig::default(),
            &[(0.0, RigMode::USB, 3_000)],
        ))
    }

    #[test]
    fn add_and_list() {
        let p = make_pipeline();
        let mgr = SdrVirtualChannelManager::new(p, 1, 4);
        // Set center to 14.1 MHz so that 14.074 MHz is within ±960 kHz.
        mgr.update_center_hz(14_100_000);
        assert_eq!(mgr.channels().len(), 1); // primary only

        let (id, _rx) = mgr.add_channel(14_074_000, &RigMode::USB).unwrap();
        assert_eq!(mgr.channels().len(), 2);

        let ch = mgr.channels().into_iter().find(|c| c.id == id).unwrap();
        assert_eq!(ch.freq_hz, 14_074_000);
        assert!(!ch.permanent);
    }

    #[test]
    fn remove_virtual_channel() {
        let p = make_pipeline();
        let mgr = SdrVirtualChannelManager::new(p, 1, 4);
        mgr.update_center_hz(14_100_000);
        let (id, _) = mgr.add_channel(14_074_000, &RigMode::USB).unwrap();
        mgr.remove_channel(id).unwrap();
        assert_eq!(mgr.channels().len(), 1);
    }

    #[test]
    fn cannot_remove_primary() {
        let p = make_pipeline();
        let mgr = SdrVirtualChannelManager::new(p, 1, 4);
        let primary_id = mgr.channels()[0].id;
        let err = mgr.remove_channel(primary_id).unwrap_err();
        assert!(matches!(err, VChanError::Permanent));
    }

    #[test]
    fn cap_enforced() {
        let p = make_pipeline();
        let mgr = SdrVirtualChannelManager::new(p, 1, 2); // primary + 1 virtual max
        mgr.update_center_hz(14_100_000);
        mgr.add_channel(14_074_000, &RigMode::USB).unwrap();
        let err = mgr.add_channel(14_075_000, &RigMode::USB).unwrap_err();
        assert!(matches!(err, VChanError::CapReached { .. }));
    }

    #[test]
    fn out_of_bandwidth() {
        let p = make_pipeline();
        let mgr = SdrVirtualChannelManager::new(p, 1, 4);
        // center_hz = 0, half_span = 960_000 Hz — 10 MHz is way out
        let err = mgr.add_channel(10_000_000, &RigMode::USB).unwrap_err();
        assert!(matches!(err, VChanError::OutOfBandwidth { .. }));
    }

    #[test]
    fn hidden_background_channels_are_outside_visible_cap() {
        let p = make_pipeline();
        let mgr = SdrVirtualChannelManager::new(p, 1, 2); // primary + 1 visible max
        mgr.update_center_hz(14_100_000);

        mgr.add_channel(14_074_000, &RigMode::USB).unwrap();
        let hidden_id = Uuid::new_v4();
        mgr.ensure_background_channel_pcm(hidden_id, 14_075_000, &RigMode::DIG)
            .unwrap();

        let visible = mgr.channels();
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().all(|channel| channel.id != hidden_id));
    }

    #[test]
    fn retune_keeps_virtual_channel_allocated() {
        let p = make_pipeline();
        let mgr = SdrVirtualChannelManager::new(p, 1, 4);
        mgr.update_center_hz(14_100_000);
        let mut destroyed_rx = mgr.subscribe_destroyed();

        let (id, _) = mgr.add_channel(14_074_000, &RigMode::USB).unwrap();
        mgr.update_center_hz(16_000_000);

        assert!(mgr.channels().iter().any(|channel| channel.id == id));
        assert!(matches!(
            destroyed_rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));

        mgr.update_center_hz(14_100_000);
        assert!(mgr.channels().iter().any(|channel| channel.id == id));
    }
}
