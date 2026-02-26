// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Local audio bridge for trx-client.
//!
//! Bridges remote Opus RX audio to a local output device and captures local
//! input device audio for upstream TX Opus frames.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{info, warn};

use crate::config::AudioBridgeConfig;
use trx_core::audio::AudioStreamInfo;

const BRIDGE_RETRY_DELAY: Duration = Duration::from_secs(2);

pub fn spawn_audio_bridge(
    cfg: AudioBridgeConfig,
    rx_audio_tx: broadcast::Sender<Bytes>,
    tx_audio_tx: mpsc::Sender<Bytes>,
    mut stream_info_rx: watch::Receiver<Option<AudioStreamInfo>>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while !*shutdown_rx.borrow() {
            let info = match wait_for_stream_info(&mut stream_info_rx, &mut shutdown_rx).await {
                Some(info) => info,
                None => return,
            };

            info!(
                "Audio bridge: starting with stream {}Hz {}ch {}ms",
                info.sample_rate, info.channels, info.frame_duration_ms
            );

            let stop = Arc::new(AtomicBool::new(false));
            let playback_stop = stop.clone();
            let capture_stop = stop.clone();

            let mut rx_packets = rx_audio_tx.subscribe();
            let (rx_bridge_tx, rx_bridge_rx) = std_mpsc::sync_channel::<Bytes>(128);
            let rx_forward_stop = stop.clone();
            let rx_forward = tokio::spawn(async move {
                while !rx_forward_stop.load(Ordering::Relaxed) {
                    match rx_packets.recv().await {
                        Ok(pkt) => {
                            let _ = rx_bridge_tx.try_send(pkt);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                    }
                }
            });

            let playback_cfg = cfg.clone();
            let playback_info = info.clone();
            let playback = std::thread::spawn(move || {
                if let Err(e) =
                    run_playback(playback_cfg, playback_info, rx_bridge_rx, playback_stop)
                {
                    warn!("Audio bridge playback stopped: {}", e);
                }
            });

            let capture_cfg = cfg.clone();
            let capture_info = info.clone();
            let tx_audio_tx_clone = tx_audio_tx.clone();
            let capture = std::thread::spawn(move || {
                if let Err(e) =
                    run_capture(capture_cfg, capture_info, tx_audio_tx_clone, capture_stop)
                {
                    warn!("Audio bridge capture stopped: {}", e);
                }
            });

            tokio::select! {
                _ = shutdown_rx.changed() => {}
                changed = stream_info_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
            }

            stop.store(true, Ordering::Relaxed);
            rx_forward.abort();
            let _ = playback.join();
            let _ = capture.join();

            if *shutdown_rx.borrow() {
                break;
            }
            tokio::time::sleep(BRIDGE_RETRY_DELAY).await;
        }
        info!("Audio bridge stopped");
    })
}

async fn wait_for_stream_info(
    stream_info_rx: &mut watch::Receiver<Option<AudioStreamInfo>>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Option<AudioStreamInfo> {
    loop {
        if *shutdown_rx.borrow() {
            return None;
        }
        if let Some(info) = stream_info_rx.borrow().clone() {
            return Some(info);
        }
        tokio::select! {
            changed = stream_info_rx.changed() => {
                if changed.is_err() {
                    return None;
                }
            }
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    return None;
                }
            }
        }
    }
}

