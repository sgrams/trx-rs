// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

mod audio_bridge;
mod audio_client;
mod config;
mod remote_client;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use clap::Parser;
use tokio::signal;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{error, info};

use trx_app::{init_logging, normalize_name};
use trx_core::audio::AudioStreamInfo;

use trx_core::decode::DecodedMessage;
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::DynResult;
use trx_frontend::{FrontendRegistrationContext, FrontendRuntimeContext};
use trx_frontend_http::register_frontend_on as register_http_frontend;
use trx_frontend_http_json::register_frontend_on as register_http_json_frontend;
use trx_frontend_rigctl::register_frontend_on as register_rigctl_frontend;

use audio_client::AudioConnectConfig;
use config::{ClientConfig, RemoteEntry};
use remote_client::{parse_audio_url, parse_remote_url, RemoteClientConfig};

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
    frontend_runtime.http_show_sdr_gain_control = cfg.frontends.http.show_sdr_gain_control;
    frontend_runtime.http_initial_map_zoom = cfg.frontends.http.initial_map_zoom;
    frontend_runtime.http_spectrum_coverage_margin_hz =
        cfg.frontends.http.spectrum_coverage_margin_hz;
    frontend_runtime.http_spectrum_usable_span_ratio =
        cfg.frontends.http.spectrum_usable_span_ratio;
    frontend_runtime.http_decode_history_retention_min =
        cfg.frontends.http.decode_history_retention_min;
    frontend_runtime.http_decode_history_retention_min_by_rig = cfg
        .frontends
        .http
        .decode_history_retention_min_by_rig
        .clone();

    // Resolve remote entries: CLI --url > [[remotes]] > legacy [remote] > error
    let resolved_remotes: Vec<RemoteEntry> = if let Some(ref url) = cli.url {
        // CLI --url creates a single implicit remote entry
        let rig_id = cli.rig_id.clone().or_else(|| cfg.remote.rig_id.clone());
        let name = rig_id.clone().unwrap_or_else(|| "default".to_string());
        let token = cli.token.clone().or_else(|| cfg.remote.auth.token.clone());
        let poll_interval_ms = cli.poll_interval_ms.unwrap_or(cfg.remote.poll_interval_ms);
        vec![RemoteEntry {
            name,
            url: url.clone(),
            rig_id,
            auth: config::RemoteAuthConfig { token },
            poll_interval_ms,
        }]
    } else {
        let entries = cfg.resolved_remotes();
        if entries.is_empty() {
            return Err(
                "No remote servers configured. Use --url or add [[remotes]] entries in config."
                    .into(),
            );
        }
        entries
    };

    // Set initial active rig to the configured default or first remote entry.
    let default_rig = cli
        .rig_id
        .clone()
        .or_else(|| cfg.frontends.http.default_rig_name.clone())
        .or_else(|| resolved_remotes.first().map(|e| e.name.clone()));
    if let Ok(mut guard) = frontend_runtime.remote_active_rig_id.lock() {
        *guard = default_rig.clone();
    }

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
    let http_json_listen = cli
        .http_json_listen
        .unwrap_or(cfg.frontends.http_json.listen);
    let http_json_port = cli.http_json_port.unwrap_or(cfg.frontends.http_json.port);
    let callsign = cli
        .callsign
        .clone()
        .or_else(|| cfg.general.callsign.clone());
    frontend_runtime.owner_callsign = callsign.clone();
    frontend_runtime.owner_website_url = cfg.general.website_url.clone();
    frontend_runtime.owner_website_name = cfg.general.website_name.clone();
    frontend_runtime.ais_vessel_url_base = cfg.general.ais_vessel_url_base.clone();

    let remote_names: Vec<&str> = resolved_remotes.iter().map(|e| e.name.as_str()).collect();
    info!(
        "Starting trx-client (remotes: [{}], frontends: {})",
        remote_names.join(", "),
        frontends.join(", ")
    );

    let (tx, rx) = mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut task_handles: Vec<JoinHandle<()>> = Vec::new();

    let initial_state = RigState::new_uninitialized();
    let (state_tx, state_rx) = watch::channel(initial_state);

    // Group remote entries by (addr, token) so entries sharing a server share
    // one TCP connection.  Each group gets its own run_remote_client task.
    use std::collections::BTreeMap;
    use std::sync::RwLock;

    // Parse all endpoints upfront.
    let parsed_remotes: Vec<(RemoteEntry, remote_client::RemoteEndpoint)> = resolved_remotes
        .iter()
        .map(|entry| {
            let ep = parse_remote_url(&entry.url)
                .map_err(|e| format!("Invalid URL for remote '{}': {}", entry.name, e))?;
            Ok((entry.clone(), ep))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let global_audio_addr = cfg
        .frontends
        .audio
        .server_url
        .as_deref()
        .map(|url| {
            parse_audio_url(url)
                .map(|endpoint| endpoint.connect_addr())
                .map_err(|e| format!("Invalid audio URL override '{}': {}", url, e))
        })
        .transpose()?;

    // Build per-short-name audio connection defaults.
    let mut audio_connect: HashMap<String, AudioConnectConfig> = HashMap::new();
    for (entry, ep) in &parsed_remotes {
        let connect = if let Some(url) = cfg.frontends.audio.rig_urls.get(&entry.name) {
            let addr = parse_audio_url(url)
                .map(|endpoint| endpoint.connect_addr())
                .map_err(|e| {
                    format!(
                        "Invalid audio URL override for remote '{}': {}",
                        entry.name, e
                    )
                })?;
            AudioConnectConfig::fixed(addr)
        } else if let Some(addr) = global_audio_addr.clone() {
            AudioConnectConfig::fixed(addr)
        } else {
            let audio_port = cfg
                .frontends
                .audio
                .rig_ports
                .get(&entry.name)
                .copied()
                .unwrap_or(cfg.frontends.audio.server_port);
            AudioConnectConfig::from_host_port(ep.host.clone(), audio_port)
        };
        audio_connect.insert(entry.name.clone(), connect);
    }

    // Group by (connect_addr, token).
    let mut server_groups: BTreeMap<(String, Option<String>), Vec<&RemoteEntry>> = BTreeMap::new();
    let mut endpoint_by_addr: HashMap<String, remote_client::RemoteEndpoint> = HashMap::new();
    for (entry, ep) in &parsed_remotes {
        let key = (ep.connect_addr(), entry.auth.token.clone());
        endpoint_by_addr
            .entry(ep.connect_addr())
            .or_insert_with(|| ep.clone());
        server_groups.entry(key).or_default().push(entry);
    }

    // Per-server request senders for the routing dispatcher.
    let mut route_map: HashMap<String, mpsc::Sender<RigRequest>> = HashMap::new();

    for ((addr, token), entries) in &server_groups {
        // Build the rig_id → short_name mapping for this server group.
        let mut rig_id_to_short_name: HashMap<Option<String>, String> = HashMap::new();
        for entry in entries {
            rig_id_to_short_name.insert(entry.rig_id.clone(), entry.name.clone());
        }

        let poll_interval = entries
            .iter()
            .map(|e| e.poll_interval_ms)
            .min()
            .unwrap_or(750);

        let (server_tx, server_rx) = mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);
        for entry in entries {
            route_map.insert(entry.name.clone(), server_tx.clone());
        }

        let remote_cfg = RemoteClientConfig {
            addr: addr.clone(),
            token: token.clone(),
            selected_rig_id: frontend_runtime.remote_active_rig_id.clone(),
            known_rigs: frontend_runtime.remote_rigs.clone(),
            rig_states: frontend_runtime.rig_states.clone(),
            poll_interval: Duration::from_millis(poll_interval),
            spectrum: frontend_runtime.spectrum.clone(),
            rig_spectrums: frontend_runtime.rig_spectrums.clone(),
            server_connected: frontend_runtime.server_connected.clone(),
            rig_server_connected: frontend_runtime.rig_server_connected.clone(),
            rig_id_to_short_name,
            short_name_to_rig_id: Arc::new(RwLock::new(HashMap::new())),
        };
        let state_tx = state_tx.clone();
        let remote_shutdown_rx = shutdown_rx.clone();
        task_handles.push(tokio::spawn(async move {
            if let Err(e) = remote_client::run_remote_client(
                remote_cfg,
                server_rx,
                state_tx,
                remote_shutdown_rx,
            )
            .await
            {
                error!("Remote client error: {}", e);
            }
        }));
    }

    // Request routing dispatcher: receives from the single frontend-facing
    // channel and dispatches to the per-server channel based on rig_id_override
    // (short name).
    let route_map = Arc::new(route_map);
    let default_rig_for_router = frontend_runtime.remote_active_rig_id.clone();
    {
        let route_map = route_map.clone();
        let mut frontend_rx = rx;
        task_handles.push(tokio::spawn(async move {
            while let Some(req) = frontend_rx.recv().await {
                let target = req
                    .rig_id_override
                    .as_deref()
                    .map(String::from)
                    .or_else(|| default_rig_for_router.lock().ok().and_then(|g| g.clone()));
                let sender = target
                    .as_deref()
                    .and_then(|name| route_map.get(name))
                    .or_else(|| route_map.values().next());
                if let Some(sender) = sender {
                    let _ = sender.send(req).await;
                } else {
                    let _ = req.respond_to.send(Err(trx_core::RigError::communication(
                        "no remote server available for this rig",
                    )));
                }
            }
        }));
    }

    // Extract first remote host for audio backward-compat fallback.
    let remote_host = parsed_remotes
        .first()
        .map(|(_, ep)| ep.host.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());

    // Audio streaming setup
    let mut pending_audio_client = None;
    let mut pending_audio_bridge = None;
    if cfg.frontends.audio.enabled {
        let (rx_audio_tx, _) = broadcast::channel::<Bytes>(256);
        let (tx_audio_tx, tx_audio_rx) = mpsc::channel::<Bytes>(64);
        let (stream_info_tx, stream_info_rx) = watch::channel::<Option<AudioStreamInfo>>(None);
        let (decode_tx, _) = broadcast::channel::<DecodedMessage>(256);

        frontend_runtime.audio_rx = Some(rx_audio_tx.clone());
        frontend_runtime.audio_tx = Some(tx_audio_tx);
        frontend_runtime.audio_info = Some(stream_info_rx);
        frontend_runtime.decode_rx = Some(decode_tx.clone());

        // Virtual-channel audio: shared broadcaster map + command channel.
        let (vchan_cmd_tx, vchan_cmd_rx) = mpsc::channel::<trx_frontend::VChanAudioCmd>(256);
        *frontend_runtime.vchan_audio_cmd.lock().unwrap() = Some(vchan_cmd_tx);

        let (vchan_destroyed_tx, _) = broadcast::channel::<uuid::Uuid>(64);
        frontend_runtime.vchan_destroyed = Some(vchan_destroyed_tx.clone());
        let ais_history = frontend_runtime.ais_history.clone();
        let vdes_history = frontend_runtime.vdes_history.clone();
        let aprs_history = frontend_runtime.aprs_history.clone();
        let hf_aprs_history = frontend_runtime.hf_aprs_history.clone();
        let cw_history = frontend_runtime.cw_history.clone();
        let ft8_history = frontend_runtime.ft8_history.clone();
        let wspr_history = frontend_runtime.wspr_history.clone();
        let replay_history_sink: Arc<dyn Fn(DecodedMessage) + Send + Sync> = Arc::new(move |msg| {
            let now = std::time::Instant::now();
            match msg {
                DecodedMessage::Ais(mut message) => {
                    if message.ts_ms.is_none() {
                        message.ts_ms = Some(current_timestamp_ms());
                    }
                    if let Ok(mut history) = ais_history.lock() {
                        history.push_back((now, None, message));
                    }
                }
                DecodedMessage::Vdes(mut message) => {
                    if message.ts_ms.is_none() {
                        message.ts_ms = Some(current_timestamp_ms());
                    }
                    if let Ok(mut history) = vdes_history.lock() {
                        history.push_back((now, None, message));
                    }
                }
                DecodedMessage::Aprs(mut packet) => {
                    if packet.ts_ms.is_none() {
                        packet.ts_ms = Some(current_timestamp_ms());
                    }
                    if let Ok(mut history) = aprs_history.lock() {
                        history.push_back((now, None, packet));
                    }
                }
                DecodedMessage::HfAprs(mut packet) => {
                    if packet.ts_ms.is_none() {
                        packet.ts_ms = Some(current_timestamp_ms());
                    }
                    if let Ok(mut history) = hf_aprs_history.lock() {
                        history.push_back((now, None, packet));
                    }
                }
                DecodedMessage::Cw(event) => {
                    if let Ok(mut history) = cw_history.lock() {
                        history.push_back((now, None, event));
                    }
                }
                DecodedMessage::Ft8(message) => {
                    if let Ok(mut history) = ft8_history.lock() {
                        history.push_back((now, None, message));
                    }
                }
                DecodedMessage::Ft4(_) => {
                    // FT4 history is managed by the frontend HTTP audio collector
                }
                DecodedMessage::Ft2(_) => {
                    // FT2 history is managed by the frontend HTTP audio collector
                }
                DecodedMessage::Wspr(message) => {
                    if let Ok(mut history) = wspr_history.lock() {
                        history.push_back((now, None, message));
                    }
                }
                DecodedMessage::WxsatImage(_) => {}
                DecodedMessage::LrptImage(_) => {}
            }
        });

        info!("Audio enabled: decode channel set");

        let audio_shutdown_rx = shutdown_rx.clone();
        let vchan_audio_map = frontend_runtime.vchan_audio.clone();
        let rig_audio_rx_map = frontend_runtime.rig_audio_rx.clone();
        let rig_audio_info_map = frontend_runtime.rig_audio_info.clone();
        let rig_vchan_cmd_map = frontend_runtime.rig_vchan_audio_cmd.clone();
        let default_audio_connect = if let Some(addr) = global_audio_addr {
            AudioConnectConfig::fixed(addr)
        } else {
            AudioConnectConfig::from_host_port(remote_host.clone(), cfg.frontends.audio.server_port)
        };
        pending_audio_client = Some(tokio::spawn(audio_client::run_multi_rig_audio_manager(
            default_audio_connect,
            audio_connect,
            frontend_runtime.remote_active_rig_id.clone(),
            frontend_runtime.remote_rigs.clone(),
            rx_audio_tx,
            tx_audio_rx,
            stream_info_tx,
            decode_tx,
            Some(replay_history_sink),
            audio_shutdown_rx,
            vchan_audio_map,
            vchan_cmd_rx,
            Some(vchan_destroyed_tx),
            rig_audio_rx_map,
            rig_audio_info_map,
            rig_vchan_cmd_map,
        )));

        if cfg.frontends.audio.bridge.enabled {
            pending_audio_bridge = Some(cfg.frontends.audio.bridge.clone());
        }
    } else {
        info!("Audio disabled in config, decode will not be available");
    }

    let frontend_runtime_ctx = Arc::new(frontend_runtime);

    // Start decode history collector before audio client starts replay.
    // Frontend tasks are spawned asynchronously, so starting the collector
    // here avoids missing the initial server-side history burst.
    if cfg.frontends.audio.enabled {
        trx_frontend_http::server::audio::start_decode_history_collector(
            frontend_runtime_ctx.clone(),
        );
    }

    // Spawn frontends with runtime context
    for frontend in &frontends {
        let frontend_state_rx = state_rx.clone();

        // rigctl: always spawn one listener per configured rig entry.
        if frontend == "rigctl" {
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
                let (proxy_tx, mut proxy_rx) = mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);
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

    // Start the audio connection only after frontends are running so decode
    // subscribers can capture the server's initial history replay.
    if let Some(handle) = pending_audio_client {
        task_handles.push(handle);
    }
    if let Some(bridge_cfg) = pending_audio_bridge {
        info!("Audio bridge enabled (local virtual-device integration)");
        task_handles.push(audio_bridge::spawn_audio_bridge(
            bridge_cfg,
            frontend_runtime_ctx
                .audio_rx
                .as_ref()
                .expect("audio rx must be set")
                .clone(),
            frontend_runtime_ctx
                .audio_tx
                .as_ref()
                .expect("audio tx must be set")
                .clone(),
            frontend_runtime_ctx
                .audio_info
                .as_ref()
                .expect("audio info must be set")
                .clone(),
            shutdown_rx.clone(),
        ));
    }

    Ok(AppState {
        shutdown_tx,
        task_handles,
        request_tx: tx,
    })
}

fn current_timestamp_ms() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}
