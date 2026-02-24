# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Build
cargo build --release

# Run tests
cargo test

# Lint
cargo clippy

# Run a single test (by name pattern)
cargo test <test_name>

# Run tests for a specific crate
cargo test -p trx-core

# Generate example config
./target/release/trx-server --print-config > trx-server.toml
./target/release/trx-client --print-config > trx-client.toml

# Run server
./target/release/trx-server --config trx-server.toml
# or via CLI args:
./target/release/trx-server -r ft817 "/dev/ttyUSB0 9600"

# Run client
./target/release/trx-client --config trx-client.toml
```

## Crate Layout

This is a Cargo workspace. All crates live under `src/`:

```
src/
  trx-core/           # Core types, traits, state machine, controller
  trx-protocol/       # Client↔server protocol conversion, auth, codec
  trx-app/            # Shared application helpers (config, plugins, logging)
  trx-server/         # Server binary (rig_task, audio, APRS-IS, PSKReporter)
    trx-backend/      # Backend abstraction trait + factory
      trx-backend-ft817/    # Yaesu FT-817 CAT implementation
      trx-backend-ft450d/   # Yaesu FT-450D CAT implementation
  trx-client/         # Client binary (connects to server, runs frontends)
    trx-frontend/     # Frontend trait (FrontendSpawner)
      trx-frontend-http/      # Web UI with REST API, SSE, and auth
      trx-frontend-http-json/ # JSON-over-TCP control frontend
      trx-frontend-rigctl/    # Hamlib-compatible rigctl TCP interface
  decoders/
    trx-aprs/         # APRS packet decoder
    trx-cw/           # CW (Morse) decoder
    trx-ft8/          # FT8 decoder (wraps external ft8_lib C library)
    trx-wspr/         # WSPR decoder
    trx-decode-log/   # Shared decoder logging (JSON Lines, date-rotated files)
```

## Architecture

The project is split into a **server** (connects to the radio hardware) and a **client** (exposes user-facing frontends). They communicate over a JSON TCP connection (default port 4530). Audio streams over a separate TCP connection (default port 4531) using Opus encoding.

### Data flow

```
Radio hardware
    ↕ serial/TCP
trx-server (rig_task.rs)
    ↕ trx-protocol JSON-TCP (port 4530)
trx-client (remote_client.rs)
    ↕ internal channels
Frontends: HTTP (8080), rigctl (4532), http-json (ephemeral)
```

### trx-core controller

The rig controller (`src/trx-core/src/rig/controller/`) is the central state management component:

- **`machine.rs`** — `RigMachineState` enum with states: `Disconnected`, `Connecting`, `Initializing`, `PoweredOff`, `Ready`, `Transmitting`, `Error`
- **`handlers.rs`** — `RigCommandHandler` trait; commands: `SetFreq`, `SetMode`, `SetPtt`, `PowerOn/Off`, `ToggleVfo`, `Lock/Unlock`, `GetSnapshot`, etc.
- **`events.rs`** — `RigListener` trait and `RigEventEmitter` for broadcasting frequency/mode/PTT/state/meter/lock/power changes
- **`policies.rs`** — `RetryPolicy` (`ExponentialBackoff`, `FixedDelay`, `NoRetry`) and `PollingPolicy` (`AdaptivePolling`, `FixedPolling`, `NoPolling`)

### Decoders

Signal decoders run as background tasks in `trx-server`, consuming decoded audio. `trx-ft8` wraps a C library (`external/ft8_lib`). Decoded frames can be forwarded to PSKReporter and APRS-IS (IGate) uplinks, or logged via `trx-decode-log`.

### Plugin system

Both `trx-server` and `trx-client` can load shared-library plugins exporting a `trx_register` symbol. Search paths: `./plugins`, `~/.config/trx-rs/plugins`, `TRX_PLUGIN_DIRS` env var.

## Commit Format

```
[<type>](<crate>): <description>
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`. Use `(trx-rs)` for repo-wide changes. Sign commits with `git commit -s`. Write isolated commits per crate.
