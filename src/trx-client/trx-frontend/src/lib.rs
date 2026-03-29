// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::RwLock;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use uuid::Uuid;

use trx_core::audio::AudioStreamInfo;
use trx_core::decode::{
    AisMessage, AprsPacket, CwEvent, DecodedMessage, Ft8Message, VdesMessage, WsprMessage,
};
use trx_core::rig::state::{RigSnapshot, SpectrumData};
use trx_core::{DynResult, RigRequest, RigState};

/// Shared, timestamped decode history for a single decoder type.
///
/// Each entry is `(record_time, optional_rig_id, decoded_message)`.
pub type DecodeHistory<T> = Arc<Mutex<VecDeque<(Instant, Option<String>, T)>>>;

/// Command sent by the HTTP frontend to the audio-client task to manage a
/// virtual channel's audio stream over the server's audio TCP connection.
#[derive(Debug)]
pub enum VChanAudioCmd {
    /// Create the server-side DSP channel (if it does not exist) and subscribe
    /// to its Opus audio stream.  `freq_hz` and `mode` are used if the server
    /// needs to create the channel.
    Subscribe {
        uuid: Uuid,
        freq_hz: u64,
        mode: String,
        bandwidth_hz: u32,
        decoder_kinds: Vec<String>,
    },
    /// Create a hidden server-side DSP channel for background decoding.
    /// These channels are not enumerated as user-visible virtual channels and
    /// do not request an Opus audio stream back to the frontend.
    SubscribeBackground {
        uuid: Uuid,
        freq_hz: u64,
        mode: String,
        bandwidth_hz: u32,
        decoder_kinds: Vec<String>,
    },
    /// Unsubscribe from audio (encoder task is stopped) but keep the DSP channel.
    Unsubscribe(Uuid),
    /// Unsubscribe and destroy the DSP channel.
    Remove(Uuid),
    /// Update the dial frequency of an existing virtual channel.
    SetFreq { uuid: Uuid, freq_hz: u64 },
    /// Update the demodulation mode of an existing virtual channel.
    SetMode { uuid: Uuid, mode: String },
    /// Update the audio filter bandwidth of an existing virtual channel.
    SetBandwidth { uuid: Uuid, bandwidth_hz: u32 },
}

#[derive(Clone, Debug)]
pub struct RemoteRigEntry {
    pub rig_id: String,
    pub display_name: Option<String>,
    pub state: RigSnapshot,
    pub audio_port: Option<u16>,
}

/// Trait implemented by concrete frontends to expose a runner entrypoint.
pub trait FrontendSpawner {
    fn spawn_frontend(
        state_rx: watch::Receiver<RigState>,
        rig_tx: mpsc::Sender<RigRequest>,
        callsign: Option<String>,
        listen_addr: SocketAddr,
        context: Arc<FrontendRuntimeContext>,
    ) -> JoinHandle<()>;
}

/// Spectrum snapshot shared between the spectrum polling task and SSE clients.
///
/// Stored in a `watch::channel`; each SSE client subscribes and is woken
/// exactly when new data arrives (no 40 ms polling loop needed on the reader
/// side).  `Arc<SpectrumData>` makes clone O(1) regardless of bin count.
#[derive(Debug, Default, Clone)]
pub struct SharedSpectrum {
    /// Latest spectrum frame; `None` when the active backend has no spectrum.
    pub frame: Option<Arc<SpectrumData>>,
    /// RDS JSON pre-serialised at ingestion so SSE clients don't repeat the
    /// work on every tick.
    pub rds_json: Option<String>,
    /// Virtual-channel RDS JSON pre-serialised at ingestion.
    pub vchan_rds_json: Option<String>,
}

impl SharedSpectrum {
    /// Replace the stored frame, pre-serialising RDS in one pass.
    pub fn set(
        &mut self,
        frame: Option<SpectrumData>,
        vchan_rds: Option<Vec<trx_core::rig::state::VchanRdsEntry>>,
    ) {
        self.rds_json = frame
            .as_ref()
            .and_then(|f| f.rds.as_ref())
            .and_then(|r| serde_json::to_string(r).ok());
        self.vchan_rds_json = vchan_rds
            .as_ref()
            .and_then(|list| serde_json::to_string(list).ok());
        self.frame = frame.map(Arc::new);
    }
}

pub type FrontendSpawnFn = fn(
    watch::Receiver<RigState>,
    mpsc::Sender<RigRequest>,
    Option<String>,
    SocketAddr,
    Arc<FrontendRuntimeContext>,
) -> JoinHandle<()>;

