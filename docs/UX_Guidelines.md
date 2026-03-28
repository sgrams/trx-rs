# UX Guidelines

This document captures the UI/UX design patterns, conventions, and principles observed across
the trx-rs application. It covers the web frontend, CLI interfaces, configuration wizard, API
design, and error handling.

*Last reviewed: 2026-03-28*

---

## 1. Web Frontend (trx-frontend-http)

### 1.1 Layout and Navigation

The web UI is a single-page application served from embedded assets (no build step). It uses
a **tab-based** navigation model with six top-level tabs:

| Tab | Icon | Purpose |
|---|---|---|
| **Main** | House | Primary radio control: spectrum, frequency, mode, PTT, VFO, SDR controls |
| **Bookmarks** | Bookmark | Saved frequency/mode presets with folder organisation |
| **Digital modes** | Bar chart | FT8/FT4/FT2, WSPR, CW, APRS, AIS, VDES decode tables |
| **Map** | Pin | Leaflet map for APRS/AIS/FT8 station plotting |
| **Settings** | Wrench | Scheduler, background decode, history retention |
| **About** | Info circle | Server/client/radio/audio/decoder/integration details |

Tabs use inline SVG icons with a text label below. On narrow viewports the tab bar wraps and
subtitles collapse to save space.

The **Settings** and **About** tabs each use a secondary **sub-tab bar** for further grouping
(e.g. Settings > Scheduler | Background Decode | History).

### 1.2 Theming

The UI supports **dark mode** (default) and **light mode** toggled via a header button. Theme
preference persists in `localStorage`.

Additionally, nine **colour styles** are available via a dropdown:

- Original (default), Arctic, Lime, Contrast, Neon Disco, Donald (golden-rain), Amber, Fire, Phosphor

Each style provides a full CSS custom-property override set for both dark and light variants.
Styles are applied via `data-style` and `data-theme` attributes on `<html>`.

All colours reference CSS custom properties (`--bg`, `--card-bg`, `--text`, `--accent-green`,
`--border-light`, etc.) so components never use hard-coded colour values.

### 1.3 Typography

- **Body**: `system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif`
- **Frequency display**: `DSEG14 Classic` (14-segment display font, loaded from CDN with `preload`)
- **Labels**: uppercase, 0.68-0.78 rem, `font-weight: 700`, `letter-spacing: 0.04em`
- **Section labels** use pill-shaped badges (`border-radius: 999px`) with muted text

### 1.4 Responsive Design

Six breakpoints handle layout adaptation:

| Breakpoint | Behaviour |
|---|---|
| `> 1100px` | Full width with bookmark side gutters on spectrum |
| `< 1100px` | Side bookmark panels hidden |
| `< 900px` | Card fills viewport width, reduced padding |
| `< 760px` | Tab bar wraps, controls stack vertically, safe-area-inset padding for notched devices |
| `< 640px` | Bottom-fixed tab bar (mobile), subtitles hidden, compact header |
| `< 520px` | Further compact adjustments |

Touch-specific: `@media (hover: none) and (pointer: coarse)` enlarges hit targets.

The spectrum panel hints adapt: mouse users see "Scroll to zoom / Ctrl+Scroll to tune /
Drag to pan" while touch users see "Pinch to zoom / Drag to pan".

### 1.5 Interactive Controls

- **Jog wheel**: Circular CSS-styled draggable dial for frequency tuning (skeuomorphic radial-gradient, grab cursor, shadow/inset). Plus/minus buttons flank it.
- **Step unit buttons**: Segmented button group (MHz / kHz / Hz) with `.active` highlight
- **Step scale**: 1x / 0.1x multiplier toggle
- **Frequency input**: Monospace DSEG14 font, editable `<input>` with disabled opacity fix
- **Mode selector**: `<select>` dropdown populated from rig capabilities
- **PTT / Power / Lock buttons**: Three-column grid in the transmit/power section
- **VFO picker**: Button group (horizontal on desktop, vertical stack on mobile)
- **WFM/SAM controls**: Compact labelled controls (de-emphasis, audio mode, denoise, stereo pilot flag, CCI/ACI interference bars)
- **SDR settings row**: AGC checkbox, RF/LNA gain inputs with Set buttons, noise blanker

### 1.6 Spectrum and Waterfall

The spectrum panel uses `<canvas>` elements (WebGL renderer optional) and offers:

