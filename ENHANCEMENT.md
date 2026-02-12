# Top 5 Real Architecture Issues

## 1) Global plugin compatibility registries still exist
### Files
- `src/trx-server/trx-backend/src/lib.rs`
- `src/trx-client/trx-frontend/src/lib.rs`

### Why this matters
`OnceLock<Mutex<...>>` registry shims still hold mutable global state. This keeps plugin registration behavior implicit and harder to test.

### Fix steps
1. Introduce explicit plugin registration API that takes a mutable context.
2. Make plugin loader return registration data instead of relying on global side effects.
3. Remove global `register_*`/`snapshot_bootstrap_context` wrappers after migration.

## 2) No supervised shutdown/lifecycle model
### Files
- `src/trx-server/src/main.rs`
- `src/trx-client/src/main.rs`

### Why this matters
Many tasks are detached via `tokio::spawn` and process shutdown mostly waits on Ctrl+C. Task failures and cancellation order are not centrally managed.

### Fix steps
1. Add shared cancellation token.
2. Track tasks in `JoinSet`.
3. On shutdown: stop listeners, cancel workers, await joins with timeout, then exit.

## 3) Protocol/network hardening gaps
### Files
- `src/trx-client/src/remote_client.rs`
- `src/trx-server/src/listener.rs`
- `src/trx-client/trx-frontend/trx-frontend-http-json/src/server.rs`

### Why this matters
`parse_remote_url` is ad-hoc and line-based listeners accept unbounded lines. This risks parsing edge cases and memory pressure.

### Fix steps
1. Replace string URL parsing with typed address parsing (support IPv4/IPv6/hostnames explicitly).
2. Enforce maximum line/frame size for JSON-over-TCP.
3. Add read/write/request timeouts and explicit error messages.

## 4) Config has parse defaults but weak semantic validation
### Files
- `src/trx-server/src/config.rs`
- `src/trx-client/src/config.rs`

### Why this matters
Config loads successfully even when values are semantically bad (timings, ports, audio params), leading to runtime failures.

### Fix steps
1. Add `validate()` to server/client config models.
2. Validate ranges and required field combinations.
3. Call `validate()` in startup before spawning tasks; fail fast with clear path-based errors.

## 5) Integration coverage is still thin at boundaries
### Files
- `src/trx-server/src/listener.rs`
- `src/trx-client/src/remote_client.rs`
- `src/trx-client/trx-frontend/trx-frontend-http-json/src/server.rs`
- `src/trx-app/src/plugins.rs`

### Why this matters
Most coverage is unit-level. Critical network/plugin/runtime flows can regress without tests.

### Fix steps
1. Add integration tests for JSON TCP auth/command flow.
2. Add reconnect tests for remote client.
3. Add plugin load/failure isolation tests.
4. Add shutdown behavior tests once lifecycle supervision is added.
