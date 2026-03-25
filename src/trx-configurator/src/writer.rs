// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::path::Path;

use toml_edit::{value, DocumentMut, Item, Table};

use crate::prompts::{
    ClientGeneral, FrontendsSetup, ListenSetup, RemoteSetup, RigSetup, ServerGeneral,
};
use crate::ConfigType;

// ── Helpers ─────────────────────────────────────────────────────────────

fn commented(table: &mut Table, key: &str, val: Item, comment: &str) {
    table.insert(key, val);
    if let Some(mut kv) = table.key_mut(key) {
        kv.leaf_decor_mut()
            .set_prefix(format!("# {}\n", comment));
    }
}

fn header_comment(table: &mut Table, comment: &str) {
    table.decor_mut().set_prefix(format!("\n# {}\n", comment));
}

// ── Server document builder ─────────────────────────────────────────────

fn build_server_tables(
    general: ServerGeneral,
    rig: RigSetup,
    listen: ListenSetup,
) -> Table {
    let mut root = Table::new();

    // [general]
    {
        let mut t = Table::new();
        header_comment(&mut t, "General settings");
        commented(&mut t, "callsign", value(&general.callsign), "Station callsign");
        commented(&mut t, "log_level", value(&general.log_level), "Log level (trace, debug, info, warn, error)");
        if let Some(lat) = general.latitude {
            commented(&mut t, "latitude", value(lat), "Station latitude (decimal degrees, WGS84)");
        }
        if let Some(lon) = general.longitude {
            commented(&mut t, "longitude", value(lon), "Station longitude (decimal degrees, WGS84)");
        }
        root.insert("general", Item::Table(t));
    }

    // [rig]
    {
        let mut t = Table::new();
        header_comment(&mut t, "Rig backend configuration");
        commented(&mut t, "model", value(&rig.model), "Rig model (ft817, ft450d, soapysdr)");
        commented(&mut t, "initial_freq_hz", value(144_300_000i64), "Initial frequency in Hz");
        commented(&mut t, "initial_mode", value("USB"), "Initial mode");

        // [rig.access]
        let mut access = Table::new();
        header_comment(&mut access, "Rig access method");
        commented(&mut access, "type", value(&rig.access_type), "Access type: serial, tcp, or sdr");
        match rig.access_type.as_str() {
            "serial" => {
                if let Some(port) = &rig.serial_port {
                    commented(&mut access, "port", value(port.as_str()), "Serial port path");
                }
                if let Some(baud) = rig.serial_baud {
                    commented(&mut access, "baud", value(baud as i64), "Baud rate");
                }
            }
            "tcp" => {
                if let Some(host) = &rig.tcp_host {
                    commented(&mut access, "host", value(host.as_str()), "Remote host");
                }
                if let Some(port) = rig.tcp_port {
                    commented(&mut access, "tcp_port", value(port as i64), "Remote port");
                }
            }
            "sdr" => {
                if let Some(args) = &rig.sdr_args {
                    commented(&mut access, "args", value(args.as_str()), "SoapySDR device args string");
                }
            }
            _ => {}
        }
        t.insert("access", Item::Table(access));
        root.insert("rig", Item::Table(t));
    }

    // [behavior]
    {
        let mut t = Table::new();
        header_comment(&mut t, "Polling and retry behavior");
        commented(&mut t, "poll_interval_ms", value(500i64), "Rig polling interval (ms)");
        commented(&mut t, "poll_interval_tx_ms", value(100i64), "Polling interval during TX (ms)");
        commented(&mut t, "max_retries", value(3i64), "Maximum retry attempts");
        commented(&mut t, "retry_base_delay_ms", value(100i64), "Base retry delay (ms)");
        root.insert("behavior", Item::Table(t));
    }

    // [listen]
    {
        let mut t = Table::new();
        header_comment(&mut t, "JSON TCP listener for client connections");
        commented(&mut t, "enabled", value(true), "Enable the TCP listener");
        commented(&mut t, "listen", value(&listen.listen), "IP address to listen on");
        commented(&mut t, "port", value(listen.port as i64), "TCP port for client connections");
        root.insert("listen", Item::Table(t));
    }

    // [audio]
    {
        let mut t = Table::new();
        header_comment(&mut t, "Audio streaming");
        commented(&mut t, "enabled", value(true), "Enable audio streaming");
        commented(&mut t, "listen", value("127.0.0.1"), "Audio listen address");
        commented(&mut t, "port", value(4531i64), "Audio TCP port");
        commented(&mut t, "sample_rate", value(48000i64), "Sample rate in Hz");
        commented(&mut t, "channels", value(2i64), "Channel count (1 = mono, 2 = stereo)");
        commented(&mut t, "frame_duration_ms", value(20i64), "Opus frame duration (ms)");
        commented(&mut t, "bitrate_bps", value(256000i64), "Opus bitrate (bps)");
        root.insert("audio", Item::Table(t));
    }

    root
}

pub fn build_server(
    general: ServerGeneral,
    rig: RigSetup,
    listen: ListenSetup,
) -> DocumentMut {
    let mut doc = DocumentMut::new();
    doc.decor_mut().set_prefix("# trx-server configuration\n# Generated by trx-configurator\n");
    let tables = build_server_tables(general, rig, listen);
    for (key, item) in tables.iter() {
        doc.insert(key, item.clone());
    }
    doc
}

// ── Client document builder ─────────────────────────────────────────────

