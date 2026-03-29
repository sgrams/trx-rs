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

</details>

---

## High Priority (P1)

### Decoder task duplication in audio.rs

**Location:** `src/trx-server/src/audio.rs` (3,826 LOC)

Nine decoder tasks (APRS, AIS, VDES, CW, FT2, FT4, FT8, WSPR, LRPT) each
implement the same pattern: subscribe to PCM broadcast, watch for state changes
(mode/frequency/reset), call `block_in_place()` for synchronous decoding, record
to history, and forward to `decode_tx`. This results in ~1,000 lines of
near-identical boilerplate with 14+ `block_in_place()` calls and 12+
`.resubscribe()` calls.

**Risk:** A bug fix or behavior change (e.g., lag handling, error recovery) must
be replicated across all 9 decoders manually.

**Suggestion:** Extract a `DecoderTask<D>` generic that encapsulates the
subscribe → watch → decode → record → forward lifecycle. Each decoder implements
a trait with `process_block()` and `reset()` methods. Estimated reduction: ~500
lines.

### Missing tests for critical modules

**Location:** `src/trx-server/src/audio.rs` (3,826 LOC, 0 tests),
`src/trx-client/trx-frontend/trx-frontend-http/src/api.rs` (2,831 LOC, 0 tests),
`src/trx-client/src/main.rs` (679 LOC, 0 tests)

These are among the largest files in the codebase and have zero unit tests.
`history_store.rs` and `auth.rs` now have good coverage; `handlers.rs` has 4
tests. The remaining files require ALSA/hardware mocking infrastructure or HTTP
test harnesses.

**Suggestion:** Start with `api.rs` — actix-web's `test::TestRequest` makes
endpoint testing feasible without hardware. Extract pure logic from `audio.rs`
into testable helpers where possible.

### Missing integration tests for multi-rig scenarios

No tests verify state isolation or command routing between rigs in multi-rig
configurations, despite the codebase supporting per-rig task isolation with
`HashMap<rig_id, RigHandle>` routing.

**Risk:** Cross-rig state pollution on refactors.

**Suggestion:** Add integration test covering simultaneous frequency/mode changes
on two rigs with a dummy backend.

---

## Medium Priority (P2)

### Decode log silent failures

**Location:** `src/decoders/trx-decode-log/src/lib.rs`

- Line 160: `let _ = state.writer.flush();` silently discards flush errors. Disk
  full or permission changes cause silent data loss.
- Lines 137–150: If file rotation fails (open error), subsequent writes retry the
  same path indefinitely with no fallback writer or degradation logging.

**Suggestion:** Log flush errors via `warn!`. On rotation failure, keep the old
writer and log a degradation warning rather than silently failing.

### `api.rs` file size and organization

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/api.rs` (2,831 LOC)

Contains ~25+ endpoint handlers spanning decoder history, frequency/mode control,
virtual channel management, spectrum, and SSE streams with no logical separation.

**Suggestion:** Consider splitting into `decoder_api.rs`, `vchan_api.rs`,
`rig_api.rs` in a future refactor.

### Background decode state complexity

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/src/background_decode.rs:350–444`

The `run()` method's inner loop contains 8+ nested conditional branches
(users_connected, scheduler_has_control, scheduled_bookmark_ids, virtual channel
coverage, spectrum availability, offset bounds). Correct but difficult to modify
or extend.

**Suggestion:** Extract the decision logic into a pure function returning a
`ChannelAction` enum. Improves testability and makes the state machine explicit.

### Actix-web pinned to exact version

**Location:** `src/trx-client/trx-frontend/trx-frontend-http/Cargo.toml`

`actix-web = "=4.4.1"` prevents automatic patch-level security updates. Later
4.x releases may include security fixes.

**Suggestion:** Relax to `actix-web = "4.4"` to allow patch updates, or
periodically review and bump the pinned version.

### Magic numbers in VDES plausibility scoring

**Location:** `src/decoders/trx-vdes/src/lib.rs:261–280`

Plausibility thresholds (`-35`, `15`) are inline magic numbers with no
documentation of the scoring scale or units.

**Suggestion:** Define named constants:
```rust
const PLAUSIBILITY_UNSYNCED_THRESHOLD: i32 = -35;
const PLAUSIBILITY_LOW_CONFIDENCE_THRESHOLD: i32 = 15;
```

---

## Low Priority (P3)

### FT-817 VFO inference fragile with same frequency

**Location:** `src/trx-server/trx-backend/trx-backend-ft817/src/lib.rs:233–265`

When both VFOs share the same frequency, inference defaults to VFO A. Resolved
after VFO toggle primes both sides. Well-documented in code comments but remains
a known limitation.

### Excessive string cloning in remote client

**Location:** `src/trx-client/src/remote_client.rs`

~105 `.clone()` calls on String fields, many in hot paths during poll loops
(spectrum, state updates). Most are necessary for ownership across async
boundaries, but some could use borrowed references or `Cow<str>`.

**Suggestion:** Audit hot-path clones in `run_remote_client`, particularly around
spectrum polling loops. Low priority unless profiling shows allocation pressure.

### Missing doc comments on public decoder structs

**Location:** `src/decoders/trx-ais/src/lib.rs`, `src/decoders/trx-vdes/src/lib.rs`,
`src/decoders/trx-rds/src/lib.rs`

Public decoder structs (`AisDecoder`, `VdesDecoder`, `RdsDecoder`) lack doc
comments describing valid sample rates, preconditions, and guarantees.

### Turbo decoder precondition not asserted

**Location:** `src/decoders/trx-vdes/src/turbo.rs:208–249`

`turbo_decode_soft()` accesses interleaver/deinterleaver vectors without bounds
checks. The precondition `interleaver.len() == info_len` is clear from context
and enforced by the caller, but not formally documented or debug-asserted.

### No tracing spans for decoder performance

**Location:** `src/trx-server/src/audio.rs`

Decoders use `info!`/`warn!` logs but don't emit tracing spans. No way to
measure per-decoder latency without sampling logs.

**Suggestion:** Add `tracing::info_span!` around `block_in_place()` calls for
opt-in performance measurement.
