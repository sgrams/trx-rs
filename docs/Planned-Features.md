# Planned Features

## Recorder

The recorder captures the demodulated audio stream alongside associated metadata (FFT data, decoded signals, rig state) into a structured session on disk, with full playback and seeking support from within the application.

### Requirements

| ID | Description |
|----|-------------|
| REQ-REC-001 | When the user starts recording, the system shall record the currently demodulated audio stream. |
| REQ-REC-002 | When recording audio, the system shall store the recording in OPUS format. |
| REQ-REC-003 | While recording audio, the system shall automatically detect whether the recording should be stored in mono or stereo and select the appropriate format. |
| REQ-REC-004 | While recording is active, the system shall simultaneously record FFT data and all currently visible decoded elements, including APRS and FT8. |
| REQ-REC-005 | While recording metadata, the system shall store FFT data and decoded signal data in a structured data file format. |
| REQ-PLAY-001 | Where recorded sessions exist, the system shall allow playback of recordings from within the same application. |
| REQ-PLAY-002 | During playback, the system shall allow the user to seek to any position in the recording. |
| REQ-SYNC-001 | The system shall maintain time synchronization between the audio recording and the associated data file with at least one-second resolution. |
| REQ-REC-006 | While recording is active, the system shall allow the current cursor position to be stored. |

---

### Architecture

#### New Crate: `trx-recorder`

A new crate `src/trx-server/trx-recorder/` handles all record and playback logic. It is a library crate consumed by `trx-server`.

```
src/trx-server/
  trx-recorder/
    src/
      lib.rs          # Public API: RecorderHandle, start_recorder_task()
      session.rs      # RecordingSession: file management, open/close/finalise
      writer.rs       # AudioWriter: PCM → Opus encoder
      data_file.rs    # DataFileWriter: structured JSON Lines data track
      index.rs        # SeekIndex: time → byte-offset table for audio seeking
      playback.rs     # PlaybackEngine: file → PCM broadcast for clients
      config.rs       # RecorderConfig (serde, derives Default)
```

#### Integration Points in `trx-server`

| Source | What is tapped | How |
|--------|---------------|-----|
| `audio.rs` `pcm_tx` | Raw demodulated PCM frames | New `broadcast::Receiver<Vec<f32>>` subscriber |
| `audio.rs` spectrum broadcast | FFT/spectrum frames per `RigState.spectrum` | New subscriber on the spectrum watch channel |
| `audio.rs` decoded-message broadcast | FT8, WSPR, CW, APRS, FT4, FT2, APRS-HF frames | New `broadcast::Receiver<DecodedMessage>` subscriber |
| `rig_task.rs` state watch | Frequency/mode/PTT changes | `watch::Receiver<RigState>` clone |
| New `RecorderCommand` enum | Start, Stop, MarkCursor | Injected into the existing command pipeline |

No existing code paths are modified beyond:
1. Passing a `RecorderHandle` (cheap `Arc` wrapper) into the audio and rig tasks.
2. Adding `RecorderCommand` variants to the command enum (alongside existing `SetFreq`, `SetMode`, etc.).
3. Adding a `[recorder]` section to `ServerConfig`.

---

### Session Layout on Disk

Each recording is a **session directory** named by UTC start time and opening rig state:

```
<output_dir>/
  20260317T142301Z_14074000_USB/
    audio.opus
    data.jsonl          # structured event log (see below)
    index.bin           # seek index: sorted table of (offset_ms u64, audio_byte u64)
```

`output_dir` defaults to `~/.local/share/trx-rs/recordings`.

#### Audio File (REQ-REC-001, REQ-REC-002, REQ-REC-003)

- **Format**: Opus, using the `opus` crate (already a workspace dependency via `trx-backend-soapysdr`). Seek index (`index.bin`) provides byte → time mapping.
- **Channel count**: determined at session open from `AudioConfig.channels`. If `channels == 1` → mono; if `channels == 2` → stereo. Written into the file header and recorded in the session's first data event.
- **Sample rate**: preserved from `AudioConfig.sample_rate` (default 48 000 Hz).

#### Data File (REQ-REC-004, REQ-REC-005)

`data.jsonl` — one JSON object per line, each with a required `offset_ms` field giving the millisecond offset from session start (satisfies REQ-SYNC-001 at ≥1 s resolution):

```jsonl
{"offset_ms":0,"type":"session_start","freq_hz":14074000,"mode":"USB","channels":1,"sample_rate":48000,"format":"opus"}
{"offset_ms":1000,"type":"rig_state","freq_hz":14074000,"mode":"USB","ptt":false}
{"offset_ms":2000,"type":"fft","bins_db":[-90.1,-88.4,...]}
{"offset_ms":3412,"type":"ft8","snr_db":-12,"dt_s":0.3,"freq_hz":14074350,"message":"CQ W5XYZ EN34"}
{"offset_ms":4100,"type":"aprs","from":"W5XYZ-9","to":"APRS","path":"WIDE1-1","info":"!3351.00N/09722.00W-"}
{"offset_ms":5000,"type":"cursor","label":"interesting QSO"}
{"offset_ms":61000,"type":"session_end"}
```

