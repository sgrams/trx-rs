// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use actix_web::{delete, get, put, web, HttpResponse, Responder};
use pickledb::{PickleDb, PickleDbDumpPolicy, SerializationMethod};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::time;
use tracing::warn;
use trx_frontend::{FrontendRuntimeContext, SharedSpectrum, VChanAudioCmd};
use uuid::Uuid;

use crate::server::bookmarks::{Bookmark, BookmarkStore};
use crate::server::scheduler::SchedulerStatusMap;
use crate::server::vchan::{ClientChannel, ClientChannelManager};

const SUPPORTED_DECODER_KINDS: &[&str] = &["aprs", "ais", "ft8", "wspr", "hf-aprs"];
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
            PickleDb::load(path, PickleDbDumpPolicy::AutoDump, SerializationMethod::Json)
                .unwrap_or_else(|_| {
                    PickleDb::new(path, PickleDbDumpPolicy::AutoDump, SerializationMethod::Json)
                })
        } else {
            PickleDb::new(path, PickleDbDumpPolicy::AutoDump, SerializationMethod::Json)
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

    pub fn get(&self, rig_id: &str) -> Option<BackgroundDecodeConfig> {
        let db = self.db.read().unwrap_or_else(|e| e.into_inner());
        db.get::<BackgroundDecodeConfig>(&format!("bgd:{rig_id}"))
    }

    pub fn upsert(&self, config: &BackgroundDecodeConfig) -> bool {
        let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
        db.set(&format!("bgd:{}", config.rig_id), config).is_ok()
    }

    pub fn remove(&self, rig_id: &str) -> bool {
        let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
        db.rem(&format!("bgd:{rig_id}")).unwrap_or(false)
    }
}

pub struct BackgroundDecodeManager {
    store: Arc<BackgroundDecodeStore>,
    bookmarks: Arc<BookmarkStore>,
    context: Arc<FrontendRuntimeContext>,
    scheduler_status: SchedulerStatusMap,
    vchan_mgr: Arc<ClientChannelManager>,
    status: Arc<RwLock<HashMap<String, BackgroundDecodeStatus>>>,
    notify_tx: broadcast::Sender<()>,
}

