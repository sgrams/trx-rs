# Improvement Areas

A comprehensive audit of the trx-rs codebase covering code quality, architecture,
security, testing, and performance. Each item includes the affected location and
a suggested fix.

*Last updated: 2026-03-29*

---

## Resolved Items

<details>
<summary>Click to expand resolved items from previous audits</summary>

### Plugin signing and cross-platform validation — DROPPED

Plugin system has been removed from the codebase. No longer applicable.

### Session store mutex poisoning (auth.rs) — RESOLVED

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/auth.rs`

All 6 `.write().unwrap()` / `.lock().unwrap()` calls replaced with
`.unwrap_or_else(|e| { warn!(...); e.into_inner() })` pattern. Lock poisoning now
logs a warning and recovers the inner data instead of crashing.

### No rate limiting on TCP listener — RESOLVED

**Location:** `src/trx-server/src/listener.rs`

Added `ConnectionTracker` with per-IP connection limiting (default: 10 concurrent
connections per IP). Connections exceeding the limit are rejected with a log warning.
Slots are released when clients disconnect.

### RigState is a 33-field flat struct — RESOLVED

**Location:** `src/trx-core/src/rig/state.rs`

Decoder fields grouped into `DecoderConfig` (8 bools) and `DecoderResetSeqs`
(8 u64 counters). Both use `#[serde(flatten)]` for backward-compatible JSON wire
format. Updated across all consumers.

### No `spawn_blocking` timeout — RESOLVED

**Location:** `src/trx-server/src/listener.rs`

Satellite pass computation wrapped in `tokio::time::timeout(30s, ...)` with
graceful fallback to empty results on timeout or panic.

### Command handler boilerplate — RESOLVED

**Location:** `src/trx-core/src/rig/controller/handlers.rs`

Created `rig_command!` declarative macro. 7 unit commands use the macro; 4 commands
with custom fields/validation remain as explicit impls.

### No command execution timeouts at CommandExecutor level — RESOLVED

**Location:** `src/trx-server/src/rig_task.rs`

`tokio::time::timeout(command_exec_timeout, process_command(...))` wraps all
command execution. Default timeout: 10s, configurable via `RigTaskConfig`.

### No forward compatibility in protocol — RESOLVED

**Location:** `src/trx-protocol/src/types.rs`, `src/trx-protocol/src/codec.rs`

Added optional `protocol_version: Option<u32>` to `ClientEnvelope` and
`ClientResponse`. `parse_envelope()` distinguishes malformed JSON from
unrecognised `cmd` values.

### `unsafe` string construction in spectrum encoding — RESOLVED

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/api.rs`

Replaced `unsafe { String::from_utf8_unchecked(out) }` with safe
`String::from_utf8(out).expect(...)`.

### `#[allow(dead_code)]` cleanup — RESOLVED

Reduced from 6 to 4 annotations, all in trx-backend-soapysdr where fields serve
as lifetime anchors (`device`, `iq_tx`) or document reserved capacity
(`fixed_slot_count`, `process_pair`).

### VDES decoder incomplete FEC — RESOLVED

Turbo FEC decoder, CRC-16-CCITT validation, and M.2092-1 link-layer frame parsing
implemented.

### Plugin system lacks versioning — DROPPED

Plugin system removed from the codebase.

### Configurator serial detection stubbed — RESOLVED

Implemented using `tokio_serial::available_ports()` with USB, Bluetooth, PCI, and
Unknown port type descriptions.

### Inconsistent frequency/rig naming — DOCUMENTED AS INTENTIONAL

Field names reflect distinct semantic contexts: `freq_hz` (dial), `center_hz`
(SDR capture center), `cw_center_hz` (CW tone); `rig_id` (config key), `id`
(runtime UUID); `model` (hardware string), `rig_model` (config parameter).

### Decoder task duplication in audio.rs — RESOLVED

**Location:** `src/trx-server/src/audio.rs`

