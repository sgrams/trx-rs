//! Decoder log files implementation plan (server-side)
//!
//! Goal:
//! - Persist decoded messages to disk.
//! - Split output by decoder name (APRS/CW/FT8/WSPR).
//! - Make file names/paths configurable in `trx-server.toml`.
//! - Keep runtime overhead low and avoid decoder stalls.
//!
//! This file is a planning artifact, not production code.

/// Ordered rollout phases.
#[allow(dead_code)]
pub enum Phase {
    ConfigSchema,
    RuntimeWriters,
    DecoderHookup,
    FileFormat,
    RotationAndRetention,
    ErrorHandling,
    Tests,
    Docs,
}

/// MVP checklist.
#[allow(dead_code)]
pub const MVP_CHECKLIST: &[&str] = &[
    "Add [decode_logs] config section in trx-server config",
    "Add per-decoder output targets (aprs/cw/ft8/wspr)",
    "Implement async file writer worker(s)",
    "Emit one line per decoded event (JSONL)",
    "Hook writers into existing record/send points in audio.rs",
    "Ensure logger failures never crash decoder tasks",
    "Add tests for config parsing + file output",
    "Document usage in trx-server.toml.example + CONFIGURATION.md",
];

/// Proposed config shape.
#[allow(dead_code)]
pub const CONFIG_PROPOSAL: &str = r#"
[decode_logs]
enabled = false
format = "jsonl"           # jsonl (initial MVP)
flush_interval_ms = 250     # buffered flush interval
create_dirs = true

[decode_logs.files]
# Any omitted decoder uses default pattern in base_dir
base_dir = "./logs/decoders"
aprs = "aprs.log"
cw = "cw.log"
ft8 = "ft8.log"
wspr = "wspr.log"

[decode_logs.rotation]
enabled = false
max_bytes = 10485760        # 10 MiB
max_files = 10
"#;

/// Config behavior rules.
#[allow(dead_code)]
pub const CONFIG_RULES: &[&str] = &[
    "If decode_logs.enabled=false, no file writers are started",
    "If enabled=true and path missing, use base_dir + '<name>.log'",
    "Paths may be absolute or relative to current working directory",
    "If create_dirs=true, create parent directories on startup",
    "Invalid paths -> startup warning + decoder logging disabled for that target",
];

/// File layout / names strategy.
#[allow(dead_code)]
pub const NAMING_PLAN: &[&str] = &[
    "Split by decoder name: aprs/cw/ft8/wspr",
    "Allow custom names per decoder via [decode_logs.files]",
    "Support one shared directory + per-decoder file names",
    "Keep deterministic defaults to simplify ops and tailing",
];

/// Runtime architecture.
#[allow(dead_code)]
pub const RUNTIME_ARCHITECTURE: &[&str] = &[
    "Create DecoderLogRouter with optional sender per decoder",
    "Spawn one async writer task per enabled decoder target",
    "Writer task receives already-serialized lines over bounded mpsc",
    "On backpressure/full queue: drop oldest/newest by policy + increment metric",
    "Periodic flush by timer; flush on shutdown signal",
];

/// Where to hook in existing server code.
#[allow(dead_code)]
pub const INTEGRATION_POINTS: &[&str] = &[
    "src/trx-server/src/main.rs: initialize router from config before decoder tasks",
    "src/trx-server/src/audio.rs: after record_* and before/after decode_tx.send(...)",
    "APRS: record_aprs_packet path in run_aprs_decoder",
    "CW: event emission path in run_cw_decoder",
    "FT8: record_ft8_message path in run_ft8_decoder",
    "WSPR: record_wspr_message path in run_wspr_decoder",
];

/// Line format (MVP JSONL).
#[allow(dead_code)]
pub const JSONL_SCHEMA: &[&str] = &[
    "ts_utc: RFC3339 timestamp generated at log write time",
    "decoder: one of aprs|cw|ft8|wspr",
    "rig_freq_hz: optional current RF base frequency",
    "payload: decoder-specific message object (existing serde struct)",
    "example: {\"ts_utc\":\"...\",\"decoder\":\"ft8\",\"payload\":{...}}",
];

/// Rotation/retention plan (post-MVP but scoped).
#[allow(dead_code)]
pub const ROTATION_PLAN: &[&str] = &[
    "MVP can skip rotation if disabled",
    "If enabled: rotate file when size exceeds max_bytes",
    "Rename N->N+1, keep max_files, truncate/create active file",
    "Rotation performed inside writer task to avoid lock contention",
];

/// Failure handling policy.
#[allow(dead_code)]
pub const FAILURE_POLICY: &[&str] = &[
    "Decoder pipeline must continue if file IO fails",
    "Log write failures with throttled warnings",
    "If a target becomes unavailable, retry reopen periodically",
    "Never panic from logger worker on malformed payload",
];

/// Testing plan.
#[allow(dead_code)]
pub const TEST_PLAN: &[&str] = &[
    "Unit: config parse/default/validation for decode_logs",
    "Unit: per-decoder path resolution",
    "Unit: JSONL serializer output for each decoder type",
    "Integration: run decoder emit path and assert lines written to correct files",
    "Integration: disabled mode creates no files",
    "Integration: queue overflow policy counters/warnings",
    "Integration: rotation behavior when enabled",
];

/// Files expected to change.
#[allow(dead_code)]
pub const FILES_TO_TOUCH: &[&str] = &[
    "src/trx-server/src/config.rs",
    "src/trx-server/src/main.rs",
    "src/trx-server/src/audio.rs",
    "src/trx-server/src/<new>/decode_logs.rs",
    "trx-server.toml.example",
    "CONFIGURATION.md",
    "tests under src/trx-server (unit/integration)",
];

/// Implementation order recommendation.
#[allow(dead_code)]
pub const EXECUTION_ORDER: &[&str] = &[
    "1) Config + validation + defaults",
    "2) Minimal writer (single file, JSONL)",
    "3) Split-by-decoder routing",
    "4) Hook into APRS/CW/FT8/WSPR emit sites",
    "5) Add rotation/retention",
    "6) Add tests + docs",
];
