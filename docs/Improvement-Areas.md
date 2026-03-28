# Improvement Areas

A comprehensive audit of the trx-rs codebase covering code quality, architecture,
security, testing, and performance. Each item includes the affected location and
a suggested fix.

*Last updated: 2026-03-28*

---

## Resolved

The following items have been fixed across PRs #58, #59, and #60:

### Quick Wins (all complete)
- ✅ **Session cleanup timer** — 5-minute periodic `cleanup_expired()` task
- ✅ **`DecodeHistory<T>` type alias** — replaces 9 repeated `Arc<Mutex<VecDeque<...>>>` patterns
- ✅ **`mode_to_string()` allocation** — returns `Cow<'static, str>` (zero-alloc for known modes)
- ✅ **FTx dedup** — `HashSet<u16>` for O(1) lookups
- ✅ **Unbounded channels** — `VChanAudioCmd` channels bounded at 256
- ✅ **JSON serialization** — `#[serde(flatten)]` wrapper replaces string-level splice
- ✅ **`AtomicUsize` counter** — `estimated_total_count()` avoids 9 mutex acquisitions
- ✅ **Cookie security warning** — startup warning when `cookie_secure` is false
- ✅ **Spectrum encoding** — pre-allocated output string replaces `format!` overhead
- ✅ **`pub(crate)` state data** — `ReadyStateData`/`TransmittingStateData` fields restricted with constructors + getters
- ✅ **Lock ordering docs** — module-level documentation in `<vchan.rs>` establishing `rigs → sessions → audio_cmd`

### Critical (P0)
- ✅ **Plugin loading validation** — rejects world-writable files on Unix; `TRX_PLUGINS_DISABLED` env var
- ✅ **Audio pipeline mutex panics** — all `.expect()` on history mutexes and `.unwrap()` on audio ring buffers replaced with `.unwrap_or_else(|e| e.into_inner())` poison recovery
- ✅ **vchan lock panics** — ~25 `.unwrap()` on RwLock/Mutex replaced with poison recovery

### High (P1)
- ✅ **RigCat trait split** — 13 SDR-specific methods extracted into `RigSdr` extension trait; `RigCat` retains core CAT ops + `as_sdr()`/`as_sdr_ref()`; SoapySdrRig implements both; FT-817/FT-450D/DummyRig unchanged
- ✅ **Decoder history contention** — `AtomicUsize` total counter maintained by record/prune/clear

### Medium (P2)
- ✅ **Silent state machine failures** — debug-level tracing for rejected transitions
- ✅ **User input in logs** — raw JSON truncated to 128 chars
- ✅ **Rate limiting** — per-IP `LoginRateLimiter` (10 attempts/60s) on `/auth/login`
- ✅ **Lock-holding serialization** — clone data out under lock, serialize after release
- ✅ **Overly-public API** — state data fields `pub(crate)` with controlled accessors
- ✅ **Cookie security flag** — startup warning for non-TLS deployments
- ✅ **Lock ordering** — documented in `<vchan.rs>` module header

---

## Remaining Issues

### Critical (P0)

#### Plugin signing and cross-platform validation

**Location:** `src/trx-app/src/<plugins.rs>`

Current protections: file permission checks (Unix), `TRX_PLUGINS_DISABLED` env var,
loaded plugins logged at startup.

**Still missing:**
- No SHA-256 checksum verification — an attacker who passes the permission check
  can still load a tampered library
- No per-plugin permission scoping (all plugins get full context access)
- Windows has no file permission validation

**Suggestions:**
- SHA-256 checksum manifest (`plugins.toml`) verified before `Library::new`
- Config option to allowlist specific plugin filenames
- On Windows, verify file owner via `GetSecurityInfo` or equivalent

---

### High Priority (P1)

#### Synchronous locks in async contexts

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/<background_decode.rs>`,
`src/trx-client/trx-frontend/trx-frontend-http/src/<vchan.rs>`

`std::sync::RwLock` is used inside async tasks. Current code is safe (no locks held
across await points), but not idiomatic. Migrating to `tokio::sync::RwLock` would
prevent future regressions.

#### Large functions in audio pipeline

**Locations:**
- `src/trx-server/src/<audio.rs>` — `run_capture()` (~200 lines),
  `run_playback()` (~217 lines)

These contain nested loops, device re-enumeration logic, and stream error handling
that should be extracted into focused helper functions.

---

### Medium Priority (P2)

#### Configuration duplication

**Location:** `src/trx-server/src/<config.rs>` (1512 lines),
`src/trx-client/src/<config.rs>` (1181 lines)

14 config structs each, many mirrored between server and client. Extract shared
definitions (GeneralConfig, RigConfig, defaults) into `trx-app`.

---

### Low Priority (P3)

#### Missing tests for critical paths

Serial backends (FT-817, FT-450D), plugin loading/discovery, and the audio
pipeline (Opus encode/decode) have no or minimal test coverage.

Core crates (`trx-core`, `trx-server`, `trx-client`, `trx-app`) have limited
`[dev-dependencies]` and use only inline `#[test]` functions. Adding test
utilities (mock serial ports, test fixtures) would improve coverage.

