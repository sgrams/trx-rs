# trx-rs Manual

## What trx-rs is

`trx-rs` is a modular amateur radio control stack written in Rust. It splits
hardware access, DSP, transport, and user-facing interfaces into separate
components so a radio or SDR can be controlled locally while audio, decoding,
and remote control are exposed elsewhere on the network.

In practice, `trx-server` owns the rig or SDR backend and runs the DSP
pipeline, while `trx-client` connects to it and provides frontends such as the
web UI, JSON control, and rigctl-compatible access. The workspace also includes
protocol decoders and plugin-based extension points for adding backends and
frontends.

---

## Configuration

Both `trx-server` and `trx-client` use TOML configuration files. Use
`--print-config` to generate a fully commented example.

### File Locations

**trx-server** lookup order:
1. `--config <FILE>`
2. `./trx-server.toml`
3. `~/.trx-server.toml`
4. `~/.config/trx-rs/server.toml`
5. `/etc/trx-rs/server.toml`

**trx-client** lookup order:
1. `--config <FILE>`
2. `./trx-client.toml`
3. `~/.config/trx-rs/client.toml`
4. `/etc/trx-rs/client.toml`

CLI arguments override config file values.

### Environment Variables

- `TRX_PLUGIN_DIRS`: additional plugin directories (path-separated), used by
  both server and client.

### Server Options

#### `[general]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `callsign` | string | `"N0CALL"` | Station callsign |
| `log_level` | string | — | `trace`, `debug`, `info`, `warn`, or `error` |
| `latitude` | float | — | Station latitude (-90..90) |
| `longitude` | float | — | Station longitude (-180..180) |

`latitude` and `longitude` must be set together or both omitted.

#### `[rig]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `model` | string | — | Backend name (`ft817`, `ft450d`, `soapysdr`) |
| `initial_freq_hz` | u64 | `144300000` | Startup frequency (must be > 0) |
| `initial_mode` | string | `"USB"` | Startup mode |

#### `[rig.access]`

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | `serial`, `tcp`, or `sdr` |
| `port` | string | Serial port path (serial mode) |
| `baud` | u32 | Serial baud rate (serial mode) |
| `host` | string | Remote host (tcp mode) |
| `tcp_port` | u16 | Remote port (tcp mode) |
| `args` | string | SoapySDR device args (sdr mode, e.g. `"driver=rtlsdr"`) |

#### `[behavior]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `poll_interval_ms` | u64 | `500` | Rig polling interval |
| `poll_interval_tx_ms` | u64 | `100` | Polling interval during TX |
| `max_retries` | u32 | `3` | Connection retry limit |
| `retry_base_delay_ms` | u64 | `100` | Base retry delay |

#### `[listen]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `true` | Enable JSON TCP listener |
| `listen` | ip | `127.0.0.1` | Bind address |
| `port` | u16 | `4530` | Bind port |

#### `[listen.auth]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `tokens` | string[] | `[]` | Allowed auth tokens (empty = no auth) |

#### `[audio]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `true` | Enable audio streaming |
| `listen` | ip | `127.0.0.1` | Bind address |
| `port` | u16 | `4531` | Bind port |
| `rx_enabled` | bool | `true` | Enable RX audio |
| `tx_enabled` | bool | `true` | Enable TX audio |
| `device` | string | — | CPAL device name (empty = default) |
| `sample_rate` | u32 | `48000` | Sample rate (8000–192000) |
| `channels` | u8 | `1` | Channel count (1 or 2) |
| `frame_duration_ms` | u16 | `20` | Opus frame duration (3, 5, 10, 20, 40, 60) |
| `bitrate_bps` | u32 | `24000` | Opus bitrate |

When audio is enabled, at least one of `rx_enabled` or `tx_enabled` must be true.

#### `[sdr]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `sample_rate` | u32 | `1920000` | IQ capture rate in Hz |
| `bandwidth` | u32 | `1500000` | Hardware IF filter bandwidth in Hz |
| `center_offset_hz` | i64 | `100000` | Offset from dial to avoid DC spur |

#### `[sdr.gain]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `mode` | string | `"auto"` | `"auto"` (hardware AGC) or `"manual"` |
| `value` | f64 | `30.0` | Gain in dB (manual mode only) |

#### `[sdr.squelch]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Enable software squelch |
| `threshold_db` | f32 | `-65.0` | Open threshold in dBFS (-140..0) |
| `hysteresis_db` | f32 | `3.0` | Close hysteresis in dB (0..40) |
| `tail_ms` | u32 | `180` | Tail hold time in ms (0..10000) |

#### `[[sdr.channels]]`

