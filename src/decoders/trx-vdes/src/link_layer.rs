// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! VDES link-layer frame parsing per ITU-R M.2092-1.
//!
//! After FEC decoding and CRC validation, the decoded information bits
//! contain a link-layer frame with the following structure:
//!
//! ```text
//! ┌────────┬──────────┬──────────┬──────────┬─────────┬─────────┐
//! │ MsgID  │ Repeat   │ SessionID│ SourceID │ Payload │ CRC-16  │
//! │ 4 bits │ 2 bits   │ 6 bits   │ 32 bits  │ variable│ 16 bits │
//! └────────┴──────────┴──────────┴──────────┴─────────┴─────────┘
//! ```
//!
//! This module provides structured parsing of the link-layer header and
//! payload fields for each VDES message type (0–6), including:
//! - Station addressing (source/destination MMSIs)
//! - ASM (Application Specific Message) identification
//! - Geographic bounding box parsing (Message 6)
//! - ACK/NACK channel quality reporting (Message 5)

use crate::crc;

/// Parsed link-layer frame result.
#[derive(Debug, Clone)]
pub struct LinkLayerFrame {
    /// Message type ID (0–6).
    pub message_id: u8,
    /// Repeat indicator (0–3).
    pub repeat: u8,
    /// Session ID (0–63).
    pub session_id: u8,
    /// Source station ID (MMSI-like, 32 bits).
    pub source_id: u32,
    /// Destination station ID for addressed messages.
    pub destination_id: Option<u32>,
    /// Data bit count from the header.
    pub data_count: Option<u16>,
    /// ASM (Application Specific Message) identifier.
    pub asm_identifier: Option<u16>,
    /// ACK/NACK bitmask (Message 5).
    pub ack_nack_mask: Option<u16>,
    /// Channel quality indicator (Message 5).
    pub channel_quality: Option<u8>,
    /// Geographic bounding box: (sw_lat, sw_lon, ne_lat, ne_lon) in degrees.
    pub geo_box: Option<GeoBox>,
    /// Application payload bits (after header, before CRC).
    pub payload_bits: Vec<u8>,
    /// Whether the CRC-16 validated successfully.
    pub crc_ok: bool,
    /// Human-readable message type label.
    pub label: &'static str,
}

/// Geographic bounding box for Message 6.
#[derive(Debug, Clone)]
pub struct GeoBox {
    pub ne_lat: f64,
    pub ne_lon: f64,
    pub sw_lat: f64,
    pub sw_lon: f64,
}

impl GeoBox {
    /// Center latitude of the bounding box.
    pub fn center_lat(&self) -> f64 {
        (self.ne_lat + self.sw_lat) * 0.5
    }
    /// Center longitude of the bounding box.
    pub fn center_lon(&self) -> f64 {
        (self.ne_lon + self.sw_lon) * 0.5
    }
}

/// Minimum bit length for a valid link-layer frame (header + CRC).
const MIN_FRAME_BITS: usize = 4 + 2 + 6 + 32 + 16; // 60 bits

/// Parse a decoded bit stream into a link-layer frame.
///
/// `bits` should be the FEC-decoded information bits including the trailing
/// 16-bit CRC.  Returns `None` if the frame is too short or the message ID
/// is invalid.
pub fn parse_link_layer(bits: &[u8]) -> Option<LinkLayerFrame> {
    if bits.len() < MIN_FRAME_BITS {
        return None;
    }

    let crc_ok = crc::check_crc16(bits);

    // Strip CRC for payload parsing
    let data_bits = &bits[..bits.len() - 16];

    let message_id = read_bits_u8(data_bits, 0, 4)?;
    if message_id > 6 {
        return None;
    }

    let repeat = read_bits_u8(data_bits, 4, 2).unwrap_or(0);
    let session_id = read_bits_u8(data_bits, 6, 6).unwrap_or(0);
    let source_id = read_bits_u32(data_bits, 12, 32).unwrap_or(0);

    let mut frame = LinkLayerFrame {
        message_id,
        repeat,
        session_id,
        source_id,
        destination_id: None,
        data_count: None,
        asm_identifier: None,
        ack_nack_mask: None,
        channel_quality: None,
        geo_box: None,
        payload_bits: Vec::new(),
        crc_ok,
        label: message_label(message_id),
    };

    match message_id {
        0 => parse_msg0(data_bits, &mut frame),
        1 => parse_msg1(data_bits, &mut frame),
        2 => parse_msg2(data_bits, &mut frame),
        3 => parse_msg3(data_bits, &mut frame),
        4 => parse_msg4(data_bits, &mut frame),
        5 => parse_msg5(data_bits, &mut frame),
        6 => parse_msg6(data_bits, &mut frame),
        _ => {}
    }

    Some(frame)
}

