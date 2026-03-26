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

/// Runtime context for frontend operation, containing audio channels and decode state.
pub struct FrontendRuntimeContext {
    /// Audio RX broadcast channel (server → browser)
    pub audio_rx: Option<broadcast::Sender<Bytes>>,
    /// Audio TX channel (browser → server)
    pub audio_tx: Option<mpsc::Sender<Bytes>>,
    /// Audio stream info watch channel
    pub audio_info: Option<watch::Receiver<Option<AudioStreamInfo>>>,
    /// Decode message broadcast channel
    pub decode_rx: Option<broadcast::Sender<DecodedMessage>>,
    /// Decode history entry: (record_time, rig_id, message).
    /// AIS decode history
    pub ais_history: DecodeHistory<AisMessage>,
    /// VDES decode history
    pub vdes_history: DecodeHistory<VdesMessage>,
    /// APRS decode history
    pub aprs_history: DecodeHistory<AprsPacket>,
    /// HF APRS decode history
    pub hf_aprs_history: DecodeHistory<AprsPacket>,
    /// CW decode history
    pub cw_history: DecodeHistory<CwEvent>,
    /// FT8 decode history
    pub ft8_history: DecodeHistory<Ft8Message>,
    /// FT4 decode history
    pub ft4_history: DecodeHistory<Ft8Message>,
    /// FT2 decode history
    pub ft2_history: DecodeHistory<Ft8Message>,
    /// WSPR decode history
    pub wspr_history: DecodeHistory<WsprMessage>,
    /// Authentication tokens for HTTP-JSON frontend
    pub auth_tokens: HashSet<String>,
    /// Active HTTP SSE clients (incremented on /events connect, decremented on disconnect).
    pub sse_clients: Arc<AtomicUsize>,
    /// Active rigctl TCP clients.
    pub rigctl_clients: Arc<AtomicUsize>,
    /// Active audio WebSocket streams.
    pub audio_clients: Arc<AtomicUsize>,
    /// rigctl listen endpoint, if enabled.
    pub rigctl_listen_addr: Arc<Mutex<Option<SocketAddr>>>,
    /// Guard to avoid spawning duplicate decode collectors.
    pub decode_collector_started: AtomicBool,
    /// HTTP frontend authentication configuration (enabled, passphrases, TTL, etc.)
    pub http_auth_enabled: bool,
    /// HTTP frontend auth rx passphrase
    pub http_auth_rx_passphrase: Option<String>,
    /// HTTP frontend auth control passphrase
    pub http_auth_control_passphrase: Option<String>,
    /// HTTP frontend auth tx access control enabled
    pub http_auth_tx_access_control_enabled: bool,
    /// HTTP frontend auth session TTL in seconds
    pub http_auth_session_ttl_secs: u64,
    /// HTTP frontend auth cookie secure flag
    pub http_auth_cookie_secure: bool,
    /// HTTP frontend auth cookie same-site policy
    pub http_auth_cookie_same_site: String,
    /// Whether the HTTP UI should expose the RF Gain control.
    pub http_show_sdr_gain_control: bool,
    /// Initial APRS map zoom level when receiver coordinates are available.
    pub http_initial_map_zoom: u8,
    /// Spectrum center-retune guard margin on each side of the tuned passband.
    pub http_spectrum_coverage_margin_hz: u32,
    /// Fraction of the sampled spectrum span treated as usable by the web UI.
    pub http_spectrum_usable_span_ratio: f32,
    /// Default decode history retention in minutes.
    pub http_decode_history_retention_min: u64,
    /// Per-rig decode history retention overrides in minutes.
    pub http_decode_history_retention_min_by_rig: HashMap<String, u64>,
    /// Currently selected remote rig id (used by remote client routing).
    pub remote_active_rig_id: Arc<Mutex<Option<String>>>,
    /// Cached remote rig list from GetRigs polling.
    pub remote_rigs: Arc<Mutex<Vec<RemoteRigEntry>>>,
    /// Per-rig state watch channels, keyed by rig_id.
    /// Populated by the remote client poll loop so each SSE session can
    /// subscribe to a specific rig's state independently.
    pub rig_states: Arc<RwLock<HashMap<String, watch::Sender<RigState>>>>,
    /// Owner callsign from trx-client config/CLI for frontend display.
    pub owner_callsign: Option<String>,
    /// Optional website URL for the web UI header title link.
    pub owner_website_url: Option<String>,
    /// Optional website name for the web UI header title label.
    pub owner_website_name: Option<String>,
    /// Optional base URL used to link AIS vessel names as `<base><mmsi>`.
    pub ais_vessel_url_base: Option<String>,
    /// Spectrum sender; SSE clients subscribe via `spectrum.subscribe()`.
    pub spectrum: Arc<watch::Sender<SharedSpectrum>>,
    /// Per-rig spectrum watch channels, keyed by rig_id.
    /// Populated by the remote client spectrum polling task so each SSE
    /// session can subscribe to a specific rig's spectrum independently.
    pub rig_spectrums: Arc<RwLock<HashMap<String, watch::Sender<SharedSpectrum>>>>,
    /// Per-rig RX audio broadcast senders, keyed by rig_id.
    /// Each rig's audio client task publishes Opus frames here.
    pub rig_audio_rx: Arc<RwLock<HashMap<String, broadcast::Sender<Bytes>>>>,
    /// Per-rig audio stream info watch channels, keyed by rig_id.
    pub rig_audio_info: Arc<RwLock<HashMap<String, watch::Sender<Option<AudioStreamInfo>>>>>,
    /// Per-rig virtual-channel command senders, keyed by rig_id.
    pub rig_vchan_audio_cmd: Arc<RwLock<HashMap<String, mpsc::Sender<VChanAudioCmd>>>>,
    /// Per-virtual-channel Opus audio senders.
    /// Key: server-side virtual channel UUID.
    /// Value: `broadcast::Sender<Bytes>` that receives per-channel Opus packets
    /// forwarded by the audio-client task from `AUDIO_MSG_RX_FRAME_CH` frames.
    pub vchan_audio: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>,
    /// Channel to send `VChanAudioCmd` to the audio-client task, which in turn
    /// forwards `VCHAN_SUB` / `VCHAN_UNSUB` frames over the audio TCP connection.
    /// `None` when no audio connection is active.
    pub vchan_audio_cmd: Arc<Mutex<Option<mpsc::Sender<VChanAudioCmd>>>>,
    /// Broadcast sender that fires whenever the server destroys a virtual
    /// channel (e.g. out-of-bandwidth after center-frequency retune).
    /// The HTTP frontend subscribes to clean up `ClientChannelManager`.
    pub vchan_destroyed: Option<broadcast::Sender<Uuid>>,
    /// Whether the remote client currently has an active TCP connection to
    /// trx-server.  Set to `true` on successful connect, `false` on drop.
    pub server_connected: Arc<AtomicBool>,
}