/// Context for registering and spawning frontends.
#[derive(Clone)]
pub struct FrontendRegistrationContext {
    spawners: HashMap<String, FrontendSpawnFn>,
}

impl FrontendRegistrationContext {
    /// Create a new empty registration context.
    pub fn new() -> Self {
        Self {
            spawners: HashMap::new(),
        }
    }

    /// Register a frontend spawner under a stable name (e.g. "http").
    pub fn register_frontend(&mut self, name: &str, spawner: FrontendSpawnFn) {
        let key = normalize_name(name);
        self.spawners.insert(key, spawner);
    }

    /// Check whether a frontend name is registered.
    pub fn is_frontend_registered(&self, name: &str) -> bool {
        let key = normalize_name(name);
        self.spawners.contains_key(&key)
    }

    /// List registered frontend names.
    pub fn registered_frontends(&self) -> Vec<String> {
        let mut names: Vec<String> = self.spawners.keys().cloned().collect();
        names.sort();
        names
    }

    /// Spawn a registered frontend by name with runtime context.
    pub fn spawn_frontend(
        &self,
        name: &str,
        state_rx: watch::Receiver<RigState>,
        rig_tx: mpsc::Sender<RigRequest>,
        callsign: Option<String>,
        listen_addr: SocketAddr,
        context: Arc<FrontendRuntimeContext>,
    ) -> DynResult<JoinHandle<()>> {
        let key = normalize_name(name);
        let spawner = self
            .spawners
            .get(&key)
            .ok_or_else(|| format!("Unknown frontend: {}", name))?;
        Ok(spawner(state_rx, rig_tx, callsign, listen_addr, context))
    }

    /// Merge another registration context into this one.
    pub fn extend_from(&mut self, other: &FrontendRegistrationContext) {
        for (name, spawner) in &other.spawners {
            self.spawners.insert(name.clone(), *spawner);
        }
    }
}