fn build_client_tables(
    general: ClientGeneral,
    remote: RemoteSetup,
    frontends: FrontendsSetup,
) -> Table {
    let mut root = Table::new();

    // [general]
    {
        let mut t = Table::new();
        header_comment(&mut t, "General settings");
        commented(&mut t, "callsign", value(&general.callsign), "Station callsign");
        commented(&mut t, "log_level", value(&general.log_level), "Log level (trace, debug, info, warn, error)");
        root.insert("general", Item::Table(t));
    }

    // [remote]
    {
        let mut t = Table::new();
        header_comment(&mut t, "Remote server connection");
        commented(&mut t, "url", value(&remote.url), "Server address (host:port)");
        commented(&mut t, "poll_interval_ms", value(750i64), "State poll interval (ms)");

        let mut auth = Table::new();
        if let Some(token) = &remote.token {
            commented(&mut auth, "token", value(token.as_str()), "Auth token");
        }
        if !auth.is_empty() {
            t.insert("auth", Item::Table(auth));
        }
        root.insert("remote", Item::Table(t));
    }

    // [frontends.http]
    {
        let mut frontends_table = Table::new();
        header_comment(&mut frontends_table, "Frontend configuration");

        let mut http = Table::new();
        commented(&mut http, "enabled", value(frontends.http_enabled), "Enable HTTP web frontend");
        commented(&mut http, "listen", value("127.0.0.1"), "Listen address");
        commented(&mut http, "port", value(frontends.http_port as i64), "HTTP port");
        frontends_table.insert("http", Item::Table(http));

        let mut rigctl = Table::new();
        commented(&mut rigctl, "enabled", value(frontends.rigctl_enabled), "Enable Hamlib rigctl frontend");
        commented(&mut rigctl, "listen", value("127.0.0.1"), "Listen address");
        commented(&mut rigctl, "port", value(frontends.rigctl_port as i64), "rigctl port");
        frontends_table.insert("rigctl", Item::Table(rigctl));

        let mut http_json = Table::new();
        commented(&mut http_json, "enabled", value(true), "Enable JSON-over-TCP frontend");
        commented(&mut http_json, "listen", value("127.0.0.1"), "Listen address");
        commented(&mut http_json, "port", value(0i64), "Port (0 = ephemeral)");
        frontends_table.insert("http_json", Item::Table(http_json));

        let mut audio = Table::new();
        commented(&mut audio, "enabled", value(true), "Enable audio client");
        commented(&mut audio, "server_port", value(4531i64), "Server audio port");
        frontends_table.insert("audio", Item::Table(audio));

        root.insert("frontends", Item::Table(frontends_table));
    }

    root
}

pub fn build_client(
    general: ClientGeneral,
    remote: RemoteSetup,
    frontends: FrontendsSetup,
) -> DocumentMut {
    let mut doc = DocumentMut::new();
    doc.decor_mut().set_prefix("# trx-client configuration\n# Generated by trx-configurator\n");
    let tables = build_client_tables(general, remote, frontends);
    for (key, item) in tables.iter() {
        doc.insert(key, item.clone());
    }
    doc
}

// ── Combined document builder ───────────────────────────────────────────

pub fn build_combined(
    s_general: ServerGeneral,
    rig: RigSetup,
    listen: ListenSetup,
    c_general: ClientGeneral,
    remote: RemoteSetup,
    frontends: FrontendsSetup,
) -> DocumentMut {
    let mut doc = DocumentMut::new();
    doc.decor_mut().set_prefix("# trx-rs combined configuration\n# Generated by trx-configurator\n");

    let server = build_server_tables(s_general, rig, listen);
    let mut server_item = Item::Table(server);
    if let Some(t) = server_item.as_table_mut() {
        header_comment(t, "Server configuration");
    }
    doc.insert("trx-server", server_item);

    let client = build_client_tables(c_general, remote, frontends);
    let mut client_item = Item::Table(client);
    if let Some(t) = client_item.as_table_mut() {
        header_comment(t, "Client configuration");
    }
    doc.insert("trx-client", client_item);

    doc
}

// ── Default builder ─────────────────────────────────────────────────────

pub fn build_default(config_type: ConfigType) -> DocumentMut {
    let s_general = ServerGeneral {
        callsign: "N0CALL".to_string(),
        log_level: "info".to_string(),
        latitude: None,
        longitude: None,
    };
    let rig = RigSetup {
        model: "ft817".to_string(),
        access_type: "serial".to_string(),
        serial_port: Some("/dev/ttyUSB0".to_string()),
        serial_baud: Some(9600),
        tcp_host: None,
        tcp_port: None,
        sdr_args: None,
    };
    let listen = ListenSetup {
        listen: "127.0.0.1".to_string(),
        port: 4530,
    };
    let c_general = ClientGeneral {
        callsign: "N0CALL".to_string(),
        log_level: "info".to_string(),
    };
    let remote = RemoteSetup {
        url: "localhost:4530".to_string(),
        token: None,
    };
    let frontends = FrontendsSetup {
        http_enabled: true,
        http_port: 8080,
        rigctl_enabled: false,
        rigctl_port: 4532,
    };

    match config_type {
        ConfigType::Server => build_server(s_general, rig, listen),
        ConfigType::Client => build_client(c_general, remote, frontends),
        ConfigType::Combined => build_combined(s_general, rig, listen, c_general, remote, frontends),
    }
}

// ── File writer ─────────────────────────────────────────────────────────

pub fn write_file(doc: &DocumentMut, path: &Path) -> Result<(), String> {
    if path.exists() {
        let overwrite = dialoguer::Confirm::new()
            .with_prompt(format!("{} already exists. Overwrite?", path.display()))
            .default(false)
            .interact()
            .map_err(|e| e.to_string())?;
        if !overwrite {
            return Err("Aborted.".to_string());
        }
    }

    std::fs::write(path, doc.to_string()).map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    println!("Wrote {}", path.display());
    Ok(())
}
