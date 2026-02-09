// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio capture, playback, and TCP streaming for trx-server.

use std::net::SocketAddr;
use std::time::{Duration, Instant};
use std::{collections::VecDeque, sync::Mutex};
use std::sync::OnceLock;

use bytes::Bytes;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{error, info, warn};

use trx_core::audio::{
    read_audio_msg, write_audio_msg, AudioStreamInfo, AUDIO_MSG_APRS_DECODE,
    AUDIO_MSG_CW_DECODE, AUDIO_MSG_RX_FRAME, AUDIO_MSG_STREAM_INFO, AUDIO_MSG_TX_FRAME,
};
use trx_core::decode::{AprsPacket, DecodedMessage};
use trx_core::rig::state::{RigMode, RigState};

use crate::config::AudioConfig;
use crate::decode;

const APRS_HISTORY_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

fn aprs_history() -> &'static Mutex<VecDeque<(Instant, AprsPacket)>> {
    static HISTORY: OnceLock<Mutex<VecDeque<(Instant, AprsPacket)>>> = OnceLock::new();
    HISTORY.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn prune_aprs_history(history: &mut VecDeque<(Instant, AprsPacket)>) {
    let cutoff = Instant::now() - APRS_HISTORY_RETENTION;
    while let Some((ts, _)) = history.front() {
        if *ts < cutoff {
            history.pop_front();
        } else {
            break;
        }
    }
}

pub fn record_aprs_packet(pkt: AprsPacket) {
    let mut history = aprs_history().lock().expect("aprs history mutex poisoned");
    history.push_back((Instant::now(), pkt));
    prune_aprs_history(&mut history);
}

pub fn snapshot_aprs_history() -> Vec<AprsPacket> {
    let mut history = aprs_history().lock().expect("aprs history mutex poisoned");
    prune_aprs_history(&mut history);
    history.iter().map(|(_, pkt)| pkt.clone()).collect()
}

pub fn clear_aprs_history() {
    let mut history = aprs_history().lock().expect("aprs history mutex poisoned");
    history.clear();
}

/// Spawn the audio capture thread.
///
/// Opens the configured input device via cpal, accumulates PCM samples into
/// frames of `frame_duration_ms` length, encodes each frame with Opus, and
/// broadcasts the resulting packets.
pub fn spawn_audio_capture(
    cfg: &AudioConfig,
    tx: broadcast::Sender<Bytes>,
    pcm_tx: Option<broadcast::Sender<Vec<f32>>>,
) -> std::thread::JoinHandle<()> {
    let sample_rate = cfg.sample_rate;
    let channels = cfg.channels as u16;
    let frame_duration_ms = cfg.frame_duration_ms;
    let bitrate_bps = cfg.bitrate_bps;
    let device_name = cfg.device.clone();

    std::thread::spawn(move || {
        if let Err(e) =
            run_capture(sample_rate, channels, frame_duration_ms, bitrate_bps, device_name, tx, pcm_tx)
        {
            error!("Audio capture thread error: {}", e);
        }
    })
}

