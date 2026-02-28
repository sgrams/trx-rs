// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

mod audio_bridge;
mod audio_client;
mod config;
mod remote_client;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::Duration;

use bytes::Bytes;
use clap::Parser;
use tokio::signal;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{error, info};

use trx_app::{init_logging, load_frontend_plugins, normalize_name};
use trx_core::audio::AudioStreamInfo;

use trx_core::decode::DecodedMessage;
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::DynResult;
use trx_frontend::{FrontendRegistrationContext, FrontendRuntimeContext};
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
    /// Target rig ID on a multi-rig remote server
    #[arg(long = "rig-id")]
    rig_id: Option<String>,
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

#[tokio::main]
async fn main() -> DynResult<()> {
    let app_state = async_init().await?;
    signal::ctrl_c().await?;
    info!("Ctrl+C received, shutting down");

    let _ = app_state.shutdown_tx.send(true);
    drop(app_state.request_tx);
    tokio::time::sleep(Duration::from_millis(400)).await;

    for handle in &app_state.task_handles {
        if !handle.is_finished() {
            handle.abort();
        }
    }
    for handle in app_state.task_handles {
        let _ = handle.await;
    }
    Ok(())
}

/// Holds the state needed after async initialization completes.
struct AppState {
    shutdown_tx: watch::Sender<bool>,
    task_handles: Vec<JoinHandle<()>>,
    request_tx: mpsc::Sender<RigRequest>,
}

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
        println!("{}", ClientConfig::example_combined_toml());
        std::process::exit(0);
    }

    let (cfg, config_path) = if let Some(ref path) = cli.config {
        let cfg = ClientConfig::load_from_file(path)?;
        (cfg, Some(path.clone()))
    } else {
        ClientConfig::load_from_default_paths()?
    };
    cfg.validate()
        .map_err(|e| format!("Invalid client configuration: {}", e))?;

    init_logging(cfg.general.log_level.as_deref());

    let frontend_ctx_ptr = NonNull::from(&mut frontend_reg_ctx).cast();
    let _plugin_libs = load_frontend_plugins(frontend_ctx_ptr);

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

    // Set HTTP frontend authentication config
    frontend_runtime.http_auth_enabled = cfg.frontends.http.auth.enabled;
    frontend_runtime.http_auth_rx_passphrase = cfg.frontends.http.auth.rx_passphrase.clone();
    frontend_runtime.http_auth_control_passphrase =
        cfg.frontends.http.auth.control_passphrase.clone();
    frontend_runtime.http_auth_tx_access_control_enabled =
        cfg.frontends.http.auth.tx_access_control_enabled;
    frontend_runtime.http_auth_session_ttl_secs = cfg.frontends.http.auth.session_ttl_min * 60;
    frontend_runtime.http_auth_cookie_secure = cfg.frontends.http.auth.cookie_secure;
    frontend_runtime.http_auth_cookie_same_site = match cfg.frontends.http.auth.cookie_same_site {
        config::CookieSameSite::Strict => "Strict".to_string(),
        config::CookieSameSite::Lax => "Lax".to_string(),
        config::CookieSameSite::None => "None".to_string(),
    };

    // Resolve remote URL: CLI > config [remote] section > error
    let remote_url = cli
        .url
        .clone()
        .or_else(|| cfg.remote.url.clone())
        .ok_or("Remote URL not specified. Use --url or set [remote].url in config.")?;

    let remote_endpoint =
        parse_remote_url(&remote_url).map_err(|e| format!("Invalid remote URL: {}", e))?;

    let remote_token = cli.token.clone().or_else(|| cfg.remote.auth.token.clone());
    let remote_rig_id = cli.rig_id.clone().or_else(|| cfg.remote.rig_id.clone());
    if let Ok(mut guard) = frontend_runtime.remote_active_rig_id.lock() {
        *guard = remote_rig_id.clone();
    }

    let poll_interval_ms = cli.poll_interval_ms.unwrap_or(cfg.remote.poll_interval_ms);

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
    frontend_runtime.owner_callsign = callsign.clone();

    info!(
        "Starting trx-client (remote: {}, frontends: {})",
        remote_endpoint.connect_addr(),
        frontends.join(", ")
    );

    let (tx, rx) = mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut task_handles: Vec<JoinHandle<()>> = Vec::new();

    let initial_state = RigState::new_uninitialized();
    let (state_tx, state_rx) = watch::channel(initial_state);

    // Extract host for audio before moving remote_addr
    let remote_host = remote_endpoint.host.clone();

    let remote_cfg = RemoteClientConfig {
        addr: remote_endpoint.connect_addr(),
        token: remote_token,
        selected_rig_id: frontend_runtime.remote_active_rig_id.clone(),
        known_rigs: frontend_runtime.remote_rigs.clone(),
        poll_interval: Duration::from_millis(poll_interval_ms),
        spectrum: frontend_runtime.spectrum.clone(),
    };
    let remote_shutdown_rx = shutdown_rx.clone();
    task_handles.push(tokio::spawn(async move {
        if let Err(e) =
            remote_client::run_remote_client(remote_cfg, rx, state_tx, remote_shutdown_rx).await
        {
            error!("Remote client error: {}", e);
        }
    }));

    // Audio streaming setup
    if cfg.frontends.audio.enabled {
        let (rx_audio_tx, _) = broadcast::channel::<Bytes>(256);
        let (tx_audio_tx, tx_audio_rx) = mpsc::channel::<Bytes>(64);
        let (stream_info_tx, stream_info_rx) = watch::channel::<Option<AudioStreamInfo>>(None);
        let (decode_tx, _) = broadcast::channel::<DecodedMessage>(256);

        frontend_runtime.audio_rx = Some(rx_audio_tx.clone());
        frontend_runtime.audio_tx = Some(tx_audio_tx);
        frontend_runtime.audio_info = Some(stream_info_rx);
        frontend_runtime.decode_rx = Some(decode_tx.clone());

        info!(
            "Audio enabled: default port {}, decode channel set",
            cfg.frontends.audio.server_port
        );

        let audio_rig_ports: HashMap<String, u16> = cfg.frontends.audio.rig_ports.clone();
        let audio_shutdown_rx = shutdown_rx.clone();
        task_handles.push(tokio::spawn(audio_client::run_audio_client(
            remote_host,
            cfg.frontends.audio.server_port,
            audio_rig_ports,
            frontend_runtime.remote_active_rig_id.clone(),
            frontend_runtime.remote_rigs.clone(),
            rx_audio_tx,
            tx_audio_rx,
            stream_info_tx,
            decode_tx,
            audio_shutdown_rx,
        )));

        if cfg.frontends.audio.bridge.enabled {
            info!("Audio bridge enabled (local virtual-device integration)");
            task_handles.push(audio_bridge::spawn_audio_bridge(
                cfg.frontends.audio.bridge.clone(),
                frontend_runtime
                    .audio_rx
                    .as_ref()
                    .expect("audio rx must be set")
                    .clone(),
                frontend_runtime
                    .audio_tx
                    .as_ref()
                    .expect("audio tx must be set")
                    .clone(),
                frontend_runtime
                    .audio_info
                    .as_ref()
                    .expect("audio info must be set")
                    .clone(),
                shutdown_rx.clone(),
            ));
        }
    } else {
        info!("Audio disabled in config, decode will not be available");
    }

    let frontend_runtime_ctx = Arc::new(frontend_runtime);

    // Spawn frontends with runtime context
    for frontend in &frontends {
        let frontend_state_rx = state_rx.clone();

        // rigctl with per-rig port mapping: spawn one listener per rig entry.
        if frontend == "rigctl" && !cfg.frontends.rigctl.rig_ports.is_empty() {
            let mut first = true;
            for (rig_id, &port) in &cfg.frontends.rigctl.rig_ports {
                let addr = SocketAddr::from((rigctl_listen, port));
                if first {
                    if let Ok(mut listen_addr) = frontend_runtime_ctx.rigctl_listen_addr.lock() {
                        *listen_addr = Some(addr);
                    }
                    first = false;
                }
                // Proxy channel: inject rig_id_override before forwarding to main tx.
                let (proxy_tx, mut proxy_rx) =
                    mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);
                let main_tx = tx.clone();
                let rig_id_owned = rig_id.clone();
                tokio::spawn(async move {
                    while let Some(req) = proxy_rx.recv().await {
                        let forwarded = RigRequest {
                            cmd: req.cmd,
                            respond_to: req.respond_to,
                            rig_id_override: Some(rig_id_owned.clone()),
                        };
                        let _ = main_tx.send(forwarded).await;
                    }
                });
                info!("rigctl frontend for rig '{}' on {}", rig_id, addr);
                frontend_reg_ctx.spawn_frontend(
                    frontend,
                    state_rx.clone(),
                    proxy_tx,
                    callsign.clone(),
                    addr,
                    frontend_runtime_ctx.clone(),
                )?;
            }
            continue;
        }

        let addr = match frontend.as_str() {
            "http" => SocketAddr::from((http_listen, http_port)),
            "rigctl" => SocketAddr::from((rigctl_listen, rigctl_port)),
            "httpjson" => SocketAddr::from((http_json_listen, http_json_port)),
            other => {
                return Err(format!("Frontend missing listen configuration: {}", other).into());
            }
        };
        if frontend == "rigctl" {
            if let Ok(mut listen_addr) = frontend_runtime_ctx.rigctl_listen_addr.lock() {
                *listen_addr = Some(addr);
            }
        }
        frontend_reg_ctx.spawn_frontend(
            frontend,
            frontend_state_rx,
            tx.clone(),
            callsign.clone(),
            addr,
            frontend_runtime_ctx.clone(),
        )?;
    }

    Ok(AppState {
        shutdown_tx,
        task_handles,
        request_tx: tx,
    })
}
