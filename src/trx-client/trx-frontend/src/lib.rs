// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use trx_core::audio::AudioStreamInfo;
use trx_core::decode::{AprsPacket, CwEvent, DecodedMessage, Ft8Message, WsprMessage};
use trx_core::rig::state::{RigSnapshot, SpectrumData};
use trx_core::{DynResult, RigRequest, RigState};

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

#[derive(Debug, Default)]
pub struct SharedSpectrum {
    revision: u64,
    frame: Option<SpectrumData>,
}

impl SharedSpectrum {
    pub fn replace(&mut self, frame: Option<SpectrumData>) {
        self.revision = self.revision.wrapping_add(1);
        self.frame = frame;
    }

    pub fn snapshot(&self) -> (u64, Option<SpectrumData>) {
        (self.revision, self.frame.clone())
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
    /// APRS decode history (timestamp, packet)
    pub aprs_history: Arc<Mutex<VecDeque<(Instant, AprsPacket)>>>,
    /// CW decode history (timestamp, event)
    pub cw_history: Arc<Mutex<VecDeque<(Instant, CwEvent)>>>,
    /// FT8 decode history (timestamp, message)
    pub ft8_history: Arc<Mutex<VecDeque<(Instant, Ft8Message)>>>,
    /// WSPR decode history (timestamp, message)
    pub wspr_history: Arc<Mutex<VecDeque<(Instant, WsprMessage)>>>,
    /// Authentication tokens for HTTP-JSON frontend
    pub auth_tokens: HashSet<String>,
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
    /// Currently selected remote rig id (used by remote client routing).
    pub remote_active_rig_id: Arc<Mutex<Option<String>>>,
    /// Cached remote rig list from GetRigs polling.
    pub remote_rigs: Arc<Mutex<Vec<RemoteRigEntry>>>,
    /// Owner callsign from trx-client config/CLI for frontend display.
    pub owner_callsign: Option<String>,
    /// Optional website URL for the web UI header title link.
    pub owner_website_url: Option<String>,
    /// Latest spectrum frame from the active SDR rig; None for non-SDR backends.
    pub spectrum: Arc<Mutex<SharedSpectrum>>,
}

impl FrontendRuntimeContext {
    /// Create a new empty runtime context.
    pub fn new() -> Self {
        Self {
            audio_rx: None,
            audio_tx: None,
            audio_info: None,
            decode_rx: None,
            aprs_history: Arc::new(Mutex::new(VecDeque::new())),
            cw_history: Arc::new(Mutex::new(VecDeque::new())),
            ft8_history: Arc::new(Mutex::new(VecDeque::new())),
            wspr_history: Arc::new(Mutex::new(VecDeque::new())),
            auth_tokens: HashSet::new(),
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
            remote_active_rig_id: Arc::new(Mutex::new(None)),
            remote_rigs: Arc::new(Mutex::new(Vec::new())),
            owner_callsign: None,
            owner_website_url: None,
            spectrum: Arc::new(Mutex::new(SharedSpectrum::default())),
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