Supported `type` values:

| Type | Source | Cadence |
|------|--------|---------|
| `session_start` | recorder | once, at open |
| `session_end` | recorder | once, at close |
| `rig_state` | `watch::Receiver<RigState>` change | on change |
| `fft` | spectrum data from `RigState.spectrum` | ≤1 Hz (configurable, default 1 s) |
| `ft8` / `ft4` / `ft2` / `wspr` | `DecodedMessage` broadcast | on decode event |
| `aprs` / `aprs_hf` | `DecodedMessage` broadcast | on decode event |
| `cw` | `DecodedMessage` broadcast | on decode event |
| `cursor` | `RecorderCommand::MarkCursor { label }` | on user request |

#### Seek Index (REQ-PLAY-002)

`index.bin` is a flat binary table of 16-byte records written every `index_interval_ms` (default 1 000 ms):

```
[offset_ms: u64 LE][audio_byte_offset: u64 LE] ...
```

At playback seek time, binary search on `offset_ms` locates the nearest audio frame boundary, enabling random-access playback without full file scan.

---

### RecorderConfig

Added to `ServerConfig` under `[recorder]`:

```toml
[recorder]
enabled = false
output_dir = "~/.local/share/trx-rs/recordings"
opus_bitrate_bps = 32000
fft_record_interval_ms = 1000
index_interval_ms = 1000
max_session_duration_s = 3600   # auto-split at 1 h; 0 = unlimited
```

---

### Command API

New variants added to the existing command enum (handled in `rig_task.rs`):

```rust
StartRecording,
StopRecording,
MarkCursor { label: String },
```

These are exposed via:
- **HTTP frontend**: `POST /api/recorder/start`, `POST /api/recorder/stop`, `POST /api/recorder/cursor`
- **http-json frontend**: same commands as JSON messages

---

### Playback Engine (REQ-PLAY-001, REQ-PLAY-002)

`PlaybackEngine` opens a session directory and:

1. Reads `audio.opus` and decodes PCM frames in real time.
2. Publishes decoded PCM frames onto a `broadcast::Sender<Vec<f32>>` — the **same channel type** as the live `pcm_tx`, so existing decoder tasks and audio-streaming clients receive playback data transparently.
3. Replays `data.jsonl` events on their original `offset_ms` timestamps, injecting them into the `DecodedMessage` broadcast so the HTTP frontend displays historic decodes during playback.
4. For seek: binary-searches `index.bin` to find the audio byte offset, then replays data events from the same point.

The playback state machine has two modes, switched by a new `RigState.playback` field:

```rust
pub enum PlaybackState {
    Live,
    Playing { session: String, offset_ms: u64 },
    Paused { session: String, offset_ms: u64 },
}
```

While `PlaybackState` is not `Live`, the server suppresses live hardware polling and PCM capture to avoid mixing live and playback audio.

---

### Time Synchronisation (REQ-SYNC-001)

All timestamps use a single `session_epoch: std::time::Instant` captured at `StartRecording`. Every PCM frame, every data event, and every seek-index entry is stamped as `(Instant::now() - session_epoch).as_millis() as u64`. This gives sub-millisecond internal precision; the requirement of ≥1 s resolution is met by orders of magnitude.

Wall-clock UTC is embedded only in `session_start` (`wall_clock_utc`) and in the session directory name, providing absolute time anchoring without depending on system clock monotonicity for sync.

---

### Implementation Phases

#### Phase 1 — Audio recording (REQ-REC-001, REQ-REC-002, REQ-REC-003)

1. Add `trx-recorder` crate skeleton; `RecorderConfig`; `RecorderHandle`.
2. Implement `AudioWriter` with Opus output.
3. Subscribe `AudioWriter` to `pcm_tx` in `audio.rs`; open session on `StartRecording` command.
4. Auto-detect channel count from `AudioConfig.channels`.

#### Phase 2 — Metadata recording (REQ-REC-004, REQ-REC-005, REQ-SYNC-001)

1. Implement `DataFileWriter`; define full event schema.
2. Subscribe to `DecodedMessage` broadcast; fan-in all decoder types.
3. Subscribe to state watch; emit `rig_state` events on freq/mode change.
4. Emit `fft` events at configured interval from spectrum data.
5. Write `SeekIndex` in parallel with audio.

#### Phase 3 — Cursor (REQ-REC-006)

1. Add `MarkCursor` command + HTTP endpoint.
2. Write `cursor` event to `data.jsonl` with current `offset_ms`.

