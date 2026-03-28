// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

mod audio;
mod config;
mod error;
mod history_store;
mod listener;
mod rig_handle;
mod rig_task;

use std::collections::HashMap;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use clap::{Parser, ValueEnum};
use tokio::signal;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use trx_core::audio::AudioStreamInfo;

use trx_app::{init_logging, normalize_name};
use trx_backend::{register_builtin_backends_on, RegistrationContext, RigAccess};
use trx_core::rig::controller::{AdaptivePolling, ExponentialBackoff};
use trx_core::rig::request::RigRequest;
use trx_core::rig::state::RigState;
use trx_core::DynResult;

use audio::DecoderHistories;
use config::{RigInstanceConfig, ServerConfig};
use rig_handle::RigHandle;
use trx_decode_log::DecoderLoggers;

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

/// Resolved configuration for the first/only rig (legacy single-rig CLI path).
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

/// Derive a `RigAccess` from a rig instance config's access fields.
fn access_from_rig_instance(rig_cfg: &RigInstanceConfig) -> DynResult<RigAccess> {
    match rig_cfg.rig.access.access_type.as_deref() {
        Some("serial") | None => {
            let path = rig_cfg
                .rig
                .access
                .port
                .clone()
                .unwrap_or_else(|| "/dev/ttyUSB0".to_string());
            let baud = rig_cfg.rig.access.baud.unwrap_or(9600);
            Ok(RigAccess::Serial { path, baud })
        }
        Some("tcp") => {
            let host = rig_cfg.rig.access.host.clone().unwrap_or_default();
            let port = rig_cfg.rig.access.tcp_port.unwrap_or(0);
            Ok(RigAccess::Tcp {
                addr: format!("{}:{}", host, port),
            })
        }
        Some("sdr") => {
            let args = rig_cfg.rig.access.args.clone().unwrap_or_default();
            Ok(RigAccess::Sdr { args })
        }
        Some(other) => {
            Err(format!("Unknown access type '{}' for rig '{}'", other, rig_cfg.id).into())
        }
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

/// Sensible default audio filter bandwidth (Hz) for each demodulation mode.
#[cfg(feature = "soapysdr")]
fn default_audio_bandwidth_for_mode(mode: &trx_core::rig::state::RigMode) -> u32 {
    use trx_core::rig::state::RigMode;
    match mode {
        RigMode::LSB | RigMode::USB | RigMode::DIG => 3_000,
        RigMode::PKT => 25_000,
        RigMode::CW | RigMode::CWR => 500,
        RigMode::AM | RigMode::SAM => 9_000,
        RigMode::FM => 12_500,
        RigMode::WFM => 180_000,
        RigMode::AIS => 25_000,
        RigMode::VDES => 100_000,
        RigMode::Other(_) => 3_000,
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
        "AIS" => RigMode::AIS,
        "VDES" => RigMode::VDES,
        "DIG" => RigMode::DIG,
        "PKT" => RigMode::PKT,
        _ => initial_mode.clone(),
    }
}

#[cfg(feature = "soapysdr")]
type SdrRigBuildResult = DynResult<(
    Box<dyn trx_core::rig::RigCat>,
    tokio::sync::broadcast::Receiver<Vec<f32>>,
    (
        tokio::sync::broadcast::Receiver<Vec<f32>>,
        tokio::sync::broadcast::Receiver<Vec<f32>>,
    ),
    tokio::sync::broadcast::Receiver<Vec<num_complex::Complex<f32>>>,
    trx_core::vchan::SharedVChanManager,
)>;

type OptionalSdrRig = Option<Box<dyn trx_core::rig::RigCat>>;
type OptionalSdrPcmRx = Option<broadcast::Receiver<Vec<f32>>>;
type OptionalSdrAisPcmRx = Option<(broadcast::Receiver<Vec<f32>>, broadcast::Receiver<Vec<f32>>)>;
type OptionalSdrVdesIqRx = Option<broadcast::Receiver<Vec<num_complex::Complex<f32>>>>;

/// Build a `SoapySdrRig` with full channel config from a `RigInstanceConfig`.
#[cfg(feature = "soapysdr")]
fn build_sdr_rig_from_instance(rig_cfg: &RigInstanceConfig) -> SdrRigBuildResult {
    use trx_core::radio::freq::Freq;
    use trx_core::rig::AudioSource;

    let args = rig_cfg.rig.access.args.as_deref().unwrap_or("");
    let mut channels: Vec<(f64, trx_core::rig::state::RigMode, u32)> = rig_cfg
        .sdr
        .channels
        .iter()
        .map(|ch| {
            let if_hz = (rig_cfg.sdr.center_offset_hz + ch.offset_hz) as f64;
            let mode = parse_rig_mode(&ch.mode, &rig_cfg.rig.initial_mode);
            (if_hz, mode, ch.audio_bandwidth_hz)
        })
        .collect();

    // Ensure at least one demodulation channel so audio is available.
    if channels.is_empty() {
        tracing::warn!(
            "[{}] No [[sdr.channels]] configured; adding a default primary channel. \
             Add [[sdr.channels]] to your config for full control.",
            rig_cfg.id
        );
        let default_bw = default_audio_bandwidth_for_mode(&rig_cfg.rig.initial_mode);
        channels.push((
            rig_cfg.sdr.center_offset_hz as f64,
            rig_cfg.rig.initial_mode.clone(),
            default_bw,
        ));
    }
    let ais_channel_base_idx = channels.len();

    let sdr_rig = trx_backend::SoapySdrRig::new_with_config(
        args,
        &channels,
        &rig_cfg.sdr.gain.mode,
        rig_cfg.sdr.gain.value,
        rig_cfg.sdr.gain.max_value,
        rig_cfg.audio.sample_rate,
        rig_cfg.audio.channels as usize,
        rig_cfg.audio.frame_duration_ms,
        rig_cfg.sdr.wfm_deemphasis_us,
        Freq {
            hz: rig_cfg.rig.initial_freq_hz,
        },
        rig_cfg.rig.initial_mode.clone(),
        rig_cfg.sdr.sample_rate,
        rig_cfg.sdr.bandwidth,
        rig_cfg.sdr.center_offset_hz,
        rig_cfg.sdr.squelch.enabled,
        rig_cfg.sdr.squelch.threshold_db,
        rig_cfg.sdr.squelch.hysteresis_db,
        rig_cfg.sdr.squelch.tail_ms,
        rig_cfg.sdr.max_virtual_channels,
        rig_cfg.sdr.noise_blanker.enabled,
        rig_cfg.sdr.noise_blanker.threshold,
    )?;

    let pcm_rx = sdr_rig.subscribe_pcm();
    let ais_pcm = (
        sdr_rig.subscribe_pcm_channel(ais_channel_base_idx),
        sdr_rig.subscribe_pcm_channel(ais_channel_base_idx + 1),
    );
    // Subscribe to the first channel configured as VDES or MARINE so that the
    // IQ tap in ChannelDsp actually fires.  Fall back to channel 0 when no
    // explicit VDES channel has been configured.
    let vdes_channel_idx = channels
        .iter()
        .position(|(_, mode, _)| matches!(mode, trx_core::rig::state::RigMode::VDES))
        .unwrap_or(0);
    let vdes_iq = sdr_rig.subscribe_iq_channel(vdes_channel_idx);
    // Extract the virtual channel manager before the rig is consumed by Box.
    let vchan_manager: trx_core::vchan::SharedVChanManager = sdr_rig.channel_manager();
    Ok((
        Box::new(sdr_rig) as Box<dyn trx_core::rig::RigCat>,
        pcm_rx,
        ais_pcm,
        vdes_iq,
        vchan_manager,
    ))
}

/// Build a `RigTaskConfig` for a single rig instance.
#[allow(clippy::too_many_arguments)]
fn build_rig_task_config(
    rig_cfg: &RigInstanceConfig,
    rig_model: String,
    access: RigAccess,
    callsign: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    registry: Arc<RegistrationContext>,
    histories: Arc<DecoderHistories>,
) -> rig_task::RigTaskConfig {
    let pskreporter_status = if rig_cfg.pskreporter.enabled {
        let has_locator = rig_cfg.pskreporter.receiver_locator.is_some()
            || (latitude.is_some() && longitude.is_some());
        if has_locator {
            Some(format!(
                "Enabled ({}:{})",
                rig_cfg.pskreporter.host, rig_cfg.pskreporter.port
            ))
        } else {
            Some(format!(
                "Enabled but inactive (missing locator source) ({}:{})",
                rig_cfg.pskreporter.host, rig_cfg.pskreporter.port
            ))
        }
    } else {
        Some("Disabled".to_string())
    };

    let aprs_is_status = if rig_cfg.aprsfi.enabled {
        let cs = rig_cfg
            .aprsfi
            .callsign
            .as_deref()
            .or(callsign.as_deref())
            .unwrap_or("");
        if cs.trim().is_empty() {
            Some("Enabled but inactive (missing callsign)".to_string())
        } else {
            Some(format!(
                "Enabled ({}:{}, {})",
                rig_cfg.aprsfi.host, rig_cfg.aprsfi.port, cs
            ))
        }
    } else {
        Some("Disabled".to_string())
    };

    rig_task::RigTaskConfig {
        registry,
        rig_id: rig_cfg.id.clone(),
        rig_model,
        access,
        polling: AdaptivePolling::new(
            Duration::from_millis(rig_cfg.behavior.poll_interval_ms),
            Duration::from_millis(rig_cfg.behavior.poll_interval_tx_ms),
        ),
        retry: ExponentialBackoff::new(
            rig_cfg.behavior.max_retries.max(1),
            Duration::from_millis(rig_cfg.behavior.retry_base_delay_ms),
            Duration::from_secs(RETRY_MAX_DELAY_SECS),
        ),
        initial_freq_hz: rig_cfg.rig.initial_freq_hz,
        initial_mode: rig_cfg.rig.initial_mode.clone(),
        server_callsign: callsign,
        server_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        server_build_date: Some(env!("TRX_SERVER_BUILD_DATE").to_string()),
        server_latitude: latitude,
        server_longitude: longitude,
        pskreporter_status,
        aprs_is_status,
        histories,
        vfo_prime: rig_cfg.behavior.vfo_prime,
        prebuilt_rig: None,
    }
}

/// Spawn all audio-related tasks for one rig instance.
///
/// `sdr_pcm_rx` carries a live SDR PCM receiver when the rig uses the
/// SoapySDR backend; `None` selects the cpal capture path.
#[allow(clippy::too_many_arguments)]
fn spawn_rig_audio_stack(
    rig_cfg: &RigInstanceConfig,
    state_rx: watch::Receiver<RigState>,
    shutdown_rx: &watch::Receiver<bool>,
    histories: Arc<DecoderHistories>,
    callsign: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    listen_override: Option<IpAddr>,
    sdr_pcm_rx: OptionalSdrPcmRx,
    sdr_ais_pcm_rx: OptionalSdrAisPcmRx,
    sdr_vdes_iq_rx: OptionalSdrVdesIqRx,
    vchan_manager: Option<trx_core::vchan::SharedVChanManager>,
) -> Vec<JoinHandle<()>> {
    let mut handles: Vec<JoinHandle<()>> = Vec::new();

    if !rig_cfg.audio.enabled {
        return handles;
    }

    let audio_listen = SocketAddr::from((
        listen_override.unwrap_or(rig_cfg.audio.listen),
        rig_cfg.audio.port,
    ));
    let stream_info = AudioStreamInfo {
        sample_rate: rig_cfg.audio.sample_rate,
        channels: rig_cfg.audio.channels,
        frame_duration_ms: rig_cfg.audio.frame_duration_ms,
        bitrate_bps: rig_cfg.audio.bitrate_bps,
    };

    let (rx_audio_tx, _) = broadcast::channel::<Bytes>(256);
    let (tx_audio_tx, tx_audio_rx) = mpsc::channel::<Bytes>(64);

    // PCM tap for server-side decoders
    let (pcm_tx, _) = broadcast::channel::<Vec<f32>>(64);
    // Decoded messages broadcast
    let (decode_tx, _) = broadcast::channel::<trx_core::decode::DecodedMessage>(256);

    if rig_cfg.pskreporter.enabled {
        let cs = callsign.clone().unwrap_or_default();
        if cs.trim().is_empty() {
            warn!(
                "[{}] PSK Reporter enabled but [general].callsign is empty; uplink disabled",
                rig_cfg.id
            );
        } else {
            let pr_cfg = rig_cfg.pskreporter.clone();
            let pr_state_rx = state_rx.clone();
            let pr_decode_rx = decode_tx.subscribe();
            let pr_shutdown_rx = shutdown_rx.clone();
            handles.push(tokio::spawn(async move {
                tokio::select! {
                    _ = trx_reporting::pskreporter::run_pskreporter_uplink(
                        pr_cfg,
                        cs,
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

    if rig_cfg.aprsfi.enabled {
        let cs = rig_cfg
            .aprsfi
            .callsign
            .clone()
            .or_else(|| callsign.clone())
            .unwrap_or_default();
        if cs.trim().is_empty() {
            warn!(
                "[{}] APRS-IS IGate enabled but callsign is not set \
                 (set [aprsfi].callsign or [general].callsign); uplink disabled",
                rig_cfg.id
            );
        } else {
            let ai_cfg = rig_cfg.aprsfi.clone();
            let ai_decode_rx = decode_tx.subscribe();
            let ai_shutdown_rx = shutdown_rx.clone();
            handles.push(tokio::spawn(async move {
                tokio::select! {
                    _ = trx_reporting::aprsfi::run_aprsfi_uplink(ai_cfg, cs, latitude, longitude, ai_decode_rx) => {}
                    _ = wait_for_shutdown(ai_shutdown_rx) => {}
                }
            }));
        }
    }

    let decoder_logs = match DecoderLoggers::from_config(&rig_cfg.decode_logs) {
        Ok(v) => v,
        Err(e) => {
            warn!("[{}] Decoder file logging disabled: {}", rig_cfg.id, e);
            None
        }
    };

    if rig_cfg.audio.rx_enabled {
        if let Some(mut sdr_rx) = sdr_pcm_rx {
            // SDR path: the backend pipeline provides demodulated PCM.
            // Forward raw PCM to server-side decoders AND Opus-encode it for
            // TCP audio clients (browser RX audio).
            info!(
                "[{}] using SDR audio source — cpal capture disabled",
                rig_cfg.id
            );
            let pcm_tx_clone = pcm_tx.clone();
            let rx_audio_tx_sdr = rx_audio_tx.clone();
            let sdr_sample_rate = rig_cfg.audio.sample_rate;
            let sdr_channels = rig_cfg.audio.channels;
            let sdr_frame_samples = (rig_cfg.audio.sample_rate as usize
                * rig_cfg.audio.frame_duration_ms as usize)
                / 1000;
            let sdr_bitrate_bps = rig_cfg.audio.bitrate_bps;
            handles.push(tokio::spawn(async move {
                let opus_ch = match sdr_channels {
                    1 => opus::Channels::Mono,
                    2 => opus::Channels::Stereo,
                    n => {
                        tracing::error!("SDR audio: unsupported channel count {}", n);
                        return;
                    }
                };
                let mut encoder =
                    match opus::Encoder::new(sdr_sample_rate, opus_ch, opus::Application::Audio) {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::error!("SDR audio: Opus encoder init failed: {}", e);
                            return;
                        }
                    };
                if let Err(e) = encoder.set_bitrate(opus::Bitrate::Bits(sdr_bitrate_bps as i32)) {
                    tracing::warn!("SDR audio: set_bitrate failed: {}", e);
                }
                if let Err(e) = encoder.set_complexity(5) {
                    tracing::warn!("SDR audio: set_complexity failed: {}", e);
                }
                let mut opus_buf = vec![0u8; 4096];
                loop {
                    match sdr_rx.recv().await {
                        Ok(frame) => {
                            let pcm_frame = match sdr_channels {
                                1 => frame,
                                2 => {
                                    if frame.len() >= sdr_frame_samples * 2 {
                                        frame
                                    } else {
                                        let mut stereo = Vec::with_capacity(frame.len() * 2);
                                        for sample in frame {
                                            stereo.push(sample);
                                            stereo.push(sample);
                                        }
                                        stereo
                                    }
                                }
                                _ => unreachable!("validated above"),
                            };
                            let _ = pcm_tx_clone.send(pcm_frame.clone());
                            if rx_audio_tx_sdr.receiver_count() == 0 {
                                continue;
                            }
                            match encoder.encode_float(&pcm_frame, &mut opus_buf) {
                                Ok(len) => {
                                    let pkt = Bytes::copy_from_slice(&opus_buf[..len]);
                                    let _ = rx_audio_tx_sdr.send(pkt);
                                }
                                Err(e) => {
                                    tracing::warn!("SDR audio: Opus encode error: {}", e);
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("SDR audio bridge: dropped {} frames", n);
                        }
                        Err(_) => break,
                    }
                }
            }));
        } else {
            // cpal path (serial/TCP transceivers)
            let _capture_thread = audio::spawn_audio_capture(
                &rig_cfg.audio,
                rx_audio_tx.clone(),
                Some(pcm_tx.clone()),
                shutdown_rx.clone(),
            );
        }

        // Spawn APRS decoder task
        let aprs_pcm_rx = pcm_tx.subscribe();
        let aprs_state_rx = state_rx.clone();
        let aprs_decode_tx = decode_tx.clone();
        let aprs_sr = rig_cfg.audio.sample_rate;
        let aprs_ch = rig_cfg.audio.channels;
        let aprs_shutdown_rx = shutdown_rx.clone();
        let aprs_logs = decoder_logs.clone();
        let aprs_histories = histories.clone();
        handles.push(tokio::spawn(async move {
            tokio::select! {
                _ = audio::run_aprs_decoder(aprs_sr, aprs_ch as u16, aprs_pcm_rx, aprs_state_rx, aprs_decode_tx, aprs_logs, aprs_histories) => {}
                _ = wait_for_shutdown(aprs_shutdown_rx) => {}
            }
        }));

        // Spawn HF APRS decoder task
        let hf_aprs_pcm_rx = pcm_tx.subscribe();
        let hf_aprs_state_rx = state_rx.clone();
        let hf_aprs_decode_tx = decode_tx.clone();
        let hf_aprs_sr = rig_cfg.audio.sample_rate;
        let hf_aprs_ch = rig_cfg.audio.channels;
        let hf_aprs_shutdown_rx = shutdown_rx.clone();
        let hf_aprs_logs = decoder_logs.clone();
        let hf_aprs_histories = histories.clone();
        handles.push(tokio::spawn(async move {
            tokio::select! {
                _ = audio::run_hf_aprs_decoder(hf_aprs_sr, hf_aprs_ch as u16, hf_aprs_pcm_rx, hf_aprs_state_rx, hf_aprs_decode_tx, hf_aprs_logs, hf_aprs_histories) => {}
                _ = wait_for_shutdown(hf_aprs_shutdown_rx) => {}
            }
        }));

        if let Some((ais_a_pcm_rx, ais_b_pcm_rx)) = sdr_ais_pcm_rx {
            let ais_state_rx = state_rx.clone();
            let ais_decode_tx = decode_tx.clone();
            let ais_shutdown_rx = shutdown_rx.clone();
            let ais_histories = histories.clone();
            let ais_sr = rig_cfg.audio.sample_rate;
            let ais_ch = rig_cfg.audio.channels as u16;
            handles.push(tokio::spawn(async move {
                tokio::select! {
                    _ = audio::run_ais_decoder(ais_sr, ais_ch, ais_a_pcm_rx, ais_b_pcm_rx, ais_state_rx, ais_decode_tx, ais_histories) => {}
                    _ = wait_for_shutdown(ais_shutdown_rx) => {}
                }
            }));
        }

        if let Some(vdes_iq_rx) = sdr_vdes_iq_rx {
            let vdes_state_rx = state_rx.clone();
            let vdes_decode_tx = decode_tx.clone();
            let vdes_shutdown_rx = shutdown_rx.clone();
            let vdes_histories = histories.clone();
            // Mirror channel.rs pipeline_rates: target = audio_sr.max(96_000),
            // decim = sdr_sr / target, actual IQ rate = sdr_sr / decim.
            let vdes_sr = {
                let sdr_sr = rig_cfg.sdr.sample_rate;
                let target = rig_cfg.audio.sample_rate.max(96_000);
                let decim = (sdr_sr / target.max(1)).max(1);
                (sdr_sr / decim).max(1)
            };
            handles.push(tokio::spawn(async move {
                tokio::select! {
                    _ = audio::run_vdes_decoder(vdes_sr, vdes_iq_rx, vdes_state_rx, vdes_decode_tx, vdes_histories) => {}
                    _ = wait_for_shutdown(vdes_shutdown_rx) => {}
                }
            }));
        }

        // Spawn CW decoder task
        let cw_pcm_rx = pcm_tx.subscribe();
        let cw_state_rx = state_rx.clone();
        let cw_decode_tx = decode_tx.clone();
        let cw_sr = rig_cfg.audio.sample_rate;
        let cw_ch = rig_cfg.audio.channels;
        let cw_shutdown_rx = shutdown_rx.clone();
        let cw_logs = decoder_logs.clone();
        let cw_histories = histories.clone();
        handles.push(tokio::spawn(async move {
            tokio::select! {
                _ = audio::run_cw_decoder(cw_sr, cw_ch as u16, cw_pcm_rx, cw_state_rx, cw_decode_tx, cw_logs, cw_histories) => {}
                _ = wait_for_shutdown(cw_shutdown_rx) => {}
            }
        }));

        // Spawn FT8 decoder task
        let ft8_pcm_rx = pcm_tx.subscribe();
        let ft8_state_rx = state_rx.clone();
        let ft8_decode_tx = decode_tx.clone();
        let ft8_sr = rig_cfg.audio.sample_rate;
        let ft8_ch = rig_cfg.audio.channels;
        let ft8_shutdown_rx = shutdown_rx.clone();
        let ft8_logs = decoder_logs.clone();
        let ft8_histories = histories.clone();
        handles.push(tokio::spawn(async move {
            tokio::select! {
                _ = audio::run_ft8_decoder(ft8_sr, ft8_ch as u16, ft8_pcm_rx, ft8_state_rx, ft8_decode_tx, ft8_logs, ft8_histories) => {}
                _ = wait_for_shutdown(ft8_shutdown_rx) => {}
            }
        }));

        // Spawn FT4 decoder task
        let ft4_pcm_rx = pcm_tx.subscribe();
        let ft4_state_rx = state_rx.clone();
        let ft4_decode_tx = decode_tx.clone();
        let ft4_sr = rig_cfg.audio.sample_rate;
        let ft4_ch = rig_cfg.audio.channels;
        let ft4_shutdown_rx = shutdown_rx.clone();
        let ft4_histories = histories.clone();
        handles.push(tokio::spawn(async move {
            tokio::select! {
                _ = audio::run_ft4_decoder(ft4_sr, ft4_ch as u16, ft4_pcm_rx, ft4_state_rx, ft4_decode_tx, ft4_histories) => {}
                _ = wait_for_shutdown(ft4_shutdown_rx) => {}
            }
        }));

        // Spawn FT2 decoder task
        let ft2_pcm_rx = pcm_tx.subscribe();
        let ft2_state_rx = state_rx.clone();
        let ft2_decode_tx = decode_tx.clone();
        let ft2_sr = rig_cfg.audio.sample_rate;
        let ft2_ch = rig_cfg.audio.channels;
        let ft2_shutdown_rx = shutdown_rx.clone();
        let ft2_histories = histories.clone();
        handles.push(tokio::spawn(async move {
            tokio::select! {
                _ = audio::run_ft2_decoder(ft2_sr, ft2_ch as u16, ft2_pcm_rx, ft2_state_rx, ft2_decode_tx, ft2_histories) => {}
                _ = wait_for_shutdown(ft2_shutdown_rx) => {}
            }
        }));

        // Spawn WSPR decoder task
        let wspr_pcm_rx = pcm_tx.subscribe();
        let wspr_state_rx = state_rx.clone();
        let wspr_decode_tx = decode_tx.clone();
        let wspr_sr = rig_cfg.audio.sample_rate;
        let wspr_ch = rig_cfg.audio.channels;
        let wspr_shutdown_rx = shutdown_rx.clone();
        let wspr_logs = decoder_logs.clone();
        let wspr_histories = histories.clone();
        handles.push(tokio::spawn(async move {
            tokio::select! {
                _ = audio::run_wspr_decoder(wspr_sr, wspr_ch as u16, wspr_pcm_rx, wspr_state_rx, wspr_decode_tx, wspr_logs, wspr_histories) => {}
                _ = wait_for_shutdown(wspr_shutdown_rx) => {}
            }
        }));

        // Spawn weather satellite APT decoder task
        let wxsat_pcm_rx = pcm_tx.subscribe();
        let wxsat_state_rx = state_rx.clone();
        let wxsat_decode_tx = decode_tx.clone();
        let wxsat_sr = rig_cfg.audio.sample_rate;
        let wxsat_ch = rig_cfg.audio.channels;
        let wxsat_shutdown_rx = shutdown_rx.clone();
        let wxsat_histories = histories.clone();
        let wxsat_output_dir = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".cache"))
            .join("trx-rs")
            .join("wxsat");
        handles.push(tokio::spawn(async move {
            tokio::select! {
                _ = audio::run_wxsat_decoder(wxsat_sr, wxsat_ch as u16, wxsat_pcm_rx, wxsat_state_rx, wxsat_decode_tx, wxsat_histories, wxsat_output_dir) => {}
                _ = wait_for_shutdown(wxsat_shutdown_rx) => {}
            }
        }));

        // Spawn Meteor-M LRPT decoder task
        let lrpt_pcm_rx = pcm_tx.subscribe();
        let lrpt_state_rx = state_rx.clone();
        let lrpt_decode_tx = decode_tx.clone();
        let lrpt_sr = rig_cfg.audio.sample_rate;
        let lrpt_ch = rig_cfg.audio.channels;
        let lrpt_shutdown_rx = shutdown_rx.clone();
        let lrpt_histories = histories.clone();
        let lrpt_output_dir = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".cache"))
            .join("trx-rs")
            .join("lrpt");
        handles.push(tokio::spawn(async move {
            tokio::select! {
                _ = audio::run_lrpt_decoder(lrpt_sr, lrpt_ch as u16, lrpt_pcm_rx, lrpt_state_rx, lrpt_decode_tx, lrpt_histories, lrpt_output_dir) => {}
                _ = wait_for_shutdown(lrpt_shutdown_rx) => {}
            }
        }));
    }

    if rig_cfg.audio.tx_enabled {
        let _playback_thread =
            audio::spawn_audio_playback(&rig_cfg.audio, tx_audio_rx, shutdown_rx.clone());
    }

    let audio_shutdown_rx = shutdown_rx.clone();
    let audio_histories = histories;
    handles.push(tokio::spawn(async move {
        if let Err(e) = audio::run_audio_listener(
            audio_listen,
            rx_audio_tx,
            tx_audio_tx,
            stream_info,
            decode_tx,
            audio_shutdown_rx,
            audio_histories,
            vchan_manager,
        )
        .await
        {
            error!("Audio listener error: {:?}", e);
        }
    }));

    handles
}

#[tokio::main]
async fn main() -> DynResult<()> {
    let mut bootstrap_ctx = RegistrationContext::new();
    register_builtin_backends_on(&mut bootstrap_ctx);

    let cli = Cli::parse();

    if cli.print_config {
        println!("{}", ServerConfig::example_combined_toml());
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

    // Validate SDR-specific configuration rules.
    let sdr_errors = cfg.validate_sdr();
    if !sdr_errors.is_empty() {
        for e in &sdr_errors {
            tracing::error!("SDR config error: {}", e);
        }
        std::process::exit(1);
    }

    init_logging(cfg.general.log_level.as_deref());

    if let Some(ref path) = config_path {
        info!("Loaded configuration from {}", path.display());
    }

    let registry = Arc::new(bootstrap_ctx);

    // --- Resolve the effective rig list ---
    //
    // Legacy path: no [[rigs]] → synthesise from flat fields + CLI overrides.
    // Multi-rig path: [[rigs]] entries are used as-is; CLI rig/access flags
    // are ignored (no unambiguous target).
    let mut resolved_rigs = cfg.resolved_rigs();

    let (callsign, latitude, longitude) = if cfg.rigs.is_empty() {
        // Apply CLI overrides to the first (only) rig.
        let legacy = resolve_config(&cli, &cfg, &registry)?;

        let first = resolved_rigs
            .first_mut()
            .expect("resolved_rigs always has ≥1 entry");

        first.rig.model = Some(legacy.rig.clone());
        match &legacy.access {
            RigAccess::Serial { path, baud } => {
                first.rig.access.access_type = Some("serial".to_string());
                first.rig.access.port = Some(path.clone());
                first.rig.access.baud = Some(*baud);
            }
            RigAccess::Tcp { addr } => {
                first.rig.access.access_type = Some("tcp".to_string());
                // Split "host:port" back into parts.
                if let Some(colon) = addr.rfind(':') {
                    first.rig.access.host = Some(addr[..colon].to_string());
                    first.rig.access.tcp_port = addr[colon + 1..].parse().ok();
                }
            }
            RigAccess::Sdr { args } => {
                first.rig.access.access_type = Some("sdr".to_string());
                first.rig.access.args = Some(args.clone());
            }
        }
        (legacy.callsign, legacy.latitude, legacy.longitude)
    } else {
        // Multi-rig path: validate all rig models are registered.
        for rig_cfg in &resolved_rigs {
            if let Some(ref model) = rig_cfg.rig.model {
                let norm = normalize_name(model);
                if !registry.is_backend_registered(&norm) {
                    return Err(format!(
                        "Unknown rig model '{}' for rig '{}' (available: {})",
                        norm,
                        rig_cfg.id,
                        registry.registered_backends().join(", ")
                    )
                    .into());
                }
            }
        }
        let callsign = cli
            .callsign
            .clone()
            .or_else(|| cfg.general.callsign.clone());
        (callsign, cfg.general.latitude, cfg.general.longitude)
    };

    info!(
        "Starting trx-server with {} rig(s): {}",
        resolved_rigs.len(),
        resolved_rigs
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(ref cs) = callsign {
        info!("Callsign: {}", cs);
    }

    let mut task_handles: Vec<JoinHandle<()>> = Vec::new();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Open persistent history DB once; each rig uses rig_id-prefixed keys.
    let history_db = {
        let db = history_store::open_db();
        info!("Decode history DB: {}", history_store::db_path().display());
        Arc::new(std::sync::Mutex::new(db))
    };
    let mut rig_histories_for_flush: Vec<(String, Arc<audio::DecoderHistories>)> = Vec::new();

    // The first rig id is the default for backward-compat clients that omit rig_id.
    let default_rig_id = resolved_rigs
        .first()
        .map(|r| r.id.clone())
        .unwrap_or_else(|| "default".to_string());

    let mut rig_handles: HashMap<String, RigHandle> = HashMap::new();

    for rig_cfg in &resolved_rigs {
        let rig_model = normalize_name(rig_cfg.rig.model.as_deref().unwrap_or(""));

        let access = access_from_rig_instance(rig_cfg)?;

        match &access {
            RigAccess::Serial { path, baud } => {
                info!(
                    "[{}] Starting (rig: {}, access: serial {} @ {} baud)",
                    rig_cfg.id, rig_model, path, baud
                );
            }
            RigAccess::Tcp { addr } => {
                info!(
                    "[{}] Starting (rig: {}, access: tcp {})",
                    rig_cfg.id, rig_model, addr
                );
            }
            RigAccess::Sdr { args } => {
                info!(
                    "[{}] Starting (rig: {}, access: sdr {})",
                    rig_cfg.id, rig_model, args
                );
            }
        }

        // Build SDR rig when applicable.
        #[cfg(feature = "soapysdr")]
        let mut sdr_vchan_manager: Option<trx_core::vchan::SharedVChanManager> = None;
        #[cfg(feature = "soapysdr")]
        let (sdr_prebuilt_rig, sdr_pcm_rx, sdr_ais_pcm_rx, sdr_vdes_iq_rx): (
            OptionalSdrRig,
            OptionalSdrPcmRx,
            OptionalSdrAisPcmRx,
            OptionalSdrVdesIqRx,
        ) = if rig_cfg.rig.access.access_type.as_deref() == Some("sdr") {
            let (rig, pcm_rx, ais_pcm_rx, vdes_iq_rx, vchan_mgr) =
                build_sdr_rig_from_instance(rig_cfg)?;
            sdr_vchan_manager = Some(vchan_mgr);
            (Some(rig), Some(pcm_rx), Some(ais_pcm_rx), Some(vdes_iq_rx))
        } else {
            (None, None, None, None)
        };

        #[cfg(not(feature = "soapysdr"))]
        let (sdr_prebuilt_rig, sdr_pcm_rx, sdr_ais_pcm_rx, sdr_vdes_iq_rx): (
            OptionalSdrRig,
            OptionalSdrPcmRx,
            OptionalSdrAisPcmRx,
            OptionalSdrVdesIqRx,
        ) = (None, None, None, None);

        let histories = DecoderHistories::new();
        if let Ok(db_guard) = history_db.lock() {
            history_store::load_all(&db_guard, &rig_cfg.id, &histories);
        }
        rig_histories_for_flush.push((rig_cfg.id.clone(), histories.clone()));

        let (rig_tx, rig_rx) = mpsc::channel::<RigRequest>(RIG_TASK_CHANNEL_BUFFER);
        let mut initial_state = RigState::new_with_metadata(
            callsign.clone(),
            Some(env!("CARGO_PKG_VERSION").to_string()),
            Some(env!("TRX_SERVER_BUILD_DATE").to_string()),
            latitude,
            longitude,
            rig_cfg.rig.initial_freq_hz,
            rig_cfg.rig.initial_mode.clone(),
        );
        initial_state.pskreporter_status = if rig_cfg.pskreporter.enabled {
            Some(format!(
                "Enabled ({}:{})",
                rig_cfg.pskreporter.host, rig_cfg.pskreporter.port
            ))
        } else {
            Some("Disabled".to_string())
        };
        initial_state.aprs_is_status = if rig_cfg.aprsfi.enabled {
            Some(format!(
                "Enabled ({}:{})",
                rig_cfg.aprsfi.host, rig_cfg.aprsfi.port
            ))
        } else {
            Some("Disabled".to_string())
        };
        let (state_tx, state_rx) = watch::channel(initial_state);

        let mut task_config = build_rig_task_config(
            rig_cfg,
            rig_model,
            access,
            callsign.clone(),
            latitude,
            longitude,
            Arc::clone(&registry),
            histories.clone(),
        );
        if let Some(prebuilt) = sdr_prebuilt_rig {
            task_config.prebuilt_rig = Some(prebuilt);
            // SDR signal strength is a pre-computed field read — no serial
            // round-trip — so we can poll much faster than the CAT default.
            task_config.polling =
                AdaptivePolling::new(Duration::from_millis(100), Duration::from_millis(100));
        }

        // Spawn rig task.
        let rig_shutdown_rx = shutdown_rx.clone();
        task_handles.push(tokio::spawn(async move {
            if let Err(e) =
                rig_task::run_rig_task(task_config, rig_rx, state_tx, rig_shutdown_rx).await
            {
                error!("Rig task error: {:?}", e);
            }
        }));

        // Spawn audio stack.
        // listen_override priority: --listen CLI flag > global [audio].listen > per-rig default.
        let audio_listen_override = cli.listen.or(Some(cfg.audio.listen));
        #[cfg(feature = "soapysdr")]
        let audio_vchan_manager = sdr_vchan_manager.clone();
        #[cfg(not(feature = "soapysdr"))]
        let audio_vchan_manager: Option<trx_core::vchan::SharedVChanManager> = None;

        let audio_handles = spawn_rig_audio_stack(
            rig_cfg,
            state_rx.clone(),
            &shutdown_rx,
            histories.clone(),
            callsign.clone(),
            latitude,
            longitude,
            audio_listen_override,
            sdr_pcm_rx,
            sdr_ais_pcm_rx,
            sdr_vdes_iq_rx,
            audio_vchan_manager,
        );
        task_handles.extend(audio_handles);

        rig_handles.insert(
            rig_cfg.id.clone(),
            RigHandle {
                rig_id: rig_cfg.id.clone(),
                display_name: rig_cfg.display_name().to_string(),
                rig_tx,
                state_rx,
                audio_port: rig_cfg.audio.port,
            },
        );
    }

    // Spawn periodic flush of decode history to disk (every 60 s).
    history_store::spawn_flush_task(history_db, rig_histories_for_flush);

    // Start periodic TLE refresh from CelesTrak (on start + once/day).
    // Called once globally rather than per-rig to avoid redundant HTTP fetches
    // and write-lock contention on the TLE store.
    trx_core::geo::spawn_tle_refresh_task();

    // Start JSON TCP listener.
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
        let rigs_arc = Arc::new(rig_handles);
        let listener_shutdown_rx = shutdown_rx.clone();
        task_handles.push(tokio::spawn(async move {
            let station_coords = latitude.zip(longitude);
            if let Err(e) = listener::run_listener(
                listen_addr,
                rigs_arc,
                default_rig_id,
                auth_tokens,
                station_coords,
                listener_shutdown_rx,
            )
            .await
            {
                error!("Listener error: {:?}", e);
            }
        }));
    }

    signal::ctrl_c().await?;
    info!("Ctrl+C received, shutting down");
    let _ = shutdown_tx.send(true);
    tokio::time::sleep(Duration::from_millis(400)).await;

    for handle in &task_handles {
        if !handle.is_finished() {
            handle.abort();
        }
    }
    for handle in task_handles {
        let _ = handle.await;
    }
    // Force exit so that native threads stuck in blocking hardware I/O
    // (e.g. SoapySDR/USB transfers in D-state) cannot prevent shutdown.
    std::process::exit(0);
}
