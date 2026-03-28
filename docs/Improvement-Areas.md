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

## New Findings (2026-03-28 Deep Review)

### High Priority (P1)

#### Rig task command batching uses LIFO order

**Location:** `src/trx-server/src/<rig_task.rs>` — `batch.pop()`

Pending commands are accumulated into a `Vec` and processed with `pop()`, which
reverses arrival order. If a client sends `SetFreq(14.074)` then `SetMode(USB)`,
the mode change executes before the frequency change. This can cause unexpected
transient state on hardware that validates mode against frequency (e.g. FT-817
rejects CW below 1.8 MHz).

**Fix:** Replace `pop()` with `drain(..)` or iterate in forward order.

#### FrontendRuntimeContext god-struct (~50 fields)

**Location:** `src/trx-client/trx-frontend/src/<lib.rs>`

Mixes audio channels, decode histories, auth config, UI settings, rig routing,
virtual channel management, and branding info into a single struct passed
through `Arc`. Every frontend receives all 50 fields even if it only needs a
subset. Changes to any field group force recompilation of all frontends.

**Suggested decomposition:**
```
FrontendRuntimeContext
  ├── AudioContext          (audio_rx, audio_tx, audio_info, decode_rx)
  ├── DecodeHistoryContext  (ais, vdes, aprs, hf_aprs, cw, ft8, ft4, ft2, wspr)
  ├── HttpAuthConfig        (enabled, passphrases, session_ttl, cookie settings)
  ├── HttpUiConfig          (map_zoom, spectrum settings, history retention)
  ├── RigRoutingContext     (active_rig_id, remote_rigs, rig_states, rig_spectrums)
  ├── OwnerInfo             (callsign, website_url, website_name, ais_vessel_url)
  └── VChanContext          (vchan_audio, vchan_audio_cmd, vchan_destroyed)
```

#### Decoder history queues have no capacity bounds

**Location:** `src/trx-server/src/<audio.rs>` — `DecoderHistories`

History queues (`VecDeque`) grow unbounded until the 24h retention period expires.
Under high traffic (e.g. busy AIS channel near a port), a single queue could
accumulate millions of entries and consume gigabytes of memory.

**Fix:** Add per-decoder max capacity (e.g. 10,000 entries). Evict oldest entries
when capacity is reached, independent of time-based pruning.

#### ExponentialBackoff has no jitter

**Location:** `src/trx-core/src/rig/controller/<policies.rs>`

Multiple rigs or reconnecting clients using the same backoff parameters will retry
at identical times (thundering herd). This is especially problematic when a server
restarts and all clients reconnect simultaneously.

**Fix:** Add randomized jitter (e.g. ±25% of the computed delay) to the
`ExponentialBackoff::delay()` method.

#### No crash recovery for rig tasks

**Location:** `src/trx-server/src/<main.rs>`

If a rig task panics (e.g. due to an unexpected backend error), the task simply
disappears. The listener continues routing commands to the dead rig's channel,
where they silently timeout. No automatic restart or health monitoring exists.

**Fix:** Wrap rig tasks in a supervisor loop that detects task completion/panic
and restarts with backoff. Emit a `RigMachineState::Error` on the watch channel
so clients see the failure.

---

### Medium Priority (P2)

#### SoapySdrRig constructor takes 20+ parameters

**Location:** `src/trx-server/trx-backend/trx-backend-soapysdr/src/<lib.rs>`
— `new_with_config()`

The constructor accepts 20+ positional parameters with no builder pattern,
making call sites fragile and hard to read. Adding a new parameter requires
updating all callers.

**Fix:** Introduce a `SoapySdrConfig` builder struct with sensible defaults.

#### Dual command enums with mechanical 1:1 mapping

**Location:** `src/trx-protocol/src/<mapping.rs>` (675 lines),
`src/trx-protocol/src/<types.rs>`, `src/trx-core/src/rig/<command.rs>`

`ClientCommand` and `RigCommand` are near-identical 40+ variant enums with
purely mechanical mapping in `mapping.rs`. Adding a new command requires editing
4 files (command.rs, types.rs, mapping.rs in both directions, codec.rs).
`mapping.rs` contains an `unreachable!()` for `GetRigs` that would panic if
the listener logic changes.

**Fix:** Consider a macro that generates both enums and the mapping from a single
definition. Alternatively, collapse to a single enum with serde annotations.

#### Lock poisoning recovery hides panics

**Location:** `src/trx-server/src/<audio.rs>` — `DecoderHistories`

All mutex acquisitions use `.unwrap_or_else(|e| e.into_inner())` which silently
recovers from poisoned mutexes. While this prevents cascading panics, it hides
the original panic and may operate on inconsistent data.

**Fix:** Log a warning when recovering from a poisoned lock, and consider whether
the recovered data is actually safe to use. For history queues, clearing the
queue on poison recovery may be safer than continuing with partial data.

#### Configuration duplication between server and client

**Location:** `src/trx-server/src/<config.rs>` (1,512 lines),
`src/trx-client/src/<config.rs>` (1,181 lines)

14 config structs each, many mirrored between server and client (GeneralConfig,
rig model definitions, defaults). Shared config definitions should live in
`trx-app`.

#### Hardcoded timeouts and retention periods

**Locations:** Multiple files

| Constant | Value | Location |
|----------|-------|----------|
| COMMAND_EXEC_TIMEOUT | 10s | rig_task.rs |
| POLL_REFRESH_TIMEOUT | 8s | rig_task.rs |
| IO_TIMEOUT | 10s | listener.rs |
| REQUEST_TIMEOUT | 12s | listener.rs |
| History retention | 24h | audio.rs |
| FT-817 read timeout | 800ms | trx-backend-ft817 |
| RIG_TASK_CHANNEL_BUFFER | 32 | main.rs |

None are configurable. Making these part of the TOML config would help
deployments with slow serial links or high-latency networks.

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