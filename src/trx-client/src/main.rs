// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

mod audio_client;
mod config;
mod remote_client;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use clap::Parser;
use tokio::signal;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::info;

use trx_app::{init_logging, load_plugins, normalize_name};
use trx_core::audio::AudioStreamInfo;

use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::DynResult;
use trx_frontend::{snapshot_bootstrap_context, FrontendRegistrationContext, FrontendRuntimeContext};
use trx_core::decode::DecodedMessage;
use trx_frontend_http::register_frontend_on as register_http_frontend;
use trx_frontend_http_json::register_frontend_on as register_http_json_frontend;
use trx_frontend_rigctl::register_frontend_on as register_rigctl_frontend;

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

fn main() -> DynResult<()> {
    let rt = tokio::runtime::Runtime::new()?;

    let _app_state = rt.block_on(async_init())?;

    rt.block_on(async {
        signal::ctrl_c().await?;
        info!("Ctrl+C received, shutting down");
        Ok(())
    })
}

/// Holds the state needed after async initialization completes.
struct AppState;

async fn async_init() -> DynResult<AppState> {
    use std::sync::Arc;

    // Phase 3: Create bootstrap context for explicit initialization.
    // This replaces reliance on global mutable state by threading context through spawn_frontend.
    let mut frontend_reg_ctx = FrontendRegistrationContext::new();
    let mut frontend_runtime = FrontendRuntimeContext::new();

    register_http_frontend(&mut frontend_reg_ctx);
    register_http_json_frontend(&mut frontend_reg_ctx);
    register_rigctl_frontend(&mut frontend_reg_ctx);

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

    init_logging(cfg.general.log_level.as_deref());

    let _plugin_libs = load_plugins();
    frontend_reg_ctx.extend_from(&snapshot_bootstrap_context());

    if let Some(ref path) = config_path {
        info!("Loaded configuration from {}", path.display());
    }

    frontend_runtime.auth_tokens = cfg
        .frontends
        .http_json
        .auth
        .tokens
        .iter()
        .filter(|t| !t.is_empty())
        .cloned()
        .collect();

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
        if fes.is_empty() {
            fes.push("http".to_string());
        }
        fes
    };
    for name in &frontends {
        if !frontend_reg_ctx.is_frontend_registered(name) {
            return Err(format!(
                "Unknown frontend: {} (available: {})",
                name,
                frontend_reg_ctx.registered_frontends().join(", ")
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

    info!(
        "Starting trx-client (remote: {}, frontends: {})",
        remote_addr,
        frontends.join(", ")
    );

    let (tx, rx) = mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);

    let initial_state = RigState::new_uninitialized();
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

        frontend_runtime.audio_rx = Some(rx_audio_tx.clone());
        frontend_runtime.audio_tx = Some(tx_audio_tx);
        frontend_runtime.audio_info = Some(stream_info_rx);
        frontend_runtime.decode_rx = Some(decode_tx.clone());

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

    let frontend_runtime_ctx = Arc::new(frontend_runtime);

    // Spawn frontends with runtime context
    for frontend in &frontends {
        let frontend_state_rx = state_rx.clone();
        let addr = match frontend.as_str() {
            "http" => SocketAddr::from((http_listen, http_port)),
            "rigctl" => SocketAddr::from((rigctl_listen, rigctl_port)),
            "httpjson" => SocketAddr::from((http_json_listen, http_json_port)),
            other => {
                return Err(format!("Frontend missing listen configuration: {}", other).into());
            }
        };
        frontend_reg_ctx.spawn_frontend(
            frontend,
            frontend_state_rx,
            tx.clone(),
            callsign.clone(),
            addr,
            frontend_runtime_ctx.clone(),
        )?;
    }

    Ok(AppState)
}