impl Default for FrontendRegistrationContext {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Sub-structs for FrontendRuntimeContext decomposition
// ---------------------------------------------------------------------------

/// Audio streaming channels (server ↔ browser).
pub struct AudioContext {
    /// Audio RX broadcast channel (server → browser)
    pub rx: Option<broadcast::Sender<Bytes>>,
    /// Audio TX channel (browser → server)
    pub tx: Option<mpsc::Sender<Bytes>>,
    /// Audio stream info watch channel
    pub info: Option<watch::Receiver<Option<AudioStreamInfo>>>,
    /// Decode message broadcast channel
    pub decode_rx: Option<broadcast::Sender<DecodedMessage>>,
    /// Active audio WebSocket streams.
    pub clients: Arc<AtomicUsize>,
}

impl Default for AudioContext {
    fn default() -> Self {
        Self {
            rx: None,
            tx: None,
            info: None,
            decode_rx: None,
            clients: Arc::new(AtomicUsize::new(0)),
        }
    }
}

/// Decode history entries for all decoder types.
pub struct DecodeHistoryContext {
    pub ais: DecodeHistory<AisMessage>,
    pub vdes: DecodeHistory<VdesMessage>,
    pub aprs: DecodeHistory<AprsPacket>,
    pub hf_aprs: DecodeHistory<AprsPacket>,
    pub cw: DecodeHistory<CwEvent>,
    pub ft8: DecodeHistory<Ft8Message>,
    pub ft4: DecodeHistory<Ft8Message>,
    pub ft2: DecodeHistory<Ft8Message>,
    pub wspr: DecodeHistory<WsprMessage>,
}

impl Default for DecodeHistoryContext {
    fn default() -> Self {
        Self {
            ais: Arc::new(Mutex::new(VecDeque::new())),
            vdes: Arc::new(Mutex::new(VecDeque::new())),
            aprs: Arc::new(Mutex::new(VecDeque::new())),
            hf_aprs: Arc::new(Mutex::new(VecDeque::new())),
            cw: Arc::new(Mutex::new(VecDeque::new())),
            ft8: Arc::new(Mutex::new(VecDeque::new())),
            ft4: Arc::new(Mutex::new(VecDeque::new())),
            ft2: Arc::new(Mutex::new(VecDeque::new())),
            wspr: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

/// HTTP authentication configuration.
pub struct HttpAuthConfig {
    pub enabled: bool,
    pub rx_passphrase: Option<String>,
    pub control_passphrase: Option<String>,
    pub tx_access_control_enabled: bool,
    pub session_ttl_secs: u64,
    pub cookie_secure: bool,
    pub cookie_same_site: String,
    /// Authentication tokens for HTTP-JSON frontend.
    pub tokens: HashSet<String>,
}

impl Default for HttpAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rx_passphrase: None,
            control_passphrase: None,
            tx_access_control_enabled: true,
            session_ttl_secs: 480 * 60,
            cookie_secure: false,
            cookie_same_site: "Lax".to_string(),
            tokens: HashSet::new(),
        }
    }
}

/// HTTP UI display configuration.
pub struct HttpUiConfig {
    pub show_sdr_gain_control: bool,
    pub initial_map_zoom: u8,
    pub spectrum_coverage_margin_hz: u32,
    pub spectrum_usable_span_ratio: f32,
    pub decode_history_retention_min: u64,
    pub decode_history_retention_min_by_rig: HashMap<String, u64>,
}

impl Default for HttpUiConfig {
    fn default() -> Self {
        Self {
            show_sdr_gain_control: true,
            initial_map_zoom: 10,
            spectrum_coverage_margin_hz: 50_000,
            spectrum_usable_span_ratio: 0.92,
            decode_history_retention_min: 24 * 60,
            decode_history_retention_min_by_rig: HashMap::new(),
        }
    }
}

/// Remote rig routing and state management.
pub struct RigRoutingContext {
    /// Currently selected remote rig id.
    pub active_rig_id: Arc<Mutex<Option<String>>>,
    /// Cached remote rig list from GetRigs polling.
    pub remote_rigs: Arc<Mutex<Vec<RemoteRigEntry>>>,
    /// Cached satellite pass predictions from the server (GetSatPasses).
    pub sat_passes: Arc<RwLock<Option<trx_core::geo::PassPredictionResult>>>,
    /// Per-rig state watch channels, keyed by rig_id.
    pub rig_states: Arc<RwLock<HashMap<String, watch::Sender<RigState>>>>,
    /// Whether the remote client currently has an active TCP connection.
    pub server_connected: Arc<AtomicBool>,
    /// Per-rig server connection state.
    pub rig_server_connected: Arc<RwLock<HashMap<String, bool>>>,
}

impl Default for RigRoutingContext {
    fn default() -> Self {
        Self {
            active_rig_id: Arc::new(Mutex::new(None)),
            remote_rigs: Arc::new(Mutex::new(Vec::new())),
            sat_passes: Arc::new(RwLock::new(None)),
            rig_states: Arc::new(RwLock::new(HashMap::new())),
            server_connected: Arc::new(AtomicBool::new(false)),
            rig_server_connected: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

/// Owner/station metadata for frontend display.
#[derive(Default)]
pub struct OwnerInfo {
    pub callsign: Option<String>,
    pub website_url: Option<String>,
    pub website_name: Option<String>,
    pub ais_vessel_url_base: Option<String>,
}

/// Virtual channel audio management.
pub struct VChanContext {
    /// Per-virtual-channel Opus audio senders.
    pub audio: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>,
    /// Channel to send `VChanAudioCmd` to the audio-client task.
    pub audio_cmd: Arc<Mutex<Option<mpsc::Sender<VChanAudioCmd>>>>,
    /// Broadcast sender that fires when the server destroys a virtual channel.
    pub destroyed: Option<broadcast::Sender<Uuid>>,
    /// Per-rig virtual-channel command senders.
    pub rig_audio_cmd: Arc<RwLock<HashMap<String, mpsc::Sender<VChanAudioCmd>>>>,
}

impl Default for VChanContext {
    fn default() -> Self {
        Self {
            audio: Arc::new(RwLock::new(HashMap::new())),
            audio_cmd: Arc::new(Mutex::new(None)),
            destroyed: None,
            rig_audio_cmd: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

/// Spectrum data management.
pub struct SpectrumContext {
    /// Spectrum sender; SSE clients subscribe via `sender.subscribe()`.
    pub sender: Arc<watch::Sender<SharedSpectrum>>,
    /// Per-rig spectrum watch channels, keyed by rig_id.
    pub per_rig: Arc<RwLock<HashMap<String, watch::Sender<SharedSpectrum>>>>,
}

impl Default for SpectrumContext {
    fn default() -> Self {
        Self {
            sender: {
                let (tx, _rx) = watch::channel(SharedSpectrum::default());
                Arc::new(tx)
            },
            per_rig: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

/// Per-rig audio channels for multi-rig setups.
pub struct PerRigAudioContext {
    /// Per-rig RX audio broadcast senders.
    pub rx: Arc<RwLock<HashMap<String, broadcast::Sender<Bytes>>>>,
    /// Per-rig audio stream info watch channels.
    pub info: Arc<RwLock<HashMap<String, watch::Sender<Option<AudioStreamInfo>>>>>,
}

impl Default for PerRigAudioContext {
    fn default() -> Self {
        Self {
            rx: Arc::new(RwLock::new(HashMap::new())),
            info: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

/// Runtime context for frontend operation.
///
/// Decomposed into coherent sub-structs to improve readability and allow
/// frontends to access only the context groups they need.
pub struct FrontendRuntimeContext {
    /// Audio streaming channels.
    pub audio: AudioContext,
    /// Decode history for all decoder types.
    pub decode_history: DecodeHistoryContext,
    /// HTTP authentication configuration.
    pub http_auth: HttpAuthConfig,
    /// HTTP UI display configuration.
    pub http_ui: HttpUiConfig,
    /// Remote rig routing and state.
    pub routing: RigRoutingContext,
    /// Owner/station metadata.
    pub owner: OwnerInfo,
    /// Virtual channel management.
    pub vchan: VChanContext,
    /// Spectrum data.
    pub spectrum: SpectrumContext,
    /// Per-rig audio channels.
    pub rig_audio: PerRigAudioContext,
    /// Active HTTP SSE clients.
    pub sse_clients: Arc<AtomicUsize>,
    /// Active rigctl TCP clients.
    pub rigctl_clients: Arc<AtomicUsize>,
    /// rigctl listen endpoint, if enabled.
    pub rigctl_listen_addr: Arc<Mutex<Option<SocketAddr>>>,
    /// Guard to avoid spawning duplicate decode collectors.
    pub decode_collector_started: AtomicBool,
}

impl FrontendRuntimeContext {
    /// Get a watch receiver for a specific rig's state.
    pub fn rig_state_rx(&self, rig_id: &str) -> Option<watch::Receiver<RigState>> {
        self.routing
            .rig_states
            .read()
            .ok()
            .and_then(|map| map.get(rig_id).map(|tx| tx.subscribe()))
    }

    /// Get a watch receiver for a specific rig's spectrum.
    /// Lazily inserts a new channel if the rig_id is not yet present.
    pub fn rig_spectrum_rx(&self, rig_id: &str) -> watch::Receiver<SharedSpectrum> {
        if let Ok(map) = self.spectrum.per_rig.read() {
            if let Some(tx) = map.get(rig_id) {
                return tx.subscribe();
            }
        }
        // Insert on miss.
        if let Ok(mut map) = self.spectrum.per_rig.write() {
            map.entry(rig_id.to_string())
                .or_insert_with(|| watch::channel(SharedSpectrum::default()).0)
                .subscribe()
        } else {
            // Poisoned lock fallback: return a dummy receiver.
            watch::channel(SharedSpectrum::default()).1
        }
    }

    /// Subscribe to a specific rig's RX audio broadcast.
    pub fn rig_audio_subscribe(&self, rig_id: &str) -> Option<broadcast::Receiver<Bytes>> {
        self.rig_audio
            .rx
            .read()
            .ok()
            .and_then(|map| map.get(rig_id).map(|tx| tx.subscribe()))
    }

    /// Get a watch receiver for a specific rig's audio stream info.
    pub fn rig_audio_info_rx(
        &self,
        rig_id: &str,
    ) -> Option<watch::Receiver<Option<AudioStreamInfo>>> {
        self.rig_audio
            .info
            .read()
            .ok()
            .and_then(|map| map.get(rig_id).map(|tx| tx.subscribe()))
    }

    /// Create a new empty runtime context.
    pub fn new() -> Self {
        Self {
            audio: AudioContext::default(),
            decode_history: DecodeHistoryContext::default(),
            http_auth: HttpAuthConfig::default(),
            http_ui: HttpUiConfig::default(),
            routing: RigRoutingContext::default(),
            owner: OwnerInfo::default(),
            vchan: VChanContext::default(),
            spectrum: SpectrumContext::default(),
            rig_audio: PerRigAudioContext::default(),
            sse_clients: Arc::new(AtomicUsize::new(0)),
            rigctl_clients: Arc::new(AtomicUsize::new(0)),
            rigctl_listen_addr: Arc::new(Mutex::new(None)),
            decode_collector_started: AtomicBool::new(false),
        }
    }
}

impl Default for FrontendRuntimeContext {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}
