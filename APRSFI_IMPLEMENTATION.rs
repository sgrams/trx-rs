//! APRS.fi integration implementation draft (server-side)
//!
//! Goal:
//! - Add optional APRS.fi upload/logging support for decoded APRS packets.
//! - Keep feature disabled by default.
//! - Reuse existing decode pipeline in `trx-server`.
//!
//! This is a planning artifact, not active runtime logic.

/// Delivery phases.
#[allow(dead_code)]
pub enum Phase {
    Config,
    PacketSelection,
    UplinkWorker,
    RetryAndRateLimit,
    PrivacyControls,
    Tests,
    Docs,
}

/// Proposed config block for `trx-server.toml`.
#[allow(dead_code)]
pub const CONFIG_PROPOSAL: &str = r#"
[aprsfi]
enabled = false

# APRS.fi API token / key (required when enabled)
api_key = ""

# Optional station identity metadata
receiver_callsign = "N0CALL"
receiver_locator = "JO93"

# Upload endpoint override for testing
endpoint = "https://api.aprs.fi/api"

# Upload policy
include_third_party = false
min_interval_ms = 1000
max_queue = 1000
"#;

/// Validation rules.
#[allow(dead_code)]
pub const VALIDATION: &[&str] = &[
    "If aprsfi.enabled=false: ignore all aprsfi fields",
    "If aprsfi.enabled=true: api_key must be non-empty",
    "min_interval_ms must be > 0",
    "max_queue must be > 0",
];

/// Runtime architecture.
#[allow(dead_code)]
pub const ARCHITECTURE: &[&str] = &[
    "Spawn dedicated APRS.fi worker task in src/trx-server/src/main.rs",
    "Subscribe to decode broadcast channel (existing decode_tx.subscribe())",
    "Filter DecodedMessage::Aprs only",
    "Transform AprsPacket into APRS.fi payload DTO",
    "Queue and POST asynchronously with bounded backpressure",
    "Never block decoder tasks on network I/O",
];

/// Integration points in current code.
#[allow(dead_code)]
pub const INTEGRATION_POINTS: &[&str] = &[
    "src/trx-server/src/config.rs: add AprsFiConfig",
    "src/trx-server/src/main.rs: start worker when enabled",
    "src/trx-server/src/audio.rs: no direct changes required (consume from decode stream)",
    "src/trx-server/src/<new>/aprsfi.rs: worker + payload mapping + HTTP client",
    "trx-server.toml.example + CONFIGURATION.md: docs",
];

/// Packet handling policy.
#[allow(dead_code)]
pub const PACKET_POLICY: &[&str] = &[
    "Upload only packets with valid callsign and parseable position by default",
    "Optionally allow non-position packets if APRS.fi endpoint supports them",
    "Deduplicate burst repeats (same src/info within short window)",
    "Drop malformed frames silently with debug log",
];

/// Retry/rate limiting policy.
#[allow(dead_code)]
pub const RELIABILITY_POLICY: &[&str] = &[
    "Bounded mpsc queue (max_queue)",
    "If queue full: drop oldest or newest by configurable policy (MVP: drop newest)",
    "Exponential backoff on HTTP/network errors",
    "Respect min_interval_ms between outbound requests",
    "Throttle warning logs to avoid spam",
];

/// Privacy/safety controls.
#[allow(dead_code)]
pub const PRIVACY_CONTROLS: &[&str] = &[
    "Feature disabled by default",
    "API key never logged",
    "Optional include_third_party flag for re-published packets",
    "Document that enabling uploads sends decoded RF data to external service",
];

/// Test plan.
#[allow(dead_code)]
pub const TEST_PLAN: &[&str] = &[
    "Unit: config parse + validation",
    "Unit: APRS packet -> APRS.fi payload mapping",
    "Unit: dedupe and queue/backpressure behavior",
    "Unit: retry/backoff timing logic",
    "Integration: mock HTTP endpoint receives expected payloads",
    "Integration: disabled mode performs no outbound requests",
];

/// Suggested first implementation milestone (M1).
#[allow(dead_code)]
pub const M1: &[&str] = &[
    "Add config + validation + docs",
    "Create aprsfi worker skeleton (no uploads yet, just consume + structured logs)",
    "Add payload mapping function with tests",
    "Add feature flag + startup logs",
];

/// Suggested second milestone (M2).
#[allow(dead_code)]
pub const M2: &[&str] = &[
    "Implement real HTTP POST uploads",
    "Add retry/backoff + queue policy",
    "Add integration test with mock server",
    "Add operational metrics counters",
];