impl BackgroundDecodeManager {
    pub fn new(
        store: Arc<BackgroundDecodeStore>,
        bookmarks: Arc<BookmarkStore>,
        context: Arc<FrontendRuntimeContext>,
        scheduler_status: SchedulerStatusMap,
        vchan_mgr: Arc<ClientChannelManager>,
    ) -> Arc<Self> {
        let (notify_tx, _) = broadcast::channel(16);
        Arc::new(Self {
            store,
            bookmarks,
            context,
            scheduler_status,
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

    pub fn get_config(&self, rig_id: &str) -> BackgroundDecodeConfig {
        self.store.get(rig_id).unwrap_or_else(|| BackgroundDecodeConfig {
            rig_id: rig_id.to_string(),
            enabled: false,
            bookmark_ids: Vec::new(),
        })
    }

    pub fn put_config(&self, mut config: BackgroundDecodeConfig) -> Option<BackgroundDecodeConfig> {
        config.bookmark_ids = dedup_ids(&config.bookmark_ids);
        if self.store.upsert(&config) {
            self.trigger();
            Some(config)
        } else {
            None
        }
    }

    pub fn reset_config(&self, rig_id: &str) -> bool {
        let removed = self.store.remove(rig_id);
        self.trigger();
        removed
    }

    pub fn status(&self, rig_id: &str) -> BackgroundDecodeStatus {
        if let Ok(status) = self.status.read() {
            if let Some(entry) = status.get(rig_id) {
                return entry.clone();
            }
        }
        let cfg = self.get_config(rig_id);
        let bookmarks: HashMap<String, Bookmark> = self
            .bookmarks
            .list()
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
                        decoder_kinds: bookmark
                            .map(|item| supported_decoder_kinds(&item.decoders))
                            .unwrap_or_default(),
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
            .remote_active_rig_id
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn send_audio_cmd(&self, cmd: VChanAudioCmd) {
        if let Ok(guard) = self.context.vchan_audio_cmd.lock() {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.try_send(cmd);
            }
        }
    }

    fn remove_channel(&self, channel: &VirtualBackgroundDecodeChannel) {
        self.send_audio_cmd(VChanAudioCmd::Remove(channel.uuid));
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
            bandwidth_hz: bookmark
                .bandwidth_hz
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32,
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

    fn reconcile(&self, runtime: &mut BackgroundRuntimeState, spectrum: &SharedSpectrum) {
        let active_rig_id = self.active_rig_id();

        if runtime.current_rig_id != active_rig_id {
            if let Some(prev_rig_id) = runtime.current_rig_id.clone() {
                if let Ok(mut guard) = self.status.write() {
                    if let Some(prev_status) = guard.get_mut(&prev_rig_id) {
                        prev_status.active_rig = false;
                    }
                }
            }
            self.clear_runtime_channels(runtime);
        }

        let Some(rig_id) = active_rig_id else {
            return;
        };
        runtime.current_rig_id = Some(rig_id.clone());

        let config = self.get_config(&rig_id);
        let selected = dedup_ids(&config.bookmark_ids);
        let users_connected = self.context.sse_clients.load(Ordering::Relaxed) > 0;
        let scheduled_bookmark_ids = if users_connected {
            Vec::new()
        } else {
            self.scheduler_bookmark_ids(&rig_id)
        };
        let selected_bookmarks: HashMap<String, Bookmark> = self
            .bookmarks
            .list()
            .into_iter()
            .filter(|bookmark| selected.iter().any(|id| id == &bookmark.id))
            .map(|bookmark| (bookmark.id.clone(), bookmark))
            .collect();

        let frame = spectrum.frame.as_ref().map(Arc::as_ref);
        let center_hz = frame.map(|frame| frame.center_hz);
        let sample_rate = frame.map(|frame| frame.sample_rate);
        let half_span_hz = frame.map(|frame| i64::from(frame.sample_rate) / 2);

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

            let decoder_kinds = supported_decoder_kinds(&bookmark.decoders);
            let mut status = BackgroundDecodeBookmarkStatus {
                bookmark_id: bookmark.id.clone(),
                bookmark_name: Some(bookmark.name.clone()),
                freq_hz: Some(bookmark.freq_hz),
                mode: Some(bookmark.mode.clone()),
                decoder_kinds: decoder_kinds.clone(),
                state: "disabled".to_string(),
                channel_kind: None,
            };

            if decoder_kinds.is_empty() {
                status.state = "no_supported_decoders".to_string();
                statuses.push(status);
                continue;
            }

            if !config.enabled {
                statuses.push(status);
                continue;
            }

            if !users_connected {
                status.state = "waiting_for_user".to_string();
                statuses.push(status);
                continue;
            }

            if scheduled_bookmark_ids.iter().any(|id| id == &bookmark.id) {
                status.state = "handled_by_scheduler".to_string();
                statuses.push(status);
                continue;
            }

            if self.virtual_channels_cover_bookmark(&rig_id, bookmark) {
                status.state = "handled_by_virtual_channel".to_string();
                status.channel_kind = Some(VISIBLE_CHANNEL_KIND_NAME.to_string());
                statuses.push(status);
                continue;
            }

            let (Some(center_hz), Some(half_span_hz)) = (center_hz, half_span_hz) else {
                status.state = "waiting_for_spectrum".to_string();
                statuses.push(status);
                continue;
            };

            let offset_hz = bookmark.freq_hz as i64 - center_hz as i64;
            if offset_hz.abs() > half_span_hz {
                status.state = "out_of_span".to_string();
                statuses.push(status);
                continue;
            }

            status.state = "active".to_string();
            status.channel_kind = Some(CHANNEL_KIND_NAME.to_string());
            let desired = self.desired_channel(&rig_id, bookmark, decoder_kinds);
            desired_channels.insert(bookmark.id.clone(), desired);
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
            self.send_audio_cmd(VChanAudioCmd::SubscribeBackground {
                uuid: desired.uuid,
                freq_hz: desired.freq_hz,
                mode: desired.mode.clone(),
                bandwidth_hz: desired.bandwidth_hz,
                decoder_kinds: desired.decoder_kinds.clone(),
            });
            runtime.active_channels.insert(bookmark_id, desired);
        }

        if let Ok(mut guard) = self.status.write() {
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
        let mut spectrum_rx = self.context.spectrum.subscribe();
        let mut interval = time::interval(Duration::from_secs(2));

        loop {
            self.reconcile(&mut runtime, &spectrum_rx.borrow().clone());
            tokio::select! {
                changed = spectrum_rx.changed() => {
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

fn dedup_ids(ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for id in ids {
        if !out.iter().any(|existing| existing == id) {
            out.push(id.clone());
        }
    }
    out
}

fn supported_decoder_kinds(decoders: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for decoder in decoders {
        let decoder = decoder.trim().to_ascii_lowercase();
        if SUPPORTED_DECODER_KINDS.contains(&decoder.as_str())
            && !out.iter().any(|existing| existing == &decoder)
        {
            out.push(decoder);
        }
    }
    out
}

fn channel_matches_bookmark(channel: &ClientChannel, bookmark: &Bookmark) -> bool {
    channel.freq_hz == bookmark.freq_hz && normalized_mode(&channel.mode) == normalized_mode(&bookmark.mode)
}

fn normalized_mode(mode: &str) -> String {
    mode.trim().to_ascii_lowercase()
}

#[get("/background-decode/{rig_id}")]
pub async fn get_background_decode(
    path: web::Path<String>,
    manager: web::Data<Arc<BackgroundDecodeManager>>,
) -> impl Responder {
    HttpResponse::Ok().json(manager.get_config(&path.into_inner()))
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
    match manager.put_config(config) {
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
    manager.reset_config(&rig_id);
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
    HttpResponse::Ok().json(manager.status(&path.into_inner()))
}
