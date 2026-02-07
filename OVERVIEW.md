# trx-rs Project Overview

## What is trx-rs?

**trx-rs** is a modular transceiver (radio) control stack written in Rust. It provides a backend service for controlling amateur radio transceivers via CAT (Computer-Aided Transceiver) protocols, with multiple frontend interfaces for access and monitoring.

### Current Capabilities

| Feature | Status |
|---------|--------|
| Yaesu FT-817 CAT control | Implemented |
| HTTP/Web UI with SSE | Implemented |
| rigctl-compatible TCP | Implemented |
| VFO A/B switching | Implemented |
| PTT control | Implemented |
| Signal/TX power metering | Implemented |
| Front panel lock | Implemented |
| Multiple rig backends | Extensible (only FT-817) |
| Backend/frontend registry | Implemented |
| TCP CAT transport | Partial (config wiring only) |
| JSON TCP control (line-delimited) | Implemented (configurable frontend) |
| AppKit GUI frontend | Implemented (macOS only, optional) |
| Plugin registry loading | Implemented (shared libraries) |
| Configuration file (TOML) | Implemented |
| Rig state machine | Implemented |
| Command handlers | Implemented |
| Event notifications | Implemented (rig task emits events) |
| Retry/polling policies | Implemented |
| Controller-based rig task | Implemented |

---

## Current Architecture

```
┌──────────────────────────────────────────────────────────────────────────┐
│                              trx-server/trx-client                                      │
│  ┌────────────────────────────────────────────────────────────────────┐  │
│  │                        Application                                  │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────────┐  │  │
│  │  │    Config    │  │     CLI      │  │      Rig Task            │  │  │
│  │  │  (TOML file) │  │   (clap)     │  │   (main loop)            │  │  │
│  │  └──────────────┘  └──────────────┘  └──────────────────────────┘  │  │
│  └────────────────────────────────────────────────────────────────────┘  │
│                                  │                                        │
│              ┌───────────────────┴───────────────────┐                   │
│              ▼                                       ▼                   │
│  ┌─────────────────────┐                 ┌─────────────────────┐        │
│  │   trx-core          │                 │   Frontend Layer    │        │
│  │  ┌───────────────┐  │                 │  ┌───────────────┐  │        │
│  │  │  controller/  │  │                 │  │     HTTP      │  │        │
│  │  │  - machine    │  │                 │  │  (REST+SSE)   │  │        │
│  │  │  - handlers   │  │                 │  └───────────────┘  │        │
│  │  │  - events     │  │                 │  ┌───────────────┐  │        │
│  │  │  - policies   │  │                 │  │  HTTP JSON    │  │        │
│  │  └───────────────┘  │                 │  │  (TCP/JSON)   │  │        │
│  └─────────────────────┘                 │  └───────────────┘  │        │
│              │                           │  ┌───────────────┐  │        │
│              │                           │  │    rigctl     │  │        │
│              │                           │  │  (TCP/hamlib) │  │        │
│              │                           │  └───────────────┘  │        │
│              │                           └─────────────────────┘        │
│              ▼                                                           │
│  ┌─────────────────────┐                                                │
│  │   trx-backend       │                                                │
│  │  ┌───────────────┐  │                                                │
│  │  │ FT-817 Driver │  │                                                │
│  │  └───────────────┘  │                                                │
│  └─────────────────────┘                                                │
└──────────────────────────────────────────────────────────────────────────┘
```

### Key Components

| Component | Purpose |
|-----------|---------|
| `trx-core` | Core types, traits (`Rig`, `RigCat`), state definitions, controller components |
| `trx-core/rig/controller` | State machine, command handlers, event system, policies |
| `trx-backend` | Backend factory and abstraction layer |
| `trx-backend-ft817` | FT-817 CAT protocol implementation |
| `trx-frontend` | Frontend trait (`FrontendSpawner`) |
| `trx-frontend-http` | Web UI with REST API and SSE |
| `trx-frontend-http-json` | JSON-over-TCP control frontend |
| `trx-frontend-appkit` | AppKit GUI frontend (macOS only, optional) |
| `trx-frontend-rigctl` | Hamlib rigctl-compatible TCP interface |
| `trx-server` | Server binary — connects to rig backend, exposes JSON TCP control |
| `trx-client` | Client binary — connects to server, runs frontends (HTTP, rigctl) |

---

## Configuration

trx-rs supports TOML configuration files with the following search order:

1. `--config <path>` (explicit CLI argument)
2. `./trx-server.toml` or `./trx-client.toml` (current directory)
3. `~/.config/trx-rs/config.toml` (XDG user config)
4. `/etc/trx-rs/config.toml` (system-wide)

CLI arguments override config file values.

Plugin discovery:
- Uses shared libraries with a `trx_register` entrypoint.
- Searches `./plugins`, `~/.config/trx-rs/plugins`, and any paths in `TRX_PLUGIN_DIRS`.

### Example Configuration

```toml
[general]
callsign = "N0CALL"

[rig]
model = "ft817"
initial_freq_hz = 144300000
initial_mode = "USB"

[rig.access]
type = "serial"
port = "/dev/ttyUSB0"
baud = 9600

[frontends.http]
enabled = true
listen = "127.0.0.1"
port = 8080

[frontends.rigctl]
enabled = true
listen = "127.0.0.1"
port = 4532

[frontends.http_json]
enabled = true
listen = "127.0.0.1"
port = 9000
auth.tokens = ["demo-token"]

[behavior]
poll_interval_ms = 500
poll_interval_tx_ms = 100
max_retries = 3
retry_base_delay_ms = 100
```

