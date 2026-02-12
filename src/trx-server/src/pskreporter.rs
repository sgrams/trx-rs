// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::time::{SystemTime, UNIX_EPOCH};

use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};
use tokio::time::{self, Duration};
use tracing::{info, warn};

use trx_core::decode::DecodedMessage;
use trx_core::rig::state::RigState;

use crate::config::PskReporterConfig;

const PSK_REPORTER_IDENTIFIER: u16 = 0x000A;
const RECEIVER_DESCRIPTOR: u16 = 0x9992;
const SENDER_DESCRIPTOR: u16 = 0x9993;

const RECEIVER_RECORD_FORMAT: &[u8] = &[0x00, 0x03, 0x00, 0x00, 0x80, 0x02, 0xFF, 0xFF];
const SENDER_RECORD_FORMAT: &[u8] = &[
    0x00, 0x06, 0x00, 0x00, 0x80, 0x01, 0xFF, 0xFF, 0x80, 0x04, 0xFF, 0xFF, 0x80, 0x08, 0xFF, 0xFF,
    0x00, 0x96, 0x00, 0x04,
];

#[derive(Debug, Clone)]
struct Spot {
    sender_callsign: String,
    sender_locator: Option<String>,
    mode: &'static str,
    snr_db: f32,
    abs_freq_hz: u64,
    flow_start_seconds: u32,
}

pub async fn run_pskreporter_uplink(
    cfg: PskReporterConfig,
    receiver_callsign: String,
    latitude: Option<f64>,
    longitude: Option<f64>,
    mut state_rx: watch::Receiver<RigState>,
    mut decode_rx: broadcast::Receiver<DecodedMessage>,
) {
    let receiver_locator = match cfg.receiver_locator.clone().or_else(|| {
        if let (Some(lat), Some(lon)) = (latitude, longitude) {
            Some(maidenhead_from_lat_lon(lat, lon))
        } else {
            None
        }
    }) {
        Some(locator) => locator,
        None => {
            warn!(
                "PSK Reporter enabled but receiver locator is missing \
                 ([pskreporter].receiver_locator or [general].latitude/longitude)"
            );
            return;
        }
    };

    let software = format!("trx-server v{} by SP2SJG", env!("CARGO_PKG_VERSION"));
    let mut client = match PskReporterClient::connect(
        &cfg.host,
        cfg.port,
        receiver_callsign.clone(),
        receiver_locator.clone(),
        software,
    )
    .await
    {
        Ok(client) => client,
        Err(err) => {
            warn!("PSK Reporter init failed: {}", err);
            return;
        }
    };

    info!(
        "PSK Reporter uplink active ({}:{} as {} / {})",
        cfg.host, cfg.port, receiver_callsign, receiver_locator
    );

    let mut current_freq_hz = state_rx.borrow().status.freq.hz;
    let mut stats_received: u64 = 0;
    let mut stats_sent: u64 = 0;
    let mut stats_skipped: u64 = 0;
    let mut stats_send_err: u64 = 0;
    let mut stats_tick = time::interval(Duration::from_secs(60));

    loop {
        tokio::select! {
            _ = stats_tick.tick() => {
                info!(
                    "PSK Reporter stats: received={}, sent={}, skipped={}, send_errors={}",
                    stats_received, stats_sent, stats_skipped, stats_send_err
                );
            }
            changed = state_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                current_freq_hz = state_rx.borrow().status.freq.hz;
            }
            recv = decode_rx.recv() => {
                let decoded = match recv {
                    Ok(v) => v,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("PSK Reporter: dropped {} decode events", n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                stats_received += 1;

                let spot = match decoded_to_spot(decoded, current_freq_hz) {
                    Some(spot) => spot,
                    None => {
                        stats_skipped += 1;
                        continue;
                    }
                };
                if let Err(err) = client.send_spot(&spot).await {
                    warn!("PSK Reporter send failed: {}", err);
                    stats_send_err += 1;
                } else {
                    stats_sent += 1;
                }
            }
        }
    }
}

fn decoded_to_spot(decoded: DecodedMessage, base_freq_hz: u64) -> Option<Spot> {
    match decoded {
        DecodedMessage::Ft8(msg) => {
            let sender_callsign = parse_sender_callsign_ft8(&msg.message)?;
            let sender_locator = parse_locator(&msg.message);
            let abs_freq_hz = offset_to_abs(base_freq_hz, msg.freq_hz);
            Some(Spot {
                sender_callsign,
                sender_locator,
                mode: "FT8",
                snr_db: msg.snr_db,
                abs_freq_hz,
                flow_start_seconds: ts_ms_to_secs(msg.ts_ms),
            })
        }
        DecodedMessage::Wspr(msg) => {
            let sender_callsign = parse_sender_callsign_wspr(&msg.message)?;
            let sender_locator = parse_locator(&msg.message);
            let abs_freq_hz = offset_to_abs(base_freq_hz, msg.freq_hz);
            Some(Spot {
                sender_callsign,
                sender_locator,
                mode: "WSPR",
                snr_db: msg.snr_db,
                abs_freq_hz,
                flow_start_seconds: ts_ms_to_secs(msg.ts_ms),
            })
        }
        _ => None,
    }
}

fn ts_ms_to_secs(ts_ms: i64) -> u32 {
    if ts_ms <= 0 {
        return now_unix_seconds();
    }
    (ts_ms / 1000).clamp(0, u32::MAX as i64) as u32
}

fn now_unix_seconds() -> u32 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.min(u32::MAX as u64) as u32
}

