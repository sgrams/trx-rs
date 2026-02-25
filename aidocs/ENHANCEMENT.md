# Top 5 Real Architecture Issues (Post-Refactor)

## 1) Plugin ABI is still brittle and unversioned
### Files
- `src/trx-app/src/plugins.rs`
- `examples/trx-plugin-example/src/lib.rs`

### Why this matters
Plugin loading is now explicit (good), but still assumes exact symbol names and raw FFI contracts with no ABI/version handshake. A plugin built against an older/newer ABI can fail at runtime in hard-to-diagnose ways.

### Fix steps
1. Add an ABI version symbol/handshake (`trx_plugin_abi_version`) and reject incompatible plugins with clear errors.
2. Split plugin capability metadata (backend/frontend/both) from registration symbols to avoid noisy failed-load logs.
3. Provide a tiny shared plugin-API crate for stable entrypoint signatures.

## 2) Runtime supervision is still ad-hoc (sleep + abort)
### Files
- `src/trx-server/src/main.rs`
- `src/trx-client/src/main.rs`

### Why this matters
Shutdown is coordinated, but supervision still uses a fixed delay plus manual `abort()` over `Vec<JoinHandle<_>>`. This can mask task failures, race shutdown ordering, and make lifecycle behavior harder to reason about.

### Fix steps
1. Move to `JoinSet` (or a small supervisor type) for task ownership and result handling.
2. Replace fixed sleep with bounded graceful-join timeout logic.
3. Surface task failure reasons consistently in one place.

## 3) JSON/TCP transport logic is duplicated across modules
### Files
- `src/trx-server/src/listener.rs`
- `src/trx-client/trx-frontend/trx-frontend-http-json/src/server.rs`
- `src/trx-client/src/remote_client.rs`

### Why this matters
`read_limited_line`, timeout handling, and response write patterns are repeated in multiple places. This increases drift risk and makes protocol hardening changes expensive.

### Fix steps
1. Extract shared JSON-over-TCP helpers into `trx-protocol` (or a small transport crate/module).
2. Keep one source of truth for max line size, timeout behavior, and framing errors.
3. Cover shared transport with focused tests once instead of per-module copies.

## 4) Boundary tests are present but mostly ignored in constrained envs
### Files
- `src/trx-server/src/listener.rs`
- `src/trx-client/src/remote_client.rs`
- `src/trx-client/trx-frontend/trx-frontend-http-json/src/server.rs`

### Why this matters
Important network-path tests exist, but are marked `#[ignore]` in this environment due bind restrictions. Without a clear CI strategy, regressions can still slip through.

### Fix steps
1. Add CI jobs/environment where bind-based tests run by default.
2. Split pure transport logic from socket bind/accept so more behavior can be tested without real sockets.
3. Keep ignored tests minimal and document how/when they run.

## 5) Decode/history shared state still relies on global mutexes
### Files
- `src/trx-server/src/audio.rs`
- `src/trx-client/trx-frontend/src/lib.rs`
- `src/trx-client/trx-frontend/trx-frontend-http/src/audio.rs`

### Why this matters
History/state paths still use shared mutex-backed globals/contexts with `expect` on lock poisoning in hot paths. This is workable but fragile for long-running async services.

### Fix steps
1. Replace panic-on-poison lock usage with resilient handling.
2. Consider bounded channel or lock-free append/read model for decode history.
3. Define explicit ownership/lifetime for history data instead of implicit shared mutation.