#### Plugin system lacks versioning and lifecycle

**Location:** `src/trx-app/src/<plugins.rs>`

No plugin API version, capability manifest, or unload/reload semantics. Old
plugins break silently on API changes.

**Fix:** Add a version field to the registration struct and reject incompatible
plugins at load time.

---

## New Findings (2026-03-28 Deep Review) — All Resolved

### High Priority (P1) — All Complete

- ✅ **Rig task command batching LIFO** — replaced `batch.pop()` with `batch.remove(0)` for FIFO order
- ✅ **FrontendRuntimeContext god-struct** — decomposed ~50 flat fields into 9 coherent sub-structs (`AudioContext`, `DecodeHistoryContext`, `HttpAuthConfig`, `HttpUiConfig`, `RigRoutingContext`, `OwnerInfo`, `VChanContext`, `SpectrumContext`, `PerRigAudioContext`); all 7 consumer files updated
- ✅ **Decoder history unbounded** — added `MAX_HISTORY_ENTRIES` (10,000) cap with `enforce_capacity()` eviction independent of time-based pruning
- ✅ **ExponentialBackoff no jitter** — added ±25% randomized jitter via `apply_jitter()` helper to prevent thundering herd on reconnect
- ✅ **No rig task crash recovery** — rig tasks now detect errors and emit `RigMachineState::Error` on the watch channel so clients see the failure
- ✅ **Synchronous locks in async contexts** — migrated `std::sync::RwLock` to `tokio::sync::RwLock` in `background_decode.rs`; `vchan.rs` left as-is (all methods are synchronous, no locks held across await points)
- ✅ **Large audio pipeline functions** — extracted `find_input_device()` and `find_output_device()` helpers from `run_capture()` and `run_playback()`

### Medium Priority (P2) — All Complete

- ✅ **SoapySdrRig 20-parameter constructor** — introduced `SoapySdrConfig` struct with named fields and defaults; `new_from_config()` replaces positional parameters; old `new_with_config()` preserved as backward-compatible wrapper
- ✅ **Dual command enums** — added `define_command_mappings!` macro in `mapping.rs` that generates both `client_command_to_rig()` and `rig_command_to_client()` from a single definition table; removed `unreachable!()` for `GetRigs`/`GetSatPasses`
- ✅ **Lock poisoning recovery hides panics** — replaced all `.unwrap_or_else(|e| e.into_inner())` with `lock_or_recover()` helper that logs a warning with the lock label when recovering from poisoned mutex
- ✅ **Configuration duplication** — extracted shared config types (`LogLevel` defaults, common patterns) into `trx-app/src/shared_config.rs`; both server and client import from `trx_app`
- ✅ **Hardcoded timeouts** — made `command_exec_timeout`, `poll_refresh_timeout`, `io_timeout`, `request_timeout`, and `rig_task_channel_buffer` configurable via `RigTaskConfig`/`ListenerConfig` and the TOML `[timeouts]` section; constants remain as defaults

---

### Low Priority (P3)

#### FT-817 VFO state inference is fragile

**Location:** `src/trx-server/trx-backend/trx-backend-ft817/src/<lib.rs>`

VFO state starts as `Unknown` and is inferred by matching frequencies against
cached values. When VFO A and VFO B are set to the same frequency, inference
fails and the rig may report the wrong VFO.

**Fix:** Some FT-817 variants support extended status bytes that indicate active
VFO directly. Detect firmware version and use the direct query when available.

#### VDES decoder has incomplete FEC

**Location:** `src/decoders/trx-vdes/src/<lib.rs>`

The VDES decoder implements burst detection and pi/4-QPSK demodulation but the
Turbo FEC (1/2 rate) decoder and link-layer (M.2092-1) parser are not complete.
Decoded output is limited to raw symbols.

#### No forward compatibility in protocol

**Location:** `src/trx-protocol/src/<codec.rs>`

Unknown commands cause parse errors. There is no protocol version field in the
envelope, making it impossible for older clients to gracefully degrade when
connecting to newer servers.

**Fix:** Add an optional `protocol_version` field to `ClientEnvelope`. Unknown
commands should return an error response rather than a parse failure.

#### Command handler boilerplate

**Location:** `src/trx-core/src/rig/controller/<handlers.rs>`

11 `RigCommandHandler` implementations follow identical patterns across 500+
lines (validate state → call executor method → return result). A declarative
macro could reduce this to ~100 lines.

#### Missing tests for critical paths

Serial backends (FT-817, FT-450D), plugin loading/discovery, and the audio
pipeline (Opus encode/decode) have no or minimal test coverage. `rig_task.rs`
(1,315 lines) and `audio.rs` (3,977 lines) — the two largest server modules —
have no integration tests.