fn run_capture(
    sample_rate: u32,
    channels: u16,
    frame_duration_ms: u16,
    bitrate_bps: u32,
    device_name: Option<String>,
    tx: broadcast::Sender<Bytes>,
    pcm_tx: Option<broadcast::Sender<Vec<f32>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = if let Some(ref name) = device_name {
        host.input_devices()?
            .find(|d| d.name().map(|n| n == *name).unwrap_or(false))
            .ok_or_else(|| format!("audio input device '{}' not found", name))?
    } else {
        host.default_input_device()
            .ok_or("no default audio input device")?
    };

    info!(
        "Audio capture: using device '{}'",
        device.name().unwrap_or_else(|_| "unknown".into())
    );

    let config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let frame_samples = (sample_rate as usize * frame_duration_ms as usize / 1000) * channels as usize;

    let opus_channels = match channels {
        1 => opus::Channels::Mono,
        2 => opus::Channels::Stereo,
        _ => return Err(format!("unsupported channel count: {}", channels).into()),
    };

    let mut encoder = opus::Encoder::new(sample_rate, opus_channels, opus::Application::Audio)?;
    encoder.set_bitrate(opus::Bitrate::Bits(bitrate_bps as i32))?;

    let (sample_tx, sample_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(64);

    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let _ = sample_tx.try_send(data.to_vec());
        },
        move |err| {
            error!("Audio input stream error: {}", err);
        },
        None,
    )?;

    // Start paused — only capture when clients are connected
    info!("Audio capture: ready ({}Hz, {} ch, {}ms frames)", sample_rate, channels, frame_duration_ms);

    let mut pcm_buf: Vec<f32> = Vec::with_capacity(frame_samples * 2);
    let mut opus_buf = vec![0u8; 4096];
    let mut capturing = false;

    loop {
        let has_receivers = tx.receiver_count() > 0
            || pcm_tx.as_ref().map_or(false, |p| p.receiver_count() > 0);

        if has_receivers && !capturing {
            let _ = stream.play();
            capturing = true;
            info!("Audio capture: started");
        } else if !has_receivers && capturing {
            let _ = stream.pause();
            capturing = false;
            pcm_buf.clear();
            // Drain any buffered samples
            while sample_rx.try_recv().is_ok() {}
            info!("Audio capture: paused (no listeners)");
        }

        if !capturing {
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }

        match sample_rx.recv() {
            Ok(samples) => {
                pcm_buf.extend_from_slice(&samples);
                while pcm_buf.len() >= frame_samples {
                    let frame: Vec<f32> = pcm_buf.drain(..frame_samples).collect();
                    if let Some(ref pcm_tx) = pcm_tx {
                        let _ = pcm_tx.send(frame.clone());
                    }
                    match encoder.encode_float(&frame, &mut opus_buf) {
                        Ok(len) => {
                            let packet = Bytes::copy_from_slice(&opus_buf[..len]);
                            let _ = tx.send(packet);
                        }
                        Err(e) => {
                            warn!("Opus encode error: {}", e);
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }

    Ok(())
}

/// Spawn the audio playback task.
///
/// Receives Opus packets, decodes them, and plays through cpal output.
pub fn spawn_audio_playback(
    cfg: &AudioConfig,
    rx: mpsc::Receiver<Bytes>,
) -> std::thread::JoinHandle<()> {
    let sample_rate = cfg.sample_rate;
    let channels = cfg.channels as u16;
    let frame_duration_ms = cfg.frame_duration_ms;
    let device_name = cfg.device.clone();

    std::thread::spawn(move || {
        if let Err(e) = run_playback(sample_rate, channels, frame_duration_ms, device_name, rx) {
            error!("Audio playback thread error: {}", e);
        }
    })
}

fn run_playback(
    sample_rate: u32,
    channels: u16,
    frame_duration_ms: u16,
    device_name: Option<String>,
    mut rx: mpsc::Receiver<Bytes>,
) -> Result<(), Box<dyn std::error::Error>> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = if let Some(ref name) = device_name {
        host.output_devices()?
            .find(|d| d.name().map(|n| n == *name).unwrap_or(false))
            .ok_or_else(|| format!("audio output device '{}' not found", name))?
    } else {
        host.default_output_device()
            .ok_or("no default audio output device")?
    };

    info!(
        "Audio playback: using device '{}'",
        device.name().unwrap_or_else(|_| "unknown".into())
    );

    let config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let frame_samples = (sample_rate as usize * frame_duration_ms as usize / 1000) * channels as usize;

    let opus_channels = match channels {
        1 => opus::Channels::Mono,
        2 => opus::Channels::Stereo,
        _ => return Err(format!("unsupported channel count: {}", channels).into()),
    };

    let mut decoder = opus::Decoder::new(sample_rate, opus_channels)?;

    let ring = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<f32>::with_capacity(frame_samples * 8)));
    let ring_writer = ring.clone();

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut ring = ring.lock().unwrap();
            for sample in data.iter_mut() {
                *sample = ring.pop_front().unwrap_or(0.0);
            }
        },
        move |err| {
            error!("Audio output stream error: {}", err);
        },
        None,
    )?;

    // Start paused — only play when TX packets arrive
    info!("Audio playback: ready ({}Hz, {} ch)", sample_rate, channels);

    let mut pcm_buf = vec![0f32; frame_samples];
    let mut playing = false;

    while let Some(packet) = rx.blocking_recv() {
        if !playing {
            stream.play()?;
            playing = true;
            info!("Audio playback: started");
        }

        match decoder.decode_float(&packet, &mut pcm_buf, false) {
            Ok(decoded) => {
                let mut ring = ring_writer.lock().unwrap();
                ring.extend(&pcm_buf[..decoded * channels as usize]);
            }
            Err(e) => {
                warn!("Opus decode error: {}", e);
            }
        }

        // Pause when no more packets are queued to avoid ALSA underruns
        if rx.is_empty() {
            // Drain remaining samples before pausing
            std::thread::sleep(std::time::Duration::from_millis(frame_duration_ms as u64 * 2));
            if rx.is_empty() {
                let _ = stream.pause();
                playing = false;
                ring_writer.lock().unwrap().clear();
                info!("Audio playback: paused (idle)");
            }
        }
    }

    Ok(())
}

