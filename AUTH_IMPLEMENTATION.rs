//! HTTP auth implementation plan (draft)
//!
//! Scope: `trx-frontend-http` optional passphrase auth with roles:
//! - `rx` (read-only)
//! - `control` (read + mutating control)
//!
//! This is a planning artifact, not compiled runtime code.

/// Implementation phases in execution order.
#[allow(dead_code)]
pub enum Phase {
    ConfigModel,
    RuntimeState,
    AuthEndpoints,
    MiddlewareAuthorization,
    FrontendLoginGate,
    FrontendRoleGating,
    Testing,
    DocsAndExamples,
}

/// High-level delivery checklist.
#[allow(dead_code)]
pub const CHECKLIST: &[&str] = &[
    "Add optional [frontends.http.auth] config (enabled default false)",
    "Support rx_passphrase/control_passphrase + tx_access_control_enabled",
    "Create in-memory session store with role + expiry",
    "Implement /auth/login, /auth/logout, /auth/session",
    "Add middleware for route protection and role enforcement",
    "Gate TX/PTT mutating routes when tx_access_control_enabled=true",
    "Update web app to show Access denied + login until authenticated",
    "Hide/disable TX/PTT controls for rx role",
    "Add unit + integration tests for role matrix",
    "Document behavior in README + config examples",
];

/// Detailed plan per phase.
#[allow(dead_code)]
pub mod detailed_plan {
    /// Phase 1: config structs + parsing
    pub const CONFIG_MODEL: &[&str] = &[
        "Add HttpAuthConfig into client frontend config model:",
        "  enabled: bool (default false)",
        "  rx_passphrase: Option<String>",
        "  control_passphrase: Option<String>",
        "  tx_access_control_enabled: bool (default true)",
        "  session_ttl_min: u64 (default 480)",
        "  cookie_secure: bool (default false)",
        "  cookie_same_site: enum/string (default Lax)",
        "Validation: if enabled=true, require at least one passphrase",
        "Validation: accept rx-only, control-only, or both",
    ];

    /// Phase 2: runtime auth state
    pub const RUNTIME_STATE: &[&str] = &[
        "Define AuthRole { Rx, Control }",
        "Define SessionRecord { role, issued_at, expires_at, last_seen }",
        "Create SessionStore = HashMap<SessionId, SessionRecord> + Mutex/RwLock",
        "Generate session IDs via cryptographically secure random bytes",
        "Add periodic expired-session cleanup task",
        "Attach auth state to Actix app_data in HTTP server builder",
    ];

    /// Phase 3: auth endpoints
    pub const AUTH_ENDPOINTS: &[&str] = &[
        "POST /auth/login body: { passphrase }",
        "Match control passphrase first, then rx passphrase",
        "Set HttpOnly session cookie trx_http_sid with TTL",
        "Response: { authenticated: true, role: \"rx\"|\"control\" }",
        "POST /auth/logout clears cookie + removes session",
        "GET /auth/session returns current auth state/role",
        "Do not log passphrase values",
    ];

    /// Phase 4: middleware + route authorization
    pub const MIDDLEWARE_AUTHZ: &[&str] = &[
        "Install middleware only when auth.enabled=true",
        "Public allowlist: /, static assets, /auth/*",
        "Protected read routes (rx/control): /status, /events, /decode, /audio",
        "Protected control routes (control only): all mutating POST routes",
        "On missing/invalid session: return 401",
        "On insufficient role: return 403 (or 404 for hidden TX/PTT policy)",
        "If tx_access_control_enabled=true: enforce hard block for TX/PTT endpoints for non-control",
    ];

    /// Phase 5: frontend login gate and default denied state
    pub const FRONTEND_LOGIN_GATE: &[&str] = &[
        "At app startup call /auth/session before connect()",
        "If unauthenticated: show logo + 'Access denied' view + passphrase form",
        "On login success: initialize streams/events and normal UI",
        "On 401/403 from API/SSE/WS: stop streams and return to denied/login view",
        "Add logout action in header/about",
    ];

    /// Phase 6: frontend role-specific UI policy
    pub const FRONTEND_ROLE_GATING: &[&str] = &[
        "If role=rx: hide/disable TX/PTT/mutating controls",
        "If role=control: show full controls",
        "When tx_access_control_enabled=true and role!=control:",
        "  do not render PTT/TX controls at all",
        "  do not expose action affordances in DOM where possible",
    ];

    /// Phase 7: tests
    pub const TESTS: &[&str] = &[
        "Unit: config validation for enabled/disabled + passphrase combinations",
        "Unit: session creation, lookup, expiry cleanup",
        "Unit: middleware path classification and role checks",
        "Integration: unauth /set_freq => 401",
        "Integration: rx login => /status 200, /set_ptt 403",
        "Integration: control login => /set_ptt 200",
        "Integration: tx_access_control_enabled=true => tx/ptt unavailable for rx",
        "Integration: auth disabled => legacy behavior unchanged",
    ];

    /// Phase 8: docs
    pub const DOCS: &[&str] = &[
        "Update trx-client.toml.example with [frontends.http.auth]",
        "Update README with optional auth behavior and role model",
        "Document security caveats: use TLS for non-local access",
    ];
}

/// Suggested file touch list (initial estimate).
#[allow(dead_code)]
pub const FILES_TO_TOUCH: &[&str] = &[
    "src/trx-client/src/config.rs (or equivalent client config model)",
    "trx-client.toml.example",
    "src/trx-client/trx-frontend/trx-frontend-http/src/server.rs",
    "src/trx-client/trx-frontend/trx-frontend-http/src/api.rs",
    "src/trx-client/trx-frontend/trx-frontend-http/src/audio.rs",
    "src/trx-client/trx-frontend/trx-frontend-http/assets/web/index.html",
    "src/trx-client/trx-frontend/trx-frontend-http/assets/web/app.js",
    "README.md",
];

/// Rollout strategy.
#[allow(dead_code)]
pub const ROLLOUT: &[&str] = &[
    "Step 1: backend-only auth endpoints + middleware behind enabled flag",
    "Step 2: frontend login UX and role-aware UI",
    "Step 3: enforce TX/PTT hard-gate and tests",
    "Step 4: docs + example config",
];
