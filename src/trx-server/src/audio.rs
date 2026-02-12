// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio capture, playback, and TCP streaming for trx-server.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use std::{collections::VecDeque, sync::Mutex};

use bytes::Bytes;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{error, info, warn};

use trx_core::audio::{
    read_audio_msg, write_audio_msg, AudioStreamInfo, AUDIO_MSG_APRS_DECODE, AUDIO_MSG_CW_DECODE,
    AUDIO_MSG_FT8_DECODE, AUDIO_MSG_RX_FRAME, AUDIO_MSG_STREAM_INFO, AUDIO_MSG_TX_FRAME,
    AUDIO_MSG_WSPR_DECODE,
};
use trx_core::decode::{AprsPacket, DecodedMessage, Ft8Message, WsprMessage};
use trx_core::rig::state::{RigMode, RigState};
use trx_ft8::Ft8Decoder;
use trx_wspr::WsprDecoder;

use crate::config::AudioConfig;
use crate::decode;

const APRS_HISTORY_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const FT8_HISTORY_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const WSPR_HISTORY_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const FT8_SAMPLE_RATE: u32 = 12_000;
const AUDIO_STREAM_ERROR_LOG_INTERVAL: Duration = Duration::from_secs(10);

struct StreamErrorLogger {
    label: &'static str,
    state: Mutex<StreamErrorState>,
}

#[derive(Default)]
struct StreamErrorState {
    last_error: Option<String>,
    last_logged_at: Option<Instant>,
    suppressed: u64,
}

impl StreamErrorLogger {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            state: Mutex::new(StreamErrorState::default()),
        }
    }

    fn log(&self, err: &str) {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .expect("stream error logger mutex poisoned");
        let should_log_now = match (&state.last_error, state.last_logged_at) {
            (None, _) => true,
            (Some(prev), Some(ts)) => {
                prev != err || now.duration_since(ts) >= AUDIO_STREAM_ERROR_LOG_INTERVAL
            }
            (Some(_), None) => true,
        };

        if should_log_now {
            if state.suppressed > 0 {
                warn!(
                    "{} repeated {} times: {}",
                    self.label,
                    state.suppressed,
                    state.last_error.as_deref().unwrap_or("<unknown>")
                );
            }
            error!("{}: {}", self.label, err);
            state.last_error = Some(err.to_string());
            state.last_logged_at = Some(now);
            state.suppressed = 0;
        } else {
            state.suppressed += 1;
        }
    }
}

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

fn ft8_history() -> &'static Mutex<VecDeque<(Instant, Ft8Message)>> {
    static HISTORY: OnceLock<Mutex<VecDeque<(Instant, Ft8Message)>>> = OnceLock::new();
    HISTORY.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn prune_ft8_history(history: &mut VecDeque<(Instant, Ft8Message)>) {
    let cutoff = Instant::now() - FT8_HISTORY_RETENTION;
    while let Some((ts, _)) = history.front() {
        if *ts < cutoff {
            history.pop_front();
        } else {
            break;
        }
    }
}

pub fn record_ft8_message(msg: Ft8Message) {
    let mut history = ft8_history().lock().expect("ft8 history mutex poisoned");
    history.push_back((Instant::now(), msg));
    prune_ft8_history(&mut history);
}

pub fn snapshot_ft8_history() -> Vec<Ft8Message> {
    let mut history = ft8_history().lock().expect("ft8 history mutex poisoned");
    prune_ft8_history(&mut history);
    history.iter().map(|(_, msg)| msg.clone()).collect()
}

pub fn clear_ft8_history() {
    let mut history = ft8_history().lock().expect("ft8 history mutex poisoned");
    history.clear();
}

fn wspr_history() -> &'static Mutex<VecDeque<(Instant, WsprMessage)>> {
    static HISTORY: OnceLock<Mutex<VecDeque<(Instant, WsprMessage)>>> = OnceLock::new();
    HISTORY.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn prune_wspr_history(history: &mut VecDeque<(Instant, WsprMessage)>) {
    let cutoff = Instant::now() - WSPR_HISTORY_RETENTION;
    while let Some((ts, _)) = history.front() {
        if *ts < cutoff {
            history.pop_front();
        } else {
            break;
        }
    }
}

pub fn snapshot_wspr_history() -> Vec<WsprMessage> {
    let mut history = wspr_history().lock().expect("wspr history mutex poisoned");
    prune_wspr_history(&mut history);
    history.iter().map(|(_, msg)| msg.clone()).collect()
}

