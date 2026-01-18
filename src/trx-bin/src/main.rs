// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use tokio::signal;
use tokio::sync::{mpsc, watch};
use tracing::info;

mod config;
mod error;
mod plugins;
mod remote_client;
mod rig_task;

use trx_backend::{
    is_backend_registered, register_builtin_backends, registered_backends, RigAccess,
};
use trx_core::radio::freq::Freq;
use trx_core::rig::controller::{AdaptivePolling, ExponentialBackoff};
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::rig::{RigControl, RigRxStatus, RigStatus, RigTxStatus};
use trx_core::DynResult;
use trx_frontend::{is_frontend_registered, registered_frontends};
use trx_frontend_http::register_frontend as register_http_frontend;
use trx_frontend_http_json::{register_frontend as register_http_json_frontend, set_auth_tokens};
use trx_frontend_rigctl::register_frontend as register_rigctl_frontend;

#[cfg(feature = "qt-frontend")]
use trx_frontend_qt::register_frontend as register_qt_frontend;

const PKG_DESCRIPTION: &str = concat!(env!("CARGO_PKG_NAME"), " - ", env!("CARGO_PKG_DESCRIPTION"));
const PKG_LONG_ABOUT: &str = concat!(
    env!("CARGO_PKG_DESCRIPTION"),
    "\nHomepage: ",
    env!("CARGO_PKG_HOMEPAGE")
);
const RIG_TASK_CHANNEL_BUFFER: usize = 32;
const QT_FRONTEND_LISTEN_ADDR: ([u8; 4], u16) = ([127, 0, 0, 1], 0);
const RETRY_MAX_DELAY_SECS: u64 = 2;

