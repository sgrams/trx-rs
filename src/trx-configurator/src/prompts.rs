// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use dialoguer::{Confirm, Input, Select};

use crate::detect;
use crate::ConfigType;

// ── Data types returned by prompts ──────────────────────────────────────

pub struct ServerGeneral {
    pub callsign: String,
    pub log_level: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

pub struct RigSetup {
    pub model: String,
    pub access_type: String,
    pub serial_port: Option<String>,
    pub serial_baud: Option<u32>,
    pub tcp_host: Option<String>,
    pub tcp_port: Option<u16>,
    pub sdr_args: Option<String>,
}

pub struct ListenSetup {
    pub listen: String,
    pub port: u16,
}

pub struct ClientGeneral {
    pub callsign: String,
    pub log_level: String,
}

pub struct RemoteSetup {
    pub url: String,
    pub token: Option<String>,
}

pub struct FrontendsSetup {
    pub http_enabled: bool,
    pub http_port: u16,
    pub rigctl_enabled: bool,
    pub rigctl_port: u16,
}

// ── Prompt functions ────────────────────────────────────────────────────

pub fn prompt_config_type() -> ConfigType {
    let items = &["Server (trx-server.toml)", "Client (trx-client.toml)", "Combined (trx-rs.toml)"];
    let sel = Select::new()
        .with_prompt("What configuration would you like to generate?")
        .items(items)
        .default(2)
        .interact()
        .unwrap();
    match sel {
        0 => ConfigType::Server,
        1 => ConfigType::Client,
        _ => ConfigType::Combined,
    }
}

pub fn prompt_server_general() -> ServerGeneral {
    let callsign: String = Input::new()
        .with_prompt("Callsign")
        .default("N0CALL".to_string())
        .interact_text()
        .unwrap();

    let log_levels = &["trace", "debug", "info", "warn", "error"];
    let level_sel = Select::new()
        .with_prompt("Log level")
        .items(log_levels)
        .default(2)
        .interact()
        .unwrap();
    let log_level = log_levels[level_sel].to_string();

    let has_location = Confirm::new()
        .with_prompt("Set station location (latitude/longitude)?")
        .default(false)
        .interact()
        .unwrap();

    let (latitude, longitude) = if has_location {
        let lat: f64 = Input::new()
            .with_prompt("Latitude (decimal degrees, -90..90)")
            .validate_with(|input: &f64| {
                if *input >= -90.0 && *input <= 90.0 {
                    Ok(())
                } else {
                    Err("Must be between -90 and 90")
                }
            })
            .interact_text()
            .unwrap();
        let lon: f64 = Input::new()
            .with_prompt("Longitude (decimal degrees, -180..180)")
            .validate_with(|input: &f64| {
                if *input >= -180.0 && *input <= 180.0 {
                    Ok(())
                } else {
                    Err("Must be between -180 and 180")
                }
            })
            .interact_text()
            .unwrap();
        (Some(lat), Some(lon))
    } else {
        (None, None)
    };

    ServerGeneral {
        callsign,
        log_level,
        latitude,
        longitude,
    }
}

pub fn prompt_rig() -> RigSetup {
    let models = &["ft817", "ft450d", "soapysdr"];
    let model_sel = Select::new()
        .with_prompt("Rig model")
        .items(models)
        .default(0)
        .interact()
        .unwrap();
    let model = models[model_sel].to_string();

    let (access_type, serial_port, serial_baud, tcp_host, tcp_port, sdr_args) = match model.as_str()
    {
        "soapysdr" => {
            let args: String = Input::new()
                .with_prompt("SoapySDR device args (e.g. driver=rtlsdr)")
                .default("driver=rtlsdr".to_string())
                .interact_text()
                .unwrap();
            ("sdr".to_string(), None, None, None, None, Some(args))
        }
        _ => {
            let access_types = &["serial", "tcp"];
            let access_sel = Select::new()
                .with_prompt("Access type")
                .items(access_types)
                .default(0)
                .interact()
                .unwrap();
            let access_type = access_types[access_sel].to_string();

            match access_type.as_str() {
                "serial" => {
                    let port = prompt_serial_port();
                    let default_baud: u32 = match model.as_str() {
                        "ft450d" => 38400,
                        _ => 9600,
                    };
                    let baud: u32 = Input::new()
                        .with_prompt("Baud rate")
                        .default(default_baud)
                        .validate_with(|input: &u32| {
                            if [4800, 9600, 19200, 38400, 57600, 115200].contains(input) {
                                Ok(())
                            } else {
                                Err("Must be one of: 4800, 9600, 19200, 38400, 57600, 115200")
                            }
                        })
                        .interact_text()
                        .unwrap();
                    (access_type, Some(port), Some(baud), None, None, None)
                }
                "tcp" => {
                    let host: String = Input::new()
                        .with_prompt("TCP host")
                        .default("127.0.0.1".to_string())
                        .interact_text()
                        .unwrap();
                    let port: u16 = Input::new()
                        .with_prompt("TCP port")
                        .default(4530u16)
                        .validate_with(|input: &u16| {
                            if *input > 0 {
                                Ok(())
                            } else {
                                Err("Port must be > 0")
                            }
                        })
                        .interact_text()
                        .unwrap();
                    (access_type, None, None, Some(host), Some(port), None)
                }
                _ => unreachable!(),
            }
        }
    };

    RigSetup {
        model,
        access_type,
        serial_port,
        serial_baud,
        tcp_host,
        tcp_port,
        sdr_args,
    }
}

fn prompt_serial_port() -> String {
    let ports = detect::detect_serial_ports();
    if ports.is_empty() {
        Input::new()
            .with_prompt("Serial port path")
            .default("/dev/ttyUSB0".to_string())
            .interact_text()
            .unwrap()
    } else {
        let items: Vec<String> = ports
            .iter()
            .map(|(path, desc)| {
                if desc.is_empty() {
                    path.clone()
                } else {
                    format!("{} ({})", path, desc)
                }
            })
            .collect();
        let sel = Select::new()
            .with_prompt("Select serial port")
            .items(&items)
            .default(0)
            .interact()
            .unwrap();
        ports[sel].0.clone()
    }
}

pub fn prompt_listen() -> ListenSetup {
    let listen: String = Input::new()
        .with_prompt("Listen address")
        .default("127.0.0.1".to_string())
        .interact_text()
        .unwrap();

    let port: u16 = Input::new()
        .with_prompt("Listen port")
        .default(4530u16)
        .validate_with(|input: &u16| {
            if *input > 0 {
                Ok(())
            } else {
                Err("Port must be > 0")
            }
        })
        .interact_text()
        .unwrap();

    ListenSetup { listen, port }
}

pub fn prompt_client_general() -> ClientGeneral {
    let callsign: String = Input::new()
        .with_prompt("Callsign")
        .default("N0CALL".to_string())
        .interact_text()
        .unwrap();

    let log_levels = &["trace", "debug", "info", "warn", "error"];
    let level_sel = Select::new()
        .with_prompt("Log level")
        .items(log_levels)
        .default(2)
        .interact()
        .unwrap();
    let log_level = log_levels[level_sel].to_string();

    ClientGeneral {
        callsign,
        log_level,
    }
}

pub fn prompt_remote() -> RemoteSetup {
    let url: String = Input::new()
        .with_prompt("Server URL (host:port)")
        .default("localhost:4530".to_string())
        .interact_text()
        .unwrap();

    let has_token = Confirm::new()
        .with_prompt("Set auth token?")
        .default(false)
        .interact()
        .unwrap();

    let token = if has_token {
        let t: String = Input::new()
            .with_prompt("Auth token")
            .interact_text()
            .unwrap();
        Some(t)
    } else {
        None
    };

    RemoteSetup { url, token }
}

pub fn prompt_frontends() -> FrontendsSetup {
    let http_enabled = Confirm::new()
        .with_prompt("Enable HTTP web frontend?")
        .default(true)
        .interact()
        .unwrap();

    let http_port: u16 = if http_enabled {
        Input::new()
            .with_prompt("HTTP port")
            .default(8080u16)
            .validate_with(|input: &u16| {
                if *input > 0 {
                    Ok(())
                } else {
                    Err("Port must be > 0")
                }
            })
            .interact_text()
            .unwrap()
    } else {
        8080
    };

    let rigctl_enabled = Confirm::new()
        .with_prompt("Enable rigctl frontend (Hamlib-compatible)?")
        .default(false)
        .interact()
        .unwrap();

    let rigctl_port: u16 = if rigctl_enabled {
        Input::new()
            .with_prompt("rigctl port")
            .default(4532u16)
            .validate_with(|input: &u16| {
                if *input > 0 {
                    Ok(())
                } else {
                    Err("Port must be > 0")
                }
            })
            .interact_text()
            .unwrap()
    } else {
        4532
    };

    FrontendsSetup {
        http_enabled,
        http_port,
        rigctl_enabled,
        rigctl_port,
    }
}
