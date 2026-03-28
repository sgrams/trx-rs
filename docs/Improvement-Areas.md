# Improvement Areas

A comprehensive audit of the trx-rs codebase covering code quality, architecture,
security, testing, and performance. Each item includes the affected location and
a suggested fix.

*Last updated: 2026-03-26*

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