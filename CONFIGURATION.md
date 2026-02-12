# Configuration

This document lists all currently supported configuration options for `trx-server` and `trx-client`.

## File Locations

### `trx-server`
Configuration lookup order:
1. `--config <FILE>`
2. `./trx-server.toml`
3. `~/.config/trx-rs/server.toml`
4. `/etc/trx-rs/server.toml`

### `trx-client`
Configuration lookup order:
1. `--config <FILE>`
2. `./trx-client.toml`
3. `~/.config/trx-rs/client.toml`
4. `/etc/trx-rs/client.toml`

CLI options override file values.

## Environment Variables

- `TRX_PLUGIN_DIRS`: additional plugin directories (path-separated), used by both server and client.

## `trx-server` Options

### `[general]`
- `callsign` (`string`, default: `"N0CALL"`)
- `log_level` (`string`, optional): one of `trace|debug|info|warn|error`
- `latitude` (`float`, optional): `-90..=90`
- `longitude` (`float`, optional): `-180..=180`

Notes:
- `latitude` and `longitude` must be set together or both omitted.

### `[rig]`
- `model` (`string`, required effectively unless provided by CLI `--rig`)
- `initial_freq_hz` (`u64`, default: `144300000`, must be `> 0`)
- `initial_mode` (`string`, default: `"USB"`): one of `LSB|USB|CW|CWR|AM|WFM|FM|DIG|PKT`

### `[rig.access]`
- `type` (`string`, default behavior: `serial` if omitted): `serial|tcp`
- Serial mode:
- `port` (`string`)
- `baud` (`u32`)
- TCP mode:
- `host` (`string`)
- `tcp_port` (`u16`)

Notes:
- For `serial`, both `port` and `baud` are required.
- For `tcp`, both `host` and `tcp_port` are required.

### `[behavior]`
- `poll_interval_ms` (`u64`, default: `500`, must be `> 0`)
- `poll_interval_tx_ms` (`u64`, default: `100`, must be `> 0`)
- `max_retries` (`u32`, default: `3`, must be `> 0`)
- `retry_base_delay_ms` (`u64`, default: `100`, must be `> 0`)

### `[listen]`
- `enabled` (`bool`, default: `true`)
- `listen` (`ip`, default: `127.0.0.1`)
- `port` (`u16`, default: `4532`, must be `> 0` when enabled)

### `[listen.auth]`
- `tokens` (`string[]`, default: `[]`)

Notes:
- Empty token strings are invalid.
- Empty list means no auth required.

### `[audio]`
- `enabled` (`bool`, default: `true`)
- `listen` (`ip`, default: `127.0.0.1`)
- `port` (`u16`, default: `4533`, must be `> 0` when enabled)
- `rx_enabled` (`bool`, default: `true`)
- `tx_enabled` (`bool`, default: `true`)
- `device` (`string`, optional)
- `sample_rate` (`u32`, default: `48000`, valid: `8000..=192000`)
- `channels` (`u8`, default: `1`, valid: `1|2`)
- `frame_duration_ms` (`u16`, default: `20`, valid: `3|5|10|20|40|60`)
- `bitrate_bps` (`u32`, default: `24000`, must be `> 0`)

Notes:
- When `[audio].enabled = true`, at least one of `rx_enabled` or `tx_enabled` must be true.

### `[pskreporter]`
- `enabled` (`bool`, default: `false`)
- `host` (`string`, default: `"report.pskreporter.info"`, must not be empty when enabled)
- `port` (`u16`, default: `4739`, must be `> 0` when enabled)
- `receiver_locator` (`string`, optional)

Notes:
- If `receiver_locator` is omitted, server tries deriving it from `[general].latitude`/`longitude`.
- PSK Reporter software ID is hardcoded to: `trx-server v<version> by SP2SJG`.

## `trx-client` Options

### `[general]`
- `callsign` (`string`, default: `"N0CALL"`)
- `log_level` (`string`, optional): one of `trace|debug|info|warn|error`

### `[remote]`
- `url` (`string`, optional in file but required at runtime unless provided by CLI `--url`)
- `poll_interval_ms` (`u64`, default: `750`, must be `> 0`)

### `[remote.auth]`
- `token` (`string`, optional)

Notes:
- If provided, token must not be empty/whitespace.

### `[frontends.http]`
- `enabled` (`bool`, default: `true`)
- `listen` (`ip`, default: `127.0.0.1`)
- `port` (`u16`, default: `8080`, must be `> 0` when enabled)

### `[frontends.rigctl]`
- `enabled` (`bool`, default: `false`)
- `listen` (`ip`, default: `127.0.0.1`)
- `port` (`u16`, default: `4532`, must be `> 0` when enabled)

### `[frontends.http_json]`
- `enabled` (`bool`, default: `true`)
- `listen` (`ip`, default: `127.0.0.1`)
- `port` (`u16`, default: `0`)
- `auth.tokens` (`string[]`, default: `[]`)

Notes:
- `port = 0` means ephemeral bind (allowed).
- Empty token strings are invalid.

### `[frontends.audio]`
- `enabled` (`bool`, default: `true`)
- `server_port` (`u16`, default: `4533`, must be `> 0` when enabled)

## CLI Override Summary

### `trx-server`
- `--config`, `--print-config`
- `--rig`, `--access`, positional `RIG_ADDR`
- `--callsign`
- `--listen`, `--port` (JSON listener)

### `trx-client`
- `--config`, `--print-config`
- `--url`, `--token`, `--poll-interval`
- `--frontend` (comma-separated)
- `--http-listen`, `--http-port`
- `--rigctl-listen`, `--rigctl-port`
- `--http-json-listen`, `--http-json-port`
- `--callsign`
