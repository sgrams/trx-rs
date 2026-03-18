// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Client-side virtual channel registry.
//!
//! Each rig has a list of virtual channels tracked entirely within the HTTP
//! frontend process.  Channel 0 is permanent and mirrors the rig's current
//! dial frequency.  Additional channels are allocated by a tab (identified by
//! its SSE session UUID) and freed when that session disconnects or the tab
//! explicitly deletes them.
//!
//! Actual DSP on the server is unaffected by this registry in Phase 1; the
//! registry is the source of truth for metadata (freq/mode per channel) and
//! drives `SetFreq`/`SetMode` commands to the server when a tab selects or
//! tunes a channel.

use std::collections::HashMap;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use trx_frontend::VChanAudioCmd;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// HTTP-visible snapshot of one channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientChannel {
    pub id: Uuid,
    /// Position in the ordered list (0 = primary).
    pub index: usize,
    pub freq_hz: u64,
    pub mode: String,
    /// Audio filter bandwidth in Hz (0 = mode default).
    pub bandwidth_hz: u32,
    /// True for channel 0 — cannot be deleted.
    pub permanent: bool,
    /// Number of SSE sessions currently subscribed to this channel.
    pub subscribers: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedChannel {
    pub id: Uuid,
    pub freq_hz: u64,
    pub mode: String,
    pub bandwidth_hz: u32,
    pub scheduler_bookmark_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum VChanClientError {
    /// Channel cap would be exceeded.
    CapReached { max: usize },
    /// Channel UUID not found.
    NotFound,
    /// Tried to delete the permanent primary channel.
    Permanent,
}

impl std::fmt::Display for VChanClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VChanClientError::CapReached { max } => {
                write!(f, "channel cap reached (max {})", max)
            }
            VChanClientError::NotFound => write!(f, "channel not found"),
            VChanClientError::Permanent => write!(f, "cannot remove the primary channel"),
        }
    }
}

// ---------------------------------------------------------------------------
// Internal record
// ---------------------------------------------------------------------------

struct InternalChannel {
    id: Uuid,
    freq_hz: u64,
    mode: String,
    /// Audio filter bandwidth in Hz (0 = mode default).
    bandwidth_hz: u32,
    decoder_kinds: Vec<String>,
    permanent: bool,
    scheduler_bookmark_id: Option<String>,
    /// Session UUIDs currently subscribed to this channel.
    session_ids: Vec<Uuid>,
}

// ---------------------------------------------------------------------------
// ClientChannelManager
// ---------------------------------------------------------------------------

/// Per-rig channel registry shared across all actix handlers.
pub struct ClientChannelManager {
    /// rig_id → ordered channel list.
    rigs: RwLock<HashMap<String, Vec<InternalChannel>>>,
    /// session_id → (rig_id, channel_id).
    sessions: RwLock<HashMap<Uuid, (String, Uuid)>>,
    /// Broadcast used to push updated channel lists to SSE streams.
    /// Payload: JSON string (serialised `Vec<ClientChannel>`), prefixed by
    /// `"<rig_id>:"` so subscribers can filter by rig.
    pub change_tx: broadcast::Sender<String>,
    pub max_channels: usize,
    /// Optional sender to the audio-client task for virtual-channel audio commands.
    pub audio_cmd: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<VChanAudioCmd>>>,
}

impl ClientChannelManager {
    pub fn new(max_channels: usize) -> Self {
        let (change_tx, _) = broadcast::channel(64);
        Self {
            rigs: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            change_tx,
            max_channels: max_channels.max(1),
            audio_cmd: std::sync::Mutex::new(None),
        }
    }

    /// Wire the audio-command sender so the manager can dispatch
    /// `VChanAudioCmd` messages when channels are allocated/deleted/changed.
    pub fn set_audio_cmd(&self, tx: tokio::sync::mpsc::UnboundedSender<VChanAudioCmd>) {
        *self.audio_cmd.lock().unwrap() = Some(tx);
    }

    /// Fire-and-forget: send a `VChanAudioCmd` to the audio-client task.
    fn send_audio_cmd(&self, cmd: VChanAudioCmd) {
        if let Some(tx) = self.audio_cmd.lock().unwrap().as_ref() {
            let _ = tx.send(cmd);
        }
    }

