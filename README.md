<div align="center">
  <img src="assets/trx-logo.png" alt="trx-rs logo" width="25%" />
</div>

# trx-rs (work in progress)

This is an early, untested snapshot of a transceiver control stack (core + backend + frontends). Things may change quickly and APIs are not stable yet. Expect rough edges and bugs; use at your own risk and please report issues you hit. Features, tests and docs are still being written (or not).

The rig task is now driven by the controller components (state machine, handlers, and policies). Polling and retry behavior are configurable via the `[behavior]` section in the config file.

## Supported backends

- Yaesu FT-817 (feature-gated crate `trx-backend-ft817`)
- Planned: other rigs I own; contributions and reports are welcome.

## Frontends

- HTTP status/control frontend (`trx-frontend-http`)
- JSON TCP control frontend (`trx-frontend-http-json`)
- Qt/QML GUI frontend (`trx-frontend-qt`, Linux only, optional via `qt-frontend` feature)
- rigctl-compatible TCP frontend (`trx-frontend-rigctl`, listens on 127.0.0.1:4532)

## Plugin discovery

`trx-bin` can load shared-library plugins that register backends/frontends via a
`trx_register` entrypoint. Search paths:

- `./plugins`
- `~/.config/trx-rs/plugins`
- `TRX_PLUGIN_DIRS` (path-separated)

Example plugin: `examples/trx-plugin-example`

## Qt remote client

The Qt frontend can run as a remote client over the JSON TCP interface.
Configure the server with `frontends.http_json.auth.tokens` and the client with
`frontends.qt.remote.enabled`, `frontends.qt.remote.url`, and
`frontends.qt.remote.auth.token`.

## License

This project is licensed under the BSD-2-Clause license. See `LICENSES/` for bundled third-party license files.
