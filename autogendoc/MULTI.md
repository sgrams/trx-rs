# Multi-Rig Support

This document specifies the requirements for running N simultaneous rig backends in one `trx-server` process and the protocol/config changes required to support them.

---

## Progress

> **For AI agents:** This section is the single source of truth for implementation status.
> Each task has a unique ID (e.g. `MR-01`), a status badge, a description, the files it touches, and any blocking dependencies.
>
> Status legend: `[ ]` not started · `[~]` in progress · `[x]` done · `[!]` blocked

### Foundational (parallel)

| ID | Status | Task | Files | Needs |
|----|--------|------|-------|-------|
| MR-01 | `[x]` | Add `rig_id: Option<String>` to `ClientEnvelope`; add `rig_id: Option<String>` to `ClientResponse`; add `ClientCommand::GetRigs`; add `GetRigsResponseBody` + `RigEntry`; add sentinel arm in `mapping.rs` | `src/trx-protocol/src/types.rs`, `mapping.rs`, `lib.rs` | — |
| MR-02 | `[x]` | Add `RigInstanceConfig`; add `rigs: Vec<RigInstanceConfig>` to `ServerConfig`; implement `resolved_rigs()`; extend `validate()` for unique IDs + unique audio ports | `src/trx-server/src/config.rs` | — |
| MR-03 | `[x]` | Remove four `OnceLock` statics from `audio.rs`; add `DecoderHistories { aprs, ft8, wspr }` struct + `new()`; convert history free-fns to take `&DecoderHistories`; update decoder task signatures + `run_audio_listener` | `src/trx-server/src/audio.rs` | — |
| MR-04 | `[x]` | Create `src/trx-server/src/rig_handle.rs` with `RigHandle { rig_id, rig_tx, state_rx }`; declare mod in `main.rs` | `src/trx-server/src/rig_handle.rs`, `main.rs` | — |

### Sequential

| ID | Status | Task | Files | Needs |
|----|--------|------|-------|-------|
| MR-05 | `[x]` | Add `rig_id: String` + `histories: Arc<DecoderHistories>` to `RigTaskConfig`; fix `clear_*_history` calls in `process_command` | `src/trx-server/src/rig_task.rs` | MR-03 |
| MR-06 | `[x]` | Rewrite `run_listener` to take `Arc<HashMap<String, RigHandle>>` + `default_rig_id`; route by `envelope.rig_id`; add `GetRigs` fast path; populate `rig_id` in every `ClientResponse` | `src/trx-server/src/listener.rs` | MR-01, MR-04 |
| MR-07 | `[x]` | Rewrite `main.rs` spawn loop over `resolved_rigs()`; extract `spawn_rig_audio_stack()`; per-rig pskreporter + aprsfi; build `HashMap<String, RigHandle>`; pass to `run_listener` | `src/trx-server/src/main.rs` | MR-02–06 |

### Tests

| ID | Status | Task | Files | Needs |
|----|--------|------|-------|-------|
| MR-08 | `[x]` | Config tests: `resolved_rigs()` with multi-rig TOML and legacy TOML; duplicate ID/port rejection | `src/trx-server/src/config.rs` | MR-02 |
| MR-09 | `[x]` | Protocol tests: `ClientEnvelope` absent `rig_id` parses; `rig_id` in responses; `GetRigs` round-trip; existing tests still pass | `src/trx-protocol/src/codec.rs` | MR-01 |

---

## Goals

- Run N simultaneous rig backends (SDR, transceivers, or any mix) in one server process
- Route control commands to the correct rig via `rig_id` in the JSON protocol
- Backward compatibility: single-rig configs (`[rig]`/`[audio]` at top level) continue to work unchanged
- Per-rig audio streaming on separate TCP ports
- New `GetRigs` command to enumerate all connected rigs and their states

---

## Non-Goals

- Load-balancing or failover between rigs
- Sharing a single audio port across multiple rigs (each rig keeps its own port)

---

## Architecture

```
Single [listen] port (4530)
  └─ listener.rs: Arc<HashMap<rig_id, RigHandle>>
       ├─ route by envelope.rig_id  (absent → first rig, backward compat)
       └─ GetRigs → aggregate all states

Per-rig:
  rig_task ←→ RigHandle (rig_tx + state_rx)
  audio capture → pcm_tx → decoder tasks → decode_tx
  run_audio_listener (own TCP port per rig)
  pskreporter + aprsfi tasks
```

---

## TOML Format

### Multi-rig (`[[rigs]]` array)

```toml
[general]
callsign = "W1AW"

[listen]
port = 4530

[[rigs]]
id = "hf"
[rigs.rig]
model = "ft450d"
initial_freq_hz = 14074000
[rigs.rig.access]
type = "serial"
port = "/dev/ttyUSB0"
baud = 9600
[rigs.audio]
port = 4531

[[rigs]]
id = "sdr"
[rigs.rig]
model = "soapysdr"
[rigs.rig.access]
type = "sdr"
args = "driver=rtlsdr"
[rigs.audio]
port = 4532
[rigs.sdr]
sample_rate = 1920000
```

### Legacy (flat `[rig]` + `[audio]`) — continues to work unchanged

```toml
[rig]
model = "ft817"
[rig.access]
type = "serial"
port = "/dev/ttyUSB0"
baud = 9600
[audio]
port = 4531
```

Legacy configs are synthesised into a single-element `[[rigs]]` list with `id = "default"` via `resolved_rigs()`.

---

## Protocol Wire Format

Request (`rig_id` optional; absent = first rig):
```json
{"rig_id": "hf", "cmd": "set_freq", "freq_hz": 14074000}
{"cmd": "get_state"}
```

Response (`rig_id` always present):
```json
{"success": true, "rig_id": "hf", "state": {...}}
{"success": false, "rig_id": "default", "error": "Unknown rig_id: xyz"}
```

`GetRigs` response:
```json
{"success": true, "rig_id": "server", "rigs": [
  {"rig_id": "hf",  "state": {...}},
  {"rig_id": "sdr", "state": {...}}
]}
```

---

## Validation Rules (startup)

- When `[[rigs]]` is non-empty: each `id` must be unique (case-sensitive).
- When `[[rigs]]` is non-empty: each `audio.port` must be unique.
- When `[[rigs]]` is empty: legacy flat fields are used with `id = "default"`.
- Mixing `[[rigs]]` and legacy flat `[rig]`/`[audio]` is undefined; `[[rigs]]` takes precedence.