- **Drag to pan**, **scroll to zoom**, **Ctrl+scroll to tune**
- Bandwidth edges are draggable to resize the filter
- Keyboard shortcuts: `+`/`-` zoom, arrows pan, `0` reset
- **Minimap** for orientation when zoomed
- **Resize grip** to adjust spectrum height
- Controls: bandwidth input, auto-BW, sweet-spot, peak hold (0-60s), floor (dB), range (dB), auto-level, contrast gamma slider
- **Waterfall/waveform split slider** (20%-80%, default 50/50)
- **Bookmark axis** overlays on left/right sides at wider viewports
- **Decoder overlays**: RDS station name, AIS/VDES/FT8/APRS/CW bar overlays using `aria-live="polite"`

### 1.7 Real-Time Data

- **SSE (Server-Sent Events)** on `/events` for rig state updates. Each SSE session gets a
  UUID, enabling per-tab rig selection without interfering with other tabs.
- **Named events**: `data` (state), `session` (session UUID), `channels` (virtual channels),
  `b` (spectrum bins as base64), `rds`, `vchan_rds`, `ping` (5-second heartbeat)
- **WebSocket** on `/audio` for Opus-encoded RX audio streaming
- **Connection lost banner**: `#server-lost-banner` with pulsing dot, text "trx-server
  connection lost -- waiting for reconnect", uses `aria-live="assertive"`
- **Loading state**: Centered "Initializing (rig)..." with subtitle, content hidden until ready

### 1.8 Accessibility

- All interactive elements have `aria-label` attributes
- Spectrum overlays use `aria-live="polite"` for screen reader announcements
- Connection-lost banner uses `aria-live="assertive"`
- `aria-hidden="true"` on decorative canvases and visual-only elements
- SVG icons include `aria-hidden="true"` with descriptive labels on parent buttons
- Spectrum resize grip has both `title` and `aria-label`

### 1.9 Authentication UX

When auth is enabled, an **auth gate** blocks the UI with:

- Title: "Access Required"
- Subtitle: "Enter passphrase to continue"
- Password input + Login button (green accent, full-width)
- Optional "Continue as Guest" button (shown when RX passphrase is not set)
- Error message area (red `#ff6b6b`)
- Role badge display

Two roles: **Rx** (read-only) and **Control** (full access including TX/PTT).

Session cookie: `trx_http_sid`, HttpOnly, configurable Secure and SameSite attributes.

The header shows a Login/Logout button when auth is enabled (`#header-auth-btn`).

### 1.10 Multi-Rig Support

- **Header rig switcher**: `<select>` dropdown in the top bar for switching between connected rigs
- Per-tab rig binding: each SSE session independently selects a rig via `?remote=` query parameter
- Rig state isolation: only the disconnected rig shows the connection-lost banner
- About tab shows active rig, available rigs list

---

## 2. REST API Design

### 2.1 Conventions

- **Read operations** use `GET` (e.g. `/status`, `/events`, `/decode/history`, `/rigs`, `/bookmarks`)
- **Mutations** use `POST` for actions and toggles (e.g. `/set_freq`, `/toggle_power`, `/toggle_ft8_decode`)
- **CRUD resources** use proper verbs: `GET /bookmarks`, `POST /bookmarks`, `PUT /bookmarks/{id}`,
  `DELETE /bookmarks/{id}`
- **Batch operations**: `POST /bookmarks/batch_delete`, `POST /bookmarks/batch_move`
- **Nested resources**: `/channels/{remote}/{channel_id}/subscribe`, `/scheduler/{remote}/status`
- Responses are JSON with `Content-Type: application/json`
- SSE stream uses `Content-Type: text/event-stream` with `no-cache` and `keep-alive` headers

### 2.2 Request Timeout

All rig command requests have a **15-second timeout** (`REQUEST_TIMEOUT`). If the command
doesn't complete in time, the request returns an error rather than hanging.

### 2.3 Error Responses

- `401 Unauthorized`: `{"error": "Invalid credentials"}` or `{"error": "Authentication required"}`
- `429 Too Many Requests`: `{"error": "Too many login attempts, please try again later"}`
- `404 Not Found`: Auth endpoints when auth is disabled
- `500 Internal Server Error`: Serialization failures
- Rate limiting: 10 attempts per 60-second window per IP, counter resets on successful login

