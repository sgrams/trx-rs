// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

mod audio_client;
mod config;
mod plugins;
mod remote_client;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use clap::Parser;
use tokio::signal;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::info;

use trx_core::audio::AudioStreamInfo;

use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::rig::{RigControl, RigRxStatus, RigStatus, RigTxStatus};
use trx_core::radio::freq::Freq;
use trx_core::DynResult;
use trx_frontend::{is_frontend_registered, registered_frontends};
use trx_core::decode::DecodedMessage;
use trx_frontend_http::{register_frontend as register_http_frontend, set_audio_channels, set_decode_channel};
use trx_frontend_http_json::{register_frontend as register_http_json_frontend, set_auth_tokens};
use trx_frontend_rigctl::register_frontend as register_rigctl_frontend;

#[cfg(feature = "appkit-frontend")]
use trx_frontend_appkit::register_frontend as register_appkit_frontend;
#[cfg(feature = "appkit-frontend")]
use trx_frontend_appkit::run_appkit_main_thread;

use config::ClientConfig;
use remote_client::{parse_remote_url, RemoteClientConfig};

const PKG_DESCRIPTION: &str = concat!(env!("CARGO_PKG_NAME"), " - remote rig client");
const RIG_TASK_CHANNEL_BUFFER: usize = 32;
#[derive(Debug, Parser)]
#[command(
    author = env!("CARGO_PKG_AUTHORS"),
    version = env!("CARGO_PKG_VERSION"),
    about = PKG_DESCRIPTION,
)]
struct Cli {
    /// Path to configuration file
    #[arg(long = "config", short = 'C', value_name = "FILE")]
    config: Option<PathBuf>,
    /// Print example configuration and exit
    #[arg(long = "print-config")]
    print_config: bool,
    /// Remote server URL (host:port)
    #[arg(short = 'u', long = "url")]
    url: Option<String>,
    /// Authentication token for the remote server
    #[arg(long = "token")]
    token: Option<String>,
    /// Poll interval in milliseconds
    #[arg(long = "poll-interval")]
    poll_interval_ms: Option<u64>,
    /// Frontend(s) to expose locally (e.g. http,rigctl)
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
    /// Optional callsign/owner label to show in the frontend
    #[arg(short = 'c', long = "callsign")]
    callsign: Option<String>,
}

/// Normalize a rig/frontend name to lowercase alphanumeric.
fn normalize_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn main() -> DynResult<()> {
    let rt = tokio::runtime::Runtime::new()?;

    #[allow(unused_variables)]
    let app_state = rt.block_on(async_init())?;

    #[cfg(feature = "appkit-frontend")]
    if app_state.has_appkit {
        // Keep a runtime context active on the main thread so that
        // tokio::spawn inside run_appkit_main_thread works.
        let _guard = rt.enter();

        // AppKit needs the process main thread. Spawn Ctrl+C handler on the
        // runtime, then hand main thread to AppKit (blocks forever).
        rt.spawn(async {
            signal::ctrl_c().await.ok();
            info!("Ctrl+C received, shutting down");
            std::process::exit(0);
        });
        run_appkit_main_thread(app_state.state_rx, app_state.rig_tx);
        unreachable!();
    }

    // No AppKit — block on Ctrl+C as before.
    rt.block_on(async {
        signal::ctrl_c().await?;
        info!("Ctrl+C received, shutting down");
        Ok(())
    })
}

/// Holds the state needed after async initialization completes.
struct AppState {
    #[allow(dead_code)]
    has_appkit: bool,
    #[cfg(feature = "appkit-frontend")]
    state_rx: watch::Receiver<RigState>,
    #[cfg(feature = "appkit-frontend")]
    rig_tx: mpsc::Sender<RigRequest>,
}