fn offset_to_abs(base_freq_hz: u64, offset_hz: f32) -> u64 {
    let freq = base_freq_hz as f64 + offset_hz as f64;
    if freq.is_finite() && freq > 0.0 {
        freq.round() as u64
    } else {
        base_freq_hz
    }
}

fn parse_sender_callsign_ft8(message: &str) -> Option<String> {
    let tokens: Vec<String> = message.split_whitespace().map(normalize_token).collect();
    if tokens.is_empty() {
        return None;
    }
    let head = tokens[0].as_str();
    if matches!(head, "CQ" | "QRZ" | "DE") {
        if let Some(second) = tokens.get(1) {
            if is_callsign(second) {
                return Some(second.clone());
            }
        }
    }
    tokens.into_iter().find(|t| is_callsign(t))
}

fn parse_sender_callsign_wspr(message: &str) -> Option<String> {
    message
        .split_whitespace()
        .map(normalize_token)
        .find(|t| is_callsign(t))
}

fn normalize_token(token: &str) -> String {
    token
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/')
        .to_ascii_uppercase()
}

fn parse_locator(message: &str) -> Option<String> {
    message.split_whitespace().find_map(|raw| {
        let t = normalize_token(raw);
        if is_locator(&t) {
            Some(t)
        } else {
            None
        }
    })
}

fn is_callsign(token: &str) -> bool {
    if token.len() < 3 || token.len() > 13 {
        return false;
    }
    if matches!(token, "CQ" | "QRZ" | "DE") {
        return false;
    }
    if !token
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '/')
    {
        return false;
    }
    token.chars().any(|c| c.is_ascii_uppercase()) && token.chars().any(|c| c.is_ascii_digit())
}

fn is_locator(token: &str) -> bool {
    if !(token.len() == 4 || token.len() == 6) {
        return false;
    }
    let b = token.as_bytes();
    if !(b[0].is_ascii_uppercase()
        && (b[0] as char) >= 'A'
        && (b[0] as char) <= 'R'
        && b[1].is_ascii_uppercase()
        && (b[1] as char) >= 'A'
        && (b[1] as char) <= 'R')
    {
        return false;
    }
    if !(b[2].is_ascii_digit() && b[3].is_ascii_digit()) {
        return false;
    }
    if token.len() == 6 {
        (b[4] as char).is_ascii_uppercase()
            && (b[4] as char) >= 'A'
            && (b[4] as char) <= 'X'
            && (b[5] as char).is_ascii_uppercase()
            && (b[5] as char) >= 'A'
            && (b[5] as char) <= 'X'
    } else {
        true
    }
}

fn maidenhead_from_lat_lon(lat: f64, lon: f64) -> String {
    let lat = lat.clamp(-90.0, 90.0 - f64::EPSILON);
    let lon = lon.clamp(-180.0, 180.0 - f64::EPSILON);
    let mut adj_lon = lon + 180.0;
    let mut adj_lat = lat + 90.0;

    let field_lon = (adj_lon / 20.0).floor() as u8;
    let field_lat = (adj_lat / 10.0).floor() as u8;
    adj_lon -= (field_lon as f64) * 20.0;
    adj_lat -= (field_lat as f64) * 10.0;

    let square_lon = (adj_lon / 2.0).floor() as u8;
    let square_lat = adj_lat.floor() as u8;
    adj_lon -= (square_lon as f64) * 2.0;
    adj_lat -= square_lat as f64;

    let subsquare_lon = (adj_lon / (5.0 / 60.0)).floor() as u8;
    let subsquare_lat = (adj_lat / (2.5 / 60.0)).floor() as u8;

    format!(
        "{}{}{}{}{}{}",
        (b'A' + field_lon) as char,
        (b'A' + field_lat) as char,
        (b'0' + square_lon) as char,
        (b'0' + square_lat) as char,
        (b'A' + subsquare_lon.min(23)) as char,
        (b'A' + subsquare_lat.min(23)) as char
    )
}

