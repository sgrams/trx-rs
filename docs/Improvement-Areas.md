# Improvement Areas

A comprehensive audit of the trx-rs codebase covering code quality, architecture,
security, testing, and performance. Each item includes the affected location and
a suggested fix.

*Last updated: 2026-03-29*

---

## Critical (P0)

### ~~Plugin signing and cross-platform validation~~ — RESOLVED

**Location:** `src/trx-app/src/plugins.rs`

**Resolution:** Created `plugins.rs` module with:
- SHA-256 checksum verification via `plugins.toml` manifest
- Per-plugin filename allowlisting
- Plugin API version compatibility check (rejects incompatible versions)
- Unix: file permission validation (rejects world-writable, wrong-owner files)
- Windows: basic permission warning
- `TRX_PLUGINS_DISABLED` environment variable support
- Full test coverage for checksum, allowlist, version, and success paths

---

## High Priority (P1)

### ~~Session store mutex poisoning (auth.rs)~~ — RESOLVED

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/auth.rs`

**Resolution:** All 6 `.write().unwrap()` / `.lock().unwrap()` calls replaced with
`.unwrap_or_else(|e| { warn!(...); e.into_inner() })` pattern. Lock poisoning now
logs a warning and recovers the inner data instead of crashing.

### ~~No rate limiting on TCP listener~~ — RESOLVED

**Location:** `src/trx-server/src/listener.rs`

**Resolution:** Added `ConnectionTracker` with per-IP connection limiting
(default: 10 concurrent connections per IP). Connections exceeding the limit
are rejected with a log warning. Slots are released when clients disconnect.

### ~~RigState is a 33-field flat struct~~ — RESOLVED

**Location:** `src/trx-core/src/rig/state.rs`

**Resolution:** Decoder fields grouped into two sub-structs:
- `DecoderConfig`: 8 `*_decode_enabled` bool fields
- `DecoderResetSeqs`: 8 `*_decode_reset_seq` u64 counters

Both use `#[serde(flatten)]` to maintain backward-compatible JSON wire format.
Updated across all consumers: `rig_task.rs`, `audio.rs`, `api.rs`,
`remote_client.rs`, `server.rs` (rigctl, http-json), `codec.rs`.

### ~~No `spawn_blocking` timeout~~ — RESOLVED

**Location:** `src/trx-server/src/listener.rs`

**Resolution:** Satellite pass computation wrapped in `tokio::time::timeout(30s, ...)`
with graceful fallback to empty results on timeout or panic.

---

## Medium Priority (P2)

### ~~Command handler boilerplate~~ — RESOLVED

**Location:** `src/trx-core/src/rig/controller/handlers.rs`

**Resolution:** Created `rig_command!` declarative macro that generates unit-struct
command implementations from a concise table of (name, preconditions, execute body).
7 unit commands (PowerOn, PowerOff, ToggleVfo, Lock, Unlock, GetTxLimit,
GetSnapshot) now use the macro. Commands with custom fields/validation (SetFreq,
SetMode, SetPtt, SetTxLimit) remain as explicit impls.

### ~~No command execution timeouts at CommandExecutor level~~ — ALREADY RESOLVED

**Location:** `src/trx-server/src/rig_task.rs`

`tokio::time::timeout(command_exec_timeout, process_command(...))` already wraps
all command execution (lines 370–425). Default timeout: 10s. No further changes
needed.

### ~~No forward compatibility in protocol~~ — RESOLVED

**Location:** `src/trx-protocol/src/types.rs`, `src/trx-protocol/src/codec.rs`

**Resolution:**
- Added optional `protocol_version: Option<u32>` to both `ClientEnvelope` and
  `ClientResponse` (current version: 1, defined as `PROTOCOL_VERSION` constant).
- `parse_envelope()` now distinguishes between truly malformed JSON and valid
  JSON with an unrecognised `cmd` value, enabling clearer error messages.

### ~~`unsafe` string construction in spectrum encoding~~ — RESOLVED

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/api.rs:63`

**Resolution:** Replaced `unsafe { String::from_utf8_unchecked(out) }` with
`String::from_utf8(out).expect("base64 output is always valid ASCII")`.

### ~~6 `#[allow(dead_code)]` annotations~~ — RESOLVED

**Resolution:**
- `is_tx_endpoint` in auth.rs: made `pub` and removed annotation (used in tests,
  available for TX access control integration).
- `session_ttl()` in config.rs: removed annotation (public API method).
- `device` in real_iq_source.rs: annotation kept (lifetime anchor for stream).
- `iq_tx` in vchan_impl.rs: annotation kept (broadcast sender kept alive).
- `fixed_slot_count` in vchan_impl.rs: annotation kept (documents reserved slots).
- `process_pair` in demod.rs: annotation kept (stereo AGC variant for future use).

---

## Low Priority (P3)

### ~~Missing tests for critical modules~~ — PARTIALLY RESOLVED

- `history_store.rs`: Added 4 unit tests covering timestamp generation,
  serde round-trip, save/load round-trip, and expiry filtering.
- `audio.rs`, `api.rs`, `main.rs`: Remain without tests (require ALSA/hardware
  mocking infrastructure that is beyond the scope of this pass).
- `rig_task.rs`: Existing 4 tests adequate; integration tests deferred.

### ~~FT-817 VFO state inference is fragile~~ — IMPROVED

**Location:** `src/trx-server/trx-backend/trx-backend-ft817/src/lib.rs`

**Resolution:** Improved `update_vfo_freq()` to handle the ambiguous case where
both VFOs share the same frequency. When VFO B has a cached frequency that
differs from the current reading, inference correctly assigns to VFO A. When
frequencies match (ambiguous), defaults to VFO A — resolved after VFO toggle
primes both sides. Added detailed comments explaining the inference logic.

### VDES decoder has incomplete FEC

**Location:** `src/decoders/trx-vdes/src/lib.rs`

Burst detection and pi/4-QPSK demodulation work, but Turbo FEC (1/2 rate) and
link-layer (M.2092-1) parsing are not implemented. CRC validation is stubbed
(`crc_ok: false`). Output limited to raw symbols. This is a substantial DSP
implementation task requiring Turbo code decoder research.

### ~~Plugin system lacks versioning and lifecycle~~ — RESOLVED

**Location:** `src/trx-app/src/plugins.rs`

**Resolution:** Plugin manifest includes `api_version` field. `validate_plugin()`
rejects plugins with incompatible API versions. Current API version: 1.

### ~~Configurator serial detection is stubbed~~ — RESOLVED

**Location:** `src/trx-configurator/src/detect.rs`

**Resolution:** Implemented `detect_serial_ports()` using `tokio_serial::available_ports()`.
Returns `(port_name, description)` pairs with USB vendor/product info, Bluetooth,
PCI, and Unknown port type descriptions.

### Inconsistent frequency/rig naming across crates

Field naming varies across the codebase (`freq_hz` vs `center_hz`, `rig_id` vs
`id`, `model` vs `rig_model`). Analysis shows these reflect distinct semantic
contexts rather than true inconsistencies:
- `freq_hz`: dial frequency; `center_hz`: SDR capture center; `cw_center_hz`: CW tone
- `rig_id`: stable config key; `id`: runtime UUID
- `model`: hardware model string; `rig_model`: config parameter

**Decision:** Documented as intentional. Renaming would break the wire protocol
and provide minimal benefit. The `_hz` suffix convention is consistently applied.
