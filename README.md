<div align="center">
  <img src="assets/trx-logo.png" alt="trx-rs logo" width="25%" />
</div>

# trx-rs

`trx-rs` is a modular amateur radio control stack written in Rust.
It splits radio hardware access from user-facing interfaces so you can run
rig control, SDR DSP, decoding, audio streaming, and web access as separate,
composable pieces.

The project is built around two primary binaries:

- `trx-server`: talks to radios and SDR backends
- `trx-client`: connects to the server and exposes frontends such as the web UI

## Web UI Demo

> GIF placeholder: add an animated walkthrough of the website here.

## What It Does

- Controls supported radios over networked client/server boundaries
- Exposes a browser UI, a rigctl-compatible frontend, and JSON-based control
- Supports SDR workflows with live spectrum, waterfall, demodulation, and decode
- Streams Opus audio between server, client, and browser
- Runs multiple decoders including AIS, APRS, CW, FT8, RDS, VDES, and WSPR
- Supports multi-rig deployments and SDR virtual channels
- Loads backends and frontends via plugins

## Architecture

At a high level:

1. `trx-server` owns the radio hardware and DSP pipeline.
2. `trx-client` connects to the server over TCP for control and audio.
3. Frontends hang off `trx-client`, including the HTTP web UI.

This separation is intentional: it keeps hardware access local to one host while
making control and monitoring available elsewhere on the network.

## Workspace Layout

- `src/trx-core`: shared types, rig state, controller logic
- `src/trx-protocol`: client/server protocol types and codecs
- `src/trx-app`: shared app bootstrapping, config, logging, plugins
- `src/trx-server`: server binary and backend integration
- `src/trx-client`: client binary and remote connection handling
- `src/trx-client/trx-frontend`: frontend abstraction
- `src/decoders`: protocol-specific decoder crates
- `examples/trx-plugin-example`: minimal plugin example

## Supported Pieces

### Backends

- Yaesu FT-817
- Yaesu FT-450D
- SoapySDR-based SDR backend

### Frontends

- HTTP web frontend
- rigctl-compatible TCP frontend
- JSON-over-TCP frontend

### Decoders

- AIS
- APRS
- CW
- FT8
- RDS
- VDES
- WSPR

## Build Requirements

You will need a Rust toolchain (stable) plus a few system libraries.

### Common (all builds)

| Library | Purpose |
|---------|---------|
| **libopus** (dev) | Opus audio codec for streaming between server, client, and browser |
| **pkg-config** or **pkgconf** | Locates system libraries at build time |
| **cmake** | Builds the vendored Opus C source (`audiopus_sys`) when a system copy is not found |

### Platform audio

| Platform | Library | Purpose |
|----------|---------|---------|
| **Linux** | **libasound2** (dev) | ALSA backend for `cpal` audio capture/playback |
| **macOS** | Core Audio (ships with Xcode) | No extra packages needed |

### SDR support (optional)

| Library | Purpose |
|---------|---------|
| **libsoapysdr** (dev) | SDR device abstraction used by the `trx-backend-soapysdr` crate |

SoapySDR is enabled by default. Build without it using `--no-default-features`:

```bash
cargo build --release --no-default-features
```

### Install commands

**Debian / Ubuntu:**

```bash
# Required
sudo apt install build-essential pkg-config cmake libopus-dev libasound2-dev

# Optional — SDR support
sudo apt install libsoapysdr-dev
```

**Fedora:**

```bash
# Required
sudo dnf install gcc pkg-config cmake opus-devel alsa-lib-devel

# Optional — SDR support
sudo dnf install SoapySDR-devel
```

**Arch Linux:**

```bash
# Required
sudo pacman -S base-devel pkgconf cmake opus alsa-lib

# Optional — SDR support
sudo pacman -S soapysdr
```

**macOS (Homebrew):**

```bash
# Required
brew install cmake opus

# Optional — SDR support
brew install soapysdr
```

## Configuration

Both `trx-server` and `trx-client` read from a shared `trx-rs.toml`.

- Default lookup order: current directory, `~/.config/trx-rs`, `/etc/trx-rs`
- Use `--config <FILE>` to point at an explicit config file
- Use `--print-config` to print an example combined config

Start from [`trx-rs.toml.example`](trx-rs.toml.example).

## Quick Start

### 1. Build

```bash
cargo build
```

### 2. Create a config file

```bash
cp trx-rs.toml.example trx-rs.toml
```

Adjust backend, frontend, audio, and auth settings for your environment.

### 3. Run the server

```bash
cargo run -p trx-server
```

### 4. Run the client

```bash
cargo run -p trx-client
```

### 5. Open the web UI

Open the configured HTTP frontend address in a browser.

## Web Frontend Highlights

- Real-time spectrum and waterfall
- Frequency, mode, and bandwidth control
- Decoder dashboards and history
- SDR virtual channels
- Browser RX/TX audio
- Optional authentication with read-only and control roles

## Authentication

The HTTP frontend supports optional passphrase-based authentication.

- `rx`: read-only access
- `control`: full control access

When exposing the web UI beyond a trusted LAN, run it behind HTTPS and enable
secure cookie settings in the config.

## Audio

Audio is transported as Opus between server, client, and browser.

- `trx-server` captures and encodes audio
- `trx-client` relays audio to the HTTP frontend
- Browsers connect over `/audio`

## Documentation

- [User Manual](docs/User-Manual.md): configuration, features, and usage
- [Architecture](docs/Architecture.md): system design, crate layout, data flow, and internals
- [`CONTRIBUTING.md`](CONTRIBUTING.md): contribution and commit rules

## Project Status

This is an active project with evolving APIs and frontend behavior. Expect some
rough edges and ongoing refactors.

## License

Licensed under BSD-2-Clause.

See [`LICENSES`](LICENSES) for bundled third-party license files.
