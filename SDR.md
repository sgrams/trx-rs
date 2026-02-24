# SDR Backend Requirements

This document specifies the requirements for a SoapySDR-based RX-only backend (`trx-backend-soapysdr`) and the associated IQ-to-audio pipeline changes in `trx-server`.

---

## Progress

> **For AI agents:** This section is the single source of truth for implementation status.
> Each task has a unique ID (e.g. `SDR-01`), a status badge, a description, the files it touches, and any blocking dependencies.
> Pick any task whose status is `[ ]` and whose `Needs` list is fully `[x]`. Update status to `[~]` while working, `[x]` when merged. Record notes under the task if you hit non-obvious issues.
>
> Status legend: `[ ]` not started · `[~]` in progress · `[x]` done · `[!]` blocked

### Foundational (must land first)

| ID | Status | Task | Touches |
|----|--------|------|---------|
| SDR-01 | `[x]` | Add `AudioSource` trait to `trx-core`; add `as_audio_source()` default on `RigCat` | `src/trx-core/src/rig/mod.rs` |
| SDR-02 | `[x]` | Add `RigAccess::Sdr { args: String }` variant; register `soapysdr` factory (feature-gated `soapysdr`) | `src/trx-server/trx-backend/src/lib.rs` |
| SDR-03 | `[ ]` | Add `SdrConfig`, `SdrGainConfig`, `SdrChannelConfig` structs; parse `type = "sdr"` in `AccessConfig`; add `sdr: SdrConfig` to `ServerConfig`; add startup validation rules (§11) | `src/trx-server/src/config.rs` |

### New crate: `trx-backend-soapysdr`

| ID | Status | Task | Touches | Needs |
|----|--------|------|---------|-------|
| SDR-04 | `[ ]` | Create crate scaffold: `Cargo.toml` (deps: `soapysdr`, `num-complex`, `tokio`), empty `lib.rs` | `src/trx-server/trx-backend/trx-backend-soapysdr/` | SDR-01, SDR-02 |
| SDR-05 | `[ ]` | Implement `demod.rs`: SSB (USB/LSB), AM envelope, FM quadrature, CW narrow BPF+envelope | `…/src/demod.rs` | SDR-04 |
| SDR-06 | `[ ]` | Implement `dsp.rs`: IQ broadcast loop (SoapySDR read thread → `broadcast::Sender<Vec<Complex<f32>>>`); per-channel mixer → FIR LPF → decimator → demod → frame accumulator → `broadcast::Sender<Vec<f32>>` | `…/src/dsp.rs` | SDR-04, SDR-05 |
| SDR-07 | `[ ]` | Implement `SoapySdrRig` in `lib.rs`: `RigCat` (RX methods + `not_supported` stubs for TX), `AudioSource`, gain control (manual/auto with fallback), primary channel freq/mode tracking | `…/src/lib.rs` | SDR-03, SDR-06 |

### Server integration

| ID | Status | Task | Touches | Needs |
|----|--------|------|---------|-------|
| SDR-08 | `[ ]` | `main.rs`: after building rig, if `as_audio_source()` is `Some` skip cpal, subscribe each decoder and the Opus encoder to the appropriate channel PCM senders; validate `stream_opus` count ≤ 1 | `src/trx-server/src/main.rs` | SDR-03, SDR-07 |
| SDR-09 | `[ ]` | Add `trx-backend-soapysdr` to workspace `Cargo.toml`; update `CONFIGURATION.md` with new `[sdr]` / `[[sdr.channels]]` options | `Cargo.toml`, `CONFIGURATION.md` | SDR-04 |

### Validation & tests

| ID | Status | Task | Touches | Needs |
|----|--------|------|---------|-------|
| SDR-10 | `[ ]` | Unit tests for `demod.rs`: known-input tone through each demodulator, check output frequency correct | `…/src/demod.rs` | SDR-05 |
| SDR-11 | `[ ]` | Unit tests for config validation: channel IF out-of-range, dual `stream_opus`, TX enabled with SDR backend, AGC fallback warning | `src/trx-server/src/config.rs` | SDR-03 |

---

## Goals

- Receive-only backend that uses any SoapySDR-compatible device (RTL-SDR, Airspy, HackRF, SDRplay, etc.) as the rig
- Full IQ pipeline: raw IQ samples → demodulated PCM → existing decoders (FT8, WSPR, APRS, CW) with zero decoder-side changes
- Wideband capture: one SDR IQ stream feeds multiple simultaneous virtual receivers, each independently tuned and demodulated
- Configurable per-channel filters and demodulation modes
- Demodulated audio streamed to clients as Opus over the existing TCP audio channel

---

## Non-Goals

