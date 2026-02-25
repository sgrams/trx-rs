# UI Capability Gating

This document specifies how `trx-client`'s HTTP frontend adapts its controls to the capabilities of the connected rig backend. Devices such as SDR receivers expose filter controls but not TX controls; traditional transceivers are the reverse.

---

## Progress

> **For AI agents:** This section is the single source of truth for implementation status.
> Each task has a unique ID (e.g. `UC-01`), a status badge, a description, the files it touches, and any blocking dependencies.
>
> Status legend: `[ ]` not started · `[~]` in progress · `[x]` done · `[!]` blocked

### Foundational (parallel)

| ID | Status | Task | Files | Needs |
|----|--------|------|-------|-------|
| UC-01 | `[x]` | Extend `RigCapabilities` with `tx`, `tx_limit`, `vfo_switch`, `filter_controls`, `signal_meter` bool flags | `src/trx-core/src/rig/state.rs` | — |
| UC-02 | `[x]` | Update capability declarations in all backends to set new flags | `src/trx-server/trx-backend/trx-backend-ft817/src/lib.rs`, `trx-backend-ft450d/src/lib.rs`, `trx-backend-soapysdr/src/lib.rs` | UC-01 |
| UC-03 | `[x]` | Add `RigFilterState` struct; add `filter: Option<RigFilterState>` to `RigSnapshot`; populate from SDR rig state | `src/trx-core/src/rig/state.rs`, `src/trx-server/trx-backend/trx-backend-soapysdr/src/lib.rs` | — |
| UC-04 | `[x]` | Add `SetBandwidth`, `SetFirTaps` to `ClientCommand`; add mapping arms; update `rig_task.rs` to dispatch them | `src/trx-protocol/src/types.rs`, `mapping.rs`, `src/trx-server/src/rig_task.rs` | UC-03 |

### HTTP layer

| ID | Status | Task | Files | Needs |
|----|--------|------|-------|-------|
| UC-05 | `[x]` | Add `/set_bandwidth` and `/set_fir_taps` HTTP endpoints | `src/trx-client/trx-frontend/trx-frontend-http/src/api.rs` | UC-04 |

### Frontend

| ID | Status | Task | Files | Needs |
|----|--------|------|-------|-------|
| UC-06 | `[x]` | Read `state.info.capabilities` on each SSE event; toggle visibility of TX controls, meter rows, VFO button, lock button | `assets/web/app.js` | UC-01, UC-02 |
| UC-07 | `[x]` | Add "Filters" control panel (bandwidth, FIR taps, CW tone Hz); show only when `capabilities.filter_controls` | `assets/web/index.html`, `assets/web/app.js` | UC-05, UC-06 |

### Tests

| ID | Status | Task | Files | Needs |
|----|--------|------|-------|-------|
| UC-08 | `[x]` | Unit tests: SDR backend declares `tx=false`, `filter_controls=true`; FT-817/450D declare `tx=true`, `filter_controls=false` | `src/trx-server/trx-backend/trx-backend-soapysdr/src/lib.rs`, `trx-backend-ft817`, `trx-backend-ft450d` | UC-02 |
| UC-09 | `[x]` | Protocol round-trip test: `RigSnapshot` serialises `filter` field when `Some`, omits it when `None` | `src/trx-protocol/src/codec.rs` or `types.rs` | UC-03 |

---

## Goals

- All UI control groups are **shown/hidden purely from `RigCapabilities`** flags received in the initial `GET /status` and each SSE `status` event — no hard-coding per model name
- SDR backends show filter controls (bandwidth, FIR taps, CW tone); hide TX controls (PTT, power, TX limit, TX meters, TX audio)
- Transceiver backends show TX controls; hide filter controls
- Adding a new backend requires only setting the right capability flags — no frontend changes

---

## Non-Goals

- Per-channel filter control (multi-channel SDR tuning) — out of scope; only the primary channel is exposed here
- Dynamic capability changes at runtime (capability flags are set once at rig init and treated as static)
- Changing the rigctl or http-json frontends (HTTP frontend only)

---

## Capability Flags

### New flags added to `RigCapabilities` (UC-01)