Defines virtual receiver channels within the wideband IQ stream. The first
channel is the primary channel (controlled by `set_freq`/`set_mode`).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `id` | string | `""` | Human-readable label |
| `offset_hz` | i64 | `0` | Frequency offset from dial |
| `mode` | string | `"auto"` | Demod mode (`auto`, `LSB`, `USB`, `CW`, `AM`, `FM`, `WFM`, etc.) |
| `audio_bandwidth_hz` | u32 | `3000` | Post-demod audio bandwidth |
| `fir_taps` | usize | `64` | FIR filter tap count |
| `cw_center_hz` | u32 | `700` | CW tone centre frequency |
| `wfm_bandwidth_hz` | u32 | `75000` | WFM pre-demod filter bandwidth |
| `decoders` | string[] | `[]` | Decoder IDs for this channel (`ft8`, `wspr`, `aprs`, `cw`) |
| `stream_opus` | bool | `false` | Stream this channel's audio to clients |

Notes:
- Each decoder ID may appear in at most one channel.
- At most one channel may set `stream_opus = true`.
- Channel IF constraint: `|center_offset_hz + offset_hz| < sample_rate / 2`.

#### `[pskreporter]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Enable PSKReporter uplink |
| `host` | string | `"report.pskreporter.info"` | Server host |
| `port` | u16 | `4739` | Server port |
| `receiver_locator` | string | — | Maidenhead grid (derived from lat/lon if omitted) |

#### `[aprsfi]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Enable APRS-IS IGate |
| `host` | string | `"rotate.aprs.net"` | Server host |
| `port` | u16 | `14580` | Server port |
| `passcode` | i32 | `-1` | APRS-IS passcode (-1 = auto from callsign) |

Notes:
- `[general].callsign` must be non-empty when enabled.
- Only APRS packets with valid CRC are forwarded.
- Reconnects with exponential backoff (1 s → 60 s) on TCP errors.

#### `[decode_logs]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Enable decoder logging |
| `dir` | string | `"$XDG_DATA_HOME/trx-rs/decoders"` | Log directory |
| `aprs_file` | string | `"TRXRS-APRS-%YYYY%-%MM%-%DD%.log"` | APRS log filename |
| `cw_file` | string | `"TRXRS-CW-%YYYY%-%MM%-%DD%.log"` | CW log filename |
| `ft8_file` | string | `"TRXRS-FT8-%YYYY%-%MM%-%DD%.log"` | FT8 log filename |
| `wspr_file` | string | `"TRXRS-WSPR-%YYYY%-%MM%-%DD%.log"` | WSPR log filename |

Files are appended in JSON Lines format. Supported date tokens: `%YYYY%`,
`%MM%`, `%DD%` (UTC).

#### Multi-Rig Configuration

Use `[[rigs]]` arrays instead of the flat `[rig]` section for multi-rig setups:

```toml
[[rigs]]
id    = "ft817_0"
name  = "HF Transceiver"
[rigs.rig]
model = "ft817"
[rigs.rig.access]
type  = "serial"
path  = "/dev/ttyUSB0"
baud  = 9600

[[rigs]]
id    = "sdr_0"
name  = "VHF/UHF SDR"
[rigs.rig]
model = "soapysdr"
[rigs.rig.access]
type  = "sdr"
args  = "driver=rtlsdr"
```

When `[[rigs]]` is present it takes priority over the flat `[rig]` section.
Rigs without an explicit `id` get auto-generated IDs like `ft817_0`, `soapysdr_1`.

### Client Options

#### `[general]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `callsign` | string | `"N0CALL"` | Station callsign |
| `log_level` | string | — | `trace`, `debug`, `info`, `warn`, or `error` |

#### `[remote]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | — | Server address (e.g. `localhost:4530`) |
| `poll_interval_ms` | u64 | `750` | State poll interval |

#### `[remote.auth]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `token` | string | — | Auth token (must not be empty if set) |

#### `[frontends.http]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `true` | Enable web UI |
| `listen` | ip | `127.0.0.1` | Bind address |
| `port` | u16 | `8080` | Bind port |

#### `[frontends.rigctl]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Enable Hamlib rigctl |
| `listen` | ip | `127.0.0.1` | Bind address |
| `port` | u16 | `4532` | Bind port |

#### `[frontends.http_json]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `true` | Enable JSON-over-TCP |
| `listen` | ip | `127.0.0.1` | Bind address |
| `port` | u16 | `0` | Bind port (0 = ephemeral) |
| `auth.tokens` | string[] | `[]` | Allowed auth tokens |