- Transmit (TX/PTT) of any kind
- Replacing or deprecating the existing cpal-based audio path (it stays for transceiver backends)

---

## 1. Device Abstraction

### 1.1 `RigAccess` extension

A new access type `sdr` is added alongside `serial` and `tcp`:

```toml
[rig.access]
type = "sdr"
args = "driver=rtlsdr"             # SoapySDR device args string
```

The `args` value is passed verbatim to `SoapySDR::Device::new(args)`. It follows SoapySDR's key=value comma-separated convention (e.g., `driver=airspy`, `driver=rtlsdr,serial=00000001`).

### 1.2 `AudioSource` trait

A new trait is added to `trx-core` (`src/trx-core/src/rig/mod.rs`):

```rust
pub trait AudioSource: Send + Sync {
    /// Subscribe to demodulated PCM audio from the primary channel.
    fn subscribe_pcm(&self) -> broadcast::Receiver<Vec<f32>>;
}
```

`RigCat` gains a default opt-in method:

```rust
pub trait RigCat: Rig + Send {
    // ... existing methods ...
    fn as_audio_source(&self) -> Option<&dyn AudioSource> { None }
}
```

`SoapySdrRig` overrides `as_audio_source()` to return `Some(self)`. When the server detects this, it skips spawning the cpal capture thread entirely.

### 1.3 TX-only `RigCat` methods

The following methods return `RigError::not_supported(...)` on the SDR backend:

- `set_ptt()`
- `power_on()` / `power_off()`
- `get_tx_power()`
- `get_tx_limit()` / `set_tx_limit()`
- `toggle_vfo()` (not applicable; channels are defined statically in config)
- `lock()` / `unlock()`

The following methods are fully supported:

- `get_status()` → returns primary channel's current `(freq, mode, None)`
- `set_freq()` → re-tunes the SDR center frequency (keeping `center_offset_hz` invariant) and updates all channel mixer offsets
- `set_mode()` → changes the primary channel's demodulator
- `get_signal_strength()` → returns instantaneous RSSI for the primary channel (dBFS mapped to 0–255 S-unit range)

---

## 2. IQ Pipeline Architecture

### 2.1 Center frequency offset

SDR hardware has a DC offset spur at exactly 0 Hz in the IQ spectrum. To keep the primary channel off DC, the SDR is tuned to a frequency offset from the desired dial frequency:

```
sdr_center_freq = dial_freq - center_offset_hz
```

With `center_offset_hz = 200000` and dial freq 14.074 MHz, the SDR tunes to 13.874 MHz. The 14.074 MHz signal appears at +200 kHz in the IQ spectrum and is mixed down to baseband in software.

`center_offset_hz` is a global SDR parameter (not per-channel). A reasonable default is `100000` (100 kHz).

### 2.2 Wideband channel model

One SoapySDR RX stream produces IQ samples at `sdr.sample_rate` (e.g. 1.92 MHz). This stream is shared among all configured channels. Each channel defines an independent virtual receiver:

```
SoapySDR RX stream  (complex f32, sdr_sample_rate Hz)
    │
    ├──► Channel 0  (primary)   offset_hz=0,      mode=USB,  bw=3000 Hz
    ├──► Channel 1  (wspr)      offset_hz=+21600, mode=USB,  bw=3000 Hz
    └──► Channel N  ...
```

A **channel's frequency** in the real spectrum is:

```
channel_real_freq = dial_freq + channel.offset_hz
```

A **channel's IF frequency** within the IQ stream is:

```
channel_if_hz = center_offset_hz + channel.offset_hz
```

This is the frequency at which the channel's signal appears in the captured IQ bandwidth, and is what the channel's mixer shifts to baseband.

**Constraint:** `|channel_if_hz|` must be less than `sdr_sample_rate / 2` for every channel. The server validates this at startup and rejects invalid configs.

### 2.3 Per-channel DSP chain

Each channel runs the following stages independently on the shared IQ stream:

```
IQ input (complex f32, sdr_sample_rate)
    1. Mixer:     multiply by exp(-j·2π·channel_if_hz·n/sdr_sample_rate)
                  → complex f32 centred at 0 Hz
    2. FIR LPF:   cutoff = audio_bandwidth_hz / 2, order configurable
    3. Decimator: sdr_sample_rate / audio_sample_rate  (must be integer; resampler used otherwise)
    4. Demodulator (mode-dependent, see §3)
    5. Output: real f32 at audio_sample_rate
    6. Frame accumulator: chunks of frame_duration_ms
    7. broadcast::Sender<Vec<f32>>  →  decoders + optional Opus encoder
```

Channels run concurrently in separate tasks, all reading from the same raw IQ broadcast channel.

### 2.4 IQ broadcast channel