    // -- helpers --------------------------------------------------------

    fn broadcast_change(&self, rig_id: &str, channels: &[InternalChannel]) {
        let list: Vec<ClientChannel> = channels
            .iter()
            .enumerate()
            .map(|(idx, c)| ClientChannel {
                id: c.id,
                index: idx,
                freq_hz: c.freq_hz,
                mode: c.mode.clone(),
                bandwidth_hz: c.bandwidth_hz,
                permanent: c.permanent || c.scheduler_bookmark_id.is_some(),
                subscribers: c.session_ids.len(),
            })
            .collect();
        if let Ok(json) = serde_json::to_string(&list) {
            let _ = self.change_tx.send(format!("{}:{}", rig_id, json));
        }
    }

    // -- public API -------------------------------------------------------

    /// Ensure channel 0 exists for `rig_id`.  Call this when the SSE stream
    /// first delivers rig state so the primary channel reflects the current freq.
    pub fn init_rig(&self, rig_id: &str, freq_hz: u64, mode: &str) {
        let mut rigs = self.rigs.write().unwrap();
        let channels = rigs.entry(rig_id.to_string()).or_default();
        if channels.is_empty() {
            channels.push(InternalChannel {
                id: Uuid::new_v4(),
                freq_hz,
                mode: mode.to_string(),
                bandwidth_hz: 0,
                decoder_kinds: Vec::new(),
                permanent: true,
                scheduler_bookmark_id: None,
                session_ids: Vec::new(),
            });
        }
    }

    /// Update channel 0's freq/mode when the server pushes a new rig state.
    pub fn update_primary(&self, rig_id: &str, freq_hz: u64, mode: &str) {
        let mut rigs = self.rigs.write().unwrap();
        if let Some(channels) = rigs.get_mut(rig_id) {
            if let Some(ch) = channels.first_mut() {
                if ch.freq_hz != freq_hz || ch.mode != mode {
                    ch.freq_hz = freq_hz;
                    ch.mode = mode.to_string();
                    self.broadcast_change(rig_id, channels);
                }
            }
        }
    }