fn run_playback(
    cfg: AudioBridgeConfig,
    info: AudioStreamInfo,
    rx_packets: std_mpsc::Receiver<Bytes>,
    stop: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let host = cpal::default_host();
    let device = select_output_device(&host, cfg.rx_output_device.as_deref())?;
    let stream_cfg = cpal::StreamConfig {
        channels: info.channels as u16,
        sample_rate: cpal::SampleRate(info.sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };
    let channels = stream_cfg.channels as usize;
    let frame_samples =
        (info.sample_rate as usize * info.frame_duration_ms as usize / 1000) * channels;

    let opus_channels = match stream_cfg.channels {
        1 => opus::Channels::Mono,
        2 => opus::Channels::Stereo,
        _ => return Err(format!("unsupported channel count {}", stream_cfg.channels).into()),
    };
    let mut decoder = opus::Decoder::new(info.sample_rate, opus_channels)?;
    let mut pcm_buf = vec![0f32; 5760 * channels];

    let ring = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(
        frame_samples * 8,
    )));
    let ring_cb = ring.clone();
    let rx_gain = cfg.rx_gain.max(0.0);

    let err_stop = stop.clone();
    let stream = device.build_output_stream(
        &stream_cfg,
        move |data: &mut [f32], _| {
            let mut rb = ring_cb.lock().expect("audio playback ring mutex poisoned");
            for sample in data.iter_mut() {
                let v = rb.pop_front().unwrap_or(0.0) * rx_gain;
                *sample = v.clamp(-1.0, 1.0);
            }
        },
        move |err| {
            warn!("Audio bridge playback stream error: {}", err);
            err_stop.store(true, Ordering::Relaxed);
        },
        None,
    )?;

    stream.play()?;
    info!(
        "Audio bridge playback active on '{}'",
        device.name().unwrap_or_else(|_| "unknown".to_string())
    );

    while !stop.load(Ordering::Relaxed) {
        match rx_packets.recv_timeout(Duration::from_millis(200)) {
            Ok(packet) => match decoder.decode_float(&packet, &mut pcm_buf, false) {
                Ok(decoded_samples_per_channel) => {
                    let decoded_total = decoded_samples_per_channel * channels;
                    let mut rb = ring.lock().expect("audio playback ring mutex poisoned");
                    rb.extend(pcm_buf[..decoded_total].iter().copied());
                    let max_len = frame_samples * 16;
                    if rb.len() > max_len {
                        let drain = rb.len() - max_len;
                        rb.drain(..drain);
                    }
                }
                Err(e) => warn!("Audio bridge Opus RX decode error: {}", e),
            },
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
            Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = stream.pause();
    Ok(())
}

fn run_capture(
    cfg: AudioBridgeConfig,
    info: AudioStreamInfo,
    tx_audio_tx: mpsc::Sender<Bytes>,
    stop: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let host = cpal::default_host();
    let device = select_input_device(&host, cfg.tx_input_device.as_deref())?;
    let stream_cfg = cpal::StreamConfig {
        channels: info.channels as u16,
        sample_rate: cpal::SampleRate(info.sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };
    let channels = stream_cfg.channels as usize;
    let frame_samples =
        (info.sample_rate as usize * info.frame_duration_ms as usize / 1000) * channels;

    let opus_channels = match stream_cfg.channels {
        1 => opus::Channels::Mono,
        2 => opus::Channels::Stereo,
        _ => return Err(format!("unsupported channel count {}", stream_cfg.channels).into()),
    };
    let mut encoder =
        opus::Encoder::new(info.sample_rate, opus_channels, opus::Application::Audio)?;
    encoder.set_bitrate(opus::Bitrate::Bits(24_000))?;
    let mut opus_buf = vec![0u8; 4096];

    let (sample_tx, sample_rx) = std_mpsc::sync_channel::<Vec<f32>>(64);
    let err_stop = stop.clone();
    let stream = device.build_input_stream(
        &stream_cfg,
        move |data: &[f32], _| {
            let _ = sample_tx.try_send(data.to_vec());
        },
        move |err| {
            warn!("Audio bridge capture stream error: {}", err);
            err_stop.store(true, Ordering::Relaxed);
        },
        None,
    )?;

    stream.play()?;
    info!(
        "Audio bridge capture active on '{}'",
        device.name().unwrap_or_else(|_| "unknown".to_string())
    );

    let tx_gain = cfg.tx_gain.max(0.0);
    let mut pcm = Vec::<f32>::with_capacity(frame_samples * 2);

    while !stop.load(Ordering::Relaxed) {
        match sample_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(samples) => {
                pcm.extend(samples.into_iter().map(|s| (s * tx_gain).clamp(-1.0, 1.0)));
                while pcm.len() >= frame_samples {
                    let frame: Vec<f32> = pcm.drain(..frame_samples).collect();
                    match encoder.encode_float(&frame, &mut opus_buf) {
                        Ok(len) => {
                            let pkt = Bytes::copy_from_slice(&opus_buf[..len]);
                            let _ = tx_audio_tx.try_send(pkt);
                        }
                        Err(e) => warn!("Audio bridge Opus TX encode error: {}", e),
                    }
                }
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
            Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = stream.pause();
    Ok(())
}

fn select_output_device(
    host: &cpal::Host,
    preferred_name: Option<&str>,
) -> Result<cpal::Device, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(name) = preferred_name {
        if let Some(device) = host
            .output_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        {
            return Ok(device);
        }
        return Err(format!("output device '{}' not found", name).into());
    }
    host.default_output_device()
        .ok_or_else(|| "no default output device".into())
}

fn select_input_device(
    host: &cpal::Host,
    preferred_name: Option<&str>,
) -> Result<cpal::Device, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(name) = preferred_name {
        if let Some(device) = host
            .input_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        {
            return Ok(device);
        }
        return Err(format!("input device '{}' not found", name).into());
    }
    host.default_input_device()
        .ok_or_else(|| "no default input device".into())
}
