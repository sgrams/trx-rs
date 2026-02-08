// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

mod audio;
mod config;
mod decode;
mod error;
mod listener;
mod plugins;
mod rig_task;

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use clap::{Parser, ValueEnum};
use tokio::signal;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{error, info};

use trx_core::audio::AudioStreamInfo;

use trx_backend::{is_backend_registered, register_builtin_backends, registered_backends, RigAccess};
use trx_core::radio::freq::Freq;
use trx_core::rig::controller::{AdaptivePolling, ExponentialBackoff};
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::rig::{RigControl, RigRxStatus, RigStatus, RigTxStatus};
use trx_core::DynResult;

use config::ServerConfig;

const PKG_DESCRIPTION: &str = concat!(env!("CARGO_PKG_NAME"), " - rig server daemon");
const RIG_TASK_CHANNEL_BUFFER: usize = 32;
const RETRY_MAX_DELAY_SECS: u64 = 2;

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
    /// Rig backend to use (e.g. ft817)
    #[arg(short = 'r', long = "rig")]
    rig: Option<String>,
    /// Access method to reach the rig CAT interface
    #[arg(short = 'a', long = "access", value_enum)]
    access: Option<AccessKind>,
    /// Rig CAT address:
    /// when access is serial: <path> <baud>;
    /// when access is TCP: <host>:<port>
    #[arg(value_name = "RIG_ADDR")]
    rig_addr: Option<String>,
    /// Optional callsign/owner label
    #[arg(short = 'c', long = "callsign")]
    callsign: Option<String>,
    /// IP address for the JSON TCP listener
    #[arg(short = 'l', long = "listen")]
    listen: Option<IpAddr>,
    /// Port for the JSON TCP listener
    #[arg(short = 'p', long = "port")]
    port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AccessKind {
    Serial,
    Tcp,
}

/// Normalize a rig name to lowercase alphanumeric.
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
    callsign: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