#[derive(Debug, Parser)]
#[command(
    author = env!("CARGO_PKG_AUTHORS"),
    version = env!("CARGO_PKG_VERSION"),
    about = PKG_DESCRIPTION,
    long_about = PKG_LONG_ABOUT
)]
struct Cli {
    /// Path to configuration file (default: search trx-rs.toml, ~/.config/trx-rs/config.toml, /etc/trx-rs/config.toml)
    #[arg(long = "config", short = 'C', value_name = "FILE")]
    config: Option<PathBuf>,
    /// Print example configuration and exit
    #[arg(long = "print-config")]
    print_config: bool,
    /// Rig backend to use (e.g. ft817)
    #[arg(short = 'r', long = "rig")]
    rig: Option<String>,
    /// Access method to reach the rig CAT interface
    #[arg(short = 'a', long = "access", value_enum)]
    access: Option<AccessKind>,
    /// Frontend(s) to expose for control/status (e.g. http,rigctl)
    #[arg(short = 'f', long = "frontend", value_delimiter = ',', num_args = 1..)]
    frontends: Option<Vec<String>>,
    /// HTTP frontend listen address
    #[arg(long = "http-listen")]
    http_listen: Option<IpAddr>,
    /// HTTP frontend listen port
    #[arg(long = "http-port")]
    http_port: Option<u16>,
    /// rigctl frontend listen address
    #[arg(long = "rigctl-listen")]
    rigctl_listen: Option<IpAddr>,
    /// rigctl frontend listen port
    #[arg(long = "rigctl-port")]
    rigctl_port: Option<u16>,
    /// JSON TCP frontend listen address
    #[arg(long = "http-json-listen")]
    http_json_listen: Option<IpAddr>,
    /// JSON TCP frontend listen port
    #[arg(long = "http-json-port")]
    http_json_port: Option<u16>,
    /// Rig CAT address:
    /// when access is serial: <path> <baud>;
    /// when access is TCP: <host>:<port>
    #[arg(value_name = "RIG_ADDR")]
    rig_addr: Option<String>,
    /// Optional callsign/owner label to show in the frontend
    #[arg(short = 'c', long = "callsign")]
    callsign: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AccessKind {
    Serial,
    Tcp,
}

fn normalize_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Parse a serial rig address of the form "<path> <baud>".
fn parse_serial_addr(addr: &str) -> DynResult<(String, u32)> {
    let mut parts = addr.split_whitespace();
    let path = parts
        .next()
        .ok_or("Serial rig address must be '<path> <baud>'")?;
    let baud_str = parts
        .next()
        .ok_or("Serial rig address must be '<path> <baud>'")?;
    if parts.next().is_some() {
        return Err("Serial rig address must be '<path> <baud>' (got extra data)".into());
    }
    let baud: u32 = baud_str
        .parse()
        .map_err(|e| format!("Invalid baud '{}': {}", baud_str, e))?;
    Ok((path.to_string(), baud))
}

/// Resolved configuration after merging config file and CLI arguments.
struct ResolvedConfig {
    rig: String,
    access: RigAccess,
    frontends: Vec<String>,
    http_listen: IpAddr,
    http_port: u16,
    rigctl_listen: IpAddr,
    rigctl_port: u16,
    http_json_listen: IpAddr,
    http_json_port: u16,
    callsign: Option<String>,
}

impl ResolvedConfig {
    /// Build resolved config from CLI args and config file.
    fn from_cli_and_config(
        cli: &Cli,
        cfg: &config::Config,
        qt_remote_enabled: bool,
    ) -> DynResult<Self> {
        // Resolve rig model: CLI > config > error
        let rig_str = cli.rig.clone().or_else(|| cfg.rig.model.clone());
        let rig = match rig_str.as_deref() {
            Some(name) => normalize_name(name),
            None if qt_remote_enabled => "remote".to_string(),
            None => {
                return Err(
                    "Rig model not specified. Use --rig or set [rig].model in config.".into(),
                )
            }
        };
        if !qt_remote_enabled && !is_backend_registered(&rig) {
            return Err(format!(
                "Unknown rig model: {} (available: {})",
                rig,
                registered_backends().join(", ")
            )
            .into());
        }

        let access = if qt_remote_enabled {
            RigAccess::Tcp {
                addr: "remote".to_string(),
            }
        } else {
            // Resolve access method: CLI > config > default to serial
            let access_type = cli
                .access
                .as_ref()
                .map(|a| match a {
                    AccessKind::Serial => "serial",
                    AccessKind::Tcp => "tcp",
                })
                .or(cfg.rig.access.access_type.as_deref());

            match access_type {
                Some("serial") | None => {
                    // Try CLI rig_addr first, then config
                    let (path, baud) = if let Some(ref addr) = cli.rig_addr {
                        parse_serial_addr(addr)?
                    } else if let (Some(port), Some(baud)) =
                        (&cfg.rig.access.port, cfg.rig.access.baud)
                    {
                        (port.clone(), baud)
                    } else {
                        return Err("Serial access requires port and baud. Use '<path> <baud>' argument or set [rig.access].port and .baud in config.".into());
                    };
                    RigAccess::Serial { path, baud }
                }
                Some("tcp") => {
                    let addr = if let Some(ref addr) = cli.rig_addr {
                        addr.clone()
                    } else if let (Some(host), Some(port)) =
                        (&cfg.rig.access.host, cfg.rig.access.tcp_port)
                    {
                        format!("{}:{}", host, port)
                    } else {
                        return Err("TCP access requires host:port. Use argument or set [rig.access].host and .tcp_port in config.".into());
                    };
                    RigAccess::Tcp { addr }
                }
                Some(other) => return Err(format!("Unknown access type: {}", other).into()),
            }
        };

        // Resolve frontends: CLI > config > default
        let frontends = if let Some(ref fes) = cli.frontends {
            fes.iter().map(|f| normalize_name(f)).collect()
        } else {
            let mut fes = Vec::new();
            if cfg.frontends.http.enabled {
                fes.push("http".to_string());
            }
            if cfg.frontends.rigctl.enabled {
                fes.push("rigctl".to_string());
            }
            if cfg.frontends.http_json.enabled {
                fes.push("httpjson".to_string());
            }
            if cfg.frontends.qt.enabled {
                fes.push("qt".to_string());
            }
            if fes.is_empty() {
                fes.push("http".to_string()); // Default
            }
            fes
        };
        for name in &frontends {
            if !is_frontend_registered(name) {
                return Err(format!(
                    "Unknown frontend: {} (available: {})",
                    name,
                    registered_frontends().join(", ")
                )
                .into());
            }
        }

        // Resolve HTTP settings: CLI > config
        let http_listen = cli.http_listen.unwrap_or(cfg.frontends.http.listen);
        let http_port = cli.http_port.unwrap_or(cfg.frontends.http.port);

        // Resolve rigctl settings: CLI > config
        let rigctl_listen = cli.rigctl_listen.unwrap_or(cfg.frontends.rigctl.listen);
        let rigctl_port = cli.rigctl_port.unwrap_or(cfg.frontends.rigctl.port);

        // Resolve JSON TCP settings: CLI > config
        let http_json_listen = cli
            .http_json_listen
            .unwrap_or(cfg.frontends.http_json.listen);
        let http_json_port = cli.http_json_port.unwrap_or(cfg.frontends.http_json.port);

        // Resolve callsign: CLI > config
        let callsign = cli
            .callsign
            .clone()
            .or_else(|| cfg.general.callsign.clone());

        Ok(Self {
            rig,
            access,
            frontends,
            http_listen,
            http_port,
            rigctl_listen,
            rigctl_port,
            http_json_listen,
            http_json_port,
            callsign,
        })
    }
}

#[tokio::main]
async fn main() -> DynResult<()> {
    init_tracing();
    register_builtin_backends();
    let _plugin_libs = plugins::load_plugins();
    register_http_frontend();
    register_http_json_frontend();
    #[cfg(feature = "qt-frontend")]
    register_qt_frontend();
    register_rigctl_frontend();

    let cli = Cli::parse();

    // Handle --print-config
    if cli.print_config {
        println!("{}", config::Config::example_toml());
        return Ok(());
    }

    // Load configuration file
    let (cfg, config_path) = if let Some(ref path) = cli.config {
        let cfg = config::Config::load_from_file(path)?;
        (cfg, Some(path.clone()))
    } else {
        config::Config::load_from_default_paths()?
    };

    if let Some(ref path) = config_path {
        info!("Loaded configuration from {}", path.display());
    }

    set_auth_tokens(cfg.frontends.http_json.auth.tokens.clone());

    let qt_remote_enabled = cfg.frontends.qt.enabled && cfg.frontends.qt.remote.enabled;
    if qt_remote_enabled
        && cfg
            .frontends
            .qt
            .remote
            .url
            .as_deref()
            .unwrap_or("")
            .is_empty()
    {
        return Err("Qt remote mode enabled but frontends.qt.remote.url is missing".into());
    }

    // Merge CLI and config
    let resolved = ResolvedConfig::from_cli_and_config(&cli, &cfg, qt_remote_enabled)?;

    // Log startup info
    if qt_remote_enabled {
        info!("Starting trxd in Qt remote client mode");
    } else {
        match &resolved.access {
            RigAccess::Serial { path, baud } => {
                info!(
                    "Starting trxd (rig: {}, access: serial {} @ {} baud)",
                    resolved.rig, path, baud
                );
            }
            RigAccess::Tcp { addr } => {
                info!(
                    "Starting trxd (rig: {}, access: tcp {})",
                    resolved.rig, addr
                );
            }
        }
    }
    // Channel used to communicate with the rig task.
    let (tx, rx) = mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);
    let initial_state = RigState {
        rig_info: None,
        status: RigStatus {
            freq: Freq {
                hz: cfg.rig.initial_freq_hz,
            },
            mode: cfg.rig.initial_mode.clone(),
            tx_en: false,
            vfo: None,
            tx: Some(RigTxStatus {
                power: None,
                limit: None,
                swr: None,
                alc: None,
            }),
            rx: Some(RigRxStatus { sig: None }),
            lock: Some(false),
        },
        initialized: false,
        control: RigControl {
            rpt_offset_hz: None,
            ctcss_hz: None,
            dcs_code: None,
            lock: Some(false),
            clar_hz: None,
            clar_on: None,
            enabled: Some(false),
        },
    };
    let (state_tx, state_rx) = watch::channel(initial_state.clone());

