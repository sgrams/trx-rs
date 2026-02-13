# HTTP Frontend Authentication Draft

## Goal
Add optional passphrase authentication for `trx-frontend-http` with two roles:
- `rx` passphrase: read-only access
- `control` passphrase: read + control (RX+TX)

API/control routes stay locked until a user logs in from the web UI.

This design keeps current behavior when auth is disabled.

## Scope
- Protect HTTP API endpoints used by the web UI.
- Protect SSE (`/events`, `/decode`) and audio WebSocket (`/audio`).
- Keep static assets and login page accessible so user can authenticate.
- Do not change rigctl/http_json auth behavior in this draft.

## Security Model
- Two optional passphrases configured locally (`rx`, `control`).
- On successful login, server issues short-lived session cookie.
- Session required for all protected routes, with role attached.
- Brute-force mitigation via simple per-IP rate limiting.
- TX access can be globally hidden/blocked unless `control` role is present.

This is not multi-user IAM; it is a pragmatic local/ham-shack gate.

## Config Proposal
Add to `trx-client.toml`:

```toml
[frontends.http.auth]
enabled = false
# Plaintext passphrases (as requested)
rx_passphrase = "rx-only-passphrase"
control_passphrase = "full-control-passphrase"

# If true, TX/PTT controls/endpoints are never available without control auth.
tx_access_control_enabled = true

# Session lifetime in minutes
session_ttl_min = 480

# Cookie security
cookie_secure = false      # true if served via HTTPS
cookie_same_site = "Lax"  # Strict|Lax|None
```

Validation rules:
- If `enabled=false`, all auth fields ignored.
- If `enabled=true`, require at least one passphrase (`rx` and/or `control`).
- `rx_passphrase` only: read-only deployment.
- `control_passphrase` only: control-capable deployment.
- both set: mixed deployment with role split.

Behavior by mode:
- `enabled=false` (default): no authentication, current behavior unchanged.
- `enabled=true`: authentication enforced per role/route rules in this document.

## Runtime Structures
Add in `src/trx-client/trx-frontend/src/lib.rs` (or HTTP crate-local state):
- `HttpAuthConfig`:
  - `enabled: bool`
  - `rx_passphrase: Option<String>`
  - `control_passphrase: Option<String>`
  - `tx_access_control_enabled: bool`
  - `session_ttl: Duration`
  - `cookie_secure: bool`
  - `same_site: SameSite`
- `SessionStore` in-memory map:
  - key: random session id (128-bit+)
  - value: `{ role, issued_at, expires_at, last_seen, ip_hash? }`

Role enum:
- `AuthRole::Rx`
- `AuthRole::Control`

Periodic cleanup task (e.g., every 5 min) removes expired sessions.

## Route Design
New endpoints:
- `POST /auth/login`
  - body: `{ "passphrase": "..." }`
  - server checks passphrase against `control` first, then `rx`
  - on success: set `HttpOnly` cookie `trx_http_sid`, return `{ role: "rx"|"control" }`
  - on failure: 401 generic error
- `POST /auth/logout`
  - clears cookie and invalidates server session
- `GET /auth/session`
  - returns `{ authenticated: true|false, role?: "rx"|"control" }`

Protected existing endpoints:
- Control APIs (`control` role required): `/set_freq`, `/set_mode`, `/set_ptt`, `/toggle_power`, `/toggle_vfo`, `/lock`, `/unlock`, `/set_tx_limit`, `/toggle_*_decode`, `/clear_*_decode`, CW tuning endpoints, etc.
- Read APIs (`rx` or `control`): `/status`, `/events`, `/decode`, `/audio`

TX/PTT hard-gate behavior when `tx_access_control_enabled=true`:
- Do not render TX/PTT controls for unauthenticated or `rx` role.
- Reject TX/PTT and mutating control endpoints unless role is `control`.
- Prefer returning `404` for hidden TX/PTT endpoints to avoid capability leakage
  (or `403` if explicit error semantics are preferred).

Public endpoints:
- `/` (HTML shell)
- static assets (`/style.css`, `/app.js`, plugin js, logo, favicon)
- `/auth/*`

## Middleware Behavior
Implement Actix middleware/wrap fn in `trx-frontend-http`:
- Resolve session from cookie.
- Validate in store and expiry.
- If missing/invalid:
  - API routes: return `401` JSON/text
  - SSE/WS routes: return `401`
- If valid:
  - enforce route role (`rx` or `control`)
  - return `403` when authenticated but role is insufficient
  - continue request
  - optionally slide expiry (`last_seen + ttl`) with cap.

Keep middleware route-aware by checking request path against allowlist.

## Passphrase Handling
- Use exact passphrase comparison against config values (no hash layer in this draft).
- Still use constant-time string comparison helper to reduce timing leakage.
- Keep passphrases out of logs and API responses.

## Cookie Settings
Session cookie:
- `HttpOnly=true`
- `Secure` configurable (true for TLS)
- `SameSite=Lax` default
- `Path=/`
- Max-Age = session TTL

## Frontend Flow
In `assets/web/app.js`:
1. On startup call `/auth/session`.
2. If unauthenticated, show blocking screen with logo + `Access denied`.
3. Submit to `/auth/login`.
4. On success initialize normal app flow (`connect()`, decode stream).
5. If role is `rx`, disable/hide all TX/PTT/mutating controls.
6. If role is `control`, enable full UI.
7. If protected call returns 401/403, stop streams and return to login panel.
8. Add logout button in About tab or header.

UI minimal requirement:
- Default unauthenticated view: logo + `Access denied` + passphrase field + login button.
- Generic error message on failure.
- No passphrase persistence in localStorage.

## Implementation Steps
1. Extend client config structs + parser defaults.
2. Build auth state (passphrases + session store) in HTTP server startup.
3. Add `/auth/login`, `/auth/logout`, `/auth/session` handlers.
4. Add middleware and protect selected routes.
5. Update frontend JS with login gate and 401 handling.
6. Add docs to `README.md` + `trx-client.toml.example`.
7. Add role matrix tests and frontend role UI handling.

## Test Plan
Unit tests:
- Config validation combinations.
- Login success/failure.
- Session expiry.
- Middleware path allowlist/protection.
- Role enforcement (`rx` denied on control routes).
- TX visibility policy (`tx_access_control_enabled`) endpoint behavior.

Integration tests (Actix test server):
- Unauthed call to `/set_freq` -> 401.
- `rx` login -> cookie set -> `/status` accepted, `/set_freq` -> 403.
- `control` login -> `/set_freq` accepted.
- With `tx_access_control_enabled=true`, unauth/`rx` cannot use `/set_ptt`.
- Expired session -> 401.
- `/events` and `/audio` reject unauthenticated clients.

Manual checks:
- Browser login works.
- WSJT-X/hamlib unaffected (non-http frontends).
- Auth disabled mode behaves exactly as before.

## Operational Notes
- This is in-memory session state. Restart invalidates sessions.
- For reverse proxy deployments, use TLS and set `cookie_secure=true`.
- If remote exposure is possible, use strong passphrase and firewall.

## Future Extensions
- Optional API bearer token for automation scripts.
- Optional migration to hashed passphrases if threat model increases.
- Persistent sessions with signed tokens/JWT (if needed).
- Optional TOTP second factor for internet-exposed deployments.