/// Message 0: Broadcast (unaddressed data)
///
/// ```text
/// ┌──────┬────────┬─────────┬──────────┬───────────┬─────────┐
/// │MsgID │Repeat  │SessionID│SourceID  │ DataCount │ Payload │
/// │4     │2       │6        │32        │ 11        │variable │
/// └──────┴────────┴─────────┴──────────┴───────────┴─────────┘
/// ```
fn parse_msg0(bits: &[u8], frame: &mut LinkLayerFrame) {
    frame.data_count = read_bits_u16(bits, 44, 11);
    let start = 55;
    frame.payload_bits = extract_payload(bits, start, frame.data_count);
}

/// Message 1: Scheduled (standard TDMA)
///
/// ```text
/// ┌──────┬────────┬─────────┬──────────┬───────────┬────────────┬─────────┐
/// │MsgID │Repeat  │SessionID│SourceID  │ DataCount │ ASM Ident  │ Payload │
/// │4     │2       │6        │32        │ 11        │ 16         │variable │
/// └──────┴────────┴─────────┴──────────┴───────────┴────────────┴─────────┘
/// ```
fn parse_msg1(bits: &[u8], frame: &mut LinkLayerFrame) {
    frame.data_count = read_bits_u16(bits, 44, 11);
    frame.asm_identifier = read_bits_u16(bits, 55, 16);
    let start = 71;
    frame.payload_bits = extract_payload(bits, start, frame.data_count);
}

/// Message 2: Scheduled (ITDMA)
fn parse_msg2(bits: &[u8], frame: &mut LinkLayerFrame) {
    frame.data_count = read_bits_u16(bits, 44, 11);
    frame.asm_identifier = read_bits_u16(bits, 55, 16);
    let start = 71;
    frame.payload_bits = extract_payload(bits, start, frame.data_count);
}

/// Message 3: Addressed (standard TDMA)
///
/// ```text
/// ┌──────┬────────┬─────────┬──────────┬─────────────┬───────────┬────────────┬─────────┐
/// │MsgID │Repeat  │SessionID│ SourceID │DestinationID│ DataCount │ ASM Ident  │ Payload │
/// │4     │2       │6        │32        │32           │ 11        │ 16         │variable │
/// └──────┴────────┴─────────┴──────────┴─────────────┴───────────┴────────────┴─────────┘
/// ```
fn parse_msg3(bits: &[u8], frame: &mut LinkLayerFrame) {
    frame.destination_id = read_bits_u32(bits, 44, 32);
    frame.data_count = read_bits_u16(bits, 76, 11);
    frame.asm_identifier = read_bits_u16(bits, 87, 16);
    let start = 103;
    frame.payload_bits = extract_payload(bits, start, frame.data_count);
}

/// Message 4: Addressed (ITDMA)
fn parse_msg4(bits: &[u8], frame: &mut LinkLayerFrame) {
    frame.destination_id = read_bits_u32(bits, 44, 32);
    frame.data_count = read_bits_u16(bits, 76, 11);
    frame.asm_identifier = read_bits_u16(bits, 87, 16);
    let start = 103;
    frame.payload_bits = extract_payload(bits, start, frame.data_count);
}

/// Message 5: Acknowledge (ACK/NACK)
///
/// ```text
/// ┌──────┬────────┬─────────┬──────────┬─────────────┬────────────┬─────────────┐
/// │MsgID │Repeat  │SessionID│ SourceID │DestinationID│ ACK/NACK   │ ChQuality   │
/// │4     │2       │6        │32        │32           │ 16         │ 8           │
/// └──────┴────────┴─────────┴──────────┴─────────────┴────────────┴─────────────┘
/// ```
fn parse_msg5(bits: &[u8], frame: &mut LinkLayerFrame) {
    frame.destination_id = read_bits_u32(bits, 44, 32);
    frame.ack_nack_mask = read_bits_u16(bits, 76, 16);
    frame.channel_quality = read_bits_u8(bits, 92, 8);
}

