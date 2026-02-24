// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

mod aprsfi;
mod audio;
mod config;
mod error;
mod listener;
mod pskreporter;
mod rig_task;

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::Duration;

use bytes::Bytes;
use clap::{Parser, ValueEnum};
use tokio::signal;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use trx_core::audio::AudioStreamInfo;

use trx_app::{init_logging, load_backend_plugins, normalize_name};
use trx_backend::{register_builtin_backends_on, RegistrationContext, RigAccess};
use trx_core::rig::controller::{AdaptivePolling, ExponentialBackoff};
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::DynResult;

use config::ServerConfig;
use trx_decode_log::DecoderLoggers;

#[cfg(feature = "soapysdr")]
use trx_backend_soapysdr;

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
    /// Rig backend to use (e.g. ft817, ft450d)
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

fn resolve_config(
    cli: &Cli,
    cfg: &ServerConfig,
    registry: &RegistrationContext,
) -> DynResult<ResolvedConfig> {
    let rig_str = cli.rig.clone().or_else(|| cfg.rig.model.clone());
    let rig = match rig_str.as_deref() {
        Some(name) => normalize_name(name),
        None => {
            return Err("Rig model not specified. Use --rig or set [rig].model in config.".into())
        }
    };
    if !registry.is_backend_registered(&rig) {
        return Err(format!(
            "Unknown rig model: {} (available: {})",
            rig,
            registry.registered_backends().join(", ")
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
                } else if let (Some(port), Some(baud)) = (&cfg.rig.access.port, cfg.rig.access.baud)
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
            Some("sdr") => {
                let args = cfg.rig.access.args.clone().unwrap_or_default();
                RigAccess::Sdr { args }
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

fn build_rig_task_config(
    resolved: &ResolvedConfig,
    cfg: &ServerConfig,
    registry: std::sync::Arc<RegistrationContext>,
) -> rig_task::RigTaskConfig {
    let pskreporter_status = if cfg.pskreporter.enabled {
        let has_locator = cfg.pskreporter.receiver_locator.is_some()
            || (resolved.latitude.is_some() && resolved.longitude.is_some());
        if has_locator {
            Some(format!(
                "Enabled ({}:{})",
                cfg.pskreporter.host, cfg.pskreporter.port
            ))
        } else {
            Some(format!(
                "Enabled but inactive (missing locator source) ({}:{})",
                cfg.pskreporter.host, cfg.pskreporter.port
            ))
        }
    } else {
        Some("Disabled".to_string())
    };

    rig_task::RigTaskConfig {
        registry,
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
        server_build_date: Some(env!("TRX_SERVER_BUILD_DATE").to_string()),
        server_latitude: resolved.latitude,
        server_longitude: resolved.longitude,
        pskreporter_status,
        prebuilt_rig: None,
    }
}

async fn wait_for_shutdown(mut shutdown_rx: watch::Receiver<bool>) {
    if *shutdown_rx.borrow() {
        return;
    }
    while shutdown_rx.changed().await.is_ok() {
        if *shutdown_rx.borrow() {
            break;
        }
    }
}

/// Parse a `RigMode` from a string slice.
/// Falls back to `initial_mode` when the string is "auto" or unrecognised.
#[cfg(feature = "soapysdr")]
fn parse_rig_mode(
    s: &str,
    initial_mode: &trx_core::rig::state::RigMode,
) -> trx_core::rig::state::RigMode {
    use trx_core::rig::state::RigMode;
    match s {
        "LSB" => RigMode::LSB,
        "USB" => RigMode::USB,
        "CW" => RigMode::CW,
        "CWR" => RigMode::CWR,
        "AM" => RigMode::AM,
        "WFM" => RigMode::WFM,
        "FM" => RigMode::FM,
        "DIG" => RigMode::DIG,
        "PKT" => RigMode::PKT,
        _ => initial_mode.clone(),
    }
}

/// Build a `SoapySdrRig` with full channel config from `ServerConfig` and
/// return both the rig box and a PCM receiver subscribed to its primary channel.
///
/// Only compiled when the `soapysdr` feature is enabled.
#[cfg(feature = "soapysdr")]
fn build_sdr_rig(
    cfg: &ServerConfig,
) -> DynResult<(
    Box<dyn trx_core::rig::RigCat>,
    tokio::sync::broadcast::Receiver<Vec<f32>>,
)> {
    use trx_core::radio::freq::Freq;
    use trx_core::rig::AudioSource;

    let args = cfg.rig.access.args.as_deref().unwrap_or("");
    let channels: Vec<(f64, trx_core::rig::state::RigMode, u32, usize)> = cfg
        .sdr
        .channels
        .iter()
        .map(|ch| {
            let if_hz = (cfg.sdr.center_offset_hz + ch.offset_hz) as f64;
            let mode = parse_rig_mode(&ch.mode, &cfg.rig.initial_mode);
            (if_hz, mode, ch.audio_bandwidth_hz, ch.fir_taps)
        })
        .collect();

    let sdr_rig = trx_backend_soapysdr::SoapySdrRig::new_with_config(
        args,
        &channels,
        &cfg.sdr.gain.mode,
        cfg.sdr.gain.value,
        cfg.audio.sample_rate,
        cfg.audio.frame_duration_ms,
        Freq {
            hz: cfg.rig.initial_freq_hz,
        },
        cfg.rig.initial_mode.clone(),
        cfg.sdr.sample_rate,
    )?;

    // Subscribe to the primary channel's PCM broadcast before consuming the rig.
    let pcm_rx = sdr_rig.subscribe_pcm();
    Ok((Box::new(sdr_rig) as Box<dyn trx_core::rig::RigCat>, pcm_rx))
}

#[tokio::main]
async fn main() -> DynResult<()> {
    // Phase 3B: Create bootstrap context for explicit initialization.
    // This replaces reliance on global mutable state, though currently
    // built-in backends still register on globals for plugin compatibility.
    // Full de-globalization would require threading context through rig_task and listener.
    let mut bootstrap_ctx = RegistrationContext::new();
    register_builtin_backends_on(&mut bootstrap_ctx);

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
    cfg.validate()
        .map_err(|e| format!("Invalid server configuration: {}", e))?;

    // Validate SDR-specific configuration rules (see SDR.md §11).
    let sdr_errors = cfg.validate_sdr();
    if !sdr_errors.is_empty() {
        for e in &sdr_errors {
            tracing::error!("SDR config error: {}", e);
        }
        std::process::exit(1);
    }

    init_logging(cfg.general.log_level.as_deref());

    let bootstrap_ctx_ptr = NonNull::from(&mut bootstrap_ctx).cast();
    let _plugin_libs = load_backend_plugins(bootstrap_ctx_ptr);

    if let Some(ref path) = config_path {
        info!("Loaded configuration from {}", path.display());
    }

    let resolved = resolve_config(&cli, &cfg, &bootstrap_ctx)?;

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
        RigAccess::Sdr { args } => {
            info!(
                "Starting trx-server (rig: {}, access: sdr {})",
                resolved.rig, args
            );
        }
    }

    if let Some(ref cs) = resolved.callsign {
        info!("Callsign: {}", cs);
    }

    // For the SDR access type: build the SoapySdrRig with full channel config
    // here in main so we can subscribe to its primary-channel PCM sender
    // before passing the rig to the rig task.  The rig task skips its
    // registry factory when `prebuilt_rig` is set.
    //
    // When the `soapysdr` feature is disabled this block is elided and
    // `sdr_pcm_rx` is always `None`, preserving the cpal path.
    #[cfg(feature = "soapysdr")]
    let (sdr_prebuilt_rig, sdr_pcm_rx): (
        Option<Box<dyn trx_core::rig::RigCat>>,
        Option<tokio::sync::broadcast::Receiver<Vec<f32>>>,
    ) = if cfg.rig.access.access_type.as_deref() == Some("sdr") {
        let (rig, pcm_rx) = build_sdr_rig(&cfg)?;
        (Some(rig), Some(pcm_rx))
    } else {
        (None, None)
    };

    #[cfg(not(feature = "soapysdr"))]
    let (sdr_prebuilt_rig, sdr_pcm_rx): (
        Option<Box<dyn trx_core::rig::RigCat>>,
        Option<tokio::sync::broadcast::Receiver<Vec<f32>>>,
    ) = (None, None);

    let (tx, rx) = mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);
    let mut task_handles: Vec<JoinHandle<()>> = Vec::new();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let initial_state = RigState::new_with_metadata(
        resolved.callsign.clone(),
        Some(env!("CARGO_PKG_VERSION").to_string()),
        Some(env!("TRX_SERVER_BUILD_DATE").to_string()),
        resolved.latitude,
        resolved.longitude,
        cfg.rig.initial_freq_hz,
        cfg.rig.initial_mode.clone(),
    );
    let mut initial_state = initial_state;
    initial_state.pskreporter_status = if cfg.pskreporter.enabled {
        Some(format!(
            "Enabled ({}:{})",
            cfg.pskreporter.host, cfg.pskreporter.port
        ))
    } else {
        Some("Disabled".to_string())
    };
    let (state_tx, state_rx) = watch::channel(initial_state);
    // Keep receivers alive so channels don't close prematurely
    let _state_rx = state_rx;

    let mut rig_task_config =
        build_rig_task_config(&resolved, &cfg, std::sync::Arc::new(bootstrap_ctx));

    // Pass pre-built SDR rig to the task so it skips the registry factory.
    if let Some(prebuilt) = sdr_prebuilt_rig {
        rig_task_config.prebuilt_rig = Some(prebuilt);
    }

    let rig_shutdown_rx = shutdown_rx.clone();
    task_handles.push(tokio::spawn(async move {
        if let Err(e) = rig_task::run_rig_task(rig_task_config, rx, state_tx, rig_shutdown_rx).await
        {
            error!("Rig task error: {:?}", e);
        }
    }));

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
        let listener_shutdown_rx = shutdown_rx.clone();
        task_handles.push(tokio::spawn(async move {
            if let Err(e) = listener::run_listener(
                listen_addr,
                rig_tx,
                auth_tokens,
                state_rx_listener,
                listener_shutdown_rx,
            )
            .await
            {
                error!("Listener error: {:?}", e);
            }
        }));
    }

    if cfg.audio.enabled {
        let audio_listen =
            SocketAddr::from((cli.listen.unwrap_or(cfg.audio.listen), cfg.audio.port));
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

        if cfg.pskreporter.enabled {
            let callsign = resolved.callsign.clone().unwrap_or_default();
            if callsign.trim().is_empty() {
                warn!("PSK Reporter enabled but [general].callsign is empty; uplink disabled");
            } else {
                let pr_cfg = cfg.pskreporter.clone();
                let pr_state_rx = _state_rx.clone();
                let pr_decode_rx = decode_tx.subscribe();
                let pr_shutdown_rx = shutdown_rx.clone();
                let latitude = resolved.latitude;
                let longitude = resolved.longitude;
                task_handles.push(tokio::spawn(async move {
                    tokio::select! {
                        _ = pskreporter::run_pskreporter_uplink(
                            pr_cfg,
                            callsign,
                            latitude,
                            longitude,
                            pr_state_rx,
                            pr_decode_rx
                        ) => {}
                        _ = wait_for_shutdown(pr_shutdown_rx) => {}
                    }
                }));
            }
        }

        if cfg.aprsfi.enabled {
            let callsign = resolved.callsign.clone().unwrap_or_default();
            if callsign.trim().is_empty() {
                warn!("APRS-IS IGate enabled but [general].callsign is empty; uplink disabled");
            } else {
                let ai_cfg = cfg.aprsfi.clone();
                let ai_decode_rx = decode_tx.subscribe();
                let ai_shutdown_rx = shutdown_rx.clone();
                task_handles.push(tokio::spawn(async move {
                    tokio::select! {
                        _ = aprsfi::run_aprsfi_uplink(ai_cfg, callsign, ai_decode_rx) => {}
                        _ = wait_for_shutdown(ai_shutdown_rx) => {}
                    }
                }));
            }
        }

        let decoder_logs = match DecoderLoggers::from_config(&cfg.decode_logs) {
            Ok(v) => v,
            Err(e) => {
                warn!("Decoder file logging disabled: {}", e);
                None
            }
        };

        if cfg.audio.rx_enabled {
            if let Some(mut sdr_rx) = sdr_pcm_rx {
                // SDR path: the backend pipeline provides demodulated PCM,
                // so cpal capture is skipped entirely.
                // The SDR PCM frames are bridged into pcm_tx so the existing
                // decoder spawn code below receives them unchanged.
                tracing::info!("using SDR audio source — cpal capture disabled");
                let pcm_tx_clone = pcm_tx.clone();
                task_handles.push(tokio::spawn(async move {
                    loop {
                        match sdr_rx.recv().await {
                            Ok(frame) => {
                                let _ = pcm_tx_clone.send(frame);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("SDR audio bridge: dropped {} frames", n);
                            }
                            Err(_) => break,
                        }
                    }
                }));
            } else {
                // cpal path (existing serial/TCP transceivers)
                let _capture_thread = audio::spawn_audio_capture(
                    &cfg.audio,
                    rx_audio_tx.clone(),
                    Some(pcm_tx.clone()),
                );
            }

            // Spawn APRS decoder task
            let aprs_pcm_rx = pcm_tx.subscribe();
            let aprs_state_rx = _state_rx.clone();
            let aprs_decode_tx = decode_tx.clone();
            let aprs_sr = cfg.audio.sample_rate;
            let aprs_ch = cfg.audio.channels;
            let aprs_shutdown_rx = shutdown_rx.clone();
            let aprs_logs = decoder_logs.clone();
            task_handles.push(tokio::spawn(async move {
                tokio::select! {
                    _ = audio::run_aprs_decoder(aprs_sr, aprs_ch as u16, aprs_pcm_rx, aprs_state_rx, aprs_decode_tx, aprs_logs) => {}
                    _ = wait_for_shutdown(aprs_shutdown_rx) => {}
                }
            }));

            // Spawn CW decoder task
            let cw_pcm_rx = pcm_tx.subscribe();
            let cw_state_rx = _state_rx.clone();
            let cw_decode_tx = decode_tx.clone();
            let cw_sr = cfg.audio.sample_rate;
            let cw_ch = cfg.audio.channels;
            let cw_shutdown_rx = shutdown_rx.clone();
            let cw_logs = decoder_logs.clone();
            task_handles.push(tokio::spawn(async move {
                tokio::select! {
                    _ = audio::run_cw_decoder(cw_sr, cw_ch as u16, cw_pcm_rx, cw_state_rx, cw_decode_tx, cw_logs) => {}
                    _ = wait_for_shutdown(cw_shutdown_rx) => {}
                }
            }));

            // Spawn FT8 decoder task
            let ft8_pcm_rx = pcm_tx.subscribe();
            let ft8_state_rx = _state_rx.clone();
            let ft8_decode_tx = decode_tx.clone();
            let ft8_sr = cfg.audio.sample_rate;
            let ft8_ch = cfg.audio.channels;
            let ft8_shutdown_rx = shutdown_rx.clone();
            let ft8_logs = decoder_logs.clone();
            task_handles.push(tokio::spawn(async move {
                tokio::select! {
                    _ = audio::run_ft8_decoder(ft8_sr, ft8_ch as u16, ft8_pcm_rx, ft8_state_rx, ft8_decode_tx, ft8_logs) => {}
                    _ = wait_for_shutdown(ft8_shutdown_rx) => {}
                }
            }));

            // Spawn WSPR decoder task
            let wspr_pcm_rx = pcm_tx.subscribe();
            let wspr_state_rx = _state_rx.clone();
            let wspr_decode_tx = decode_tx.clone();
            let wspr_sr = cfg.audio.sample_rate;
            let wspr_ch = cfg.audio.channels;
            let wspr_shutdown_rx = shutdown_rx.clone();
            let wspr_logs = decoder_logs.clone();
            task_handles.push(tokio::spawn(async move {
                tokio::select! {
                    _ = audio::run_wspr_decoder(wspr_sr, wspr_ch as u16, wspr_pcm_rx, wspr_state_rx, wspr_decode_tx, wspr_logs) => {}
                    _ = wait_for_shutdown(wspr_shutdown_rx) => {}
                }
            }));
        }
        if cfg.audio.tx_enabled {
            let _playback_thread = audio::spawn_audio_playback(&cfg.audio, tx_audio_rx);
        }

        let audio_shutdown_rx = shutdown_rx.clone();
        task_handles.push(tokio::spawn(async move {
            if let Err(e) = audio::run_audio_listener(
                audio_listen,
                rx_audio_tx,
                tx_audio_tx,
                stream_info,
                decode_tx,
                audio_shutdown_rx,
            )
            .await
            {
                error!("Audio listener error: {:?}", e);
            }
        }));
    }

    signal::ctrl_c().await?;
    info!("Ctrl+C received, shutting down");
    let _ = shutdown_tx.send(true);
    drop(tx);
    tokio::time::sleep(Duration::from_millis(400)).await;

    for handle in &task_handles {
        if !handle.is_finished() {
            handle.abort();
        }
    }
    for handle in task_handles {
        let _ = handle.await;
    }
    Ok(())
}
