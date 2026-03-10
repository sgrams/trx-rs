# Virtual Channels

## Overview

Virtual channels allow a single SDR rig to simultaneously receive multiple signals within
its capture bandwidth. Each virtual channel has its own frequency offset, mode, and
independent decoder pipeline. Traditional (non-SDR) rigs expose no virtual channels.

## Concepts

### Channel 0 (Primary)

The permanent default channel. Always exists. Controlled by normal rig commands
(`SetFreq`, `SetMode`, etc.). Cannot be deallocated.

### Virtual Channels (1+)

Dynamically allocated. Each has:
- An IF offset relative to the SDR center frequency
- Its own mode / demodulator
- Its own Opus audio stream
- Its own decoder subscriptions (FT8, APRS, CW, etc.)
- A ref-count of SSE sessions currently subscribed to it

A virtual channel is freed when its ref-count drops to zero (last subscriber
disconnects or switches away), except channel 0 which is permanent.

### Session Binding

Each SSE session (`/events`) is assigned a server-generated `session_id` (UUID),
returned in the initial `session` SSE event on connect. The client uses this ID
when allocating a virtual channel:

```
POST /rigs/{rig_id}/channels
{ "session_id": "<uuid>", "freq_hz": 14095600, "mode": "CW" }
```

The server tracks (session_id → channel_id) and decrements the channel ref-count
when the SSE stream drops. A session may only own one virtual channel at a time;
allocating a second implicitly releases the first.

### Center Frequency Constraint

The SDR center frequency is shared across all channels. When more than one channel
is active, attempting to set the center frequency (channel 0 freq) to a value that
would place any other channel outside the capture bandwidth returns **409 Conflict**.
Similarly, tuning a virtual channel outside the current capture bandwidth returns
**409 Conflict**.

### Renumbering

Channels are identified internally by UUID. The display index (0, 1, 2, …) is
derived from the server-returned ordered list. Indices are reassigned after
deallocation so the list stays compact.

## Capacity

Configured in the server config (`trx-server.toml`):

```toml
[rig.sdr_options]
max_virtual_channels = 4  # default: 4 (including channel 0)
```

Attempting to allocate beyond the cap returns **429 Too Many Requests**.

## HTTP API

All endpoints require at minimum the **Rx** role for reads; **Control** for writes.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/rigs/{rig_id}/channels` | List all active channels |
| POST | `/rigs/{rig_id}/channels` | Allocate a new virtual channel |
| DELETE | `/rigs/{rig_id}/channels/{channel_id}` | Deallocate (not channel 0) |
| GET | `/rigs/{rig_id}/channels/{channel_id}` | Get channel state |
| PUT | `/rigs/{rig_id}/channels/{channel_id}` | Set freq/mode for a channel |

### GET /rigs/{rig_id}/channels

```json
[
  {
    "id": "00000000-0000-0000-0000-000000000000",
    "index": 0,
    "freq_hz": 14074000,
    "mode": "USB",
    "subscribers": 2,
    "permanent": true
  },
  {
    "id": "a1b2c3d4-...",
    "index": 1,
    "freq_hz": 14095600,
    "mode": "CW",
    "subscribers": 1,
    "permanent": false
  }
]
```

### POST /rigs/{rig_id}/channels

Request body (optional):
```json
{ "freq_hz": 14095600, "mode": "CW" }
```

Returns the new channel object. Errors:
- 429: cap reached
- 409: freq outside capture bandwidth
- 400: rig does not support virtual channels (traditional rig)

### PUT /rigs/{rig_id}/channels/{channel_id}

```json
{ "freq_hz": 14100000, "mode": "USB" }
```

Errors:
- 409: freq outside capture bandwidth (when other channels are active)
- 404: channel not found

### DELETE /rigs/{rig_id}/channels/{channel_id}

Deallocates the channel. All sessions subscribed to it are moved to channel 0
via an SSE event `channel-evicted`.

## SSE Events

New SSE event types:

| Event | Payload | Description |
|-------|---------|-------------|
| `channels` | `ChannelList` | Full channel list snapshot |
| `channel-updated` | `Channel` | One channel changed freq/mode/subscribers |
| `channel-evicted` | `{evicted_id, fallback_id}` | Client's channel was deallocated |

The `channels` snapshot is sent on connect (after the rig state snapshot) and
whenever the channel list changes.

## Audio WebSocket

Each channel exposes an independent Opus audio stream:

```
GET /rigs/{rig_id}/channels/{channel_id}/audio  (WebSocket upgrade)
```

The legacy `/audio` endpoint continues to work and serves channel 0.

## Decode SSE

Each channel exposes its own independent decoder event stream:

```
GET /rigs/{rig_id}/channels/{channel_id}/decode  (SSE)
```

The legacy `/decode` endpoint continues to work and serves channel 0's decoders.
Decoded frames are tagged with the channel's freq/mode at decode time.

## Data Model (Rust)

```rust
// In trx-core or trx-server