| Flag | Type | Meaning |
|------|------|---------|
| `tx` | `bool` | Backend supports transmit: PTT, power on/off, TX meters, TX audio |
| `tx_limit` | `bool` | Backend supports `get_tx_limit` / `set_tx_limit` |
| `vfo_switch` | `bool` | Backend supports `toggle_vfo` |
| `filter_controls` | `bool` | Backend supports runtime filter adjustment (bandwidth, FIR taps) |
| `signal_meter` | `bool` | Backend returns a meaningful RX signal strength value |

Existing flags `lock` and `lockable` are unchanged.

### Backend declarations (UC-02)

| Backend | `tx` | `tx_limit` | `vfo_switch` | `filter_controls` | `signal_meter` | `lock`/`lockable` |
|---------|------|-----------|--------------|-------------------|----------------|-------------------|
| FT-817 | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ / ✓ |
| FT-450D | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ / ✓ |
| SoapySDR | ✗ | ✗ | ✗ | ✓ | ✓ | ✗ / ✗ |

---

## Filter State

### `RigFilterState` struct (UC-03)

Added to `trx-core/src/rig/state.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigFilterState {
    pub bandwidth_hz: u32,      // Audio bandwidth of primary channel
    pub fir_taps: u32,          // FIR filter tap count
    pub cw_center_hz: u32,      // CW tone centre frequency (audio domain)
}
```

Added to `RigSnapshot`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub filter: Option<RigFilterState>,
```

The SDR backend populates this from the primary channel's live DSP state. All other backends leave it `None`.

---

## New Protocol Commands

### `ClientCommand` additions (UC-04)

```rust
SetBandwidth { bandwidth_hz: u32 },
SetFirTaps   { taps: u32 },
```

`SetCwToneHz` already exists and is reused.

### Mapping (UC-04)

```rust
ClientCommand::SetBandwidth { bandwidth_hz } =>
    RigCommand::SetBandwidth(bandwidth_hz),
ClientCommand::SetFirTaps { taps } =>
    RigCommand::SetFirTaps(taps),
```

The SDR backend applies changes to the live DSP chain immediately. Other backends return `RigError::not_supported(...)`.

---

## New HTTP Endpoints (UC-05)

| Endpoint | Method | Query param | Action |
|----------|--------|-------------|--------|
| `/set_bandwidth` | POST | `hz: u32` | Sets primary channel audio bandwidth |
| `/set_fir_taps` | POST | `taps: u32` | Sets primary channel FIR tap count |

---

## Frontend Visibility Map (UC-06, UC-07)

| UI element / group | Shown when |
|--------------------|-----------|
| PTT button | `capabilities.tx` |
| Power button | `capabilities.tx` |
| TX meters (power bar, SWR bar) | `capabilities.tx && state.status.tx_en` |
| TX Limit row | `capabilities.tx_limit` |
| TX Audio toggle + volume | `capabilities.tx` |
| VFO selector buttons | `capabilities.vfo_switch` |
| Lock button | `capabilities.lock` |
| Signal meter | `capabilities.signal_meter` |
| Filters panel | `capabilities.filter_controls` |

Visibility is applied in a single `applyCapabilities(caps)` function called from the SSE `status` handler, using `element.classList.toggle('hidden', !condition)`.

### Filter panel layout (UC-07)

```
┌─ Filters ──────────────────────────────────┐
│  Bandwidth  [──────●──────] 3000 Hz        │
│  FIR taps   [32 ▾] (32 / 64 / 128 / 256)  │
│  CW tone    [──●───────────] 700 Hz        │
└────────────────────────────────────────────┘
```

Each control dispatches to its REST endpoint on `change`/`input` (debounced 200 ms). The panel is hidden by default (`class="hidden"`) and revealed when `capabilities.filter_controls` is set.

---

## Implementation Notes

- `applyCapabilities()` must run **before** the first paint (call it synchronously on the initial `/status` response, not only on SSE events) to avoid layout flash of unsupported controls.
- `hidden` CSS class should set `display: none` and `aria-hidden: true`.
- The existing `set_cw_tone` endpoint and CW decoder panel remain in the CW decoder tab — they are decoder settings, not filter settings. The Filters panel bandwidth/taps apply to the DSP chain; CW tone moves to both places or is de-duplicated in a follow-up.
- If a future backend supports TX but not `tx_limit`, only the TX Limit row is hidden; PTT remains.