    /// List all channels for a rig (returns empty vec if rig unknown).
    pub fn channels(&self, rig_id: &str) -> Vec<ClientChannel> {
        let rigs = self.rigs.read().unwrap();
        rigs.get(rig_id)
            .map(|chs| {
                chs.iter()
                    .enumerate()
                    .map(|(idx, c)| ClientChannel {
                        id: c.id,
                        index: idx,
                        freq_hz: c.freq_hz,
                        mode: c.mode.clone(),
                        bandwidth_hz: c.bandwidth_hz,
                        permanent: c.permanent || c.scheduler_bookmark_id.is_some(),
                        subscribers: c.session_ids.len(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Allocate a new virtual channel for `session_id`.
    /// If `session_id` already owns a channel on this rig, it is released first.
    /// Returns the new `ClientChannel` snapshot.
    pub fn allocate(
        &self,
        session_id: Uuid,
        rig_id: &str,
        freq_hz: u64,
        mode: &str,
    ) -> Result<ClientChannel, VChanClientError> {
        let mut rigs = self.rigs.write().unwrap();
        let channels = rigs.entry(rig_id.to_string()).or_default();

        if channels.len() >= self.max_channels {
            return Err(VChanClientError::CapReached {
                max: self.max_channels,
            });
        }

        let id = Uuid::new_v4();
        let idx = channels.len();
        channels.push(InternalChannel {
            id,
            freq_hz,
            mode: mode.to_string(),
            bandwidth_hz: 0,
            decoder_kinds: Vec::new(),
            permanent: false,
            scheduler_bookmark_id: None,
            session_ids: vec![session_id],
        });

        let snapshot = ClientChannel {
            id,
            index: idx,
            freq_hz,
            mode: mode.to_string(),
            bandwidth_hz: 0,
            permanent: false,
            subscribers: 1,
        };

        self.broadcast_change(rig_id, channels);

        // Update session → channel mapping.
        drop(rigs);
        self.sessions
            .write()
            .unwrap()
            .insert(session_id, (rig_id.to_string(), id));

        // Request server-side DSP channel + audio subscription.
        self.send_audio_cmd(VChanAudioCmd::Subscribe {
            uuid: id,
            freq_hz,
            mode: mode.to_string(),
            bandwidth_hz: 0,
            decoder_kinds: Vec::new(),
        });

        Ok(snapshot)
    }

    /// Subscribe an SSE session to a channel (by channel UUID).
    /// Idempotent.  Returns `None` if channel not found.
    pub fn subscribe_session(
        &self,
        session_id: Uuid,
        rig_id: &str,
        channel_id: Uuid,
    ) -> Option<ClientChannel> {
        // Release previous subscription on this rig.
        self.release_session_on_rig(session_id, rig_id);

        let mut rigs = self.rigs.write().unwrap();
        let channels = rigs.get_mut(rig_id)?;
        let (idx, ch) = channels
            .iter_mut()
            .enumerate()
            .find(|(_, c)| c.id == channel_id)?;

        if !ch.session_ids.contains(&session_id) {
            ch.session_ids.push(session_id);
        }
        let snapshot = ClientChannel {
            id: ch.id,
            index: idx,
            freq_hz: ch.freq_hz,
            mode: ch.mode.clone(),
            bandwidth_hz: ch.bandwidth_hz,
            permanent: ch.permanent || ch.scheduler_bookmark_id.is_some(),
            subscribers: ch.session_ids.len(),
        };

        self.broadcast_change(rig_id, channels);

        drop(rigs);
        self.sessions
            .write()
            .unwrap()
            .insert(session_id, (rig_id.to_string(), channel_id));

        Some(snapshot)
    }

    /// Release all channel subscriptions for `session_id` across all rigs.
    /// Auto-removes non-permanent channels that reach 0 subscribers.
    pub fn release_session(&self, session_id: Uuid) {
        let mapping = {
            let mut sessions = self.sessions.write().unwrap();
            sessions.remove(&session_id)
        };
        if let Some((rig_id, _)) = mapping {
            self.release_session_on_rig(session_id, &rig_id);
        }
    }

    fn release_session_on_rig(&self, session_id: Uuid, rig_id: &str) {
        let mut rigs = self.rigs.write().unwrap();
        let Some(channels) = rigs.get_mut(rig_id) else {
            return;
        };
        let mut changed = false;
        let mut removed_channel_ids = Vec::new();
        for ch in channels.iter_mut() {
            if let Some(pos) = ch.session_ids.iter().position(|&s| s == session_id) {
                ch.session_ids.remove(pos);
                changed = true;
            }
        }
        let mut idx = 0;
        while idx < channels.len() {
            if !channels[idx].permanent
                && channels[idx].scheduler_bookmark_id.is_none()
                && channels[idx].session_ids.is_empty()
            {
                removed_channel_ids.push(channels[idx].id);
                channels.remove(idx);
                changed = true;
            } else {
                idx += 1;
            }
        }
        if changed {
            self.broadcast_change(rig_id, channels);
        }
        drop(rigs);

        for channel_id in removed_channel_ids {
            self.send_audio_cmd(VChanAudioCmd::Remove(channel_id));
        }
    }

    /// Explicitly delete a channel by UUID (any session may do this).
    pub fn delete_channel(&self, rig_id: &str, channel_id: Uuid) -> Result<(), VChanClientError> {
        let mut rigs = self.rigs.write().unwrap();
        let channels = rigs.get_mut(rig_id).ok_or(VChanClientError::NotFound)?;
        let pos = channels
            .iter()
            .position(|c| c.id == channel_id)
            .ok_or(VChanClientError::NotFound)?;
        if channels[pos].permanent || channels[pos].scheduler_bookmark_id.is_some() {
            return Err(VChanClientError::Permanent);
        }
        // Collect evicted sessions to clean up the session map.
        let evicted: Vec<Uuid> = channels[pos].session_ids.clone();
        channels.remove(pos);
        self.broadcast_change(rig_id, channels);
        drop(rigs);

        let mut sessions = self.sessions.write().unwrap();
        for sid in evicted {
            sessions.remove(&sid);
        }

        // Remove server-side DSP channel and stop audio encoding.
        self.send_audio_cmd(VChanAudioCmd::Remove(channel_id));

        Ok(())
    }

    /// Remove a channel by UUID across all rigs (called when the server destroys
    /// it due to out-of-band center-frequency change).  Does NOT send a
    /// `VChanAudioCmd::Remove` since the server-side channel is already gone.
    pub fn remove_by_uuid(&self, channel_id: Uuid) {
        let evicted_sessions: Vec<Uuid>;
        let rig_id_opt: Option<String>;
        {
            let mut rigs = self.rigs.write().unwrap();
            let mut found = false;
            let mut evicted = Vec::new();
            let mut found_rig = None;
            for (rig_id, channels) in rigs.iter_mut() {
                if let Some(pos) = channels.iter().position(|c| c.id == channel_id) {
                    evicted = channels[pos].session_ids.clone();
                    channels.remove(pos);
                    self.broadcast_change(rig_id, channels);
                    found_rig = Some(rig_id.clone());
                    found = true;
                    break;
                }
            }
            evicted_sessions = evicted;
            rig_id_opt = found_rig;
            let _ = found; // suppress warning
        }
        // Clean up session → channel mapping for sessions that were subscribed.
        if rig_id_opt.is_some() {
            let mut sessions = self.sessions.write().unwrap();
            for sid in evicted_sessions {
                if matches!(sessions.get(&sid), Some((_, ch)) if *ch == channel_id) {
                    sessions.remove(&sid);
                }
            }
        }
    }

    /// Update freq/mode metadata for a channel.
    pub fn set_channel_freq(
        &self,
        rig_id: &str,
        channel_id: Uuid,
        freq_hz: u64,
    ) -> Result<(), VChanClientError> {
        let mut rigs = self.rigs.write().unwrap();
        let channels = rigs.get_mut(rig_id).ok_or(VChanClientError::NotFound)?;
        let ch = channels
            .iter_mut()
            .find(|c| c.id == channel_id)
            .ok_or(VChanClientError::NotFound)?;
        ch.freq_hz = freq_hz;
        self.broadcast_change(rig_id, channels);
        drop(rigs);
        self.send_audio_cmd(VChanAudioCmd::SetFreq {
            uuid: channel_id,
            freq_hz,
        });
        Ok(())
    }

    pub fn set_channel_mode(
        &self,
        rig_id: &str,
        channel_id: Uuid,
        mode: &str,
    ) -> Result<(), VChanClientError> {
        let mut rigs = self.rigs.write().unwrap();
        let channels = rigs.get_mut(rig_id).ok_or(VChanClientError::NotFound)?;
        let ch = channels
            .iter_mut()
            .find(|c| c.id == channel_id)
            .ok_or(VChanClientError::NotFound)?;
        ch.mode = mode.to_string();
        self.broadcast_change(rig_id, channels);
        drop(rigs);
        self.send_audio_cmd(VChanAudioCmd::SetMode {
            uuid: channel_id,
            mode: mode.to_string(),
        });
        Ok(())
    }

    pub fn set_channel_bandwidth(
        &self,
        rig_id: &str,
        channel_id: Uuid,
        bandwidth_hz: u32,
    ) -> Result<(), VChanClientError> {
        let mut rigs = self.rigs.write().unwrap();
        let channels = rigs.get_mut(rig_id).ok_or(VChanClientError::NotFound)?;
        let ch = channels
            .iter_mut()
            .find(|c| c.id == channel_id)
            .ok_or(VChanClientError::NotFound)?;
        ch.bandwidth_hz = bandwidth_hz;
        self.broadcast_change(rig_id, channels);
        drop(rigs);
        self.send_audio_cmd(VChanAudioCmd::SetBandwidth {
            uuid: channel_id,
            bandwidth_hz,
        });
        Ok(())
    }

    /// Return the channel a session is currently subscribed to.
    pub fn session_channel(&self, session_id: Uuid) -> Option<(String, Uuid)> {
        self.sessions.read().unwrap().get(&session_id).cloned()
    }

    /// Return the selected channel's tune metadata.
    pub fn selected_channel(&self, rig_id: &str, channel_id: Uuid) -> Option<SelectedChannel> {
        let rigs = self.rigs.read().unwrap();
        let channels = rigs.get(rig_id)?;
        let channel = channels.iter().find(|channel| channel.id == channel_id)?;
        Some(SelectedChannel {
            id: channel.id,
            freq_hz: channel.freq_hz,
            mode: channel.mode.clone(),
            bandwidth_hz: channel.bandwidth_hz,
            scheduler_bookmark_id: channel.scheduler_bookmark_id.clone(),
        })
    }

    /// Reconcile visible scheduler-managed channels for a rig.
    ///
    /// These channels are user-visible virtual channels sourced from the
    /// scheduler's currently active extra bookmarks. They are kept separate
    /// from user-allocated channels so connect-time sync can materialise them
    /// without duplicating arbitrary user state.
    pub fn sync_scheduler_channels(
        &self,
        rig_id: &str,
        desired: &[(String, u64, String, u32, Vec<String>)],
    ) {
        let mut rigs = self.rigs.write().unwrap();
        let Some(channels) = rigs.get_mut(rig_id) else {
            return;
        };

        let mut changed = false;
        let desired_map: HashMap<String, (u64, String, u32, Vec<String>)> = desired
            .iter()
            .map(
                |(bookmark_id, freq_hz, mode, bandwidth_hz, decoder_kinds)| {
                    (
                        bookmark_id.clone(),
                        (*freq_hz, mode.clone(), *bandwidth_hz, decoder_kinds.clone()),
                    )
                },
            )
            .collect();
        let desired_ids: std::collections::HashSet<&str> =
            desired_map.keys().map(String::as_str).collect();

        let mut idx = 0;
        while idx < channels.len() {
            let remove = if let Some(bookmark_id) = channels[idx].scheduler_bookmark_id.as_deref() {
                !desired_ids.contains(bookmark_id) && channels[idx].session_ids.is_empty()
            } else {
                false
            };
            if remove {
                let channel_id = channels[idx].id;
                channels.remove(idx);
                self.send_audio_cmd(VChanAudioCmd::Remove(channel_id));
                changed = true;
                continue;
            }
            idx += 1;
        }

        for channel in channels.iter_mut() {
            let Some(bookmark_id) = channel.scheduler_bookmark_id.as_deref() else {
                continue;
            };
            let Some((freq_hz, mode, bandwidth_hz, decoder_kinds)) = desired_map.get(bookmark_id)
            else {
                continue;
            };
            if channel.freq_hz != *freq_hz {
                channel.freq_hz = *freq_hz;
                self.send_audio_cmd(VChanAudioCmd::SetFreq {
                    uuid: channel.id,
                    freq_hz: *freq_hz,
                });
                changed = true;
            }
            if channel.mode != *mode {
                channel.mode = mode.clone();
                self.send_audio_cmd(VChanAudioCmd::SetMode {
                    uuid: channel.id,
                    mode: mode.clone(),
                });
                changed = true;
            }
            if channel.bandwidth_hz != *bandwidth_hz {
                channel.bandwidth_hz = *bandwidth_hz;
                self.send_audio_cmd(VChanAudioCmd::SetBandwidth {
                    uuid: channel.id,
                    bandwidth_hz: *bandwidth_hz,
                });
                changed = true;
            }
            if channel.decoder_kinds != *decoder_kinds {
                channel.decoder_kinds = decoder_kinds.clone();
                self.send_audio_cmd(VChanAudioCmd::Subscribe {
                    uuid: channel.id,
                    freq_hz: channel.freq_hz,
                    mode: channel.mode.clone(),
                    bandwidth_hz: channel.bandwidth_hz,
                    decoder_kinds: channel.decoder_kinds.clone(),
                });
                changed = true;
            }
        }

        for (bookmark_id, freq_hz, mode, bandwidth_hz, decoder_kinds) in desired {
            let exists = channels.iter().any(|channel| {
                channel.scheduler_bookmark_id.as_deref() == Some(bookmark_id.as_str())
            });
            if exists {
                continue;
            }
            if channels.len() >= self.max_channels {
                break;
            }
            let channel_id = Uuid::new_v4();
            channels.push(InternalChannel {
                id: channel_id,
                freq_hz: *freq_hz,
                mode: mode.clone(),
                bandwidth_hz: *bandwidth_hz,
                decoder_kinds: decoder_kinds.clone(),
                permanent: false,
                scheduler_bookmark_id: Some(bookmark_id.clone()),
                session_ids: Vec::new(),
            });
            self.send_audio_cmd(VChanAudioCmd::Subscribe {
                uuid: channel_id,
                freq_hz: *freq_hz,
                mode: mode.clone(),
                bandwidth_hz: *bandwidth_hz,
                decoder_kinds: decoder_kinds.clone(),
            });
            changed = true;
        }

        if changed {
            self.broadcast_change(rig_id, channels);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_session_removes_last_non_permanent_channel() {
        let mgr = ClientChannelManager::new(4);
        let rig_id = "rig-a";
        let session_id = Uuid::new_v4();

        mgr.init_rig(rig_id, 14_074_000, "USB");
        let channel = mgr
            .allocate(session_id, rig_id, 14_075_000, "DIG")
            .expect("allocate vchan");

        assert_eq!(mgr.channels(rig_id).len(), 2);

        mgr.release_session(session_id);

        let channels = mgr.channels(rig_id);
        assert_eq!(channels.len(), 1);
        assert!(channels.iter().all(|ch| ch.id != channel.id));
        assert!(mgr.session_channel(session_id).is_none());
    }

    #[test]
    fn sync_scheduler_channels_materializes_visible_scheduler_channels() {
        let mgr = ClientChannelManager::new(4);
        let rig_id = "rig-a";

        mgr.init_rig(rig_id, 14_074_000, "USB");
        mgr.sync_scheduler_channels(
            rig_id,
            &[(
                "bm-ft8".to_string(),
                14_074_000,
                "DIG".to_string(),
                3_000,
                vec!["ft8".to_string()],
            )],
        );

        let channels = mgr.channels(rig_id);
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[1].freq_hz, 14_074_000);
        assert_eq!(channels[1].mode, "DIG");
        assert_eq!(channels[1].bandwidth_hz, 3_000);
        assert_eq!(channels[1].subscribers, 0);
        assert!(channels[1].permanent);
    }

    #[test]
    fn release_session_keeps_scheduler_managed_channels() {
        let mgr = ClientChannelManager::new(4);
        let rig_id = "rig-a";
        let session_id = Uuid::new_v4();

        mgr.init_rig(rig_id, 14_074_000, "USB");
        let _channel = mgr
            .allocate(session_id, rig_id, 14_075_000, "DIG")
            .expect("allocate vchan");
        mgr.sync_scheduler_channels(
            rig_id,
            &[(
                "bm-ft8".to_string(),
                14_074_000,
                "DIG".to_string(),
                3_000,
                vec!["ft8".to_string()],
            )],
        );

        mgr.release_session(session_id);

        let channels = mgr.channels(rig_id);
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[1].mode, "DIG");
        assert_eq!(channels[1].subscribers, 0);
    }

    #[test]
    fn subscribed_scheduler_channel_survives_scheduler_clear_until_released() {
        let mgr = ClientChannelManager::new(4);
        let rig_id = "rig-a";
        let session_id = Uuid::new_v4();

        mgr.init_rig(rig_id, 14_074_000, "USB");
        mgr.sync_scheduler_channels(
            rig_id,
            &[(
                "bm-aprs".to_string(),
                144_800_000,
                "PKT".to_string(),
                12_500,
                vec!["aprs".to_string()],
            )],
        );

        let channel_id = mgr.channels(rig_id)[1].id;
        mgr.subscribe_session(session_id, rig_id, channel_id)
            .expect("subscribe scheduler channel");

        mgr.sync_scheduler_channels(rig_id, &[]);

        let channels = mgr.channels(rig_id);
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[1].id, channel_id);
        assert_eq!(channels[1].subscribers, 1);

        mgr.release_session(session_id);
        mgr.sync_scheduler_channels(rig_id, &[]);

        let channels = mgr.channels(rig_id);
        assert_eq!(channels.len(), 1);
    }
}