#### Phase 4 — Playback (REQ-PLAY-001, REQ-PLAY-002)

1. Implement `PlaybackEngine`; Opus decode + PCM broadcast.
2. Add `PlaybackState` to `RigState`; suppress live capture during playback.
3. Implement seek via `index.bin` binary search.
4. Replay `data.jsonl` events; feed into `DecodedMessage` broadcast.
5. Expose start/stop/seek endpoints in `trx-frontend-http`.

---

### Dependencies to Add

| Crate | Use | Already present? |
|-------|-----|-----------------|
| `opus` | Opus encode/decode | Yes (via trx-backend-soapysdr) |
| `serde_json` | data.jsonl serialisation | Yes |
| `tokio::fs` | async file I/O | Yes |

---

### Open Questions

1. **Playback isolation**: Should playback be exclusive (block all CAT commands) or concurrent? Initial design blocks CAT polling; revisit if users need to change frequency during playback.
2. **Session listing API**: The HTTP frontend needs an endpoint to enumerate sessions (`GET /api/recorder/sessions`). Schema TBD in Phase 4.
3. **Storage limits**: `max_session_duration_s` auto-splits sessions; a `max_total_size_gb` housekeeping option may be needed but is out of scope for initial phases.

---

## Configurator Helper

An interactive CLI tool that guides users through creating configuration files
for trx-rs. Instead of editing TOML by hand, the user answers prompts and the
tool generates valid, commented configuration files.

### Overview

The configurator is a standalone Rust binary (`trx-configurator`) that reuses
the existing config structs from `trx-app`, `trx-server`, and `trx-client`. It
walks the user through a question-driven flow, validates inputs against the same
rules the binaries use at startup, and writes one or more of:

- `trx-server.toml` — server configuration
- `trx-client.toml` — client configuration
- `trx-rs.toml` — combined server + client configuration

The user chooses which file(s) to generate.

### Requirements

| ID | Description |
|----|-------------|
| REQ-CFG-001 | The tool shall interactively prompt the user for configuration values. |
| REQ-CFG-002 | The tool shall generate `trx-server.toml`, `trx-client.toml`, or `trx-rs.toml` per user selection. |
| REQ-CFG-003 | The tool shall validate all inputs using the same validation logic as the server and client binaries. |
| REQ-CFG-004 | The tool shall write commented TOML with descriptions of each field. |
| REQ-CFG-005 | The tool shall detect connected serial devices and offer them for rig access configuration. |
| REQ-CFG-006 | The tool shall detect available SoapySDR devices and offer them for SDR backend configuration. |
| REQ-CFG-007 | The tool shall support a non-interactive mode that generates a default config file. |
| REQ-CFG-008 | The tool shall not overwrite existing files without confirmation. |

### Architecture

#### New Crate: `trx-configurator`

A new binary crate at `src/trx-configurator/` that depends on `trx-app` for
config types and validation.

```
src/trx-configurator/
  src/
    main.rs          # CLI entry point, mode selection
    prompts.rs       # Interactive prompt helpers (with defaults, validation)
    detect.rs        # Hardware detection (serial ports, SoapySDR devices)
    writer.rs        # TOML serialisation with inline comments
```

#### Flow

```
trx-configurator
  ├── What would you like to generate?
  │     [ ] trx-server.toml
  │     [ ] trx-client.toml
  │     [ ] trx-rs.toml (combined)
  │
  ├── (if server)
  │     ├── General: callsign, location
  │     ├── Rig: model selection, access (serial/tcp/sdr)
  │     │     └── detect serial ports / SoapySDR devices
  │     ├── Listen: address, port
  │     ├── Audio: sample rate, channels, codec settings
  │     ├── SDR: (if soapysdr selected) gain, channels, decoders
  │     ├── Uplinks: PSKReporter, APRS-IS
  │     └── Decode logs: enable, directory
  │
  ├── (if client)
  │     ├── Remote: server URL, auth token
  │     ├── Frontends: HTTP, rigctl, http-json (enable/disable, ports)
  │     └── Audio: bridge settings
  │
  └── Write file(s) with confirmation

```

#### Hardware Detection

- **Serial ports**: enumerate available serial devices using `serialport` crate
  (already a transitive dependency). Present as selectable list with device
  path and description.
- **SoapySDR devices**: if built with `soapysdr` feature, call
  `SoapySDR::enumerate("")` to list available SDR hardware. Present device
  driver, label, and serial number.

#### Dependencies

| Crate | Use | Already present? |
|-------|-----|-----------------|
| `dialoguer` | Interactive prompts, selection, confirmation | No |
| `toml_edit` | TOML serialisation preserving comments | No |
| `trx-app` | Config types and validation | Yes |
| `serialport` | Serial port enumeration | Yes (transitive) |
| `soapysdr` | SDR device enumeration (optional) | Yes (feature-gated) |