impl FrontendRuntimeContext {
    /// Get a watch receiver for a specific rig's state.
    pub fn rig_state_rx(&self, rig_id: &str) -> Option<watch::Receiver<RigState>> {
        self.rig_states
            .read()
            .ok()
            .and_then(|map| map.get(rig_id).map(|tx| tx.subscribe()))
    }

    /// Get a watch receiver for a specific rig's spectrum.
    /// Lazily inserts a new channel if the rig_id is not yet present.
    pub fn rig_spectrum_rx(&self, rig_id: &str) -> watch::Receiver<SharedSpectrum> {
        if let Ok(map) = self.rig_spectrums.read() {
            if let Some(tx) = map.get(rig_id) {
                return tx.subscribe();
            }
        }
        // Insert on miss.
        if let Ok(mut map) = self.rig_spectrums.write() {
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
        self.rig_audio_rx
            .read()
            .ok()
            .and_then(|map| map.get(rig_id).map(|tx| tx.subscribe()))
    }

    /// Get a watch receiver for a specific rig's audio stream info.
    pub fn rig_audio_info_rx(
        &self,
        rig_id: &str,
    ) -> Option<watch::Receiver<Option<AudioStreamInfo>>> {
        self.rig_audio_info
            .read()
            .ok()
            .and_then(|map| map.get(rig_id).map(|tx| tx.subscribe()))
    }

    /// Create a new empty runtime context.
    pub fn new() -> Self {
        Self {
            audio_rx: None,
            audio_tx: None,
            audio_info: None,
            decode_rx: None,
            ais_history: Arc::new(Mutex::new(VecDeque::new())),
            vdes_history: Arc::new(Mutex::new(VecDeque::new())),
            aprs_history: Arc::new(Mutex::new(VecDeque::new())),
            hf_aprs_history: Arc::new(Mutex::new(VecDeque::new())),
            cw_history: Arc::new(Mutex::new(VecDeque::new())),
            ft8_history: Arc::new(Mutex::new(VecDeque::new())),
            ft4_history: Arc::new(Mutex::new(VecDeque::new())),
            ft2_history: Arc::new(Mutex::new(VecDeque::new())),
            wspr_history: Arc::new(Mutex::new(VecDeque::new())),
            auth_tokens: HashSet::new(),
            sse_clients: Arc::new(AtomicUsize::new(0)),
            rigctl_clients: Arc::new(AtomicUsize::new(0)),
            audio_clients: Arc::new(AtomicUsize::new(0)),
            rigctl_listen_addr: Arc::new(Mutex::new(None)),
            decode_collector_started: AtomicBool::new(false),
            http_auth_enabled: false,
            http_auth_rx_passphrase: None,
            http_auth_control_passphrase: None,
            http_auth_tx_access_control_enabled: true,
            http_auth_session_ttl_secs: 480 * 60,
            http_auth_cookie_secure: false,
            http_auth_cookie_same_site: "Lax".to_string(),
            http_show_sdr_gain_control: true,
            http_initial_map_zoom: 10,
            http_spectrum_coverage_margin_hz: 50_000,
            http_spectrum_usable_span_ratio: 0.92,
            http_decode_history_retention_min: 24 * 60,
            http_decode_history_retention_min_by_rig: HashMap::new(),
            remote_active_rig_id: Arc::new(Mutex::new(None)),
            remote_rigs: Arc::new(Mutex::new(Vec::new())),
            rig_states: Arc::new(RwLock::new(HashMap::new())),
            owner_callsign: None,
            owner_website_url: None,
            owner_website_name: None,
            ais_vessel_url_base: None,
            spectrum: {
                let (tx, _rx) = watch::channel(SharedSpectrum::default());
                Arc::new(tx)
            },
            rig_spectrums: Arc::new(RwLock::new(HashMap::new())),
            rig_audio_rx: Arc::new(RwLock::new(HashMap::new())),
            rig_audio_info: Arc::new(RwLock::new(HashMap::new())),
            rig_vchan_audio_cmd: Arc::new(RwLock::new(HashMap::new())),
            vchan_audio: Arc::new(RwLock::new(HashMap::new())),
            vchan_audio_cmd: Arc::new(Mutex::new(None)),
            vchan_destroyed: None,
            server_connected: Arc::new(AtomicBool::new(false)),
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
