# trx-rs Code Design & Architecture Overview

## Table of Contents

1. [Project Purpose](#project-purpose)
2. [Technology Stack](#technology-stack)
3. [High-Level Architecture](#high-level-architecture)
4. [Crate Layout](#crate-layout)
5. [Core Library (trx-core)](#core-library-trx-core)
6. [Protocol Layer (trx-protocol)](#protocol-layer-trx-protocol)
7. [Server (trx-server)](#server-trx-server)
8. [Backend Abstraction (trx-backend)](#backend-abstraction-trx-backend)
9. [Client (trx-client)](#client-trx-client)
10. [Frontend System (trx-frontend)](#frontend-system-trx-frontend)
11. [Signal Decoders](#signal-decoders)
12. [DSP & Spectrum Pipeline](#dsp--spectrum-pipeline)
13. [Plugin System](#plugin-system)
14. [Configuration](#configuration)
15. [Concurrency Model](#concurrency-model)
16. [Authentication & Security](#authentication--security)
17. [Data Flow Diagrams](#data-flow-diagrams)

---

## Project Purpose

**trx-rs** is a modular amateur radio transceiver control daemon written in Rust. It separates radio hardware access (server) from user-facing control interfaces (client), enabling:

- **Remote control** of transceivers over TCP networks
- **Multi-rig operation** with per-rig isolation and routing
- **SDR integration** with real-time DSP (demodulation, spectrum, decode)
- **Pluggable backends** for different radio hardware
- **Multiple frontends** — web UI, Hamlib-compatible rigctl, JSON-over-TCP
- **Signal decoding** — APRS, CW, FT8, WSPR, RDS — with live streaming and logging
- **Uplinks** — PSKReporter, APRS-IS IGate

Target users are amateur radio operators who want networked, automated, or multi-radio control from a single host or across a LAN.

---

## Technology Stack

| Layer | Technology |
|-------|-----------|
| Language | Rust (2021 edition) |
| Async runtime | Tokio |
| Web framework | Actix-web (HTTP frontend) |
| Serialization | Serde / JSON |
| Config format | TOML |
| Audio codec | Opus |
| SDR interface | soapysdr crate (wraps SoapySDR C library) |
| CAT serial | tokio-serial |
| CLI | clap |
| Logging | tracing / tracing-subscriber |
| FT8 decode | ft8_lib (external C library via FFI) |

---

## High-Level Architecture

```
┌──────────────────────────────────────────────────────────┐
│                        trx-server                        │
│                                                          │
│  Radio Hardware (serial/TCP)                             │
│       ↕  CAT protocol                                    │
│  Rig Backend ──────── rig_task.rs ─── listener.rs        │
│  (ft817/ft450d/sdr)   (state machine)   (JSON TCP :4530) │
│                            │                             │
│                       audio.rs                           │
│                     (Opus :4531)                         │
│                            │                             │
│                      Decoders                            │
│               (APRS, CW, FT8, WSPR, RDS)                │
│                  PSKReporter / APRS-IS                   │
└──────────────────────────────────────────────────────────┘
                   ↕  JSON TCP (port 4530)
                   ↕  Opus audio TCP (port 4531)
┌──────────────────────────────────────────────────────────┐
│                        trx-client                        │
│                                                          │
│  remote_client.rs (polls state, routes commands)         │
│       ↕  internal mpsc/watch channels                    │
│  Frontends:                                              │
│    trx-frontend-http      (Web UI    :8080)              │
│    trx-frontend-rigctl    (rigctl    :4532)              │
│    trx-frontend-http-json (JSON/TCP  ephemeral)          │
└──────────────────────────────────────────────────────────┘
                   ↕  Browser / Hamlib / Custom tools
                        End Users
```

The server and client are separate binaries. They communicate over **JSON-over-TCP** (control) and **Opus-encoded TCP** (audio). Both binaries can load shared-library plugins at startup.

---

## Crate Layout

```
trx-rs/                          # Workspace root
├── Cargo.toml                   # Workspace manifest (shared dependencies)
├── CLAUDE.md                    # Contributor notes
│
└── src/
    ├── trx-core/                # Core types, traits, state machine
    ├── trx-protocol/            # Client↔server message types, auth, codec
    ├── trx-app/                 # Shared app helpers (config loading, plugins, logging)
    │
    ├── trx-server/              # Server binary
    │   ├── src/
    │   │   ├── main.rs
    │   │   ├── config.rs
    │   │   ├── rig_task.rs      # Per-rig polling loop
    │   │   ├── listener.rs      # JSON TCP server (:4530)
    │   │   ├── audio.rs         # Opus audio server (:4531)
    │   │   ├── pskreporter.rs   # PSKReporter uplink
    │   │   └── aprsfi.rs        # APRS-IS IGate uplink
    │   │
    │   └── trx-backend/         # Backend abstraction + factory
    │       ├── src/lib.rs       # RegistrationContext, RigAccess enum
    │       ├── trx-backend-ft817/    # Yaesu FT-817 CAT
    │       ├── trx-backend-ft450d/   # Yaesu FT-450D CAT
    │       └── trx-backend-soapysdr/ # SoapySDR SDR (RX-only)
    │           ├── src/
    │           │   ├── lib.rs        # SoapySdrRig impl
    │           │   ├── real_iq_source.rs
    │           │   ├── dsp/          # DSP pipeline, FIR, oscillator, AGC
    │           │   ├── demod/        # AM, FM, WFM, SSB, CW demodulators
    │           │   └── spectrum.rs   # FFT spectrum generation
    │
    ├── trx-client/              # Client binary
    │   ├── src/
    │   │   ├── main.rs
    │   │   ├── config.rs
    │   │   ├── remote_client.rs # TCP connection to server
    │   │   └── audio_client.rs  # Audio stream handler
    │   │
    │   └── trx-frontend/        # Frontend abstraction + registration
    │       ├── src/lib.rs       # FrontendSpawner trait, FrontendRuntimeContext
    │       ├── trx-frontend-http/      # Actix-web: REST + SSE + WebSocket
    │       ├── trx-frontend-http-json/ # JSON-over-TCP thin control frontend
    │       └── trx-frontend-rigctl/    # Hamlib-compatible rigctl TCP (:4532)
    │
    └── decoders/
        ├── trx-aprs/            # APRS packet decoder
        ├── trx-cw/              # CW / Morse decoder
        ├── trx-ft8/             # FT8 decoder (wraps ft8_lib C library)
        ├── trx-wspr/            # WSPR beacon decoder
        ├── trx-rds/             # FM RDS decoder
        └── trx-decode-log/      # JSON Lines log rotation for decoded frames
```

---

## Core Library (trx-core)

**Path:** `src/trx-core/src/`

The foundation of the system. All other crates depend on trx-core for shared types and traits.

### Key Re-exports (`lib.rs`)

```rust
pub use rig::command::RigCommand;
pub use rig::request::RigRequest;
pub use rig::response::{RigError, RigResult};
pub use rig::state::{RigMode, RigSnapshot, RigState, RigFilterState, SpectrumData};
pub use rig::AudioSource;
pub use decode::DecodedMessage;
pub use audio::AudioStreamInfo;
```

### Rig State (`rig/state.rs`)

The `RigState` struct is the canonical snapshot of a rig at any point in time:

```rust
pub struct RigState {
    pub rig_info: Option<RigInfo>,
    pub status: RigStatus,
    pub initialized: bool,
    pub control: RigControl,
    pub server_callsign: Option<String>,
    pub spectrum: Option<SpectrumData>,   // FFT frame from SDR
    pub filter: Option<RigFilterState>,   // Runtime DSP parameters
    // ... decoder enable flags, CW params, etc.
}

pub struct RigStatus {
    pub freq: Freq,
    pub mode: RigMode,
    pub tx_en: bool,
    pub vfo: Option<RigVfo>,
    pub tx: Option<RigTxStatus>,   // power, SWR, ALC
    pub rx: Option<RigRxStatus>,   // signal strength
    pub lock: Option<bool>,
}

pub enum RigMode {
    LSB, USB, CW, CWR, AM, WFM, FM, DIG, PKT, Other(String)
}
```

### Rig Commands (`rig/command.rs`)

All control actions are represented as enum variants:

```rust
pub enum RigCommand {
    // Basic control
    GetSnapshot, SetFreq(Freq), SetMode(RigMode), SetPtt(bool),
    PowerOn, PowerOff, ToggleVfo, Lock, Unlock,
    // TX
    GetTxLimit, SetTxLimit(u8),
    // Decoders
    SetAprsDecodeEnabled(bool), SetCwDecodeEnabled(bool),
    SetFt8DecodeEnabled(bool), SetWsprDecodeEnabled(bool),
    ResetAprsDecoder, ResetCwDecoder, ResetFt8Decoder, ResetWsprDecoder,
    // CW keyer
    SetCwAuto(bool), SetCwWpm(u32), SetCwToneHz(u32),
    // SDR DSP
    SetBandwidth(u32), SetFirTaps(u32), SetSdrGain(f64),
    SetCenterFreq(Freq), GetSpectrum,
    // WFM
    SetWfmDeemphasis(u32), SetWfmStereo(bool), SetWfmDenoise(bool),
}
```

### State Machine (`rig/controller/machine.rs`)

Manages the lifecycle of a rig connection:

```
Disconnected → Connecting → Initializing → PoweredOff
                                       ↘
                                        Ready ⇄ Transmitting
                                          ↓
                                        Error
                                          ↓ (recoverable)
                                        Connecting
```

```rust
pub enum RigMachineState {
    Disconnected,
    Connecting,
    Initializing,
    PoweredOff,
    Ready,
    Transmitting,
    Error(RigStateError),
}
```

Transitions are triggered by `RigEvent` (Connected, PoweredOn, PttOn, Error, etc.) and processed by `process_event(&mut self, event: RigEvent)`.

### Command Handlers (`rig/controller/handlers.rs`)

Each command implements `RigCommandHandler`:

```rust
pub trait RigCommandHandler: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult;
    fn execute<'a>(
        &'a self, executor: &'a mut dyn CommandExecutor
    ) -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>>;
}

pub enum ValidationResult {
    Ok,
    InvalidState(String),   // Wrong machine state
    InvalidParams(String),  // Bad parameters
    Locked,                 // Rig is locked
}
```

### Event System (`rig/controller/events.rs`)

Observers subscribe via the `RigListener` trait. `RigEventEmitter` maintains a list of `Arc<dyn RigListener>` and calls them on state changes.

```rust
pub trait RigListener: Send + Sync {
    fn on_frequency_change(&self, old: Option<Freq>, new: Freq) {}
    fn on_mode_change(&self, old: Option<&RigMode>, new: &RigMode) {}
    fn on_ptt_change(&self, transmitting: bool) {}
    fn on_state_change(&self, old: &RigMachineState, new: &RigMachineState) {}
    fn on_meter_update(&self, rx: Option<&RigRxStatus>, tx: Option<&RigTxStatus>) {}
    fn on_lock_change(&self, locked: bool) {}
    fn on_power_change(&self, powered: bool) {}
}
```

### Operational Policies (`rig/controller/policies.rs`)

Govern reconnection and polling behaviour:

```rust
pub trait RetryPolicy: Send {
    fn next_delay(&mut self) -> Duration;
}

pub struct ExponentialBackoff {
    initial_delay: Duration,
    max_delay: Duration,
    multiplier: f64,
    current_delay: Duration,
}

pub trait PollingPolicy: Send {
    fn next_interval(&mut self) -> Duration;
}

pub struct AdaptivePolling {
    idle_interval: Duration,
    tx_interval: Duration, // faster polling during TX
}
```

### Audio Wire Format (`audio.rs`)

```
[ 1 byte type ][ 4 bytes BE length ][ N bytes payload ]

Types:
  0x00  AudioStreamInfo  (sample rate, channels, frame duration)
  0x01  RX audio frame   (Opus-encoded PCM)
  0x02  TX audio frame   (Opus-encoded PCM)
  0x03  APRS decode
  0x04  CW decode
  0x05  FT8 decode
  0x06  WSPR decode
```

### Error Types (`rig/response.rs`)

```rust
pub struct RigError {
    pub message: String,
    pub kind: RigErrorKind,
}

pub enum RigErrorKind {
    Transient,   // Retry-able (timeout, busy)
    Permanent,   // Don't retry (unsupported operation)
}

pub type RigResult<T>  = Result<T, RigError>;
pub type DynResult<T>  = Result<T, Box<dyn std::error::Error + Send + Sync>>;
```

---

## Protocol Layer (trx-protocol)

**Path:** `src/trx-protocol/src/`

Bridges the internal `RigCommand`/`RigState` world to JSON messages exchanged over TCP.

### Message Types (`types.rs`)

```rust
// Client → Server
pub struct ClientEnvelope {
    pub token: Option<String>,    // Auth token
    pub rig_id: Option<String>,   // Multi-rig routing (None = default rig)
    pub cmd: ClientCommand,
}

pub enum ClientCommand {
    GetState, GetRigs,
    SetFreq { freq_hz: u64 }, SetCenterFreq { freq_hz: u64 },
    SetMode { mode: String }, SetPtt { ptt: bool },
    PowerOn, PowerOff, ToggleVfo, Lock, Unlock,
    GetTxLimit, SetTxLimit { limit: u8 },
    SetBandwidth { bandwidth_hz: u32 }, SetFirTaps { taps: u32 },
    SetSdrGain { gain_db: f64 },
    SetWfmDeemphasis { deemphasis_us: u32 },
    SetWfmStereo { enabled: bool }, SetWfmDenoise { enabled: bool },
    SetAprsDecodeEnabled { enabled: bool }, /* ... other decoders ... */
    GetSpectrum,
    // ...
}

// Server → Client
pub struct ClientResponse {
    pub success: bool,
    pub rig_id: Option<String>,
    pub state: Option<RigSnapshot>,   // Updated rig state
    pub rigs: Option<Vec<RigEntry>>,  // Response to GetRigs
    pub error: Option<String>,
}

pub struct RigEntry {
    pub rig_id: String,
    pub display_name: Option<String>,
    pub state: RigSnapshot,
    pub audio_port: Option<u16>,
}
```

### Type Mapping (`mapping.rs`)

`client_command_to_rig(ClientCommand) → RigCommand` and the reverse conversion ensure the protocol types stay decoupled from the core domain model.

### Authentication (`auth.rs`)

```rust
pub trait TokenValidator: Send + Sync {
    fn validate(&self, token: &str) -> bool;
}

pub struct SimpleTokenValidator { tokens: HashSet<String> }
pub struct NoAuthValidator;   // Always returns true (debug/local use)
```

---

## Server (trx-server)

**Path:** `src/trx-server/src/`

### Startup Sequence

1. Parse CLI / TOML config (`config.rs`)
2. Register backends via `RegistrationContext` (built-ins + plugins)
3. For each configured rig:
   - Build or pre-configure the rig backend
   - Spawn `run_rig_task()` as a Tokio task
4. Spawn `run_listener()` (JSON TCP on `:4530`)
5. Spawn audio streaming server (`:4531`)
6. Wait for shutdown signal

### Multi-Rig Routing

Rigs are stored in `Arc<HashMap<String, RigHandle>>`. Each `RigHandle` contains:
- `mpsc::Sender<RigRequest>` — send commands to the rig task
- `watch::Receiver<RigState>` — read latest state

`listener.rs` routes incoming `ClientEnvelope.rig_id` to the correct handle. If `rig_id` is absent, the server's default rig is used.

Auto-generated IDs follow the pattern `{model}_{index}` (e.g., `ft817_0`, `soapysdr_1`) when not explicitly set in config.

### Rig Task (`rig_task.rs`)

Each rig runs an independent async loop:

```
connect → initialize → poll loop
              ↓ on error
          retry with ExponentialBackoff
              ↓ on persistent error
          Error state → wait for recovery
```

The task:
- Drives the `RigStateMachine` through state transitions
- Polls rig status at `AdaptivePolling` intervals (faster during TX)
- Handles incoming `RigCommand`s from `mpsc::Receiver`
- Broadcasts `RigState` snapshots via `watch::Sender`

### JSON TCP Listener (`listener.rs`)

Accepts connections on port 4530. Per connection:
1. Read newline-delimited JSON (`ClientEnvelope`)
2. Validate token
3. Route to rig by `rig_id`
4. Convert `ClientCommand → RigCommand` and send to rig task
5. Await result and return `ClientResponse`

### Audio Server (`audio.rs`)

Separate TCP listener on port 4531. Per connection:
1. Send `AudioStreamInfo` header
2. Send buffered decoder history (APRS, CW, FT8, WSPR, RDS frames)
3. Stream Opus-encoded RX audio frames as they arrive
4. Interleave decoder messages (`0x03`–`0x06` frame types)

`DecoderHistories` maintains ring buffers of recent decoded events so late-connecting clients get context.

### Uplinks

| Module | Purpose |
|--------|---------|
| `pskreporter.rs` | Posts FT8/WSPR spots to pskreporter.net |
| `aprsfi.rs` | Forwards APRS packets to APRS-IS network (IGate) |

Both are optional, configured per-rig.

---

## Backend Abstraction (trx-backend)

**Path:** `src/trx-server/trx-backend/`

### Factory Pattern (`src/lib.rs`)

```rust
pub enum RigAccess {
    Serial { path: String, baud: u32 },
    Tcp    { addr: String },
    Sdr    { args: String },
}

type BackendFactory = fn(RigAccess) -> DynResult<Box<dyn RigCat>>;

pub struct RegistrationContext {
    factories: HashMap<String, BackendFactory>,
}

impl RegistrationContext {
    pub fn register_backend(&mut self, name: &str, factory: BackendFactory);
    pub fn build_rig(&self, name: &str, access: RigAccess) -> DynResult<Box<dyn RigCat>>;
}
```

Built-in registrations (via `register_builtin_backends_on`):
- `"ft817"` → `Ft817::new`
- `"ft450d"` → `Ft450d::new`
- `"soapysdr"` → `SoapySdrRig::new_with_config` (if `soapysdr` feature enabled)

### RigCat Trait (from trx-core)

All backends implement `RigCat`:

```rust
pub trait RigCat: Rig {
    async fn get_status(&mut self) -> RigResult<RigStatus>;
    async fn set_freq(&mut self, freq: Freq) -> RigResult<()>;
    async fn set_mode(&mut self, mode: RigMode) -> RigResult<()>;
    async fn set_ptt(&mut self, on: bool) -> RigResult<()>;
    async fn power_on(&mut self) -> RigResult<()>;
    async fn power_off(&mut self) -> RigResult<()>;
    async fn toggle_vfo(&mut self) -> RigResult<()>;
    // ... more operations
}
```

### FT-817 Backend (`trx-backend-ft817/`)

- CAT protocol over serial (9600 baud default)
- BCD-encoded frequency/mode commands
- VFO A/B tracking
- Meter reads: S-meter, TX power, SWR, ALC
- Bands: 160m through 70cm + GHz receive

### FT-450D Backend (`trx-backend-ft450d/`)

- Similar structure to FT-817
- Uses FT-450D-specific CAT command set

### SoapySDR Backend (`trx-backend-soapysdr/`)

RX-only SDR backend with real-time DSP:

```rust
pub struct SoapySdrRig {
    freq: Freq,
    mode: RigMode,
    pipeline: dsp::SdrPipeline,       // Multi-channel DSP
    bandwidth_hz: u32,
    fir_taps: u32,
    spectrum_buf: Arc<Mutex<Option<Vec<f32>>>>,
    center_offset_hz: i64,
    wfm_deemphasis_us: u32,
    wfm_stereo: bool,
    wfm_denoise: bool,
    gain_db: f64,
}
```

**Known limitation:** IQ sample streaming (`real_iq_source.rs:149–157`) is not yet implemented — the IQ source currently returns zero buffers. The soapysdr 0.3 crate lacks streaming APIs; direct `soapysdr-sys` FFI or a crate upgrade would be required.

---

## Client (trx-client)

**Path:** `src/trx-client/src/`

### Startup Sequence

1. Parse CLI / TOML config
2. Register frontends via `FrontendRegistrationContext` (built-ins + plugins)
3. Spawn `run_remote_client()` — connects to server, drives `watch::Sender<RigState>`
4. Spawn enabled frontends (HTTP, rigctl, http-json)
5. Wait for shutdown

### Remote Client (`remote_client.rs`)

Maintains the server TCP connection:

```rust
pub struct RemoteClientConfig {
    pub addr: String,
    pub token: Option<String>,
    pub selected_rig_id: Arc<Mutex<Option<String>>>,
    pub known_rigs: Arc<Mutex<Vec<RemoteRigEntry>>>,
    pub poll_interval: Duration,
    pub spectrum: Arc<Mutex<SharedSpectrum>>,
}
```

Workflow:
1. Connect to `addr` (host:4530)
2. Poll `GetState` at configured interval (default 750 ms)
3. Poll `GetSpectrum` at ~40 ms (25 fps) if backend supports it
4. Forward commands from frontends (`mpsc::Receiver<RigRequest>`) to server
5. Broadcast received `RigState` to all frontends via `watch::Sender`

Multi-rig: `selected_rig_id` can be changed at runtime to switch which rig the client targets. `known_rigs` is populated by periodic `GetRigs` calls.

### Audio Client (`audio_client.rs`)

Connects to the audio port (`:4531`) and relays:
- Opus-encoded audio frames → local PCM broadcast channel
- Decoder messages → frontend display

---

## Frontend System (trx-frontend)

**Path:** `src/trx-client/trx-frontend/`

### Abstraction (`src/lib.rs`)

```rust
pub trait FrontendSpawner {
    fn spawn_frontend(
        state_rx: watch::Receiver<RigState>,
        rig_tx: mpsc::Sender<RigRequest>,
        callsign: Option<String>,
        listen_addr: SocketAddr,
        context: Arc<FrontendRuntimeContext>,
    ) -> JoinHandle<()>;
}

pub struct FrontendRuntimeContext {
    pub rigctl_clients: AtomicUsize,
    pub rigctl_addr: Option<SocketAddr>,
    pub http_clients: AtomicUsize,
    pub known_rigs: Arc<Mutex<Vec<RemoteRigEntry>>>,
    pub selected_rig_id: Arc<Mutex<Option<String>>>,
    pub spectrum: Arc<Mutex<SharedSpectrum>>,
}
```

### HTTP Frontend (`trx-frontend-http/`)

Built on **Actix-web**, serves a browser-based control panel.

**REST Endpoints:**

| Method | Path | Description |
|--------|------|-------------|
| GET | `/status` | Current rig state + frontend metadata |
| POST | `/cmd/{command}` | Execute a rig command |
| GET | `/events` | SSE stream of state changes |
| GET | `/audio` | WebSocket audio stream |
| GET | `/favicon.png` | Static asset |

`/status` response includes a `FrontendMeta` block:

```rust
struct FrontendMeta {
    http_clients: usize,
    rigctl_clients: usize,
    rigctl_addr: Option<String>,
    active_rig_id: Option<String>,
    rig_ids: Vec<String>,
    owner_callsign: Option<String>,
    show_sdr_gain_control: bool,
}
```

**Web UI features:** frequency display/entry, mode selector, PTT indicator, S-meter/TX-power/SWR meters, decoder toggles, decode history, spectrum waterfall (SDR), rig picker (multi-rig).

**Modules:**

| File | Responsibility |
|------|---------------|
| `server.rs` | Actix app builder, middleware, CORS |
| `api.rs` | REST handler functions |
| `audio.rs` | WebSocket ↔ PCM audio bridge |
| `auth.rs` | Token or basic-auth middleware |
| `status.rs` | State formatting for JSON responses |

### Rigctl Frontend (`trx-frontend-rigctl/`)

Hamlib-compatible plaintext TCP interface on port 4532. Allows WSJT-X, JS8Call, and other Hamlib-aware applications to control the rig without modification.

### HTTP-JSON Frontend (`trx-frontend-http-json/`)

JSON-over-TCP frontend on an ephemeral (or configured) port. Thin wrapper that passes `ClientCommand`/`ClientResponse` pairs — useful for scripting or automation tools.

---

## Signal Decoders

**Path:** `src/decoders/`

All decoders run as background Tokio tasks inside `trx-server`. They subscribe to the PCM audio broadcast channel from the active rig and publish decoded messages.

| Crate | Decoder | Notes |
|-------|---------|-------|
| `trx-aprs` | APRS (AX.25) | Forwards to APRS-IS if enabled |
| `trx-cw` | CW / Morse | Auto WPM detection |
| `trx-ft8` | FT8 | Wraps `external/ft8_lib` C library via FFI; posts to PSKReporter |
| `trx-wspr` | WSPR beacons | Posts to PSKReporter |
| `trx-rds` | FM RDS | Station name, radiotext, time |
| `trx-decode-log` | Logging infrastructure | JSON Lines, date-rotated files |

Control commands (e.g., `SetAprsDecodeEnabled(bool)`, `ResetCwDecoder`) are routed through `rig_task.rs` to the active decoder tasks.

Decoded events are multiplexed onto the audio stream wire protocol (`0x03–0x06` frame types) and also buffered in `DecoderHistories` for replay to newly connected clients.

---

## DSP & Spectrum Pipeline

**Path:** `src/trx-server/trx-backend/trx-backend-soapysdr/src/`

### Architecture

```
IQ Samples (from SoapySDR device)
    ↓
SdrPipeline (per-channel)
    ├── Channel 0: Mixer → FIR Filter → Demod → AGC → PCM
    ├── Channel 1: Mixer → FIR Filter → Demod → AGC → PCM
    └── ...
    ↓
Audio broadcast channel (Vec<f32>)
    ↓
Decoders / Audio server
```

### Demodulators (`demod/`)

| Module | Mode |
|--------|------|
| `am.rs` | AM (envelope detection) |
| `fm.rs` | Narrowband FM |
| `wfm.rs` | Wideband FM (stereo + deemphasis + denoise) |
| `ssb.rs` | LSB and USB |
| `cw.rs` | CW (Morse, beat-frequency oscillator) |

WFM demodulator supports:
- Stereo pilot detection and L+R/L−R matrix decoding
- Configurable de-emphasis time constant (50 µs EU / 75 µs US)
- Optional noise reduction

### Spectrum (`spectrum.rs`)

Real-time FFT of the mixer output is stored in `spectrum_buf` and snapshotted on demand:

```rust
pub struct SpectrumData {
    pub magnitudes: Vec<f32>,   // FFT magnitude bins (linear)
    pub low_hz: f64,
    pub high_hz: f64,
    pub center_hz: f64,
}
```

Clients poll via `RigCommand::GetSpectrum` → `ClientCommand::GetSpectrum`. The remote client polls at ~25 fps and caches in `SharedSpectrum`. The HTTP frontend reads this cache to drive the waterfall display.

---

## Plugin System

**Path:** `src/trx-app/src/plugins.rs`

Both `trx-server` and `trx-client` support dynamic plugins loaded at startup.

### Search Paths (in order)

1. `./plugins/`
2. `~/.config/trx-rs/plugins/`
3. Directories in `TRX_PLUGIN_DIRS` environment variable (`:` on Unix, `;` on Windows)

### Backend Plugins

Export symbol: `trx_register_backend(context: *mut RegistrationContext)`

Plugins call `context.register_backend("my-rig", factory_fn)` to add new rig drivers without rebuilding the server binary.

### Frontend Plugins

Export symbol: `trx_register_frontend(context: *mut FrontendRegistrationContext)`

Plugins call `context.register_frontend("my-ui", spawner_fn)` to add new control interfaces.

An example plugin is provided at `examples/trx-plugin-example/` (not a workspace member).

---

## Configuration

**Format:** TOML. Generated with `--print-config` flag.

**Search order:**
1. `--config <path>` CLI argument
2. `./trx-server.toml` / `./trx-client.toml`
3. `~/.config/trx-rs/trx-server.toml`
4. `/etc/trx-rs/trx-server.toml`

### Server Config Structure

```toml
[general]
callsign   = "W5XYZ"
log_level  = "info"
latitude   = 35.5
longitude  = -97.5

[listen]
addr       = "127.0.0.1"
port       = 4530
audio_port = 4531

[rig]                           # Legacy single-rig flat config
model      = "ft817"
[rig.access]
type       = "serial"
path       = "/dev/ttyUSB0"
baud       = 9600

[behavior]
max_retries        = 3
retry_delay_secs   = 1
polling_interval_ms = 250

[audio]
sample_rate        = 48000
frame_duration_ms  = 20
dev                = ""         # CPAL device name (empty = default)

[sdr]                           # SoapySDR global params
args        = "driver=rtlsdr"
sample_rate = 2000000
bandwidth_hz = 2000000
gain_mode   = "manual"
gain_db     = 25.0
center_offset_hz = 0

[[sdr.channels]]
if_hz             = 0
mode              = "USB"
audio_bandwidth_hz = 2800
fir_taps          = 64

[pskreporter]
enabled    = true
callsign   = "W5XYZ"
gridsquare = "EM13AH"

[aprsfi]
enabled    = true
callsign   = "W5XYZ-11"

[decode_logs]
enabled = true
dir     = "~/.trx-rs/decode-logs"

# Multi-rig (takes priority over flat [rig] section)
[[rigs]]
id    = "ft817_0"
name  = "HF Transceiver"
[rigs.rig]
model = "ft817"
[rigs.rig.access]
type  = "serial"
path  = "/dev/ttyUSB0"
baud  = 9600

[[rigs]]
id    = "sdr_0"
name  = "VHF/UHF SDR"
[rigs.rig]
model = "soapysdr"
[rigs.rig.access]
type  = "sdr"
args  = "driver=rtlsdr"
```

### Client Config Structure

```toml
[remote]
url              = "localhost:4530"
rig_id           = ""             # Empty = server default rig
poll_interval_ms = 750

[remote.auth]
token = ""

[frontends.http]
enabled = true
listen  = "127.0.0.1"
port    = 8080

[frontends.rigctl]
enabled = true
listen  = "127.0.0.1"
port    = 4532

[frontends.http_json]
enabled = false
port    = 0
```

---

## Concurrency Model

The system is built on **Tokio** and uses channels for all cross-task communication:

| Channel | Type | Purpose |
|---------|------|---------|
| `rig_tx` / `rig_rx` | `mpsc` | Frontend → rig task (commands) |
| `state_tx` / `state_rx` | `watch` | Rig task → frontends (state updates) |
| `audio_tx` / `audio_rx` | `broadcast` | Rig → decoders / audio server (PCM frames) |
| `shutdown_tx` / `shutdown_rx` | `watch` | Main → all tasks (graceful shutdown signal) |

### Task Tree (server)

```
main
 ├── rig_task [per rig]       — polls hardware, drives state machine
 ├── listener                 — accepts JSON TCP connections
 │    └── per-connection task — reads commands, sends responses
 ├── audio_server             — accepts audio TCP connections
 │    └── per-connection task — streams Opus frames
 ├── decoder tasks            — APRS, CW, FT8, WSPR, RDS
 ├── pskreporter              — uplink task
 └── aprsfi                   — uplink task
```

### Task Tree (client)

```
main
 ├── remote_client            — polls server, maintains state_tx
 ├── audio_client             — streams audio from server
 ├── http_frontend            — Actix-web server
 ├── rigctl_frontend          — Hamlib TCP server
 └── http_json_frontend       — JSON-over-TCP server
```

---

## Authentication & Security

### Token-Based Auth (JSON TCP)

- Clients include `token` in every `ClientEnvelope`
- Server validates via `TokenValidator` trait
- `SimpleTokenValidator` — `HashSet<String>` loaded from config
- `NoAuthValidator` — always passes (debug / local-only mode)

### HTTP Frontend Auth

- Optional token or HTTP Basic Auth middleware
- Configured in `[frontends.http.auth]`
- Rate limiting supported

### Transport Security

No built-in TLS. For remote use, tunnel over SSH or place behind a TLS-terminating reverse proxy (nginx, Caddy, etc.).

---

## Data Flow Diagrams

### Command Flow (set frequency)

```
Browser → POST /cmd/set_freq?hz=14225000
  ↓ trx-frontend-http
RigRequest::Command(RigCommand::SetFreq(14225000))
  ↓ mpsc channel (rig_tx)
remote_client.rs
  ↓ TCP
listener.rs (server)
  ↓ mpsc channel
rig_task.rs → backend.set_freq(14225000)
  ↓ CAT serial / SoapySDR API
Radio hardware
  ↑ ACK
rig_task.rs updates RigState → watch::Sender
  ↑ TCP
remote_client.rs receives ClientResponse
  ↑ watch::Sender
trx-frontend-http sends SSE event to browser
```

### State Update Flow (polling)

```
rig_task.rs polls rig_status() every ~250 ms
  → RigState updated → watch::Sender<RigState>
remote_client.rs receives via watch::Receiver
  → broadcasts to frontends via watch::Sender<RigState>
HTTP frontend reads watch::Receiver
  → pushes SSE "state" event to connected browsers
```

### Spectrum Update Flow

```
SoapySdrRig::run_spectrum_snapshot()
  → FFT of IQ buffer → SpectrumData stored in Arc<Mutex<>>
remote_client.rs polls GetSpectrum every 40 ms
  → stores SpectrumData in SharedSpectrum (Arc<Mutex<>>)
HTTP frontend reads SharedSpectrum
  → renders waterfall in browser via WebSocket or polling
```

### Audio Flow

```
SoapySDR IQ → DSP pipeline → PCM (Vec<f32>)
  → broadcast::Sender<Vec<f32>>
  ↙ (decoders subscribe)   ↘ (audio server subscribes)
APRS/CW/FT8/WSPR/RDS        Opus encode
decode tasks                   ↓ TCP
  ↓                        audio client (trx-client)
DecoderHistories buffer        ↓
  ↓                        broadcast locally
listener connections           ↓
stream decoder messages    HTTP WebSocket / local speakers
```

---

*Generated from source as of commit `56d6d12` (March 2026).*
