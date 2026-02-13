<div align="center">
  <img src="assets/trx-logo.png" alt="trx-rs logo" width="25%" />
</div>

# trx-rs (work in progress)

This is an early, untested snapshot of a transceiver control stack (core + backend + frontends). Things may change quickly and APIs are not stable yet. Expect rough edges and bugs; use at your own risk and please report issues you hit. Features, tests and docs are still being written (or not).

The rig task is now driven by the controller components (state machine, handlers, and policies). Polling and retry behavior are configurable via the `[behavior]` section in the config file.

Configuration reference: see `CONFIGURATION.md` for all server/client options and defaults.

## Supported backends

- Yaesu FT-817 (feature-gated crate `trx-backend-ft817`)
- Planned: other rigs I own; contributions and reports are welcome.

## Frontends

- HTTP status/control frontend (`trx-frontend-http`)
- JSON TCP control frontend (`trx-frontend-http-json`)
- rigctl-compatible TCP frontend (`trx-frontend-rigctl`, listens on 127.0.0.1:4532)

## HTTP Frontend Authentication

The HTTP frontend supports optional passphrase-based authentication with two roles:

- **rx**: Read-only access to status, events, decode history, and audio streams
- **control**: Full access including transmit control (TX/PTT) and power toggling

Authentication is disabled by default for backward compatibility. When enabled, users must log in via a passphrase before accessing the web UI. Sessions are managed server-side with configurable time-to-live and cookie security settings.

### Configuration

Enable authentication in `trx-client.toml`:

```toml
[frontends.http.auth]
enabled = true
rx_passphrase = "read-only-secret"
control_passphrase = "full-control-secret"
session_ttl_min = 480          # 8 hours
cookie_secure = false          # Set to true for HTTPS
cookie_same_site = "Lax"
```

### Security Considerations

- **Local/LAN use**: Default settings are safe for 127.0.0.1 or trusted local networks.
- **Remote access**: For internet-exposed deployments:
  - Deploy behind HTTPS (reverse proxy or TLS termination)
  - Set `cookie_secure = true`
  - Use strong passphrases (random, 16+ chars)
  - Consider firewall rules and network segmentation
- **Passphrase storage**: Passphrases are stored in plaintext in the config file. Protect the config file with appropriate file permissions.
- **No rate limiting**: The current implementation does not include login rate limiting. For high-security scenarios, deploy behind a reverse proxy with rate limiting.

### Architecture

- **Sessions**: In-memory, expire after configured TTL (default 8 hours)
- **Cookies**: HttpOnly, configurable Secure and SameSite attributes
- **Route protection**: Middleware validates session on protected endpoints; public routes (static assets, login) are always accessible
- **TX/PTT gating**: Control-only endpoints return 404 to rx-authenticated users (when `tx_access_control_enabled=true`)

## Audio streaming

Bidirectional Opus audio streaming between server, client, and browser.

- **Server** captures audio from a configured input device (cpal), encodes to Opus, and streams over a dedicated TCP connection (default port 4533). TX audio received from clients is decoded and played back.
- **Client** connects to the server's audio TCP port and relays Opus frames to/from the HTTP frontend via a WebSocket at `/audio`.
- **Browser** connects to the `/audio` WebSocket, decodes Opus via WebCodecs `AudioDecoder`, and plays RX audio. TX audio is captured via `getUserMedia` and encoded with WebCodecs `AudioEncoder`.

Enable with `[audio] enabled = true` in the server config and `[frontends.audio] enabled = true` in the client config.

## Dependencies

### System libraries

The following system libraries are required at build time:

| Library | Purpose | Install |
|---------|---------|---------|
| **libopus** | Opus audio codec encoding/decoding | `zb install opus` (or your system package manager) |
| **cmake** | Required by the `audiopus_sys` build script if libopus is not found via pkg-config | `zb install cmake` |
| **pkg-config** / **pkgconf** | Locates system libopus during build | `zb install pkgconf` |
| **Core Audio** (macOS) / **ALSA** (Linux) | Audio device access via cpal | Provided by the OS (macOS) or `alsa-lib-dev` (Linux) |

## Plugin discovery

`trx-server` and `trx-client` can load shared-library plugins that register backends/frontends
via a `trx_register` entrypoint. Search paths:

- `./plugins`
- `~/.config/trx-rs/plugins`
- `TRX_PLUGIN_DIRS` (path-separated)

Example plugin: `examples/trx-plugin-example`

## License

This project is licensed under the BSD-2-Clause license. See `LICENSES/` for bundled third-party license files.