fn resolve_config(cli: &Cli, cfg: &ServerConfig) -> DynResult<ResolvedConfig> {
    let rig_str = cli.rig.clone().or_else(|| cfg.rig.model.clone());
    let rig = match rig_str.as_deref() {
        Some(name) => normalize_name(name),
        None => {
            return Err(
                "Rig model not specified. Use --rig or set [rig].model in config.".into(),
            )
        }
    };
    if !is_backend_registered(&rig) {
        return Err(format!(
            "Unknown rig model: {} (available: {})",
            rig,
            registered_backends().join(", ")
        )
        .into());
    }

    let access = {
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

    let callsign = cli
        .callsign
        .clone()
        .or_else(|| cfg.general.callsign.clone());

    let latitude = cfg.general.latitude;
    let longitude = cfg.general.longitude;

    Ok(ResolvedConfig {
        rig,
        access,
        callsign,
        latitude,
        longitude,
    })
}

fn build_initial_state(cfg: &ServerConfig, resolved: &ResolvedConfig) -> RigState {
    let callsign = &resolved.callsign;
    RigState {
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
        server_callsign: callsign.clone(),
        server_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        server_latitude: resolved.latitude,
        server_longitude: resolved.longitude,
        aprs_decode_enabled: false,
        cw_decode_enabled: false,
        aprs_decode_reset_seq: 0,
        cw_decode_reset_seq: 0,
    }
}

fn build_rig_task_config(
    resolved: &ResolvedConfig,
    cfg: &ServerConfig,
) -> rig_task::RigTaskConfig {
    rig_task::RigTaskConfig {
        rig_model: resolved.rig.clone(),
        access: resolved.access.clone(),
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
        server_callsign: resolved.callsign.clone(),
        server_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        server_latitude: resolved.latitude,
        server_longitude: resolved.longitude,
    }
}

#[tokio::main]
async fn main() -> DynResult<()> {
    tracing_subscriber::fmt().with_target(false).init();

    register_builtin_backends();
    let _plugin_libs = plugins::load_plugins();

    let cli = Cli::parse();

    if cli.print_config {
        println!("{}", ServerConfig::example_toml());
        return Ok(());
    }

    let (cfg, config_path) = if let Some(ref path) = cli.config {
        let cfg = ServerConfig::load_from_file(path)?;
        (cfg, Some(path.clone()))
    } else {
        ServerConfig::load_from_default_paths()?
    };

    if let Some(ref path) = config_path {
        info!("Loaded configuration from {}", path.display());
    }

    let resolved = resolve_config(&cli, &cfg)?;

    match &resolved.access {
        RigAccess::Serial { path, baud } => {
            info!(
                "Starting trx-server (rig: {}, access: serial {} @ {} baud)",
                resolved.rig, path, baud
            );
        }
        RigAccess::Tcp { addr } => {
            info!(
                "Starting trx-server (rig: {}, access: tcp {})",
                resolved.rig, addr
            );
        }
    }

    if let Some(ref cs) = resolved.callsign {
        info!("Callsign: {}", cs);
    }

    let (tx, rx) = mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);
    let initial_state = build_initial_state(&cfg, &resolved);
    let (state_tx, state_rx) = watch::channel(initial_state);
    // Keep receivers alive so channels don't close prematurely
    let _state_rx = state_rx;

    let rig_task_config = build_rig_task_config(&resolved, &cfg);
    let _rig_handle = tokio::spawn(rig_task::run_rig_task(rig_task_config, rx, state_tx));

    if cfg.listen.enabled {
        let listen_ip = cli.listen.unwrap_or(cfg.listen.listen);
        let listen_port = cli.port.unwrap_or(cfg.listen.port);
        let listen_addr = SocketAddr::from((listen_ip, listen_port));
        let auth_tokens: HashSet<String> = cfg
            .listen
            .auth
            .tokens
            .iter()
            .filter(|t| !t.is_empty())
            .cloned()
            .collect();
        let rig_tx = tx.clone();
        let state_rx_listener = _state_rx.clone();
        tokio::spawn(async move {
            if let Err(e) = listener::run_listener(listen_addr, rig_tx, auth_tokens, state_rx_listener).await {
                error!("Listener error: {:?}", e);
            }
        });
    }

    if cfg.audio.enabled {
        let audio_listen = SocketAddr::from((cli.listen.unwrap_or(cfg.audio.listen), cfg.audio.port));
        let stream_info = AudioStreamInfo {
            sample_rate: cfg.audio.sample_rate,
            channels: cfg.audio.channels,
            frame_duration_ms: cfg.audio.frame_duration_ms,
        };

        let (rx_audio_tx, _) = broadcast::channel::<Bytes>(256);
        let (tx_audio_tx, tx_audio_rx) = mpsc::channel::<Bytes>(64);

        // PCM tap for server-side decoders
        let (pcm_tx, _) = broadcast::channel::<Vec<f32>>(64);
        // Decoded messages broadcast
        let (decode_tx, _) = broadcast::channel::<trx_core::decode::DecodedMessage>(256);

        if cfg.audio.rx_enabled {
            let _capture_thread = audio::spawn_audio_capture(&cfg.audio, rx_audio_tx.clone(), Some(pcm_tx.clone()));

            // Spawn APRS decoder task
            let aprs_pcm_rx = pcm_tx.subscribe();
            let aprs_state_rx = _state_rx.clone();
            let aprs_decode_tx = decode_tx.clone();
            let aprs_sr = cfg.audio.sample_rate;
            let aprs_ch = cfg.audio.channels;
            tokio::spawn(audio::run_aprs_decoder(
                aprs_sr, aprs_ch as u16, aprs_pcm_rx, aprs_state_rx, aprs_decode_tx,
            ));

            // Spawn CW decoder task
            let cw_pcm_rx = pcm_tx.subscribe();
            let cw_state_rx = _state_rx.clone();
            let cw_decode_tx = decode_tx.clone();
            let cw_sr = cfg.audio.sample_rate;
            let cw_ch = cfg.audio.channels;
            tokio::spawn(audio::run_cw_decoder(
                cw_sr, cw_ch as u16, cw_pcm_rx, cw_state_rx, cw_decode_tx,
            ));
        }
        if cfg.audio.tx_enabled {
            let _playback_thread = audio::spawn_audio_playback(&cfg.audio, tx_audio_rx);
        }

        tokio::spawn(async move {
            if let Err(e) =
                audio::run_audio_listener(audio_listen, rx_audio_tx, tx_audio_tx, stream_info, decode_tx)
                    .await
            {
                error!("Audio listener error: {:?}", e);
            }
        });
    }

    let _tx = tx;

    signal::ctrl_c().await?;
    info!("Ctrl+C received, shutting down");
    Ok(())
}