/// Message 6: Geo-referenced data
///
/// ```text
/// ┌──────┬────────┬─────────┬──────────┬────────┬────────┬────────┬────────┬───────────┬────────────┬─────────┐
/// │MsgID │Repeat  │SessionID│ SourceID │NE Lon  │NE Lat  │SW Lon  │SW Lat  │ DataCount │ ASM Ident  │ Payload │
/// │4     │2       │6        │32        │18      │17      │18      │17      │ 11        │ 16         │variable │
/// └──────┴────────┴─────────┴──────────┴────────┴────────┴────────┴────────┴───────────┴────────────┴─────────┘
/// ```
fn parse_msg6(bits: &[u8], frame: &mut LinkLayerFrame) {
    let ne_lon = read_signed_bits(bits, 44, 18);
    let ne_lat = read_signed_bits(bits, 62, 17);
    let sw_lon = read_signed_bits(bits, 79, 18);
    let sw_lat = read_signed_bits(bits, 97, 17);

    if let (Some(ne_lon), Some(ne_lat), Some(sw_lon), Some(sw_lat)) =
        (ne_lon, ne_lat, sw_lon, sw_lat)
    {
        let ne_lon_deg = ne_lon as f64 / 600.0;
        let ne_lat_deg = ne_lat as f64 / 600.0;
        let sw_lon_deg = sw_lon as f64 / 600.0;
        let sw_lat_deg = sw_lat as f64 / 600.0;

        if valid_geo_coord(ne_lat_deg, ne_lon_deg) && valid_geo_coord(sw_lat_deg, sw_lon_deg) {
            frame.geo_box = Some(GeoBox {
                ne_lat: ne_lat_deg,
                ne_lon: ne_lon_deg,
                sw_lat: sw_lat_deg,
                sw_lon: sw_lon_deg,
            });
        }
    }

    frame.data_count = read_bits_u16(bits, 114, 11);
    frame.asm_identifier = read_bits_u16(bits, 125, 16);
    let start = 141;
    frame.payload_bits = extract_payload(bits, start, frame.data_count);
}

fn message_label(id: u8) -> &'static str {
    match id {
        0 => "Broadcast",
        1 => "Scheduled",
        2 => "Scheduled ITDMA",
        3 => "Addressed",
        4 => "Addressed ITDMA",
        5 => "Acknowledge",
        6 => "Geo-referenced",
        _ => "Unknown",
    }
}

fn extract_payload(bits: &[u8], start: usize, count: Option<u16>) -> Vec<u8> {
    let count = match count {
        Some(c) => c as usize,
        None => return Vec::new(),
    };
    let end = start.saturating_add(count).min(bits.len());
    if start >= end {
        return Vec::new();
    }
    bits[start..end].to_vec()
}