Use `trx-server --print-config` or `trx-client --print-config` to generate an example configuration.

---

## Rig Controller Components

Located in `trx-core/src/rig/controller/`:

### State Machine (`machine.rs`)

Explicit state machine for rig lifecycle management:

```rust
pub enum RigMachineState {
    Disconnected,
    Connecting { started_at: Option<u64> },
    Initializing { rig_info: Option<RigInfo> },
    PoweredOff { rig_info: RigInfo },
    Ready(ReadyStateData),
    Transmitting(TransmittingStateData),
    Error { error: RigStateError, previous_state: Box<RigMachineState> },
}
```

Events trigger state transitions:
- `RigEvent::Connected`, `Initialized`, `PoweredOn`, `PoweredOff`
- `RigEvent::PttOn`, `PttOff`
- `RigEvent::Error(RigStateError)`, `Recovered`, `Disconnected`

### Command Handlers (`handlers.rs`)

Trait-based command system with validation:

```rust
pub trait RigCommandHandler: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn can_execute(&self, ctx: &dyn CommandContext) -> ValidationResult;
    fn execute<'a>(&'a self, executor: &'a mut dyn CommandExecutor)
        -> Pin<Box<dyn Future<Output = DynResult<CommandResult>> + Send + 'a>>;
}
```

Implemented commands:
- `SetFreqCommand`, `SetModeCommand`, `SetPttCommand`
- `PowerOnCommand`, `PowerOffCommand`
- `ToggleVfoCommand`, `LockCommand`, `UnlockCommand`
- `GetTxLimitCommand`, `SetTxLimitCommand`, `GetSnapshotCommand`

The rig task (`trx-server/src/rig_task.rs`) now syncs the state machine to the live `RigState`
and emits events whenever rig status changes.

### Event Notifications (`events.rs`)

Typed event system for rig state changes:

```rust
pub trait RigListener: Send + Sync {
    fn on_frequency_change(&self, old: Option<Freq>, new: Freq);
    fn on_mode_change(&self, old: Option<&RigMode>, new: &RigMode);
    fn on_ptt_change(&self, transmitting: bool);
    fn on_state_change(&self, old: &RigMachineState, new: &RigMachineState);
    fn on_meter_update(&self, rx: Option<&RigRxStatus>, tx: Option<&RigTxStatus>);
    fn on_lock_change(&self, locked: bool);
    fn on_power_change(&self, powered: bool);
}

pub struct RigEventEmitter {
    // Manages listeners and dispatches events
}
```

### Policies (`policies.rs`)

Configurable retry and polling behavior:

```rust
pub trait RetryPolicy: Send + Sync {
    fn should_retry(&self, attempt: u32, error: &RigError) -> bool;
    fn delay(&self, attempt: u32) -> Duration;
    fn max_attempts(&self) -> u32;
}

pub trait PollingPolicy: Send + Sync {
    fn interval(&self, transmitting: bool) -> Duration;
    fn should_poll(&self, transmitting: bool) -> bool;
}
```

Implementations:
- `ExponentialBackoff` - Exponential delay with max cap
- `FixedDelay` - Constant delay between retries
- `NoRetry` - Fail immediately
- `AdaptivePolling` - Faster polling during TX
- `FixedPolling` - Constant interval
- `NoPolling` - Disable automatic polling

### Error Types

`RigError` now includes error classification:

```rust
pub struct RigError {
    pub message: String,
    pub kind: RigErrorKind,  // Transient or Permanent
}

impl RigError {
    pub fn timeout() -> Self;           // Transient
    pub fn communication(msg) -> Self;  // Transient
    pub fn invalid_state(msg) -> Self;  // Permanent
    pub fn not_supported(op) -> Self;   // Permanent
    pub fn is_transient(&self) -> bool;
}
```

---

## Remaining Improvement Opportunities

### Integration Work

1. **Plugin UX improvements** - Add structured plugin metadata (name, version, capabilities) and surface it in CLI help.

### Testing

- Add integration tests with mock backends
- Add more backend/frontend unit tests

### Features

- Add more rig backends (IC-7300, TS-590, etc.)
- Add TX limit support for FT-817 (or document per-backend constraints in UI)
- Add WebSocket support for bidirectional communication
- Add metrics/telemetry export (Prometheus)
- Add authentication for HTTP frontend

### Code Quality

- Add CI/CD pipeline
- Add pre-commit hooks

---

## Implementation Status

| Component | Status | Tests |
|-----------|--------|-------|
| State Machine | Implemented | 5 tests |
| Command Handlers | Implemented | 3 tests |
| Event Notifications | Implemented | 2 tests |
| Retry/Polling Policies | Implemented | 5 tests |
| Config File Support | Implemented | 4 tests |
| rigctl Frontend | Implemented | - |
| HTTP Frontend | Implemented | - |
| FT-817 Backend | Implemented | - |

**Total: 19 unit tests passing**

---

## Building and Running

```bash
# Build
cargo build --release

# Run server with CLI args
./target/release/trx-server -r ft817 "/dev/ttyUSB0 9600"

# Run server with config file
./target/release/trx-server --config /path/to/config.toml

# Run client
./target/release/trx-client --config /path/to/client-config.toml

# Print example config
./target/release/trx-server --print-config > trx-server.toml

# Run tests
cargo test

# Run clippy
cargo clippy
```