APRS and HF APRS decoders merged into a single parameterised
`run_aprs_decoder_inner()` function. FT8 and FT4 decoders merged into
`run_ftx_decoder_inner()`. All decoder tasks now include `tracing::info_span!`
around `block_in_place()` calls for opt-in latency measurement.

### Missing tests for critical modules — RESOLVED

**Location:** `src/trx-server/src/listener.rs`, `src/trx-client/trx-frontend/trx-frontend-http/`

Added multi-rig state isolation and command routing tests in `listener.rs`.
Added background decode `evaluate_bookmark` pure-function tests.

### Missing integration tests for multi-rig scenarios — RESOLVED

**Location:** `src/trx-server/src/listener.rs`

Added integration tests covering simultaneous state management across two rigs
with a dummy backend, verifying state isolation and command routing.

### Decode log silent failures — RESOLVED

**Location:** `src/decoders/trx-decode-log/src/lib.rs`

`flush()` errors are now logged via `warn!`. On file rotation failure, the old
writer is kept rather than silently dropping writes; a degradation warning is
emitted.

### `api.rs` file size and organization — RESOLVED

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/api/`

Split 2,831-LOC monolith into 7 logically grouped modules: `mod.rs` (shared
types and route configuration), `decoder.rs`, `rig.rs`, `vchan.rs`, `sse.rs`,
`bookmarks.rs`, `assets.rs`.

### Background decode state complexity — RESOLVED

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/background_decode.rs`

Extracted the 8-guard decision cascade into a pure `evaluate_bookmark()` function
returning `ChannelAction` enum (`Active` or `Skip { reason }`). Added unit tests
for all decision paths.

### Actix-web pinned to exact version — RESOLVED

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/Cargo.toml`

Relaxed from `actix-web = "=4.4.1"` to `actix-web = "4.4"` to allow patch-level
security updates.

### Magic numbers in VDES plausibility scoring — RESOLVED

**Location:** `src/decoders/trx-vdes/src/lib.rs`

Inline magic numbers replaced with documented named constants:
`PLAUSIBILITY_UNSYNCED_THRESHOLD` (−35) and
`PLAUSIBILITY_LOW_CONFIDENCE_THRESHOLD` (15).

### FT-817 VFO inference fragile with same frequency — DOCUMENTED

**Location:** `src/trx-server/trx-backend/trx-backend-ft817/src/lib.rs`

When both VFOs share the same frequency, inference defaults to VFO A. Resolved
after VFO toggle primes both sides. Well-documented in code comments; remains
a known limitation.

### Excessive string cloning in remote client — RESOLVED

**Location:** `src/trx-client/src/remote_client.rs`

Hot-path spectrum polling loop now caches the token to avoid per-poll cloning.
State update path restructured to send to the main watch channel last (taking
ownership) and avoid one redundant `RigState::clone()`.

### Missing doc comments on public decoder structs — RESOLVED

**Location:** `src/decoders/trx-ais/src/lib.rs`, `src/decoders/trx-vdes/src/lib.rs`,
`src/decoders/trx-rds/src/lib.rs`

Added comprehensive doc comments to `AisDecoder`, `VdesDecoder`, and `RdsDecoder`
describing valid sample rates, usage examples, and reset semantics.

### Turbo decoder precondition not asserted — RESOLVED

**Location:** `src/decoders/trx-vdes/src/turbo.rs`

Added `debug_assert_eq!` on interleaver and deinterleaver lengths in
`turbo_decode_soft()`.

### No tracing spans for decoder performance — RESOLVED

**Location:** `src/trx-server/src/audio.rs`

Added `tracing::info_span!` around `block_in_place()` calls in all 10 decoder
tasks (APRS, HF APRS, AIS A/B, VDES, CW, FT8, FT4, FT2, WSPR, LRPT) for
opt-in per-decoder latency measurement.

</details>

---

All previous improvement items have been resolved. No outstanding issues.