The SoapySDR read loop runs in a dedicated OS thread (matching the existing cpal thread model). It reads IQ sample blocks from the device and publishes them on:

```rust
broadcast::Sender<Vec<Complex<f32>>>   // capacity: configurable, default 64 blocks
```

Each channel task subscribes to this sender. Lagged receivers log a warning and continue.

---

## 3. Demodulators

Demodulator is selected per-channel based on `mode`. Modes map as follows:

| `RigMode` | Demodulator |
|-----------|-------------|
| `USB`     | SSB: mix to IF, take real part (upper sideband) |
| `LSB`     | SSB: mix to IF, take real part (lower sideband, negate IF) |
| `AM`      | Envelope detector: `sqrt(I² + Q²)`, DC-remove, normalize |
| `FM`      | Quadrature: `arg(s[n] · conj(s[n-1]))`, i.e. instantaneous frequency |
| `WFM`     | Same as FM, wider pre-demod filter (`wfm_bandwidth_hz`) |
| `CW`      | Narrow BPF centred at `cw_center_hz` (audio domain), then envelope |
| `DIG`/`PKT` | Same as USB (pass audio through for downstream digital decoders) |
| `CWR`     | Same as CW (reversed sideband, uses same audio envelope) |

For SSB modes (USB/LSB), after mixing to baseband the channel's `audio_bandwidth_hz` defines the one-sided cutoff of the post-demod LPF.

---

## 4. Gain Control

Gain is configured globally under `[sdr.gain]`.

```toml
[sdr.gain]
mode  = "auto"    # "auto" (AGC via SoapySDR) or "manual"
value = 30.0      # dB; ignored when mode = "auto"
```

- **`auto`**: calls `device.set_gain_mode(SOAPY_SDR_RX, 0, true)` to enable hardware AGC if the device supports it. If the device does not support hardware AGC, falls back to `manual` with a warning.
- **`manual`**: calls `device.set_gain(SOAPY_SDR_RX, 0, value)` with the specified total gain in dB.

Advanced per-element gain is out of scope for this phase (no `lna`/`vga`/`if` sub-keys initially).

---

## 5. Filter Configuration

Filters are configured per-channel. The following are settable:

```toml
[[sdr.channels]]
audio_bandwidth_hz = 3000    # One-sided bandwidth of post-demod BPF (Hz)
                             # For FM: deviation hint for deemphasis
fir_taps = 64                # FIR filter tap count (default 64); higher = sharper roll-off
cw_center_hz = 700           # CW tone centre in audio domain (default 700 Hz)
wfm_bandwidth_hz = 75000     # Pre-demod bandwidth for WFM only (default 75 kHz)
```

`fir_taps` controls the same FIR used in stage 2 of the DSP chain (§2.3). It applies uniformly to both the pre-demod decimation filter and the post-demod audio BPF in this phase.

---

## 6. Channel Configuration and Decoder Binding

Channels are declared as a TOML array. The first channel in the list is the **primary channel** and is the one exposed via `RigCat` (`set_freq`/`set_mode` affect it; `get_status` reads from it).

```toml
[[sdr.channels]]
id               = "primary"   # Identifier, used in logs
offset_hz        = 0           # Offset from dial frequency (Hz)
mode             = "auto"      # "auto" = follows RigCat set_mode; or fixed RigMode string
audio_bandwidth_hz = 3000
fir_taps         = 64
decoders         = ["ft8", "cw"]   # Which decoders receive this channel's PCM
stream_opus      = true            # Encode and stream via TCP audio channel

[[sdr.channels]]
id               = "wspr-14"
offset_hz        = 21600       # 14.0956 MHz when dial = 14.074 MHz
mode             = "USB"       # Fixed mode, ignores RigCat set_mode
audio_bandwidth_hz = 3000
decoders         = ["wspr"]
stream_opus      = false

[[sdr.channels]]
id               = "aprs"
offset_hz        = -673600     # e.g. 144.390 MHz when dial = 145.0635 MHz
mode             = "FM"
audio_bandwidth_hz = 8000
decoders         = ["aprs"]
stream_opus      = false
```

**`mode = "auto"`** means the channel's demodulator tracks whatever `set_mode()` last set on the backend. Only the primary channel should use `"auto"` in typical use.

**`decoders`** maps to the decoder task IDs: `"ft8"`, `"wspr"`, `"aprs"`, `"cw"`. Each named decoder subscribes to the PCM broadcast channel of the listed channel(s). A decoder can only be bound to one channel (first binding wins if duplicated).

---

## 7. Opus Streaming

Channels with `stream_opus = true` have their demodulated PCM Opus-encoded and streamed over the server's existing TCP audio port (default 4531).