fn valid_geo_coord(lat: f64, lon: f64) -> bool {
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

fn read_bits_u8(bits: &[u8], start: usize, len: usize) -> Option<u8> {
    read_bits_u32(bits, start, len).and_then(|v| u8::try_from(v).ok())
}

fn read_bits_u16(bits: &[u8], start: usize, len: usize) -> Option<u16> {
    read_bits_u32(bits, start, len).and_then(|v| u16::try_from(v).ok())
}

fn read_bits_u32(bits: &[u8], start: usize, len: usize) -> Option<u32> {
    if len == 0 || len > 32 {
        return None;
    }
    let end = start.checked_add(len)?;
    let slice = bits.get(start..end)?;
    let mut value = 0u32;
    for &bit in slice {
        value = (value << 1) | u32::from(bit & 1);
    }
    Some(value)
}

fn read_signed_bits(bits: &[u8], start: usize, len: usize) -> Option<i32> {
    let raw = read_bits_u32(bits, start, len)?;
    if len == 0 || len > 31 {
        return None;
    }
    let sign_mask = 1u32 << (len - 1);
    if raw & sign_mask == 0 {
        Some(raw as i32)
    } else {
        let extended = raw | (!0u32 << len);
        Some(extended as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc;

    fn write_bits(bits: &mut [u8], start: usize, len: usize, value: u32) {
        for idx in 0..len {
            let shift = len - idx - 1;
            bits[start + idx] = ((value >> shift) & 1) as u8;
        }
    }

    fn write_signed_bits(bits: &mut [u8], start: usize, len: usize, value: i32) {
        let mask = if len >= 32 {
            u32::MAX
        } else {
            (1u32 << len) - 1
        };
        write_bits(bits, start, len, (value as u32) & mask);
    }

    fn append_crc(bits: &mut Vec<u8>) {
        let crc = crc::crc16_ccitt_bits(&bits[..]);
        for i in (0..16).rev() {
            bits.push(((crc >> i) & 1) as u8);
        }
    }

    #[test]
    fn parse_msg0_broadcast() {
        let mut bits = vec![0u8; 100];
        write_bits(&mut bits, 0, 4, 0); // message_id = 0
        write_bits(&mut bits, 4, 2, 1); // repeat = 1
        write_bits(&mut bits, 6, 6, 5); // session_id = 5
        write_bits(&mut bits, 12, 32, 123456); // source_id
        write_bits(&mut bits, 44, 11, 20); // data_count = 20
        // Fill some payload
        for i in 55..75 {
            bits[i] = (i % 2) as u8;
        }
        append_crc(&mut bits);

        let frame = parse_link_layer(&bits).expect("should parse");
        assert_eq!(frame.message_id, 0);
        assert_eq!(frame.repeat, 1);
        assert_eq!(frame.session_id, 5);
        assert_eq!(frame.source_id, 123456);
        assert_eq!(frame.data_count, Some(20));
        assert_eq!(frame.payload_bits.len(), 20);
        assert!(frame.crc_ok);
        assert_eq!(frame.label, "Broadcast");
    }

    #[test]
    fn parse_msg3_addressed() {
        let mut bits = vec![0u8; 150];
        write_bits(&mut bits, 0, 4, 3); // message_id = 3
        write_bits(&mut bits, 4, 2, 0); // repeat
        write_bits(&mut bits, 6, 6, 10); // session_id
        write_bits(&mut bits, 12, 32, 111111); // source_id
        write_bits(&mut bits, 44, 32, 222222); // destination_id
        write_bits(&mut bits, 76, 11, 15); // data_count
        write_bits(&mut bits, 87, 16, 0x1234); // asm_identifier
        append_crc(&mut bits);

        let frame = parse_link_layer(&bits).expect("should parse");
        assert_eq!(frame.message_id, 3);
        assert_eq!(frame.source_id, 111111);
        assert_eq!(frame.destination_id, Some(222222));
        assert_eq!(frame.asm_identifier, Some(0x1234));
        assert!(frame.crc_ok);
        assert_eq!(frame.label, "Addressed");
    }

    #[test]
    fn parse_msg5_acknowledge() {
        let mut bits = vec![0u8; 120];
        write_bits(&mut bits, 0, 4, 5); // message_id = 5
        write_bits(&mut bits, 4, 2, 0);
        write_bits(&mut bits, 6, 6, 0);
        write_bits(&mut bits, 12, 32, 999999);
        write_bits(&mut bits, 44, 32, 888888);
        write_bits(&mut bits, 76, 16, 0xABCD); // ack_nack
        write_bits(&mut bits, 92, 8, 42); // channel_quality
        append_crc(&mut bits);

        let frame = parse_link_layer(&bits).expect("should parse");
        assert_eq!(frame.message_id, 5);
        assert_eq!(frame.ack_nack_mask, Some(0xABCD));
        assert_eq!(frame.channel_quality, Some(42));
        assert!(frame.crc_ok);
    }

    #[test]
    fn parse_msg6_geo_box() {
        let mut bits = vec![0u8; 200];
        write_bits(&mut bits, 0, 4, 6);
        write_bits(&mut bits, 4, 2, 0);
        write_bits(&mut bits, 6, 6, 0);
        write_bits(&mut bits, 12, 32, 54321);
        // NE corner: lon=10.0°, lat=20.0°
        write_signed_bits(&mut bits, 44, 18, (10.0_f64 * 600.0) as i32);
        write_signed_bits(&mut bits, 62, 17, (20.0_f64 * 600.0) as i32);
        // SW corner: lon=-5.0°, lat=15.0°
        write_signed_bits(&mut bits, 79, 18, (-5.0_f64 * 600.0) as i32);
        write_signed_bits(&mut bits, 97, 17, (15.0_f64 * 600.0) as i32);
        write_bits(&mut bits, 114, 11, 10); // data_count
        write_bits(&mut bits, 125, 16, 0x5678); // asm_identifier
        append_crc(&mut bits);

        let frame = parse_link_layer(&bits).expect("should parse");
        assert_eq!(frame.message_id, 6);
        let geo = frame.geo_box.expect("geo_box should be present");
        assert!((geo.ne_lon - 10.0).abs() < 0.01);
        assert!((geo.ne_lat - 20.0).abs() < 0.01);
        assert!((geo.sw_lon - (-5.0)).abs() < 0.01);
        assert!((geo.sw_lat - 15.0).abs() < 0.01);
        assert!(frame.crc_ok);
    }

    #[test]
    fn bad_crc_detected() {
        let mut bits = vec![0u8; 80];
        write_bits(&mut bits, 0, 4, 0);
        write_bits(&mut bits, 12, 32, 1);
        write_bits(&mut bits, 44, 11, 0);
        // Append wrong CRC
        bits.extend_from_slice(&[0; 16]);

        let frame = parse_link_layer(&bits).expect("should parse");
        assert!(!frame.crc_ok);
    }

    #[test]
    fn too_short_returns_none() {
        let bits = vec![0u8; 10];
        assert!(parse_link_layer(&bits).is_none());
    }
}
