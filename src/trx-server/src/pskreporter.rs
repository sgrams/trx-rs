// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};
use tokio::time::{self, Duration, Instant};
use tracing::{info, warn};

use trx_core::decode::DecodedMessage;
use trx_core::rig::state::RigState;

use crate::config::PskReporterConfig;

const PSK_REPORTER_IDENTIFIER: u16 = 0x000A;
const RECEIVER_FLOWSET: u16 = 0x9992;
const SENDER_FLOWSET: u16 = 0x9993;

// Receiver: Options Template Set (FlowSetID=0x0003, 36 bytes total)
// Template ID 0x9992 with 3 fields, scope count 1.
// Fields: receiverCallsign (30351.2), receiverLocator (30351.4), decoderSoftware (30351.8)
const RECEIVER_TEMPLATE: &[u8] = &[
    0x00, 0x03, 0x00, 0x24, // FlowSetID=3, Length=36
    0x99, 0x92, 0x00, 0x03, 0x00, 0x01, // TemplateID=0x9992, FieldCount=3, ScopeCount=1
    0x80, 0x02, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // receiverCallsign (30351.2), variable
    0x80, 0x04, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // receiverLocator (30351.4), variable
    0x80, 0x08, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // decoderSoftware (30351.8), variable
    0x00, 0x00, // padding to 4-byte boundary
];

// Sender: Template Set (FlowSetID=0x0002, 68 bytes total)
// Template ID 0x9993 with 8 fields.
// Fields: senderCallsign, frequency, sNR, iMD, mode, informationSource, senderLocator,
//         flowStartSeconds
const SENDER_TEMPLATE: &[u8] = &[
    0x00, 0x02, 0x00, 0x44, // FlowSetID=2, Length=68
    0x99, 0x93, 0x00, 0x08, // TemplateID=0x9993, FieldCount=8
    0x80, 0x01, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // senderCallsign (30351.1), variable
    0x80, 0x05, 0x00, 0x04, 0x00, 0x00, 0x76, 0x8F, // frequency (30351.5), 4 bytes
    0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x76, 0x8F, // sNR (30351.6), 1 byte
    0x80, 0x07, 0x00, 0x01, 0x00, 0x00, 0x76, 0x8F, // iMD (30351.7), 1 byte
    0x80, 0x0A, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // mode (30351.10), variable
    0x80, 0x0B, 0x00, 0x01, 0x00, 0x00, 0x76, 0x8F, // informationSource (30351.11), 1 byte
    0x80, 0x03, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // senderLocator (30351.3), variable
    0x00, 0x96, 0x00, 0x04, // flowStartSeconds (150), 4 bytes
];