### 2.4 State Enrichment

API responses merge rig state with **frontend metadata** (`FrontendMeta`) via `serde(flatten)`:

```
http_clients, rigctl_clients, audio_clients, rigctl_addr,
active_remote, remotes[], owner_callsign, owner_website_url,
owner_website_name, ais_vessel_url_base, show_sdr_gain_control,
initial_map_zoom, spectrum_coverage_margin_hz, spectrum_usable_span_ratio,
decode_history_retention_min, server_connected
```

This single-payload approach avoids extra round trips for UI configuration.

---

## 3. CLI Interface

### 3.1 Argument Style

Both `trx-server` and `trx-client` use **clap** for argument parsing with short and long flags:

```
-C, --config FILE           Path to configuration file
--print-config              Print example configuration and exit
-r, --rig NAME              Rig backend name
-l, --listen ADDR           Listen address
-p, --port NUM              Port number
```

Positional arguments are used sparingly (e.g. `RIG_ADDR` for serial/TCP address).

### 3.2 Configuration Resolution

Config files are searched in priority order:
1. Current directory: `trx-rs.toml`
2. XDG config: `~/.config/trx-rs/trx-rs.toml`
3. System: `/etc/trx-rs/trx-rs.toml`

The loaded config path is logged: `INFO Loaded configuration from /path/to/config.toml`

### 3.3 Example Config Generation

`--print-config` outputs a complete, commented TOML file to stdout with example values
(callsign `N0CALL`, coordinates `52.2297, 21.0122`). Each section has a header comment and
each field has an inline description.

### 3.4 Startup Log Sequence

Server:
```
INFO Loaded configuration from /path/to/config.toml
INFO Starting trx-server with N rig(s): [rig-names]
INFO Callsign: CALL
INFO [rig-id] Starting (rig: ft817, access: serial /dev/ttyUSB0 @ 9600 baud)
INFO Listening on 0.0.0.0:4530
```

Client:
```
INFO Loaded configuration from /path/to/config.toml
INFO Starting trx-client (remotes: [remote-names], frontends: http,rigctl)
INFO rigctl frontend for rig 'default' on 127.0.0.1:4532
```

---

## 4. Configuration Wizard (trx-configurator)

### 4.1 Interactive Mode

Uses the **dialoguer** crate for terminal prompts:

- `Select` menus for enumerated choices (config type, rig model, access type, log level)
- `Input` for free-text with defaults (callsign defaults to `N0CALL`, listen defaults to `127.0.0.1`)
- `Confirm` for yes/no questions (enable auth, set location, etc.)
- Serial port auto-detection with fallback to `/dev/ttyUSB0`

### 4.2 Non-Interactive Mode

`--defaults` generates a config file without prompts, using sensible defaults.

### 4.3 Config Validation

`--check FILE` validates an existing config file:

```
/path/to/config.toml: valid TOML
  Detected type: server
  warning: [general].log_level 'verbose' is invalid (expected: trace, debug, info, warn, error)
  1 warning(s), 0 error(s)
```

Validates: TOML syntax, unknown keys, log levels, coordinate ranges (-90..90 lat, -180..180 lon
with pair requirement), access types, port ranges (0-65535).

### 4.4 File Write Confirmation

Prompts before overwriting an existing file. Outputs `Wrote /path/to/file` on success.

---

## 5. Error Handling and User-Facing Messages

### 5.1 Error Message Conventions

- **Contextual**: Include file paths, section names, and peer addresses
  - `"Failed to parse config file /path: error details"`
  - `"Unknown rig model: X (available: ft817, ft450d, soapysdr)"`
- **Actionable**: Suggest alternatives when available
  - `"Rig model not specified. Use --rig or set [rig].model in config."`
  - `"Unknown frontend: X (available: http, rigctl, httpjson)"`
- **Structured**: Use field=value format in structured logging

### 5.2 Log Level Guidelines

| Level | Usage |
|---|---|
| `INFO` | Startup milestones, configuration loaded, listening, client connect/disconnect, decoder state changes |
| `WARN` | Non-fatal issues: command took too long, panel lock blocking, VFO priming failed, initial tune failed |
| `ERROR` | Fatal or significant failures: CAT polling errors, client errors, parse failures |

Logs suppress module targets (`with_target(false)`) for cleaner output.

### 5.3 Connection State Communication

