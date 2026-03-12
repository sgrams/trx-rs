# Virtual-Channel Audio — Implementation Plan

## Goal

Each virtual channel (SDR DSP slice) has its own Opus audio stream.  When the
browser switches to a non-primary virtual channel the `/audio` WebSocket should
deliver audio demodulated at that channel's frequency and mode, not the primary
channel's audio.

---

## Current Architecture (baseline)

```
SoapySDR HW
  └─ SdrPipeline (slot 0: primary, slot 1..N: virtual)
       pcm_tx[0]  pcm_tx[1] ... pcm_tx[N]   (broadcast::Sender<Vec<f32>>)

trx-server/src/main.rs
  subscribe_pcm(slot 0) → Opus encode → rx_audio_tx (broadcast::Sender<Bytes>)

trx-server/src/audio.rs  handle_audio_client()
  writes  [0x00] StreamInfo
          [0x0a] history blob
  loop:   [0x01] RX frame  ← only primary channel

trx-client/src/audio_client.rs
  reads all frames → rx_audio_tx.send(bytes)   (single broadcast)

FrontendRuntimeContext.audio_rx  (single broadcast::Sender<Bytes>)

audio.rs / audio_ws()
  subscribes to audio_rx → WebSocket to browser
```

Only slot 0 (primary) is ever encoded/transmitted.  All sessions hear the same
audio.

---

## Planned Architecture

```
SdrPipeline  pcm_tx[0..N]
     │
trx-server/src/audio.rs   (extended handle_audio_client)
  ┌── per-rig VChanAudioMixer ──────────────────────────────────┐
  │  tracks (server_uuid → OpusEncoder + broadcast::Sender<Bytes>) │
  │  listens for VCHAN_SUB/VCHAN_UNSUB from client              │
  │  Opus-encodes each channel's PCM independently              │
  └─────────────────────────────────────────────────────────────┘
     │ wire frames:
     │  [0x01] RX_FRAME          (primary channel, unchanged)
     │  [0x0b] RX_FRAME_CH  [16 B UUID][N B Opus]   ← NEW
     │  [0x0c] VCHAN_ALLOCATED  [16 B UUID]           ← NEW
     │  client→server:
     │  [0x0d] VCHAN_SUB    [16 B UUID]   subscribe to channel
     │  [0x0e] VCHAN_UNSUB  [16 B UUID]   unsubscribe

trx-client/src/audio_client.rs
  demux 0x0b frames by UUID → per-channel broadcast::Sender<Bytes>
  on 0x0c (allocated): publish UUID to per-channel map

FrontendRuntimeContext
  audio_rx: Option<broadcast::Sender<Bytes>>       (primary, unchanged)
  vchan_audio: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>  ← NEW

ClientChannelManager  (trx-frontend-http/src/vchan.rs)
  allocate(): after creating local entry, sends VCHAN_SUB via new
              vchan_audio_tx: mpsc::Sender<VChanAudioCmd>             ← NEW
  delete_channel(): sends VCHAN_UNSUB
  expose: subscribe_audio(channel_id) → Option<broadcast::Receiver<Bytes>>

audio_ws()  (trx-frontend-http/src/audio.rs)
  accepts ?channel_id=<uuid> query param
  if present → lookup context.vchan_audio[uuid] → subscribe
  else        → context.audio_rx (primary, current behaviour)
```

---

## Wire Protocol Additions (trx-core/src/audio.rs)

```
AUDIO_MSG_RX_FRAME_CH    = 0x0b
AUDIO_MSG_VCHAN_ALLOCATED = 0x0c
AUDIO_MSG_VCHAN_SUB      = 0x0d
AUDIO_MSG_VCHAN_UNSUB    = 0x0e
```

Frame layout for `RX_FRAME_CH`:
```
[0x0b] [4 B BE length = 16 + opus_len] [16 B UUID bytes] [opus_len B Opus]
```

Frame layout for `VCHAN_ALLOCATED`, `VCHAN_SUB`, `VCHAN_UNSUB`:
```
[type] [4 B BE length = 16] [16 B UUID bytes]
```

---

## Layer-by-Layer Changes

### 1. `trx-core/src/audio.rs`
- Add four new `AUDIO_MSG_*` constants.
- Add helper `read_vchan_frame(reader) -> (Uuid, Bytes)` and
  `write_vchan_frame(writer, msg_type, uuid, payload)`.