    if qt_remote_enabled {
        let remote_addr = remote_client::parse_remote_url(
            cfg.frontends.qt.remote.url.as_deref().unwrap_or_default(),
        )
        .map_err(|e| format!("Invalid Qt remote URL: {}", e))?;
        let remote_cfg = remote_client::RemoteClientConfig {
            addr: remote_addr,
            token: cfg.frontends.qt.remote.auth.token.clone(),
            poll_interval: Duration::from_millis(750),
        };
        let _remote_handle =
            tokio::spawn(remote_client::run_remote_client(remote_cfg, rx, state_tx));
    } else {
        // Spawn the rig task (controller-based implementation).
        let rig_task_config = rig_task::RigTaskConfig {
            rig_model: resolved.rig,
            access: resolved.access,
            polling: AdaptivePolling::new(
                Duration::from_millis(cfg.behavior.poll_interval_ms),
                Duration::from_millis(cfg.behavior.poll_interval_tx_ms),
            ),
            retry: ExponentialBackoff::new(
                cfg.behavior.max_retries.max(1),
                Duration::from_millis(cfg.behavior.retry_base_delay_ms),
                Duration::from_secs(RETRY_MAX_DELAY_SECS),
            ),
            initial_freq_hz: cfg.rig.initial_freq_hz,
            initial_mode: cfg.rig.initial_mode.clone(),
        };
        let _rig_handle = tokio::spawn(rig_task::run_rig_task(rig_task_config, rx, state_tx));
    }

    // Start frontends.
    for frontend in &resolved.frontends {
        let frontend_state_rx = state_rx.clone();
        let addr = match frontend.as_str() {
            "http" => SocketAddr::from((resolved.http_listen, resolved.http_port)),
            "rigctl" => SocketAddr::from((resolved.rigctl_listen, resolved.rigctl_port)),
            "httpjson" => SocketAddr::from((resolved.http_json_listen, resolved.http_json_port)),
            "qt" => SocketAddr::from(QT_FRONTEND_LISTEN_ADDR),
            other => {
                return Err(format!("Frontend missing listen configuration: {}", other).into());
            }
        };
        trx_frontend::spawn_frontend(
            frontend,
            frontend_state_rx,
            tx.clone(),
            resolved.callsign.clone(),
            addr,
        )?;
    }

    signal::ctrl_c().await?;
    info!("Ctrl+C received, shutting down");

    Ok(())
}

/// Initialize logging/tracing.
fn init_tracing() {
    // Uses default formatting and RUST_LOG if available.
    tracing_subscriber::fmt().with_target(false).init();
}