pub struct VirtualChannel {
    pub id: Uuid,
    pub freq_hz: u64,
    pub mode: RigMode,
    pub subscribers: usize,       // ref-count
    pub permanent: bool,          // true for channel 0
}

pub struct ChannelManager {
    channels: Vec<VirtualChannel>,   // index 0 = primary
    max_channels: usize,
    // DSP handles indexed by position
    dsp_handles: Vec<ChannelDspHandle>,
}
```

### ChannelDspHandle

Wraps a dynamically allocated `ChannelDsp` slot in the `SdrPipeline`. The
pipeline gains `add_channel()` / `remove_channel()` methods that operate on the
live IQ processing loop (via a command channel to the sdr-iq-read thread).

## DSP Integration

The existing `SdrPipeline` in `trx-backend-soapysdr` has a fixed set of
`ChannelDsp` instances (primary + AIS A + AIS B + optional VDES). Virtual
channels extend this with a dynamic slot list:

```
IQ Broadcast (broadcast::Sender<Vec<Complex<f32>>>)
    ├─ ChannelDsp[0]   ← channel 0 (primary, permanent)
    ├─ ChannelDsp[1]   ← AIS A (internal, not user-visible)
    ├─ ChannelDsp[2]   ← AIS B (internal, not user-visible)
    └─ ChannelDsp[3+]  ← user virtual channels (dynamic)
```

The IQ broadcast already fans out to all receivers; adding a new virtual channel
simply spawns a new async task that subscribes to the IQ broadcast and runs a
`ChannelDsp` for that slot. Removing a channel aborts that task.

Each virtual channel task outputs PCM frames to a `broadcast::Sender<Vec<f32>>`
stored in `ChannelManager`, which the audio WebSocket handler subscribes to.

## Frontend UI

### Channel Picker (SDR rigs only)

A compact control bar visible only when the active rig supports virtual channels:

```
[ Ch 0 (14.074 USB) ▼ ] [ + New Channel ] [ ✕ Remove This ]
```

- **Picker**: dropdown of all active channels with freq+mode label
- **+ New Channel**: allocates a new channel, switches picker to it, focuses the
  freq/mode controls
- **✕ Remove This**: deallocates the current channel (disabled for channel 0);
  confirms before sending DELETE

### Channel State

When a channel is selected in the picker, the main VFO display, mode selector,
and decoder panels reflect that channel's state (not rig-global state). Tuning
and mode changes act on the selected channel via `PUT /rigs/{rig_id}/channels/{id}`.

### SSE Fallback

On receiving `channel-evicted`, the frontend automatically:
1. Switches the picker to channel 0
2. Shows a brief toast: "Virtual channel removed — switched to primary"

## Implementation Order

1. **`SdrPipeline` dynamic channels** — `add_channel()` / `remove_channel()` via
   command channel to IQ read thread
2. **`ChannelManager`** in `trx-server` — tracks channels, ref-counts, DSP handles
3. **HTTP API** in `trx-frontend-http` — channel CRUD, audio WebSocket per channel
4. **SSE events** — `channels` snapshot on connect, `channel-updated`, `channel-evicted`
5. **Frontend** — channel picker, +/✕ buttons, VFO/mode reflecting selected channel