pub fn clear_wspr_history() {
    let mut history = wspr_history().lock().expect("wspr history mutex poisoned");
    history.clear();
}

pub fn record_wspr_message(msg: WsprMessage) {
    let mut history = wspr_history().lock().expect("wspr history mutex poisoned");
    history.push_back((Instant::now(), msg));
    prune_wspr_history(&mut history);
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
        if let Err(e) = run_capture(
            sample_rate,
            channels,
            frame_duration_ms,
            bitrate_bps,
            device_name,
            tx,
            pcm_tx,
        ) {
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

    let frame_samples =
        (sample_rate as usize * frame_duration_ms as usize / 1000) * channels as usize;

    let opus_channels = match channels {
        1 => opus::Channels::Mono,
        2 => opus::Channels::Stereo,
        _ => return Err(format!("unsupported channel count: {}", channels).into()),
    };

    let mut encoder = opus::Encoder::new(sample_rate, opus_channels, opus::Application::Audio)?;
    encoder.set_bitrate(opus::Bitrate::Bits(bitrate_bps as i32))?;

    let (sample_tx, sample_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(64);

    let input_err_logger = Arc::new(StreamErrorLogger::new("Audio input stream error"));
    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let _ = sample_tx.try_send(data.to_vec());
        },
        {
            let input_err_logger = input_err_logger.clone();
            move |err| {
                input_err_logger.log(&err.to_string());
            }
        },
        None,
    )?;

    // Start paused — only capture when clients are connected
    info!(
        "Audio capture: ready ({}Hz, {} ch, {}ms frames)",
        sample_rate, channels, frame_duration_ms
    );

    let mut pcm_buf: Vec<f32> = Vec::with_capacity(frame_samples * 2);
    let mut opus_buf = vec![0u8; 4096];
    let mut capturing = false;

    loop {
        let has_receivers =
            tx.receiver_count() > 0 || pcm_tx.as_ref().is_some_and(|p| p.receiver_count() > 0);

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

    let frame_samples =
        (sample_rate as usize * frame_duration_ms as usize / 1000) * channels as usize;

    let opus_channels = match channels {
        1 => opus::Channels::Mono,
        2 => opus::Channels::Stereo,
        _ => return Err(format!("unsupported channel count: {}", channels).into()),
    };

    let mut decoder = opus::Decoder::new(sample_rate, opus_channels)?;

    let ring = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::<f32>::with_capacity(frame_samples * 8),
    ));
    let ring_writer = ring.clone();

    let output_err_logger = Arc::new(StreamErrorLogger::new("Audio output stream error"));
    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut ring = ring.lock().unwrap();
            for sample in data.iter_mut() {
                *sample = ring.pop_front().unwrap_or(0.0);
            }
        },
        {
            let output_err_logger = output_err_logger.clone();
            move |err| {
                output_err_logger.log(&err.to_string());
            }
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
            std::thread::sleep(std::time::Duration::from_millis(
                frame_duration_ms as u64 * 2,
            ));
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
    mut state_rx: watch::Receiver<RigState>,
    decode_tx: broadcast::Sender<DecodedMessage>,
) {
    info!("CW decoder started ({}Hz, {} ch)", sample_rate, channels);
    let mut decoder = decode::cw::CwDecoder::new(sample_rate);
    let mut was_active = false;
    let mut last_reset_seq: u64 = 0;
    let mut active = matches!(state_rx.borrow().status.mode, RigMode::CW | RigMode::CWR);
    let mut last_auto = state_rx.borrow().cw_auto;
    let mut last_wpm = state_rx.borrow().cw_wpm;
    let mut last_tone = state_rx.borrow().cw_tone_hz;
    decoder.set_auto(last_auto);
    decoder.set_wpm(last_wpm);
    decoder.set_tone_hz(last_tone);

    loop {
        if !active {
            match state_rx.changed().await {
                Ok(()) => {
                    let state = state_rx.borrow();
                    active = matches!(state.status.mode, RigMode::CW | RigMode::CWR);
                    if active {
                        pcm_rx = pcm_rx.resubscribe();
                    }
                    if state.cw_auto != last_auto {
                        last_auto = state.cw_auto;
                        decoder.set_auto(last_auto);
                    }
                    if state.cw_wpm != last_wpm {
                        last_wpm = state.cw_wpm;
                        decoder.set_wpm(last_wpm);
                    }
                    if state.cw_tone_hz != last_tone {
                        last_tone = state.cw_tone_hz;
                        decoder.set_tone_hz(last_tone);
                    }
                    if state.cw_decode_reset_seq != last_reset_seq {
                        last_reset_seq = state.cw_decode_reset_seq;
                        decoder.reset();
                        info!("CW decoder reset (seq={})", last_reset_seq);
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
                        if state.cw_auto != last_auto {
                            last_auto = state.cw_auto;
                            decoder.set_auto(last_auto);
                        }
                        if state.cw_wpm != last_wpm {
                            last_wpm = state.cw_wpm;
                            decoder.set_wpm(last_wpm);
                        }
                        if state.cw_tone_hz != last_tone {
                            last_tone = state.cw_tone_hz;
                            decoder.set_tone_hz(last_tone);
                        }
                        if state.cw_decode_reset_seq != last_reset_seq {
                            last_reset_seq = state.cw_decode_reset_seq;
                            decoder.reset();
                            info!("CW decoder reset (seq={})", last_reset_seq);
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
            changed = state_rx.changed() => {
                match changed {
                    Ok(()) => {
                        let state = state_rx.borrow();
                        active = matches!(state.status.mode, RigMode::CW | RigMode::CWR);
                        if state.cw_auto != last_auto {
                            last_auto = state.cw_auto;
                            decoder.set_auto(last_auto);
                        }
                        if state.cw_wpm != last_wpm {
                            last_wpm = state.cw_wpm;
                            decoder.set_wpm(last_wpm);
                        }
                        if state.cw_tone_hz != last_tone {
                            last_tone = state.cw_tone_hz;
                            decoder.set_tone_hz(last_tone);
                        }
                        if state.cw_decode_reset_seq != last_reset_seq {
                            last_reset_seq = state.cw_decode_reset_seq;
                            decoder.reset();
                            info!("CW decoder reset (seq={})", last_reset_seq);
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

fn downmix_mono(frame: Vec<f32>, channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return frame;
    }
    let num_frames = frame.len() / channels as usize;
    let mut mono = Vec::with_capacity(num_frames);
    for i in 0..num_frames {
        mono.push(frame[i * channels as usize]);
    }
    mono
}

fn resample_to_12k(samples: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
    if sample_rate == FT8_SAMPLE_RATE {
        return Some(samples.to_vec());
    }
    if !sample_rate.is_multiple_of(FT8_SAMPLE_RATE) {
        return None;
    }
    let factor = (sample_rate / FT8_SAMPLE_RATE) as usize;
    if factor == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(samples.len() / factor);
    for chunk in samples.chunks_exact(factor) {
        let mut acc = 0.0f32;
        for &s in chunk {
            acc += s;
        }
        out.push(acc / factor as f32);
    }
    Some(out)
}

/// Run the FT8 decoder task. Only processes PCM when rig mode is DIG/USB and enabled.
pub async fn run_ft8_decoder(
    sample_rate: u32,
    channels: u16,
    mut pcm_rx: broadcast::Receiver<Vec<f32>>,
    mut state_rx: watch::Receiver<RigState>,
    decode_tx: broadcast::Sender<DecodedMessage>,
) {
    info!("FT8 decoder started ({}Hz, {} ch)", sample_rate, channels);
    let mut decoder = match Ft8Decoder::new(FT8_SAMPLE_RATE) {
        Ok(decoder) => decoder,
        Err(err) => {
            warn!("FT8 decoder init failed: {}", err);
            return;
        }
    };
    let mut last_reset_seq: u64 = 0;
    let mut active = state_rx.borrow().ft8_decode_enabled
        && matches!(state_rx.borrow().status.mode, RigMode::DIG | RigMode::USB);
    let mut ft8_buf: Vec<f32> = Vec::new();
    let mut last_slot: i64 = -1;
    let slot_len_s: i64 = 15;

    loop {
        if !active {
            match state_rx.changed().await {
                Ok(()) => {
                    let state = state_rx.borrow();
                    active = state.ft8_decode_enabled
                        && matches!(state.status.mode, RigMode::DIG | RigMode::USB);
                    if active {
                        pcm_rx = pcm_rx.resubscribe();
                    }
                    if state.ft8_decode_reset_seq != last_reset_seq {
                        last_reset_seq = state.ft8_decode_reset_seq;
                        decoder.reset();
                        ft8_buf.clear();
                    }
                    last_slot = -1;
                }
                Err(_) => break,
            }
            continue;
        }

        tokio::select! {
            recv = pcm_rx.recv() => {
                match recv {
                    Ok(frame) => {
                        let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                            Ok(dur) => dur.as_secs() as i64,
                            Err(_) => 0,
                        };
                        let slot = now / slot_len_s;
                        if slot != last_slot {
                            last_slot = slot;
                            decoder.reset();
                            ft8_buf.clear();
                        }

                        let state = state_rx.borrow();
                        if state.ft8_decode_reset_seq != last_reset_seq {
                            last_reset_seq = state.ft8_decode_reset_seq;
                            decoder.reset();
                            ft8_buf.clear();
                        }

                        let mono = downmix_mono(frame, channels);
                        let Some(resampled) = resample_to_12k(&mono, sample_rate) else {
                            warn!("FT8 decoder: unsupported sample rate {}", sample_rate);
                            break;
                        };
                        ft8_buf.extend_from_slice(&resampled);

                        while ft8_buf.len() >= decoder.block_size() {
                            let block: Vec<f32> = ft8_buf.drain(..decoder.block_size()).collect();
                            decoder.process_block(&block);
                            let results = decoder.decode_if_ready(100);
                            if !results.is_empty() {
                                for res in results {
                                    let ts_ms = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                                        Ok(dur) => dur.as_millis() as i64,
                                        Err(_) => 0,
                                    };
                                    let msg = Ft8Message {
                                        ts_ms,
                                        snr_db: res.snr_db,
                                        dt_s: res.dt_s,
                                        freq_hz: res.freq_hz,
                                        message: res.text,
                                    };
                                    record_ft8_message(msg.clone());
                                    let _ = decode_tx.send(DecodedMessage::Ft8(msg));
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("FT8 decoder: dropped {} PCM frames", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            changed = state_rx.changed() => {
                match changed {
                    Ok(()) => {
                        let state = state_rx.borrow();
                        active = state.ft8_decode_enabled
                            && matches!(state.status.mode, RigMode::DIG | RigMode::USB);
                        if state.ft8_decode_reset_seq != last_reset_seq {
                            last_reset_seq = state.ft8_decode_reset_seq;
                            decoder.reset();
                            ft8_buf.clear();
                        }
                        if !active {
                            decoder.reset();
                            ft8_buf.clear();
                            last_slot = -1;
                        } else {
                            pcm_rx = pcm_rx.resubscribe();
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

/// Run the WSPR decoder task. Mirrors FT8 lifecycle/slot behavior.
///
/// Note: decoding engine integration is intentionally staged; this task already
/// participates in enable/disable/reset flow and transport plumbing.
pub async fn run_wspr_decoder(
    sample_rate: u32,
    channels: u16,
    mut pcm_rx: broadcast::Receiver<Vec<f32>>,
    mut state_rx: watch::Receiver<RigState>,
    decode_tx: broadcast::Sender<DecodedMessage>,
) {
    info!("WSPR decoder started ({}Hz, {} ch)", sample_rate, channels);
    let decoder = match WsprDecoder::new() {
        Ok(decoder) => decoder,
        Err(err) => {
            warn!("WSPR decoder init failed: {}", err);
            return;
        }
    };
    let mut last_reset_seq: u64 = 0;
    let mut active = state_rx.borrow().wspr_decode_enabled
        && matches!(state_rx.borrow().status.mode, RigMode::DIG | RigMode::USB);
    let mut slot_buf: Vec<f32> = Vec::new();
    let mut last_slot: i64 = -1;
    let slot_len_s: i64 = 120;

    loop {
        if !active {
            match state_rx.changed().await {
                Ok(()) => {
                    let state = state_rx.borrow();
                    active = state.wspr_decode_enabled
                        && matches!(state.status.mode, RigMode::DIG | RigMode::USB);
                    if active {
                        pcm_rx = pcm_rx.resubscribe();
                    }
                    if state.wspr_decode_reset_seq != last_reset_seq {
                        last_reset_seq = state.wspr_decode_reset_seq;
                    }
                    slot_buf.clear();
                    last_slot = -1;
                }
                Err(_) => break,
            }
            continue;
        }

        tokio::select! {
            recv = pcm_rx.recv() => {
                match recv {
                    Ok(frame) => {
                        let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                            Ok(dur) => dur.as_secs() as i64,
                            Err(_) => 0,
                        };
                        let slot = now / slot_len_s;
                        if last_slot == -1 {
                            last_slot = slot;
                        } else if slot != last_slot {
                            let base_freq = state_rx.borrow().status.freq.hz;
                            match decoder.decode_slot(&slot_buf, Some(base_freq)) {
                                Ok(results) => {
                                    for res in results {
                                        let ts_ms = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                                            Ok(dur) => dur.as_millis() as i64,
                                            Err(_) => 0,
                                        };
                                        let msg = WsprMessage {
                                            ts_ms,
                                            snr_db: res.snr_db,
                                            dt_s: res.dt_s,
                                            freq_hz: res.freq_hz,
                                            message: res.message,
                                        };
                                        record_wspr_message(msg.clone());
                                        let _ = decode_tx.send(DecodedMessage::Wspr(msg));
                                    }
                                }
                                Err(err) => warn!("WSPR decode failed: {}", err),
                            }
                            slot_buf.clear();
                            last_slot = slot;
                        }

                        let state = state_rx.borrow();
                        if state.wspr_decode_reset_seq != last_reset_seq {
                            last_reset_seq = state.wspr_decode_reset_seq;
                            slot_buf.clear();
                            last_slot = slot;
                        }

                        let mono = downmix_mono(frame, channels);
                        let Some(resampled) = resample_to_12k(&mono, sample_rate) else {
                            warn!("WSPR decoder: unsupported sample rate {}", sample_rate);
                            break;
                        };
                        slot_buf.extend_from_slice(&resampled);
                        if slot_buf.len() > decoder.slot_samples() {
                            let keep = decoder.slot_samples();
                            let drain = slot_buf.len().saturating_sub(keep);
                            if drain > 0 {
                                slot_buf.drain(..drain);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WSPR decoder: dropped {} PCM frames", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            changed = state_rx.changed() => {
                match changed {
                    Ok(()) => {
                        let state = state_rx.borrow();
                        active = state.wspr_decode_enabled
                            && matches!(state.status.mode, RigMode::DIG | RigMode::USB);
                        if state.wspr_decode_reset_seq != last_reset_seq {
                            last_reset_seq = state.wspr_decode_reset_seq;
                            slot_buf.clear();
                            last_slot = -1;
                        }
                        if active {
                            pcm_rx = pcm_rx.resubscribe();
                        } else {
                            slot_buf.clear();
                            last_slot = -1;
                        }
                    }
                    Err(_) => break,
                }
            }
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
    mut shutdown_rx: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!("Audio listener on {}", addr);

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (socket, peer) = accept?;
                info!("Audio client connected: {}", peer);

                let rx_audio = rx_audio.clone();
                let tx_audio = tx_audio.clone();
                let info = stream_info.clone();
                let decode_tx = decode_tx.clone();
                let client_shutdown_rx = shutdown_rx.clone();

                tokio::spawn(async move {
                    if let Err(e) = handle_audio_client(socket, peer, rx_audio, tx_audio, info, decode_tx, client_shutdown_rx).await {
                        warn!("Audio client {} error: {:?}", peer, e);
                    }
                    info!("Audio client {} disconnected", peer);
                });
            }
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => {
                        info!("Audio listener shutting down");
                        break;
                    }
                    Ok(()) => {}
                    Err(_) => break,
                }
            }
        }
    }
    Ok(())
}

async fn handle_audio_client(
    socket: TcpStream,
    peer: SocketAddr,
    rx_audio: broadcast::Sender<Bytes>,
    tx_audio: mpsc::Sender<Bytes>,
    stream_info: AudioStreamInfo,
    decode_tx: broadcast::Sender<DecodedMessage>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let (reader, writer) = socket.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    // Send stream info
    let info_json = serde_json::to_vec(&stream_info).map_err(std::io::Error::other)?;
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
    // Send FT8 history to newly connected client.
    let history = snapshot_ft8_history();
    for msg in history {
        let msg = DecodedMessage::Ft8(msg);
        let msg_type = AUDIO_MSG_FT8_DECODE;
        if let Ok(json) = serde_json::to_vec(&msg) {
            write_audio_msg(&mut writer, msg_type, &json).await?;
        }
    }
    // Send WSPR history to newly connected client.
    let history = snapshot_wspr_history();
    for msg in history {
        let msg = DecodedMessage::Wspr(msg);
        let msg_type = AUDIO_MSG_WSPR_DECODE;
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
                                DecodedMessage::Ft8(_) => AUDIO_MSG_FT8_DECODE,
                                DecodedMessage::Wspr(_) => AUDIO_MSG_WSPR_DECODE,
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
        let msg = tokio::select! {
            msg = read_audio_msg(&mut reader) => msg,
            changed = shutdown_rx.changed() => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow() => {
                        rx_handle.abort();
                        return Ok(());
                    }
                    Ok(()) => continue,
                    Err(_) => {
                        rx_handle.abort();
                        return Ok(());
                    }
                }
            }
        };
        match msg {
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