For this phase, only **one channel** may have `stream_opus = true` (validation error otherwise). This channel's Opus stream replaces what cpal would have produced — the TCP audio protocol and client-side handling are unchanged.

The Opus encoder uses the `[audio]` config for `frame_duration_ms`, `bitrate_bps`, and `sample_rate`. The SDR pipeline must output PCM at the same `sample_rate` as `[audio]`; a mismatch is a startup validation error.

---

## 8. Full Configuration Example

```toml
[rig]
model = "soapysdr"
initial_freq_hz = 14074000
initial_mode = "USB"

[rig.access]
type = "sdr"
args = "driver=rtlsdr"

[sdr]
sample_rate      = 1920000     # IQ capture rate (Hz) — must be supported by device
bandwidth        = 1500000     # Hardware IF filter (Hz)
center_offset_hz = 200000      # SDR tunes this many Hz below dial frequency

[sdr.gain]
mode  = "auto"
value = 30.0                   # Effective only when mode = "manual"

[[sdr.channels]]
id               = "primary"
offset_hz        = 0
mode             = "auto"
audio_bandwidth_hz = 3000
fir_taps         = 64
decoders         = ["ft8", "cw"]
stream_opus      = true

[[sdr.channels]]
id               = "wspr"
offset_hz        = 21600
mode             = "USB"
audio_bandwidth_hz = 3000
decoders         = ["wspr"]
stream_opus      = false

[audio]
enabled          = true
listen           = "127.0.0.1"
port             = 4531
rx_enabled       = true
tx_enabled       = false        # No TX on SDR backend
sample_rate      = 48000
channels         = 1
frame_duration_ms = 20
bitrate_bps      = 24000
```

---

## 9. Code Changes Map

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | Add `src/trx-server/trx-backend/trx-backend-soapysdr` member |
| `src/trx-core/src/rig/mod.rs` | Add `AudioSource` trait; add `as_audio_source()` default to `RigCat` |
| `src/trx-server/trx-backend/src/lib.rs` | Add `RigAccess::Sdr { args }` variant; register `soapysdr` factory (feature-gated) |
| `src/trx-server/src/config.rs` | Add `SdrConfig`, `SdrGainConfig`, `SdrChannelConfig`; parse `type = "sdr"` in `AccessConfig`; add `sdr: SdrConfig` to `ServerConfig` |
| `src/trx-server/src/main.rs` | After building rig: if `as_audio_source()` is `Some`, skip cpal, use `AudioSource::subscribe_pcm()` for each decoder and for the Opus encoder; validate at most one `stream_opus = true` channel |
| `src/trx-server/src/audio.rs` | Expose `spawn_audio_capture` and `run_*_decoder` without assuming cpal as the sole source; no functional change needed — decoders already take `broadcast::Receiver<Vec<f32>>` |
| `src/trx-server/trx-backend/trx-backend-soapysdr/Cargo.toml` | New crate |
| `src/trx-server/trx-backend/trx-backend-soapysdr/src/lib.rs` | `SoapySdrRig`: implements `RigCat` + `AudioSource`; spawns IQ read thread and channel DSP tasks |
| `src/trx-server/trx-backend/trx-backend-soapysdr/src/dsp.rs` | IQ broadcast loop; per-channel mixer, FIR, decimator, demodulator, frame accumulator |
| `src/trx-server/trx-backend/trx-backend-soapysdr/src/demod.rs` | Mode-specific demodulators: SSB, AM envelope, FM quadrature, CW envelope |
| `CONFIGURATION.md` | Document new `[rig.access] type = "sdr"`, `[sdr]`, `[[sdr.channels]]` options |

---

## 10. External Dependencies

| Crate | Purpose |
|-------|---------|
| `soapysdr` | Rust bindings to `libSoapySDR` (C++) |
| `num-complex` | `Complex<f32>` for IQ arithmetic |

System requirement: `libSoapySDR` installed (e.g. `brew install soapysdr` on macOS, `libsoapysdr-dev` on Debian/Ubuntu).

---

## 11. Validation Rules (startup)

- `[rig.access] type = "sdr"` requires `args` to be non-empty.
- `[sdr] sample_rate` must be non-zero.
- For every channel: `|center_offset_hz + channel.offset_hz| < sdr_sample_rate / 2`.
- Exactly one channel must have `stream_opus = true` (or none; zero means no TCP audio stream).
- The audio `sample_rate` in `[audio]` must equal the target audio rate in the SDR pipeline (no cross-rate mismatch).
- `[audio] tx_enabled` must be `false` when `model = "soapysdr"`.
- A decoder name may appear in at most one channel's `decoders` list.
- If the device does not support hardware AGC and `gain.mode = "auto"`, warn and fall back to `manual` using `gain.value`.