async fn async_init() -> DynResult<AppState> {
    tracing_subscriber::fmt().with_target(false).init();

    register_http_frontend();
    register_http_json_frontend();
    register_rigctl_frontend();
    #[cfg(feature = "appkit-frontend")]
    register_appkit_frontend();
    let _plugin_libs = plugins::load_plugins();

    let cli = Cli::parse();

    if cli.print_config {
        println!("{}", ClientConfig::example_toml());
        std::process::exit(0);
    }

    let (cfg, config_path) = if let Some(ref path) = cli.config {
        let cfg = ClientConfig::load_from_file(path)?;
        (cfg, Some(path.clone()))
    } else {
        ClientConfig::load_from_default_paths()?
    };

    if let Some(ref path) = config_path {
        info!("Loaded configuration from {}", path.display());
    }

    set_auth_tokens(cfg.frontends.http_json.auth.tokens.clone());

    // Resolve remote URL: CLI > config [remote] section > error
    let remote_url = cli
        .url
        .clone()
        .or_else(|| cfg.remote.url.clone())
        .ok_or("Remote URL not specified. Use --url or set [remote].url in config.")?;

    let remote_addr =
        parse_remote_url(&remote_url).map_err(|e| format!("Invalid remote URL: {}", e))?;

    let remote_token = cli
        .token
        .clone()
        .or_else(|| cfg.remote.auth.token.clone());

    let poll_interval_ms = cli
        .poll_interval_ms
        .unwrap_or(cfg.remote.poll_interval_ms);

    // Resolve frontends: CLI > config > default to http
    let frontends: Vec<String> = if let Some(ref fes) = cli.frontends {
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
        if cfg.frontends.appkit.enabled {
            fes.push("appkit".to_string());
        }
        if fes.is_empty() {
            fes.push("http".to_string());
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

    let http_listen = cli.http_listen.unwrap_or(cfg.frontends.http.listen);
    let http_port = cli.http_port.unwrap_or(cfg.frontends.http.port);
    let rigctl_listen = cli.rigctl_listen.unwrap_or(cfg.frontends.rigctl.listen);
    let rigctl_port = cli.rigctl_port.unwrap_or(cfg.frontends.rigctl.port);
    let http_json_listen = cli
        .http_json_listen
        .unwrap_or(cfg.frontends.http_json.listen);
    let http_json_port = cli.http_json_port.unwrap_or(cfg.frontends.http_json.port);
    let callsign = cli
        .callsign
        .clone()
        .or_else(|| cfg.general.callsign.clone());

    let has_appkit = frontends.iter().any(|f| f == "appkit");

    info!(
        "Starting trx-client (remote: {}, frontends: {})",
        remote_addr,
        frontends.join(", ")
    );

    let (tx, rx) = mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);

    let initial_state = RigState {
        rig_info: None,
        status: RigStatus {
            freq: Freq { hz: 144_300_000 },
            mode: trx_core::rig::state::RigMode::USB,
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
        server_callsign: None,
        server_version: None,
        server_latitude: None,
        server_longitude: None,
        aprs_decode_enabled: false,
        cw_decode_enabled: false,
        cw_auto: true,
        cw_wpm: 15,
        cw_tone_hz: 700,
        ft8_decode_enabled: false,
        aprs_decode_reset_seq: 0,
        cw_decode_reset_seq: 0,
        ft8_decode_reset_seq: 0,
    };
    let (state_tx, state_rx) = watch::channel(initial_state);

    // Extract host for audio before moving remote_addr
    let remote_host = remote_addr
        .split(':')
        .next()
        .unwrap_or("127.0.0.1")
        .to_string();

    let remote_cfg = RemoteClientConfig {
        addr: remote_addr,
        token: remote_token,
        poll_interval: Duration::from_millis(poll_interval_ms),
    };
    let _remote_handle =
        tokio::spawn(remote_client::run_remote_client(remote_cfg, rx, state_tx));

    // Audio streaming setup
    if cfg.frontends.audio.enabled {
        let (rx_audio_tx, _) = broadcast::channel::<Bytes>(256);
        let (tx_audio_tx, tx_audio_rx) = mpsc::channel::<Bytes>(64);
        let (stream_info_tx, stream_info_rx) = watch::channel::<Option<AudioStreamInfo>>(None);
        let (decode_tx, _) = broadcast::channel::<DecodedMessage>(256);

        let audio_addr = format!("{}:{}", remote_host, cfg.frontends.audio.server_port);

        set_audio_channels(rx_audio_tx.clone(), tx_audio_tx, stream_info_rx);
        set_decode_channel(decode_tx.clone());

        info!(
            "Audio enabled: connecting to {}, decode channel set",
            audio_addr
        );

        tokio::spawn(audio_client::run_audio_client(
            audio_addr,
            rx_audio_tx,
            tx_audio_rx,
            stream_info_tx,
            decode_tx,
        ));
    } else {
        info!("Audio disabled in config, decode will not be available");
    }

    // Spawn frontends (skip appkit — it will be driven from main thread)
    for frontend in &frontends {
        if frontend == "appkit" {
            continue;
        }
        let frontend_state_rx = state_rx.clone();
        let addr = match frontend.as_str() {
            "http" => SocketAddr::from((http_listen, http_port)),
            "rigctl" => SocketAddr::from((rigctl_listen, rigctl_port)),
            "httpjson" => SocketAddr::from((http_json_listen, http_json_port)),
            other => {
                return Err(format!("Frontend missing listen configuration: {}", other).into());
            }
        };
        trx_frontend::spawn_frontend(
            frontend,
            frontend_state_rx,
            tx.clone(),
            callsign.clone(),
            addr,
        )?;
    }

    Ok(AppState {
        has_appkit,
        #[cfg(feature = "appkit-frontend")]
        state_rx,
        #[cfg(feature = "appkit-frontend")]
        rig_tx: tx,
    })
}
