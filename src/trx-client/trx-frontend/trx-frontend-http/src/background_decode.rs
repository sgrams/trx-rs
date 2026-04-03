// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use actix_web::{delete, get, put, web, HttpResponse, Responder};
use pickledb::{PickleDb, PickleDbDumpPolicy, SerializationMethod};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::time;
use tracing::warn;
use trx_frontend::{FrontendRuntimeContext, SharedSpectrum, VChanAudioCmd};
use uuid::Uuid;

use crate::server::bookmarks::{Bookmark, BookmarkStoreMap};
use crate::server::scheduler::{SchedulerStatusMap, SharedSchedulerControlManager};
use crate::server::vchan::{ClientChannel, ClientChannelManager};

use trx_protocol::decoders::resolve_bookmark_decoders;
const CHANNEL_KIND_NAME: &str = "VirtualBackgroundDecodeChannel";
const VISIBLE_CHANNEL_KIND_NAME: &str = "VirtualChannel";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackgroundDecodeConfig {
    pub rig_id: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bookmark_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BackgroundDecodeBookmarkStatus {
    pub bookmark_id: String,
    pub bookmark_name: Option<String>,
    pub freq_hz: Option<u64>,
    pub mode: Option<String>,
    #[serde(default)]
    pub decoder_kinds: Vec<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BackgroundDecodeStatus {
    pub rig_id: String,
    pub enabled: bool,
    pub active_rig: bool,
    pub center_hz: Option<u64>,
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub entries: Vec<BackgroundDecodeBookmarkStatus>,
}

#[derive(Debug)]
struct VirtualBackgroundDecodeChannel {
    uuid: Uuid,
    rig_id: String,
    bookmark_id: String,
    freq_hz: u64,
    mode: String,
    bandwidth_hz: u32,
    decoder_kinds: Vec<String>,
}

#[derive(Default)]
struct BackgroundRuntimeState {
    current_rig_id: Option<String>,
    active_channels: HashMap<String, VirtualBackgroundDecodeChannel>,
}

pub struct BackgroundDecodeStore {
    db: Arc<RwLock<PickleDb>>,
}

impl BackgroundDecodeStore {
    pub fn open(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let db = if path.exists() {
            PickleDb::load(
                path,
                PickleDbDumpPolicy::AutoDump,
                SerializationMethod::Json,
            )
            .unwrap_or_else(|_| {
                PickleDb::new(
                    path,
                    PickleDbDumpPolicy::AutoDump,
                    SerializationMethod::Json,
                )
            })
        } else {
            PickleDb::new(
                path,
                PickleDbDumpPolicy::AutoDump,
                SerializationMethod::Json,
            )
        };
        Self {
            db: Arc::new(RwLock::new(db)),
        }
    }

    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .map(|p| p.join("trx-rs").join("background_decode.db"))
            .unwrap_or_else(|| PathBuf::from("background_decode.db"))
    }

    pub async fn get(&self, rig_id: &str) -> Option<BackgroundDecodeConfig> {
        let db = self.db.read().await;
        db.get::<BackgroundDecodeConfig>(&format!("bgd:{rig_id}"))
    }

    pub async fn upsert(&self, config: &BackgroundDecodeConfig) -> bool {
        let mut db = self.db.write().await;
        db.set(&format!("bgd:{}", config.rig_id), config).is_ok()
    }

    pub async fn remove(&self, rig_id: &str) -> bool {
        let mut db = self.db.write().await;
        db.rem(&format!("bgd:{rig_id}")).unwrap_or(false)
    }
}

pub struct BackgroundDecodeManager {
    store: Arc<BackgroundDecodeStore>,
    bookmarks: Arc<BookmarkStoreMap>,
    context: Arc<FrontendRuntimeContext>,
    scheduler_status: SchedulerStatusMap,
    scheduler_control: SharedSchedulerControlManager,
    vchan_mgr: Arc<ClientChannelManager>,
    status: Arc<RwLock<HashMap<String, BackgroundDecodeStatus>>>,
    notify_tx: broadcast::Sender<()>,
}

impl BackgroundDecodeManager {
    pub fn new(
        store: Arc<BackgroundDecodeStore>,
        bookmarks: Arc<BookmarkStoreMap>,
        context: Arc<FrontendRuntimeContext>,
        scheduler_status: SchedulerStatusMap,
        scheduler_control: SharedSchedulerControlManager,
        vchan_mgr: Arc<ClientChannelManager>,
    ) -> Arc<Self> {
        let (notify_tx, _) = broadcast::channel(16);
        Arc::new(Self {
            store,
            bookmarks,
            context,
            scheduler_status,
            scheduler_control,
            vchan_mgr,
            status: Arc::new(RwLock::new(HashMap::new())),
            notify_tx,
        })
    }

    pub fn spawn(self: &Arc<Self>) {
        let manager = self.clone();
        tokio::spawn(async move {
            manager.run().await;
        });
    }

    pub async fn get_config(&self, rig_id: &str) -> BackgroundDecodeConfig {
        self.store
            .get(rig_id)
            .await
            .unwrap_or_else(|| BackgroundDecodeConfig {
                rig_id: rig_id.to_string(),
                enabled: false,
                bookmark_ids: Vec::new(),
            })
    }

    pub async fn put_config(
        &self,
        mut config: BackgroundDecodeConfig,
    ) -> Option<BackgroundDecodeConfig> {
        config.bookmark_ids = dedup_ids(&config.bookmark_ids);
        if self.store.upsert(&config).await {
            self.trigger();
            Some(config)
        } else {
            None
        }
    }

    pub async fn reset_config(&self, rig_id: &str) -> bool {
        let removed = self.store.remove(rig_id).await;
        self.trigger();
        removed
    }

    pub async fn status(&self, rig_id: &str) -> BackgroundDecodeStatus {
        {
            let status = self.status.read().await;
            if let Some(entry) = status.get(rig_id) {
                return entry.clone();
            }
        }
        let cfg = self.get_config(rig_id).await;
        let bookmarks: HashMap<String, Bookmark> = self
            .bookmarks
            .list_for_rig(rig_id)
            .into_iter()
            .map(|bookmark| (bookmark.id.clone(), bookmark))
            .collect();
        BackgroundDecodeStatus {
            rig_id: rig_id.to_string(),
            enabled: cfg.enabled,
            active_rig: self.active_rig_id().as_deref() == Some(rig_id),
            center_hz: None,
            sample_rate: None,
            entries: cfg
                .bookmark_ids
                .into_iter()
                .map(|bookmark_id| {
                    let bookmark = bookmarks.get(&bookmark_id);
                    BackgroundDecodeBookmarkStatus {
                        bookmark_id,
                        bookmark_name: bookmark.map(|item| item.name.clone()),
                        freq_hz: bookmark.map(|item| item.freq_hz),
                        mode: bookmark.map(|item| item.mode.clone()),
                        decoder_kinds: bookmark.map(bookmark_decoder_kinds).unwrap_or_default(),
                        state: "inactive".to_string(),
                        channel_kind: None,
                    }
                })
                .collect(),
        }
    }

    pub fn trigger(&self) {
        let _ = self.notify_tx.send(());
    }

    fn active_rig_id(&self) -> Option<String> {
        self.context
            .routing
            .active_rig_id
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn send_audio_cmd_to_rig(&self, rig_id: &str, cmd: VChanAudioCmd) {
        // Route through per-rig sender when available.
        if let Ok(map) = self.context.vchan.rig_audio_cmd.read() {
            if let Some(tx) = map.get(rig_id) {
                let _ = tx.try_send(cmd);
                return;
            }
        }
        // Fall back to global sender.
        if let Ok(guard) = self.context.vchan.audio_cmd.lock() {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.try_send(cmd);
            }
        }
    }

    fn remove_channel(&self, channel: &VirtualBackgroundDecodeChannel) {
        self.send_audio_cmd_to_rig(&channel.rig_id, VChanAudioCmd::Remove(channel.uuid));
    }

    fn clear_runtime_channels(&self, runtime: &mut BackgroundRuntimeState) {
        let channels: Vec<VirtualBackgroundDecodeChannel> =
            runtime.active_channels.drain().map(|(_, ch)| ch).collect();
        for channel in channels {
            self.remove_channel(&channel);
        }
        runtime.current_rig_id = None;
    }

    fn desired_channel(
        &self,
        rig_id: &str,
        bookmark: &Bookmark,
        decoder_kinds: Vec<String>,
    ) -> VirtualBackgroundDecodeChannel {
        VirtualBackgroundDecodeChannel {
            uuid: Uuid::new_v4(),
            rig_id: rig_id.to_string(),
            bookmark_id: bookmark.id.clone(),
            freq_hz: bookmark.freq_hz,
            mode: bookmark.mode.clone(),
            bandwidth_hz: bookmark.bandwidth_hz.unwrap_or(0).min(u32::MAX as u64) as u32,
            decoder_kinds,
        }
    }

    fn channel_matches(
        channel: &VirtualBackgroundDecodeChannel,
        desired: &VirtualBackgroundDecodeChannel,
    ) -> bool {
        channel.rig_id == desired.rig_id
            && channel.bookmark_id == desired.bookmark_id
            && channel.freq_hz == desired.freq_hz
            && channel.mode == desired.mode
            && channel.bandwidth_hz == desired.bandwidth_hz
            && channel.decoder_kinds == desired.decoder_kinds
    }

    fn virtual_channels_cover_bookmark(&self, rig_id: &str, bookmark: &Bookmark) -> bool {
        self.vchan_mgr
            .channels(rig_id)
            .into_iter()
            .any(|channel| channel_matches_bookmark(&channel, bookmark))
    }

    async fn reconcile(&self, runtime: &mut BackgroundRuntimeState, spectrum: &SharedSpectrum) {
        let active_rig_id = self.active_rig_id();

        if runtime.current_rig_id != active_rig_id {
            if let Some(prev_rig_id) = runtime.current_rig_id.clone() {
                let mut guard = self.status.write().await;
                if let Some(prev_status) = guard.get_mut(&prev_rig_id) {
                    prev_status.active_rig = false;
                }
            }
            self.clear_runtime_channels(runtime);
        }

        let Some(rig_id) = active_rig_id else {
            return;
        };
        runtime.current_rig_id = Some(rig_id.clone());

        let config = self.get_config(&rig_id).await;
        let selected = dedup_ids(&config.bookmark_ids);
        let users_connected = self.context.sse_clients.load(Ordering::Relaxed) > 0;
        let scheduler_has_control = self.scheduler_control.scheduler_allowed() && users_connected;
        let scheduled_bookmark_ids = if scheduler_has_control || !users_connected {
            self.scheduler_bookmark_ids(&rig_id)
        } else {
            Vec::new()
        };
        let selected_bookmarks: HashMap<String, Bookmark> = self
            .bookmarks
            .list_for_rig(&rig_id)
            .into_iter()
            .filter(|bookmark| selected.iter().any(|id| id == &bookmark.id))
            .map(|bookmark| (bookmark.id.clone(), bookmark))
            .collect();

        let frame = spectrum.frame.as_ref().map(Arc::as_ref);
        let center_hz = frame.map(|frame| frame.center_hz);
        let sample_rate = frame.map(|frame| frame.sample_rate);
        let half_span_hz = frame.map(|frame| i64::from(frame.sample_rate) / 2);

        let spectrum_span = match (center_hz, half_span_hz) {
            (Some(c), Some(h)) => Some((c as i64, h)),
            _ => None,
        };

        let scheduled_set: HashSet<String> = scheduled_bookmark_ids.into_iter().collect();

        let mut statuses = Vec::new();
        let mut desired_channels = HashMap::new();

        for bookmark_id in selected {
            let Some(bookmark) = selected_bookmarks.get(&bookmark_id) else {
                statuses.push(BackgroundDecodeBookmarkStatus {
                    bookmark_id,
                    state: "missing_bookmark".to_string(),
                    ..BackgroundDecodeBookmarkStatus::default()
                });
                continue;
            };

            let decoder_kinds = bookmark_decoder_kinds(bookmark);
            let mut status = BackgroundDecodeBookmarkStatus {
                bookmark_id: bookmark.id.clone(),
                bookmark_name: Some(bookmark.name.clone()),
                freq_hz: Some(bookmark.freq_hz),
                mode: Some(bookmark.mode.clone()),
                decoder_kinds: decoder_kinds.clone(),
                state: "disabled".to_string(),
                channel_kind: None,
            };

            let vchan_covers = self.virtual_channels_cover_bookmark(&rig_id, bookmark);

            let action = evaluate_bookmark(
                decoder_kinds.is_empty(),
                config.enabled,
                users_connected,
                scheduler_has_control,
                &scheduled_set,
                &bookmark.id,
                vchan_covers,
                spectrum_span,
                bookmark.freq_hz,
            );

            match action {
                ChannelAction::Active => {
                    status.state = "active".to_string();
                    status.channel_kind = Some(CHANNEL_KIND_NAME.to_string());
                    let desired = self.desired_channel(&rig_id, bookmark, decoder_kinds);
                    desired_channels.insert(bookmark.id.clone(), desired);
                }
                ChannelAction::Skip { reason } => {
                    status.state = reason.to_string();
                    if reason == "handled_by_virtual_channel" {
                        status.channel_kind = Some(VISIBLE_CHANNEL_KIND_NAME.to_string());
                    }
                }
            }

            statuses.push(status);
        }

        let mut to_remove = Vec::new();
        for (bookmark_id, channel) in &runtime.active_channels {
            if let Some(desired) = desired_channels.get(bookmark_id) {
                if !Self::channel_matches(channel, desired) {
                    to_remove.push(bookmark_id.clone());
                }
            } else {
                to_remove.push(bookmark_id.clone());
            }
        }
        for bookmark_id in to_remove {
            if let Some(channel) = runtime.active_channels.remove(&bookmark_id) {
                self.remove_channel(&channel);
            }
        }

        for (bookmark_id, desired) in desired_channels {
            if runtime.active_channels.contains_key(&bookmark_id) {
                continue;
            }
            self.send_audio_cmd_to_rig(
                &desired.rig_id,
                VChanAudioCmd::SubscribeBackground {
                    uuid: desired.uuid,
                    freq_hz: desired.freq_hz,
                    mode: desired.mode.clone(),
                    bandwidth_hz: desired.bandwidth_hz,
                    decoder_kinds: desired.decoder_kinds.clone(),
                },
            );
            runtime.active_channels.insert(bookmark_id, desired);
        }

        let mut guard = self.status.write().await;
        guard.insert(
            rig_id.clone(),
            BackgroundDecodeStatus {
                rig_id,
                enabled: config.enabled,
                active_rig: true,
                center_hz,
                sample_rate,
                entries: statuses,
            },
        );
    }

    fn scheduler_bookmark_ids(&self, rig_id: &str) -> Vec<String> {
        let Ok(guard) = self.scheduler_status.read() else {
            return Vec::new();
        };
        let Some(status) = guard.get(rig_id) else {
            return Vec::new();
        };
        if !status.active {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Some(id) = status.last_bookmark_id.clone() {
            out.push(id);
        }
        for id in &status.last_bookmark_ids {
            if !out.iter().any(|existing| existing == id) {
                out.push(id.clone());
            }
        }
        out
    }

    async fn run(self: Arc<Self>) {
        let mut runtime = BackgroundRuntimeState::default();
        let mut notify_rx = self.notify_tx.subscribe();
        let mut spectrum_rx: Option<tokio::sync::watch::Receiver<SharedSpectrum>> = None;
        let mut interval = time::interval(Duration::from_secs(2));

        loop {
            let users_connected = self.context.sse_clients.load(Ordering::Relaxed) > 0;
            if users_connected && spectrum_rx.is_none() {
                spectrum_rx = Some(self.context.spectrum.sender.subscribe());
            } else if !users_connected {
                spectrum_rx = None;
            }

            let spectrum = spectrum_rx
                .as_ref()
                .map(|rx| rx.borrow().clone())
                .unwrap_or_default();
            self.reconcile(&mut runtime, &spectrum).await;
            tokio::select! {
                changed = async {
                    match spectrum_rx.as_mut() {
                        Some(rx) => rx.changed().await.map_err(|_| ()),
                        None => std::future::pending::<Result<(), ()>>().await,
                    }
                } => {
                    if changed.is_err() {
                        warn!("background decode: spectrum watch closed");
                        self.clear_runtime_channels(&mut runtime);
                        break;
                    }
                }
                recv = notify_rx.recv() => {
                    match recv {
                        Ok(()) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = interval.tick() => {}
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChannelAction {
    Active,
    Skip { reason: &'static str },
}

/// Pure decision function that determines whether a bookmark should produce an
/// active background-decode channel or be skipped (with a reason).
#[allow(clippy::too_many_arguments)]
fn evaluate_bookmark(
    decoder_kinds_empty: bool,
    enabled: bool,
    users_connected: bool,
    scheduler_has_control: bool,
    scheduled_bookmark_ids: &HashSet<String>,
    bookmark_id: &str,
    vchan_covers_bookmark: bool,
    spectrum_span: Option<(i64, i64)>,
    freq_hz: u64,
) -> ChannelAction {
    if decoder_kinds_empty {
        return ChannelAction::Skip {
            reason: "no_supported_decoders",
        };
    }
    if !enabled {
        return ChannelAction::Skip { reason: "disabled" };
    }
    if !users_connected {
        return ChannelAction::Skip {
            reason: "waiting_for_user",
        };
    }
    if scheduler_has_control {
        return ChannelAction::Skip {
            reason: "scheduler_has_control",
        };
    }
    if scheduled_bookmark_ids.contains(bookmark_id) {
        return ChannelAction::Skip {
            reason: "handled_by_scheduler",
        };
    }
    if vchan_covers_bookmark {
        return ChannelAction::Skip {
            reason: "handled_by_virtual_channel",
        };
    }
    let Some((center_hz, half_span_hz)) = spectrum_span else {
        return ChannelAction::Skip {
            reason: "waiting_for_spectrum",
        };
    };
    let offset_hz = freq_hz as i64 - center_hz;
    if offset_hz.abs() > half_span_hz {
        return ChannelAction::Skip {
            reason: "out_of_span",
        };
    }
    ChannelAction::Active
}

fn dedup_ids(ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for id in ids {
        if !out.iter().any(|existing| existing == id) {
            out.push(id.clone());
        }
    }
    out
}

fn bookmark_decoder_kinds(bookmark: &Bookmark) -> Vec<String> {
    resolve_bookmark_decoders(&bookmark.decoders, &bookmark.mode, true)
}

fn channel_matches_bookmark(channel: &ClientChannel, bookmark: &Bookmark) -> bool {
    channel.freq_hz == bookmark.freq_hz
        && normalized_mode(&channel.mode) == normalized_mode(&bookmark.mode)
}

fn normalized_mode(mode: &str) -> String {
    mode.trim().to_ascii_lowercase()
}

#[get("/background-decode/{rig_id}")]
pub async fn get_background_decode(
    path: web::Path<String>,
    manager: web::Data<Arc<BackgroundDecodeManager>>,
) -> impl Responder {
    HttpResponse::Ok().json(manager.get_config(&path.into_inner()).await)
}

#[put("/background-decode/{rig_id}")]
pub async fn put_background_decode(
    path: web::Path<String>,
    body: web::Json<BackgroundDecodeConfig>,
    manager: web::Data<Arc<BackgroundDecodeManager>>,
) -> impl Responder {
    let rig_id = path.into_inner();
    let mut config = body.into_inner();
    config.rig_id = rig_id;
    match manager.put_config(config).await {
        Some(saved) => HttpResponse::Ok().json(saved),
        None => HttpResponse::InternalServerError().body("failed to save background decode config"),
    }
}

#[delete("/background-decode/{rig_id}")]
pub async fn delete_background_decode(
    path: web::Path<String>,
    manager: web::Data<Arc<BackgroundDecodeManager>>,
) -> impl Responder {
    let rig_id = path.into_inner();
    manager.reset_config(&rig_id).await;
    HttpResponse::Ok().json(BackgroundDecodeConfig {
        rig_id,
        enabled: false,
        bookmark_ids: Vec::new(),
    })
}

#[get("/background-decode/{rig_id}/status")]
pub async fn get_background_decode_status(
    path: web::Path<String>,
    manager: web::Data<Arc<BackgroundDecodeManager>>,
) -> impl Responder {
    HttpResponse::Ok().json(manager.status(&path.into_inner()).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_scheduled() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn active_when_all_conditions_met() {
        let action = evaluate_bookmark(
            false, // decoder_kinds_empty
            true,  // enabled
            true,  // users_connected
            false, // scheduler_has_control
            &empty_scheduled(),
            "bm1",
            false,                      // vchan_covers_bookmark
            Some((14_074_000, 96_000)), // spectrum_span (center, half)
            14_074_000,                 // freq_hz
        );
        assert_eq!(action, ChannelAction::Active);
    }

    #[test]
    fn skip_no_supported_decoders() {
        let action = evaluate_bookmark(
            true,
            true,
            true,
            false,
            &empty_scheduled(),
            "bm1",
            false,
            Some((14_074_000, 96_000)),
            14_074_000,
        );
        assert_eq!(
            action,
            ChannelAction::Skip {
                reason: "no_supported_decoders"
            }
        );
    }

    #[test]
    fn skip_disabled() {
        let action = evaluate_bookmark(
            false,
            false,
            true,
            false,
            &empty_scheduled(),
            "bm1",
            false,
            Some((14_074_000, 96_000)),
            14_074_000,
        );
        assert_eq!(action, ChannelAction::Skip { reason: "disabled" });
    }

    #[test]
    fn skip_waiting_for_user() {
        let action = evaluate_bookmark(
            false,
            true,
            false,
            false,
            &empty_scheduled(),
            "bm1",
            false,
            Some((14_074_000, 96_000)),
            14_074_000,
        );
        assert_eq!(
            action,
            ChannelAction::Skip {
                reason: "waiting_for_user"
            }
        );
    }

    #[test]
    fn skip_scheduler_has_control() {
        let action = evaluate_bookmark(
            false,
            true,
            true,
            true,
            &empty_scheduled(),
            "bm1",
            false,
            Some((14_074_000, 96_000)),
            14_074_000,
        );
        assert_eq!(
            action,
            ChannelAction::Skip {
                reason: "scheduler_has_control"
            }
        );
    }

    #[test]
    fn skip_handled_by_scheduler() {
        let mut scheduled = HashSet::new();
        scheduled.insert("bm1".to_string());
        let action = evaluate_bookmark(
            false,
            true,
            true,
            false,
            &scheduled,
            "bm1",
            false,
            Some((14_074_000, 96_000)),
            14_074_000,
        );
        assert_eq!(
            action,
            ChannelAction::Skip {
                reason: "handled_by_scheduler"
            }
        );
    }

    #[test]
    fn skip_handled_by_virtual_channel() {
        let action = evaluate_bookmark(
            false,
            true,
            true,
            false,
            &empty_scheduled(),
            "bm1",
            true,
            Some((14_074_000, 96_000)),
            14_074_000,
        );
        assert_eq!(
            action,
            ChannelAction::Skip {
                reason: "handled_by_virtual_channel"
            }
        );
    }

    #[test]
    fn skip_waiting_for_spectrum() {
        let action = evaluate_bookmark(
            false,
            true,
            true,
            false,
            &empty_scheduled(),
            "bm1",
            false,
            None,
            14_074_000,
        );
        assert_eq!(
            action,
            ChannelAction::Skip {
                reason: "waiting_for_spectrum"
            }
        );
    }

    #[test]
    fn skip_out_of_span() {
        let action = evaluate_bookmark(
            false,
            true,
            true,
            false,
            &empty_scheduled(),
            "bm1",
            false,
            Some((14_074_000, 96_000)), // center 14.074 MHz, half span 96 kHz
            7_074_000,                  // way outside the span
        );
        assert_eq!(
            action,
            ChannelAction::Skip {
                reason: "out_of_span"
            }
        );
    }

    #[test]
    fn active_at_edge_of_span() {
        let action = evaluate_bookmark(
            false,
            true,
            true,
            false,
            &empty_scheduled(),
            "bm1",
            false,
            Some((14_074_000, 96_000)),
            14_074_000 + 96_000, // exactly at the edge
        );
        assert_eq!(action, ChannelAction::Active);
    }

    #[test]
    fn priority_no_decoders_over_disabled() {
        // Even if disabled, "no_supported_decoders" should take precedence
        let action = evaluate_bookmark(
            true,
            false,
            true,
            false,
            &empty_scheduled(),
            "bm1",
            false,
            Some((14_074_000, 96_000)),
            14_074_000,
        );
        assert_eq!(
            action,
            ChannelAction::Skip {
                reason: "no_supported_decoders"
            }
        );
    }
}
