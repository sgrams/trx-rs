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
  trx-core/           # Core types, traits, state machine, controller (~3,500 LOC)
  trx-protocol/       # Client↔server protocol DTOs, auth, codec, mapping (~1,100 LOC)
  trx-app/            # Shared application helpers (config paths, logging init)
  trx-reporting/      # PSKReporter UDP uplink + APRS-IS TCP uplink (~1,150 LOC)
  trx-server/         # Server binary: rig_task, audio pipeline, listener (~3,700 LOC)
    trx-backend/      # Backend abstraction trait + factory + dummy
      trx-backend-ft817/    # Yaesu FT-817 binary CAT (BCD encoding)
      trx-backend-ft450d/   # Yaesu FT-450D ASCII CAT
      trx-backend-soapysdr/ # SoapySDR RX with full DSP pipeline (~5,000+ LOC)
  trx-client/         # Client binary: remote connection, frontend spawning (~1,500 LOC)
    trx-frontend/     # Frontend trait (FrontendSpawner), runtime context
      trx-frontend-http/      # Web UI: REST API, SSE, WebSocket audio, session auth
      trx-frontend-http-json/ # JSON-over-TCP control frontend
      trx-frontend-rigctl/    # Hamlib-compatible rigctl TCP interface
  trx-configurator/   # Interactive setup wizard
  decoders/
    trx-aprs/         # APRS packet decoder (AX.25 + APRS-IS)
    trx-cw/           # CW (Morse) decoder (Goertzel tone detection)
    trx-ftx/          # Pure Rust FTx decoder (FT8/FT4/FT2, LDPC/OSD) (~3,000+ LOC)
    trx-wspr/         # WSPR weak-signal decoder
    trx-ais/          # AIS maritime transponder decoder
    trx-rds/          # RDS decoder for WFM (~2,000 LOC)
    trx-vdes/         # VDES maritime data exchange decoder (~1,300 LOC)
    trx-decode-log/   # JSON Lines file logging with date rotation
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

Signal decoders run as background tasks in `trx-server`, consuming decoded audio. `trx-ftx` provides the FT8/FT4/FT2 decoder in pure Rust. Decoded frames can be forwarded to PSKReporter and APRS-IS (IGate) uplinks, or logged via `trx-decode-log`.

## Commit Format

```
[<type>](<crate>): <description>
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`. Use `(trx-rs)` for repo-wide changes. Sign commits with `git commit -s`. Write isolated commits per crate.

## Codebase Review Observations

Full architecture documentation: `docs/Architecture.md`
Improvement plan: `docs/Improvement-Areas.md`

*Last reviewed: 2026-03-28*

### Strengths

- **Explicit state machine**: `RigMachineState` FSM (7 states) prevents invalid states with a deterministic transition table and exhaustive matching. Well-tested with lifecycle, error recovery, and invalid transition tests. `ReadyStateData`/`TransmittingStateData` use `pub(crate)` fields with controlled accessors.
- **Trait-based polymorphism**: Clean abstraction boundaries (`RigCat`, `RigSdr`, `AudioSource`, `RigListener`, `RigCommandHandler`, `CommandExecutor`, `TokenValidator`, `FrontendSpawner`) enable loose coupling and testability. `RigCat`/`RigSdr` split cleanly separates CAT ops from SDR-specific methods.
- **Multi-rig architecture**: Per-rig task isolation with `HashMap<rig_id, RigHandle>` routing, per-rig state/spectrum/audio/decoder-history channels, dual-connection model (main + spectrum) in the client, and backward-compatible single-rig mode.
- **Async concurrency model**: Proper use of tokio channels -- `watch` for state snapshots, `broadcast` for PCM/decode fan-out, `mpsc` for commands. No mutex contention on hot paths. Spectrum deduplication collapses concurrent GetSpectrum requests.
- **Comprehensive SDR support**: Full DSP pipeline with multi-mode demodulation (SSB, AM, SAM, FM, WFM, AIS, VDES), virtual channel management, squelch, noise blanker, spectrum FFT, RDS decoding. AVX2-optimized FM discriminator with scalar fallbacks.
- **Pure Rust decoders**: FT8/FT4/FT2, APRS, CW, WSPR, AIS, VDES, RDS -- all implemented without C FFI dependencies. Consistent decoder pattern: stateful struct → `process_block()` → `decode_if_ready()`.
- **Good test coverage** in protocol layer: codec, mapping, auth all have thorough unit tests with round-trip verification. 45+ mapping tests cover all command variants.
- **Feature-gated backends**: ft817, ft450d, soapysdr compiled conditionally to minimize binary size. Factory pattern with name normalization for registration.
- **Defensive error handling**: Lock poisoning recovery, stream error deduplication with 60s summaries, input truncation in logs (128 chars), per-IP rate limiting on auth endpoints.
- **Well-documented DSP guidelines**: `docs/Optimization-Guidelines.md` captures lessons on NCO design, polyphase resampling, AVX2 batching, and stereo FM decoding.

### Areas for Improvement

**P1 — High:**
- **FrontendRuntimeContext** (`trx-frontend/src/lib.rs`) is a ~50-field god-struct mixing audio channels, decode histories (9 types), auth config (7 fields), UI settings, rig routing, virtual channels, and branding. Should be decomposed into sub-structs (see `docs/Improvement-Areas.md`).
- **Rig task command batching uses LIFO** (`rig_task.rs`): `batch.pop()` reverses arrival order. Commands execute newest-first, causing unexpected transient states.
- **Decoder history unbounded** (`audio.rs`): No capacity limit on `VecDeque` queues; only 24h time-based pruning. Busy AIS channels can exhaust memory.
- **ExponentialBackoff has no jitter** (`policies.rs`): All rigs/clients retry at identical times after a server restart (thundering herd).
- **No rig task crash recovery** (`main.rs`): If a rig task panics, it silently disappears. No supervisor, no restart, no health monitoring.

**P2 — Medium:**
- **Dual command enums**: `ClientCommand` and `RigCommand` are near-identical 40+ variant enums with mechanical 1:1 mapping in `mapping.rs` (675 lines). Adding a command requires 4-file changes. `GetRigs` triggers `unreachable!()`.
- **SoapySdrRig 20-parameter constructor**: No builder pattern, fragile call sites.
- **Lock poisoning recovery hides panics**: `unwrap_or_else(|e| e.into_inner())` throughout `audio.rs` silently continues with potentially inconsistent data.
- **Hardcoded timeouts**: 10+ timeout/retention constants scattered across files, none configurable via TOML.
- **Config duplication**: `config.rs` in server (1,512 LOC) and client (1,181 LOC) mirror many structs.

**P3 — Low:**
- **Command handler boilerplate**: 11 `RigCommandHandler` impls follow identical patterns across 500+ lines. Macro opportunity.
- **No integration tests** for `rig_task.rs` (1,315 LOC) or `audio.rs` (3,977 LOC) — the two largest server modules.
- **No command execution timeouts** at the `CommandExecutor` level. Backend stalls propagate up.
- **FT-817 VFO inference fragile**: Fails when VFO A and B share the same frequency.
- **VDES decoder incomplete**: Turbo FEC and link-layer parsing not implemented.
- **No protocol versioning**: Unknown commands cause parse errors with no graceful degradation.