- Server logs: `"Client connected: {peer}"`, `"Client {peer} disconnected"`, `"Client {peer} closing due to shutdown"`
- Rig task: `"[rig-id] Rig backend ready"`, `"Serial: /dev/ttyUSB0 @ 9600 baud"`
- Web UI: Connection-lost banner with reconnect indication, per-rig isolation

### 5.4 Graceful Degradation

- Startup continues after non-fatal failures: `"Initial PowerOn failed (continuing)"`
- Stream errors are deduplicated with 60-second summaries to avoid log flooding
- Lock poisoning is recovered from rather than panicking
- Unknown SSE events or lagged broadcast channels are silently skipped

---

## 6. Branding and Customisation

### 6.1 Owner Branding

Configurable via TOML and exposed via `FrontendMeta`:

- `owner_callsign` -- displayed in header subtitle and About tab
- `owner_website_url` / `owner_website_name` -- optional link in header
- `ais_vessel_url_base` -- base URL for linking AIS vessel MMSI numbers

### 6.2 UI Behaviour Configuration

- `http_show_sdr_gain_control` -- show/hide RF gain controls
- `http_initial_map_zoom` -- default map zoom level
- `http_spectrum_coverage_margin_hz` -- guard margin for spectrum center retune
- `http_spectrum_usable_span_ratio` -- fraction of spectrum span treated as usable
- `http_decode_history_retention_min` -- default history retention (per-rig overrides supported)

### 6.3 Embedded Assets

Logo and favicon are embedded at compile time via `include_bytes!`. The logo image has an
`onerror` handler to hide itself if loading fails (`this.style.display='none'`).

---

## 7. Security UX

### 7.1 Route Access Classification

Routes are classified into three tiers:

| Tier | Examples | Requirement |
|---|---|---|
| **Public** | `/`, `/index.html`, `/map`, `/auth/*`, static assets | None |
| **Read** | `/status`, `/events`, `/audio`, `/decode`, `/spectrum`, `/bookmarks` | Rx or Control role |
| **Control** | `/set_freq`, `/set_mode`, `/set_ptt`, `/toggle_power`, all other POST | Control role only |

### 7.2 Session Management

- Sessions are 128-bit random hex tokens stored in HttpOnly cookies
- Configurable TTL (default from TOML config)
- Expired sessions auto-pruned on access
- Constant-time passphrase comparison to mitigate timing attacks

### 7.3 TX Access Control

An additional `tx_access_control_enabled` flag can restrict transmit-related actions even
for Control-role users, providing an extra safety layer.

---

## 8. Virtual Channels (SDR)

Virtual channels allow SDR users to monitor multiple frequencies simultaneously:

- Channels appear in a picker row below the VFO controls
- CRUD API: `POST /channels/{remote}` to create, `DELETE` to remove, `PUT` to update freq/mode/BW
- Subscribe/unsubscribe audio per channel
- Background decode channels (hidden, no audio stream back)
- Channels auto-destroyed when out-of-bandwidth after center-frequency retune
- Channel-list changes broadcast to SSE clients via `event: channels`

---

## 9. Design Principles (Inferred)

1. **Server-rendered SPA**: All HTML/CSS/JS embedded in the binary -- zero external build tooling, no CDN dependency for core functionality (CDN used only for fonts and Leaflet maps).

2. **Progressive disclosure**: Advanced controls (WFM, SAM, SDR settings, spectrum controls) are hidden by default and revealed based on the active mode and backend type.

3. **Keyboard-first, touch-aware**: Spectrum supports full keyboard navigation alongside mouse and touch gestures. Mobile breakpoints enlarge hit targets and adapt layout.

4. **Real-time by default**: SSE + WebSocket provide sub-second state updates without polling from the browser. 5-second ping heartbeat detects stale connections.

5. **Per-tab isolation**: Each browser tab gets its own SSE session UUID and can independently select a rig, preventing cross-tab interference.

6. **Configuration over code**: UI behaviour knobs (gain visibility, map zoom, history retention, spectrum margins) are exposed as TOML config rather than requiring code changes.

7. **Graceful degradation**: The UI handles server disconnection gracefully with visible banners, and only the affected rig shows as disconnected in multi-rig setups.

8. **Defensive security defaults**: Auth disabled by default for ease of setup, but when enabled, provides role-based access, rate limiting, constant-time comparison, and HttpOnly cookies.