/// Run the APRS decoder task. Only processes PCM when rig mode is PKT.
pub async fn run_aprs_decoder(
    sample_rate: u32,
    channels: u16,
    mut pcm_rx: broadcast::Receiver<Vec<f32>>,
    mut state_rx: watch::Receiver<RigState>,
    decode_tx: broadcast::Sender<DecodedMessage>,
) {
    info!("APRS decoder started ({}Hz, {} ch)", sample_rate, channels);
    let mut decoder = decode::aprs::AprsDecoder::new(sample_rate);
    let mut was_active = false;
    let mut last_reset_seq: u64 = 0;
    let mut active = matches!(state_rx.borrow().status.mode, RigMode::PKT);

    loop {
        if !active {
            match state_rx.changed().await {
                Ok(()) => {
                    let state = state_rx.borrow();
                    active = matches!(state.status.mode, RigMode::PKT);
                    if active {
                        pcm_rx = pcm_rx.resubscribe();
                    }
                    if state.aprs_decode_reset_seq != last_reset_seq {
                        last_reset_seq = state.aprs_decode_reset_seq;
                        decoder.reset();
                        info!("APRS decoder reset (seq={})", last_reset_seq);
                    }
                }
                Err(_) => break,
            }
            continue;
        }

        tokio::select! {
            recv = pcm_rx.recv() => {
                match recv {
                    Ok(frame) => {
                        let state = state_rx.borrow();
                        if state.aprs_decode_reset_seq != last_reset_seq {
                            last_reset_seq = state.aprs_decode_reset_seq;
                            decoder.reset();
                            info!("APRS decoder reset (seq={})", last_reset_seq);
                        }

                        // Downmix to mono if stereo
                        let mono = if channels > 1 {
                            let num_frames = frame.len() / channels as usize;
                            let mut mono = Vec::with_capacity(num_frames);
                            for i in 0..num_frames {
                                mono.push(frame[i * channels as usize]);
                            }
                            mono
                        } else {
                            frame
                        };

                        was_active = true;
                        for pkt in decoder.process_samples(&mono) {
                            record_aprs_packet(pkt.clone());
                            let _ = decode_tx.send(DecodedMessage::Aprs(pkt));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("APRS decoder: dropped {} PCM frames", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            changed = state_rx.changed() => {
                match changed {
                    Ok(()) => {
                        let state = state_rx.borrow();
                        active = matches!(state.status.mode, RigMode::PKT);
                        if state.aprs_decode_reset_seq != last_reset_seq {
                            last_reset_seq = state.aprs_decode_reset_seq;
                            decoder.reset();
                            info!("APRS decoder reset (seq={})", last_reset_seq);
                        }
                        if !active && was_active {
                            decoder.reset();
                            was_active = false;
                        }
                        if active {
                            pcm_rx = pcm_rx.resubscribe();
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

/// Run the CW decoder task. Only processes PCM when rig mode is CW or CWR.
pub async fn run_cw_decoder(
    sample_rate: u32,
    channels: u16,
    mut pcm_rx: broadcast::Receiver<Vec<f32>>,
    state_rx: watch::Receiver<RigState>,
    decode_tx: broadcast::Sender<DecodedMessage>,
) {
    info!("CW decoder started ({}Hz, {} ch)", sample_rate, channels);
    let mut decoder = decode::cw::CwDecoder::new(sample_rate);
    let mut was_active = false;
    let mut last_reset_seq: u64 = 0;

    loop {
        match pcm_rx.recv().await {
            Ok(frame) => {
                let state = state_rx.borrow().clone();
                let active = true;

                // Check for reset request
                if state.cw_decode_reset_seq != last_reset_seq {
                    last_reset_seq = state.cw_decode_reset_seq;
                    decoder.reset();
                    info!("CW decoder reset (seq={})", last_reset_seq);
                }

                if !active {
                    if was_active {
                        decoder.reset();
                        was_active = false;
                    }
                    continue;
                }
                was_active = true;

                // Downmix to mono if stereo
                let mono = if channels > 1 {
                    let num_frames = frame.len() / channels as usize;
                    let mut mono = Vec::with_capacity(num_frames);
                    for i in 0..num_frames {
                        mono.push(frame[i * channels as usize]);
                    }
                    mono
                } else {
                    frame
                };

                for evt in decoder.process_samples(&mono) {
                    let _ = decode_tx.send(DecodedMessage::Cw(evt));
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("CW decoder: dropped {} PCM frames", n);
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Run the audio TCP listener, accepting client connections.
pub async fn run_audio_listener(
    addr: SocketAddr,
    rx_audio: broadcast::Sender<Bytes>,
    tx_audio: mpsc::Sender<Bytes>,
    stream_info: AudioStreamInfo,
    decode_tx: broadcast::Sender<DecodedMessage>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!("Audio listener on {}", addr);

    loop {
        let (socket, peer) = listener.accept().await?;
        info!("Audio client connected: {}", peer);

        let rx_audio = rx_audio.clone();
        let tx_audio = tx_audio.clone();
        let info = stream_info.clone();
        let decode_tx = decode_tx.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_audio_client(socket, peer, rx_audio, tx_audio, info, decode_tx).await {
                warn!("Audio client {} error: {:?}", peer, e);
            }
            info!("Audio client {} disconnected", peer);
        });
    }
}

async fn handle_audio_client(
    socket: TcpStream,
    peer: SocketAddr,
    rx_audio: broadcast::Sender<Bytes>,
    tx_audio: mpsc::Sender<Bytes>,
    stream_info: AudioStreamInfo,
    decode_tx: broadcast::Sender<DecodedMessage>,
) -> std::io::Result<()> {
    let (reader, writer) = socket.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // Send stream info
    let info_json = serde_json::to_vec(&stream_info)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    write_audio_msg(&mut writer, AUDIO_MSG_STREAM_INFO, &info_json).await?;

    // Send APRS history to newly connected client.
    let history = snapshot_aprs_history();
    for pkt in history {
        let msg = DecodedMessage::Aprs(pkt);
        let msg_type = AUDIO_MSG_APRS_DECODE;
        if let Ok(json) = serde_json::to_vec(&msg) {
            write_audio_msg(&mut writer, msg_type, &json).await?;
        }
    }

    // Spawn RX + decode forwarding task (shares the writer)
    let mut rx_sub = rx_audio.subscribe();
    let mut decode_sub = decode_tx.subscribe();
    let mut writer_for_rx = writer;
    let rx_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = rx_sub.recv() => {
                    match result {
                        Ok(packet) => {
                            if let Err(e) = write_audio_msg(&mut writer_for_rx, AUDIO_MSG_RX_FRAME, &packet).await {
                                warn!("Audio RX write to {} failed: {}", peer, e);
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Audio RX: {} dropped {} frames", peer, n);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                result = decode_sub.recv() => {
                    match result {
                        Ok(msg) => {
                            let msg_type = match &msg {
                                DecodedMessage::Aprs(_) => AUDIO_MSG_APRS_DECODE,
                                DecodedMessage::Cw(_) => AUDIO_MSG_CW_DECODE,
                            };
                            if let Ok(json) = serde_json::to_vec(&msg) {
                                if let Err(e) = write_audio_msg(&mut writer_for_rx, msg_type, &json).await {
                                    warn!("Audio decode write to {} failed: {}", peer, e);
                                    break;
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Audio decode: {} dropped {} messages", peer, n);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    // Read TX frames from client
    loop {
        match read_audio_msg(&mut reader).await {
            Ok((AUDIO_MSG_TX_FRAME, payload)) => {
                let _ = tx_audio.send(Bytes::from(payload)).await;
            }
            Ok((msg_type, _)) => {
                warn!("Audio: unexpected message type {} from {}", msg_type, peer);
            }
            Err(_) => break,
        }
    }

    rx_handle.abort();
    Ok(())
}