struct PskReporterClient {
    socket: UdpSocket,
    receiver_callsign: String,
    receiver_locator: String,
    software: String,
    sequence: u32,
    session: u32,
}

impl PskReporterClient {
    async fn connect(
        host: &str,
        port: u16,
        receiver_callsign: String,
        receiver_locator: String,
        software: String,
    ) -> Result<Self, String> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| format!("bind failed: {e}"))?;
        socket
            .connect((host, port))
            .await
            .map_err(|e| format!("connect {}:{} failed: {}", host, port, e))?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?;
        let session = (now.as_secs() as u32) ^ now.subsec_nanos() ^ (std::process::id() << 7);

        Ok(Self {
            socket,
            receiver_callsign,
            receiver_locator,
            software,
            sequence: 1,
            session,
        })
    }

    async fn send_spot(&mut self, spot: &Spot) -> Result<(), String> {
        let packet = self.make_packet(spot)?;
        self.socket
            .send(&packet)
            .await
            .map_err(|e| format!("send failed: {e}"))?;
        self.sequence = self.sequence.wrapping_add(1);
        Ok(())
    }

    fn make_packet(&self, spot: &Spot) -> Result<Vec<u8>, String> {
        let now = now_unix_seconds();
        let mut out = Vec::with_capacity(256);

        push_u16_be(&mut out, PSK_REPORTER_IDENTIFIER);
        push_u16_be(&mut out, 0); // patched later
        push_u32_be(&mut out, now);
        push_u32_be(&mut out, self.sequence);
        push_u32_be(&mut out, self.session);

        append_record(&mut out, RECEIVER_DESCRIPTOR, RECEIVER_RECORD_FORMAT);
        append_record(&mut out, SENDER_DESCRIPTOR, SENDER_RECORD_FORMAT);

        let mut receiver_payload = Vec::new();
        push_prefixed_string(&mut receiver_payload, &self.receiver_callsign)?;
        push_prefixed_string(&mut receiver_payload, &self.receiver_locator)?;
        push_prefixed_string(&mut receiver_payload, &self.software)?;
        append_record(&mut out, RECEIVER_DESCRIPTOR, &receiver_payload);

        let mut sender_payload = Vec::new();
        push_prefixed_string(&mut sender_payload, &spot.sender_callsign)?;
        push_u32_be(
            &mut sender_payload,
            spot.abs_freq_hz.min(u32::MAX as u64) as u32,
        );
        sender_payload.push(spot.snr_db.round().clamp(-128.0, 127.0) as i8 as u8);
        push_prefixed_string(&mut sender_payload, spot.mode)?;
        sender_payload.push(1); // information source = local
        push_u32_be(&mut sender_payload, spot.flow_start_seconds);
        push_prefixed_string(
            &mut sender_payload,
            spot.sender_locator.as_deref().unwrap_or(""),
        )?;
        append_record(&mut out, SENDER_DESCRIPTOR, &sender_payload);

        let len = out.len();
        if len > u16::MAX as usize {
            return Err("PSK Reporter packet too large".to_string());
        }
        let be = (len as u16).to_be_bytes();
        out[2] = be[0];
        out[3] = be[1];
        Ok(out)
    }
}

fn append_record(out: &mut Vec<u8>, descriptor: u16, payload: &[u8]) {
    push_u16_be(out, descriptor);
    push_u16_be(out, (payload.len() + 4) as u16);
    out.extend_from_slice(payload);
}

fn push_u16_be(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn push_u32_be(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn push_prefixed_string(buf: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    if bytes.len() > u8::MAX as usize {
        return Err(format!("string too long for PSK Reporter field: {}", value));
    }
    buf.push(bytes.len() as u8);
    buf.extend_from_slice(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ft8_sender_and_locator() {
        assert_eq!(
            parse_sender_callsign_ft8("CQ SP2SJG JO93"),
            Some("SP2SJG".to_string())
        );
        assert_eq!(parse_locator("CQ SP2SJG JO93"), Some("JO93".to_string()));
    }

    #[test]
    fn parses_wspr_sender() {
        assert_eq!(
            parse_sender_callsign_wspr("SP2SJG JO93 37"),
            Some("SP2SJG".to_string())
        );
    }

    #[test]
    fn maidenhead_is_six_chars() {
        let grid = maidenhead_from_lat_lon(52.2297, 21.0122);
        assert_eq!(grid.len(), 6);
    }
}