#### `[frontends.audio]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `true` | Enable audio client |
| `server_port` | u16 | `4531` | Server audio port |
| `bridge.enabled` | bool | `false` | Enable local CPAL audio bridge |
| `bridge.rx_output_device` | string | — | Local playback device |
| `bridge.tx_input_device` | string | — | Local capture device |
| `bridge.rx_gain` | float | `1.0` | RX playback gain |
| `bridge.tx_gain` | float | `1.0` | TX capture gain |

The bridge is intended for WSJT-X integration via virtual audio devices (ALSA
loopback on Linux, BlackHole on macOS).

### CLI Override Summary

**trx-server:**
`--config`, `--print-config`, `--rig`, `--access`, `--callsign`, `--listen`,
`--port`. SDR options are file-only.

**trx-client:**
`--config`, `--print-config`, `--url`, `--token`, `--poll-interval`,
`--frontend`, `--http-listen`, `--http-port`, `--rigctl-listen`,
`--rigctl-port`, `--http-json-listen`, `--http-json-port`, `--callsign`.

---

## Authentication

The HTTP frontend supports optional passphrase-based authentication with two
roles:

- **rx** — read-only access (monitoring, audio, decode streams)
- **control** — full access (frequency, mode, PTT, and all settings)

### Configuration

```toml
[frontends.http.auth]
enabled = false
rx_passphrase = "rx-only-passphrase"
control_passphrase = "full-control-passphrase"
tx_access_control_enabled = true
session_ttl_min = 480
cookie_secure = false      # true if served via HTTPS
cookie_same_site = "Lax"   # Strict|Lax|None
```

When `enabled = false` (the default), all auth is bypassed and the UI behaves
as before. When enabled, at least one passphrase must be set.

### Behaviour

- On login, the server issues an `HttpOnly` session cookie.
- Sessions are in-memory; a server restart invalidates all sessions.
- Rate limiting is applied per IP to mitigate brute-force attempts.
- When `tx_access_control_enabled = true`, TX/PTT controls are hidden and
  rejected for unauthenticated or `rx`-role users.

### Routes

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/auth/login` | POST | Submit `{ "passphrase": "..." }` |
| `/auth/logout` | POST | Clear session |
| `/auth/session` | GET | Check current session/role |

Protected routes require at least `rx` role. Control routes (set frequency,
mode, PTT, etc.) require `control` role.

### Frontend Flow

1. On load, the UI calls `/auth/session`.
2. If unauthenticated, a login screen is shown.
3. On successful login, the normal UI loads.
4. `rx` users see a read-only interface; `control` users get full controls.
5. If a session expires mid-use, streams stop and the login screen returns.

### Transport Security

There is no built-in TLS. For remote access, place trx-rs behind a
TLS-terminating reverse proxy (nginx, Caddy) and set `cookie_secure = true`.

---

## Background Decoding Scheduler

The scheduler automatically retunes the rig to pre-configured bookmarks when no
users are connected to the HTTP frontend. It runs as a background task inside
`trx-frontend-http`, polling every 30 seconds.

### Modes

#### Disabled (default)

Scheduler is inactive. The rig is not touched automatically.

#### Grayline

Retunes around the solar terminator (day/night boundary).

The user provides:
- Station latitude and longitude (decimal degrees)
- Optional transition window width (minutes, default 20)
- Bookmark IDs for four periods:
  - **Dawn** — window around sunrise (`sunrise ± window_min/2`)
  - **Day** — after dawn until dusk
  - **Dusk** — window around sunset (`sunset ± window_min/2`)
  - **Night** — after dusk until next dawn

Period precedence (most specific wins): Dawn > Dusk > Day > Night.

If no bookmark is assigned to a period, the rig is not retuned for that period.

Sunrise/sunset is computed inline using the NOAA simplified algorithm. Polar
regions (midnight sun / polar night) fall back to Day/Night accordingly.

#### TimeSpan

Retunes according to a list of user-defined time windows (UTC).

Each entry specifies:
- `start_hhmm` — start of window (e.g. 600 = 06:00 UTC)
- `end_hhmm` — end of window (e.g. 700 = 07:00 UTC)
- `bookmark_id` — bookmark to apply
- `label` — optional human-readable description

Windows that span midnight (`end_hhmm < start_hhmm`) are supported. When
multiple entries overlap, the first match (by list order) wins.

### Storage

Configuration is stored in PickleDB at `~/.config/trx-rs/scheduler.db`.

Keys: `sch:{rig_id}` → JSON `SchedulerConfig`.

### HTTP API

All read endpoints are accessible at the **Rx** role level. Write endpoints
require the **Control** role.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/scheduler/{rig_id}` | Get scheduler config for a rig |
| PUT | `/scheduler/{rig_id}` | Save scheduler config (Control only) |
| DELETE | `/scheduler/{rig_id}` | Reset config to Disabled (Control only) |
| GET | `/scheduler/{rig_id}/status` | Get last-applied bookmark and next event |

