# Improvement Areas

A comprehensive audit of the trx-rs codebase covering code quality, architecture,
security, testing, and performance. Each item includes the affected location and
a suggested fix.

*Last updated: 2026-03-29*

---

## Critical (P0)

### Plugin signing and cross-platform validation

**Location:** `src/trx-app/src/plugins.rs`

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

## High Priority (P1)

### Session store mutex poisoning (auth.rs)

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/auth.rs` (lines 89,
96, 116, 124, 151, 158, 165)

7 `.write().unwrap()` / `.lock().unwrap()` calls on the session `RwLock<HashMap>`.
If a panic occurs while holding the lock, all subsequent auth operations will panic
and crash the server.

**Fix:** Use `lock_or_recover()` helper (already used elsewhere in the codebase) or
`write().unwrap_or_else(|e| e.into_inner())` with warning logs.

### No rate limiting on TCP listener

**Location:** `src/trx-server/src/listener.rs`

The TCP listener accepts connections without per-IP rate limiting. The HTTP frontend
has rate limiting on `/auth/login`, but the raw protocol listener does not. Potential
for connection exhaustion.

**Fix:** Add per-IP connection rate limiting (similar to `LoginRateLimiter` in auth).

### RigState is a 33-field flat struct

**Location:** `src/trx-core/src/rig/state.rs` (lines 13–84)

33 fields including 8 `*_decode_enabled` bools and 8 `*_decode_reset_seq` counters
that follow identical patterns. Cloned frequently via `watch` channel broadcasts.

**Fix:** Group decoder fields into a `DecoderConfig` sub-struct and reset sequences
into a `DecoderResetSeqs` sub-struct. Reduces clone cost and makes decoder-related
changes self-contained.

### No timeout on `spawn_blocking` in listener

**Location:** `src/trx-server/src/listener.rs:351`

`tokio::task::spawn_blocking()` for satellite pass computation has no timeout. If
SGP4 propagation hangs, it consumes a thread pool slot indefinitely.

**Fix:** Wrap in `tokio::time::timeout()`.

---

## Medium Priority (P2)

### Command handler boilerplate

**Location:** `src/trx-core/src/rig/controller/handlers.rs` (lines 145–659)

11 `RigCommandHandler` implementations follow identical patterns across 500+ lines:
validate state → call executor method → return result. Differences are limited to
which executor method is called and which state preconditions are checked.

**Fix:** Declarative macro that generates implementations from a table of
(command, executor_method, preconditions) tuples. Would reduce ~500 lines to ~100.

### No command execution timeouts at CommandExecutor level

**Location:** `src/trx-server/src/rig_task.rs`

`command_exec_timeout` is defined in `RigTaskConfig` but there is no evidence of
`tokio::time::timeout()` wrapping individual executor calls. A stuck backend command
blocks the rig task indefinitely.

**Fix:** Wrap each `executor.method().await` call in `timeout(config.command_exec_timeout, ...)`.

### No forward compatibility in protocol

**Location:** `src/trx-protocol/src/codec.rs`

Unknown commands cause parse errors. No `protocol_version` field in the envelope.
Older clients cannot gracefully degrade when connecting to newer servers.

**Fix:** Add optional `protocol_version` to `ClientEnvelope`. Unknown commands
should return an error response rather than a parse failure.

### `unsafe` string construction in spectrum encoding

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/api.rs:63`

`unsafe { String::from_utf8_unchecked(out) }` builds a base64 string from bytes.
The safety comment claims ASCII-only output, which is correct for the current
implementation, but a future edit could break the invariant silently.

**Fix:** Use `String::from_utf8(out).expect("base64 is ASCII")` (negligible
performance difference on short spectrum strings) or use the `base64` crate.

### 6 `#[allow(dead_code)]` annotations

**Locations:**
- `src/trx-client/trx-frontend/trx-frontend-http/src/auth.rs:652`
- `src/trx-client/src/config.rs:266`
- `src/trx-server/trx-backend/trx-backend-soapysdr/src/vchan_impl.rs:66, 87`
- `src/trx-server/trx-backend/trx-backend-soapysdr/src/demod.rs:113`
- `src/trx-server/trx-backend/trx-backend-soapysdr/src/real_iq_source.rs:20`

**Fix:** Review each — remove dead code or remove the annotation if the code is
reachable via feature gates.

---

## Low Priority (P3)

### Missing tests for critical modules

Zero `#[test]` functions in:
- `src/trx-server/src/audio.rs` (3,812 lines) — decoder instantiation, audio streaming, history
- `src/trx-client/trx-frontend/trx-frontend-http/src/api.rs` (2,711 lines) — HTTP endpoints, SSE, spectrum encoding
- `src/trx-server/src/main.rs` (1,203 lines) — multi-rig setup, initialization
- `src/trx-server/src/history_store.rs` (193 lines) — persistence, timestamp conversion

`rig_task.rs` (1,316 lines) has 4 tests but no integration tests for command
timeout handling, polling recovery, or error state transitions.

Serial backends (FT-817, FT-450D) and plugin loading have no test coverage.

### FT-817 VFO state inference is fragile

**Location:** `src/trx-server/trx-backend/trx-backend-ft817/src/lib.rs`

VFO state starts as `Unknown` and is inferred by matching frequencies against
cached values. When VFO A and B share the same frequency, inference fails.

**Fix:** Detect firmware version and use direct VFO query when available.

### VDES decoder has incomplete FEC

**Location:** `src/decoders/trx-vdes/src/lib.rs`

Burst detection and pi/4-QPSK demodulation work, but Turbo FEC (1/2 rate) and
link-layer (M.2092-1) parsing are not implemented. CRC validation is stubbed
(`crc_ok: false`). Output limited to raw symbols.

### Plugin system lacks versioning and lifecycle

**Location:** `src/trx-app/src/plugins.rs`

No plugin API version, capability manifest, or unload/reload semantics. Old
plugins break silently on API changes.

**Fix:** Add a version field to the registration struct and reject incompatible
plugins at load time.

### Configurator serial detection is stubbed

**Location:** `src/trx-configurator/src/detect.rs:8`

Contains `TODO: use serialport::available_ports() for real detection`. The
interactive setup wizard cannot auto-detect connected rigs.

### Inconsistent frequency/rig naming across crates

Field naming is inconsistent across the codebase:
- `freq_hz` vs `frequency` vs `center_hz` (audio.rs, api.rs, config.rs)
- `rig_id` vs `id` (RigInstanceConfig vs RigState)
- `model` vs `rig_model` (RigConfig vs RigTaskConfig)

Not a correctness issue, but increases cognitive overhead and copy-paste errors.