// Send at most one packet every 5 minutes per PSKReporter spec.
const FLUSH_INTERVAL_SECS: u64 = 300;
// Retransmit template descriptors once per hour (plus first 3 packets on startup).
const TEMPLATE_RESEND_SECS: u64 = 3600;

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
    // Deduplicated pending spots: callsign → most-recent Spot.
    let mut pending: HashMap<String, Spot> = HashMap::new();
    let mut stats_received: u64 = 0;
    let mut stats_sent: u64 = 0;
    let mut stats_skipped: u64 = 0;
    let mut stats_send_err: u64 = 0;
    let mut stats_tick = time::interval(Duration::from_secs(60));
    // Delay first flush by FLUSH_INTERVAL_SECS so we accumulate a useful batch.
    let mut flush_tick = time::interval_at(
        Instant::now() + Duration::from_secs(FLUSH_INTERVAL_SECS),
        Duration::from_secs(FLUSH_INTERVAL_SECS),
    );

    loop {
        tokio::select! {
            _ = stats_tick.tick() => {
                info!(
                    "PSK Reporter stats: received={}, sent={}, skipped={}, \
                     send_errors={}, pending={}",
                    stats_received, stats_sent, stats_skipped, stats_send_err,
                    pending.len()
                );
            }
            _ = flush_tick.tick() => {
                if !pending.is_empty() {
                    let spots: Vec<Spot> = pending.drain().map(|(_, v)| v).collect();
                    let n = spots.len() as u64;
                    if let Err(err) = client.send_spots(&spots).await {
                        warn!("PSK Reporter send failed: {}", err);
                        stats_send_err += 1;
                    } else {
                        stats_sent += n;
                    }
                }
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
                // Guard against history replays: reject any message whose timestamp
                // is older than the flush window. Live FT8/WSPR messages are at most
                // a few seconds old; history items can be up to 24 hours old.
                let age = now_unix_seconds().saturating_sub(spot.flow_start_seconds);
                if age > FLUSH_INTERVAL_SECS as u32 {
                    stats_skipped += 1;
                    continue;
                }
                // Keep only the most-recent spot per callsign within the window.
                pending.insert(spot.sender_callsign.clone(), spot);
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
    // Accept both legacy decoder offsets (~kHz audio tones) and already-absolute RF Hz.
    let raw = offset_hz as f64;
    if raw.is_finite() && raw >= 100_000.0 {
        return raw.round() as u64;
    }
    let freq = base_freq_hz as f64 + raw;
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
    // Directed FT8/FT4-style messages are usually "<target> <source> ...".
    if let (Some(first), Some(second)) = (tokens.first(), tokens.get(1)) {
        if is_callsign(first) && is_callsign(second) {
            return Some(second.clone());
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
        if !is_ftx_farewell_token(&t) && is_locator(&t) {
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

fn is_ftx_farewell_token(token: &str) -> bool {
    matches!(token, "RR73" | "73" | "RR")
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
    packets_sent: u32,
    last_template_instant: Option<Instant>,
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
            packets_sent: 0,
            last_template_instant: None,
        })
    }

    async fn send_spots(&mut self, spots: &[Spot]) -> Result<(), String> {
        // Include template descriptors in first 3 packets and once per hour thereafter.
        let include_templates = self.packets_sent < 3
            || self
                .last_template_instant
                .map_or(true, |t| t.elapsed() >= Duration::from_secs(TEMPLATE_RESEND_SECS));

        let packet = self.make_packet(spots, include_templates)?;
        self.socket
            .send(&packet)
            .await
            .map_err(|e| format!("send failed: {e}"))?;

        self.packets_sent += 1;
        // Sequence number = count of reports submitted (not packets).
        self.sequence = self.sequence.wrapping_add(spots.len() as u32);
        if include_templates {
            self.last_template_instant = Some(Instant::now());
        }
        Ok(())
    }

    fn make_packet(&self, spots: &[Spot], include_templates: bool) -> Result<Vec<u8>, String> {
        let now = now_unix_seconds();
        let mut out = Vec::with_capacity(512);

        // IPFIX message header (16 bytes) — total length patched at the end.
        push_u16_be(&mut out, PSK_REPORTER_IDENTIFIER); // version 0x000A
        push_u16_be(&mut out, 0); // length — patched later
        push_u32_be(&mut out, now);
        push_u32_be(&mut out, self.sequence);
        push_u32_be(&mut out, self.session);

        // Template descriptor blocks (optional after first 3 packets).
        if include_templates {
            out.extend_from_slice(RECEIVER_TEMPLATE);
            out.extend_from_slice(SENDER_TEMPLATE);
        }

        // Receiver information data record (FlowSetID 0x9992).
        let mut rx_data: Vec<u8> = Vec::new();
        push_prefixed_string(&mut rx_data, &self.receiver_callsign)?;
        push_prefixed_string(&mut rx_data, &self.receiver_locator)?;
        push_prefixed_string(&mut rx_data, &self.software)?;
        pad_to_4(&mut rx_data);
        push_u16_be(&mut out, RECEIVER_FLOWSET);
        push_u16_be(&mut out, (rx_data.len() + 4) as u16); // length includes 4-byte set header
        out.extend_from_slice(&rx_data);

        // Sender information data records (FlowSetID 0x9993).
        // Field order must match SENDER_TEMPLATE:
        //   senderCallsign, frequency, sNR, iMD, mode, informationSource,
        //   senderLocator, flowStartSeconds
        let mut tx_data: Vec<u8> = Vec::new();
        for spot in spots {
            push_prefixed_string(&mut tx_data, &spot.sender_callsign)?;
            push_u32_be(&mut tx_data, spot.abs_freq_hz.min(u32::MAX as u64) as u32);
            tx_data.push(spot.snr_db.round().clamp(-128.0, 127.0) as i8 as u8);
            tx_data.push(0u8); // iMD — not available from FT8/WSPR decoders
            push_prefixed_string(&mut tx_data, spot.mode)?;
            tx_data.push(1u8); // informationSource = 1 (automatically extracted)
            push_prefixed_string(&mut tx_data, spot.sender_locator.as_deref().unwrap_or(""))?;
            push_u32_be(&mut tx_data, spot.flow_start_seconds);
        }
        pad_to_4(&mut tx_data);
        push_u16_be(&mut out, SENDER_FLOWSET);
        push_u16_be(&mut out, (tx_data.len() + 4) as u16);
        out.extend_from_slice(&tx_data);

        // Patch total packet length into header bytes [2..3].
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

/// Pad `buf` with null bytes until its length is a multiple of 4.
fn pad_to_4(buf: &mut Vec<u8>) {
    let rem = buf.len() % 4;
    if rem != 0 {
        buf.extend(std::iter::repeat(0u8).take(4 - rem));
    }
}

fn push_u16_be(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn push_u32_be(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn push_prefixed_string(buf: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    if bytes.len() > 254 {
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
        assert_eq!(
            parse_sender_callsign_ft8("K1ABC SP2SJG JO93"),
            Some("SP2SJG".to_string())
        );
        assert_eq!(
            parse_sender_callsign_ft8("K1ABC SP2SJG -07"),
            Some("SP2SJG".to_string())
        );
        assert_eq!(parse_locator("CQ SP2SJG JO93"), Some("JO93".to_string()));
        assert_eq!(parse_locator("CQ SP2SJG RR73"), None);
        assert_eq!(parse_locator("SP2SJG RR 73"), None);
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

    #[test]
    fn offset_to_abs_accepts_offset_and_absolute() {
        assert_eq!(offset_to_abs(14_074_000, 1_237.0), 14_075_237);
        assert_eq!(offset_to_abs(14_074_000, 14_075_237.0), 14_075_237);
    }

    #[test]
    fn receiver_template_length_correct() {
        assert_eq!(RECEIVER_TEMPLATE.len(), 36);
        let len = u16::from_be_bytes([RECEIVER_TEMPLATE[2], RECEIVER_TEMPLATE[3]]);
        assert_eq!(len as usize, RECEIVER_TEMPLATE.len());
    }

    #[test]
    fn sender_template_length_correct() {
        assert_eq!(SENDER_TEMPLATE.len(), 68);
        let len = u16::from_be_bytes([SENDER_TEMPLATE[2], SENDER_TEMPLATE[3]]);
        assert_eq!(len as usize, SENDER_TEMPLATE.len());
    }
}