### Activation Logic

Every 30 seconds the scheduler task checks:
1. No SSE clients connected
2. Active rig has a non-Disabled scheduler config
3. Current UTC time matches a scheduled window or grayline period
4. If the matching bookmark differs from last applied, send `SetFreq` + `SetMode`

The scheduler does not revert changes when users reconnect.

### Web UI

A dedicated tab with a clock icon provides:
- Rig selector (read-only, shows active rig)
- Mode picker: Disabled / Grayline / TimeSpan
- Grayline section: lat/lon inputs, transition window slider, four bookmark selectors
- TimeSpan section: table of entries with start/end times, bookmark, label
- Status card: last applied bookmark name and timestamp
- Save button (Control role only)

---

## SDR Noise Blanker

The noise blanker suppresses impulse noise (clicks, pops, ignition interference)
on raw IQ samples before any mixing or filtering takes place. It works by
tracking a running RMS level of the signal and replacing any sample whose
magnitude exceeds **threshold x RMS** with the last known clean sample.

### Configuration (server-side)

The noise blanker is configured per rig. In a multi-rig setup each
`[[rigs]]` entry has its own `[rigs.sdr.noise_blanker]` section:

```toml
[[rigs]]
id = "hf"

[rigs.rig]
type = "sdr"

[rigs.sdr.noise_blanker]
enabled = true
threshold = 10.0     # 1 – 100; lower = more aggressive blanking
```

For the legacy single-rig (flat) config the path is `[sdr.noise_blanker]`:

```toml
[sdr.noise_blanker]
enabled = true
threshold = 10.0
```

| Field       | Type  | Default | Range   | Description |
|-------------|-------|---------|---------|-------------|
| `enabled`   | bool  | false   | —       | Turn the noise blanker on or off. |
| `threshold` | float | 10.0    | 1 – 100 | Multiplier applied to the running RMS. A sample whose magnitude exceeds this multiple is replaced. Lower values blank more aggressively; higher values only catch strong impulses. |

The noise blanker is off by default.

### Choosing a threshold

The threshold controls how aggressively the blanker suppresses impulses.
A value of **N** means: blank any sample whose magnitude exceeds **N times**
the running average signal level.

| Threshold | Behavior | Use case |
|-----------|----------|----------|
| 3 – 5    | Very aggressive — blanks frequently | Dense impulse noise (motors, power lines, LED drivers nearby) |
| 8 – 12   | Moderate — catches clear spikes without touching normal signals | Typical HF conditions with occasional ignition or switching noise |
| 15 – 25  | Conservative — only blanks strong impulses well above the noise floor | Light interference, or when you want minimal artifacts on weak signals |
| 30 – 100 | Very light — rarely triggers | Faint, infrequent clicks; mostly a safety net |

**Start at 10** (the default) and adjust while listening:

- If impulse noise is still audible, lower the threshold.
- If weak signals sound choppy or distorted, raise it — the blanker may be
  mistaking signal peaks for noise.
- On bands with steady atmospheric noise (e.g. 160 m / 80 m), a threshold of
  **5 – 8** usually works well.
- On quieter VHF/UHF bands where the noise floor is low, values of **15 – 25**
  avoid false triggers from strong signals.

### Web UI

When the server reports noise-blanker support, two controls appear in the
**SDR Settings** row of the web interface:

- **Noise Blanker** checkbox — enables or disables the blanker in real time.
- **NB Threshold** number input (1–100) with a **Set** button — adjusts the
  detection threshold. Press Enter or click Set to apply.

Both controls stay hidden until the server sends filter state containing NB
fields, so they only appear when connected to an SDR backend.

### HTTP API

```
POST /set_sdr_noise_blanker?enabled=true&threshold=10
```

| Parameter   | Type   | Required | Description |
|-------------|--------|----------|-------------|
| `enabled`   | bool   | yes      | `true` or `false` |
| `threshold` | float  | yes      | Value between 1 and 100 |

### How it works

The blanker runs on every IQ block (4096 samples) *before* the mixer stage in
the DSP pipeline:

1. For each sample, compute magnitude² (`re² + im²`).
2. Compare against `threshold² × mean_sq` (the exponentially-smoothed running
   mean of magnitude²).
3. If the sample exceeds the threshold, replace it with the previous clean
   sample.
4. Otherwise, update the running mean with smoothing factor α = 1/128 and store
   the sample as the last clean value.

Because the blanker operates on raw IQ before frequency translation, it removes
impulse noise across the entire captured bandwidth regardless of the tuned
channel offset.