### 2. `trx-server/src/audio.rs`  (`handle_audio_client`)
- Accept `vchan_manager: Option<SharedVChanManager>` from `RigHandle`.
- Spawn a `VChanAudioMixer` task:
  - Holds `HashMap<Uuid, (JoinHandle, broadcast::Sender<Bytes>)>`.
  - On `VCHAN_SUB { uuid }`: call `vchan_manager.subscribe_pcm(uuid)`, spawn
    Opus-encode task, write `VCHAN_ALLOCATED { uuid }` to client.
  - On `VCHAN_UNSUB { uuid }`: abort encode task, remove from map.
  - On PCM ready: Opus-encode, write `RX_FRAME_CH { uuid, opus }`.
- Add the `vchan_manager` parameter to `run_audio_listener()` and pass it
  through from `main.rs`.

### 3. `trx-server/src/main.rs`
- Pass `rig_handle.vchan_manager.clone()` to `run_audio_listener()`.

### 4. `trx-client/src/audio_client.rs`
- Add `vchan_audio_tx: mpsc::Sender<VChanAudioEvent>` parameter
  (where `VChanAudioEvent = Allocated(Uuid, broadcast::Sender<Bytes>) | Frame(Uuid, Bytes)`).
- On `RX_FRAME_CH { uuid, opus }`: forward to per-channel sender (create if
  first frame for that uuid).
- On `VCHAN_ALLOCATED { uuid }`: signal that the channel is ready.

### 5. `trx-client/src/main.rs`
- Create `vchan_audio: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>`
  shared between audio_client task and FrontendRuntimeContext.
- Add an `mpsc::Sender<VChanAudioCmd>` that lets the HTTP frontend request
  SUB/UNSUB over the audio TCP; pass it into `run_audio_client()`.

### 6. `trx-client/trx-frontend/src/lib.rs`  (`FrontendRuntimeContext`)
- Add:
  ```rust
  pub vchan_audio: Arc<RwLock<HashMap<Uuid, broadcast::Sender<Bytes>>>>,
  pub vchan_audio_cmd: Option<mpsc::Sender<VChanAudioCmd>>,
  ```
- Initialise both to empty/None in `new()`.

### 7. `trx-client/trx-frontend/trx-frontend-http/src/vchan.rs`  (`ClientChannelManager`)
- `allocate()`: after inserting the local record, if `vchan_audio_cmd` is
  available, send `VChanAudioCmd::Subscribe(uuid)`.
- `delete_channel()`: send `VChanAudioCmd::Unsubscribe(uuid)`.
- `subscribe_audio(channel_id, context) -> Option<broadcast::Receiver<Bytes>>`:
  look up `context.vchan_audio.read()[channel_id].subscribe()`.

### 8. `trx-client/trx-frontend/trx-frontend-http/src/audio.rs`  (`audio_ws`)
- Parse optional `channel_id: Option<Uuid>` from query string.
- If `Some(uuid)`:
  - Look up `context.vchan_audio.read()[uuid]` → `broadcast::Sender<Bytes>`.
  - Subscribe, forward Opus frames exactly as today but from that sender.
- Else: current primary-channel path unchanged.

### 9. `assets/web/plugins/vchan.js`
- `vchanSubscribe()` and `vchanAllocate()` call `vchanReconnectAudio()`.
- `vchanReconnectAudio()`:
  - If on virtual channel: `reconnectAudioWs(vchanActiveId)` (pass channel UUID).
  - If on primary: `reconnectAudioWs(null)`.
- `reconnectAudioWs(channelId)` (new in `app.js` or `vchan.js`):
  - Close existing `audioWs`.
  - Reopen `new WebSocket('/audio' + (channelId ? '?channel_id=' + channelId : ''))`.

---

## Out of Scope (non-SDR rigs)

Non-SDR rigs (`vchan_manager === None`) are unaffected.  The new message types
are only exchanged when the server-side vchan manager is present.  Primary-
channel audio behaviour is 100% backwards-compatible.

---

## Implementation Order

1. `trx-core/src/audio.rs` — add constants and frame helpers  *(no breakage)*
2. `trx-server/src/audio.rs` — `VChanAudioMixer` + new frame handling
3. `trx-server/src/main.rs` — plumb vchan_manager through
4. `trx-client/src/audio_client.rs` — demux RX_FRAME_CH
5. `trx-client/src/main.rs` — shared vchan_audio map + cmd channel
6. `trx-frontend/src/lib.rs` — new FrontendRuntimeContext fields
7. `trx-frontend-http/src/vchan.rs` — SUB/UNSUB on allocate/delete
8. `trx-frontend-http/src/audio.rs` — channel_id query param routing
9. `vchan.js` — reconnect WebSocket on channel switch
