# Background Decoding Scheduler

## Overview

The Background Decoding Scheduler automatically retunes the rig to pre-configured
bookmarks when no users are connected to the HTTP frontend. It runs as a background
tokio task inside `trx-frontend-http`, polling every 30 seconds.

## Modes

### Disabled (default)
Scheduler is inactive. Rig is not touched automatically.

### Grayline
Retunes around the solar terminator (day/night boundary).

The user provides:
- Station latitude and longitude (decimal degrees)
- Optional transition window width (minutes, default 20)
- Bookmark IDs for four periods:
  - **Dawn** – window around sunrise (`sunrise ± window_min/2`)
  - **Day** – after dawn until dusk
  - **Dusk** – window around sunset (`sunset ± window_min/2`)
  - **Night** – after dusk until next dawn

Period precedence (most specific wins): Dawn > Dusk > Day > Night.

If no bookmark is assigned to a period, the rig is not retuned for that period.

Sunrise/sunset is computed inline using the NOAA simplified algorithm.
Polar regions (midnight sun / polar night) fall back to Day/Night accordingly.

### TimeSpan
Retunes according to a list of user-defined time windows (UTC).

Each entry specifies:
- `start_hhmm` – start of window (e.g. 600 = 06:00 UTC)
- `end_hhmm` – end of window (e.g. 700 = 07:00 UTC)
- `bookmark_id` – bookmark to apply
- `label` – optional human-readable description

Windows that span midnight (`end_hhmm < start_hhmm`) are supported.
When multiple entries overlap, the first match (by list order) wins.

## Storage

Configuration is stored in PickleDB at `~/.config/trx-rs/scheduler.db`.

Keys: `sch:{rig_id}` → JSON `SchedulerConfig`.

## HTTP API

All read endpoints are accessible at the **Rx** role level.
Write endpoints require the **Control** role.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/scheduler/{rig_id}` | Get scheduler config for a rig |
| PUT | `/scheduler/{rig_id}` | Save scheduler config (Control only) |
| DELETE | `/scheduler/{rig_id}` | Reset config to Disabled (Control only) |
| GET | `/scheduler/{rig_id}/status` | Get last-applied bookmark and next event |

## Activation logic

Every 30 seconds the scheduler task checks:
1. `context.sse_clients.load() == 0` — no users connected
2. Active rig has a non-Disabled scheduler config
3. Current UTC time matches a scheduled window or grayline period
4. If the matching bookmark differs from `last_applied`, send `SetFreq` + `SetMode`

The scheduler **does not** revert changes when users reconnect. Bookmarks serve as
a frequency map — the user can retune manually after connecting.

## Data model (Rust)

```rust
pub enum SchedulerMode { Disabled, Grayline, TimeSpan }

pub struct GraylineConfig {
    pub lat: f64,
    pub lon: f64,
    pub transition_window_min: u32,
    pub day_bookmark_id: Option<String>,
    pub night_bookmark_id: Option<String>,
    pub dawn_bookmark_id: Option<String>,
    pub dusk_bookmark_id: Option<String>,
}

pub struct ScheduleEntry {
    pub id: String,
    pub start_hhmm: u32,
    pub end_hhmm: u32,
    pub bookmark_id: String,
    pub label: Option<String>,
}

pub struct SchedulerConfig {
    pub rig_id: String,
    pub mode: SchedulerMode,
    pub grayline: Option<GraylineConfig>,
    pub entries: Vec<ScheduleEntry>,
}
```

## UI (Scheduler tab)

A dedicated sixth tab with a clock icon.

- **Rig selector**: shows active rig (read-only).
- **Mode picker**: Disabled / Grayline / TimeSpan radio buttons.
- **Grayline section** (visible when mode = Grayline):
  - Lat/lon inputs
  - Transition window slider (5–60 min)
  - Four bookmark selectors (Dawn / Day / Dusk / Night)
- **TimeSpan section** (visible when mode = TimeSpan):
  - Table of entries with Start, End, Bookmark, Label, Remove button
  - "Add Entry" row at the bottom
- **Status card**: last applied bookmark name and timestamp.
- Save button (Control only; form is read-only for Rx users).
