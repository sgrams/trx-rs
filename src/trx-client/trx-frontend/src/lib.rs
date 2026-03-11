// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::RwLock;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize};
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

/// Command sent by the HTTP frontend to the audio-client task to manage a
/// virtual channel's audio stream over the server's audio TCP connection.
#[derive(Debug)]
pub enum VChanAudioCmd {
    /// Create the server-side DSP channel (if it does not exist) and subscribe
    /// to its Opus audio stream.  `freq_hz` and `mode` are used if the server
    /// needs to create the channel.
    Subscribe { uuid: Uuid, freq_hz: u64, mode: String },
    /// Unsubscribe from audio (encoder task is stopped) but keep the DSP channel.
    Unsubscribe(Uuid),
    /// Unsubscribe and destroy the DSP channel.
    Remove(Uuid),
    /// Update the dial frequency of an existing virtual channel.
    SetFreq { uuid: Uuid, freq_hz: u64 },
    /// Update the demodulation mode of an existing virtual channel.
    SetMode { uuid: Uuid, mode: String },
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
}

impl SharedSpectrum {
    /// Replace the stored frame, pre-serialising RDS in one pass.
    pub fn set(&mut self, frame: Option<SpectrumData>) {
        self.rds_json = frame
            .as_ref()
            .and_then(|f| f.rds.as_ref())
            .and_then(|r| serde_json::to_string(r).ok());
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
    /// AIS decode history (timestamp, message)
    pub ais_history: Arc<Mutex<VecDeque<(Instant, AisMessage)>>>,
    /// VDES decode history (timestamp, message)
    pub vdes_history: Arc<Mutex<VecDeque<(Instant, VdesMessage)>>>,
    /// APRS decode history (timestamp, packet)
    pub aprs_history: Arc<Mutex<VecDeque<(Instant, AprsPacket)>>>,
    /// HF APRS decode history (timestamp, packet)
    pub hf_aprs_history: Arc<Mutex<VecDeque<(Instant, AprsPacket)>>>,
    /// CW decode history (timestamp, event)
    pub cw_history: Arc<Mutex<VecDeque<(Instant, CwEvent)>>>,
    /// FT8 decode history (timestamp, message)
    pub ft8_history: Arc<Mutex<VecDeque<(Instant, Ft8Message)>>>,
    /// WSPR decode history (timestamp, message)
    pub wspr_history: Arc<Mutex<VecDeque<(Instant, WsprMessage)>>>,
    /// Authentication tokens for HTTP-JSON frontend
    pub auth_tokens: HashSet<String>,
    /// Active HTTP SSE clients (incremented on /events connect, decremented on disconnect).
    pub sse_clients: Arc<AtomicUsize>,
    /// Active rigctl TCP clients.
    pub rigctl_clients: Arc<AtomicUsize>,
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
    /// Currently selected remote rig id (used by remote client routing).
    pub remote_active_rig_id: Arc<Mutex<Option<String>>>,
    /// Cached remote rig list from GetRigs polling.
    pub remote_rigs: Arc<Mutex<Vec<RemoteRigEntry>>>,
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
}

impl FrontendRuntimeContext {
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
            wspr_history: Arc::new(Mutex::new(VecDeque::new())),
            auth_tokens: HashSet::new(),
            sse_clients: Arc::new(AtomicUsize::new(0)),
            rigctl_clients: Arc::new(AtomicUsize::new(0)),
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
            remote_active_rig_id: Arc::new(Mutex::new(None)),
            remote_rigs: Arc::new(Mutex::new(Vec::new())),
            owner_callsign: None,
            owner_website_url: None,
            owner_website_name: None,
            ais_vessel_url_base: None,
            spectrum: {
                let (tx, _rx) = watch::channel(SharedSpectrum::default());
                Arc::new(tx)
            },
            vchan_audio: Arc::new(RwLock::new(HashMap::new())),
            vchan_audio_cmd: Arc::new(Mutex::new(None)),
            vchan_destroyed: None,
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
