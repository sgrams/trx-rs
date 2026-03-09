// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio streaming protocol types and framing helpers.
//!
//! Wire format: `[1 byte type][4 bytes BE length N][N bytes payload]`

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const AUDIO_MSG_STREAM_INFO: u8 = 0x00;
pub const AUDIO_MSG_RX_FRAME: u8 = 0x01;
pub const AUDIO_MSG_TX_FRAME: u8 = 0x02;
pub const AUDIO_MSG_APRS_DECODE: u8 = 0x03;
pub const AUDIO_MSG_CW_DECODE: u8 = 0x04;
pub const AUDIO_MSG_FT8_DECODE: u8 = 0x05;
pub const AUDIO_MSG_WSPR_DECODE: u8 = 0x06;
pub const AUDIO_MSG_AIS_DECODE: u8 = 0x07;
pub const AUDIO_MSG_VDES_DECODE: u8 = 0x08;
pub const AUDIO_MSG_HF_APRS_DECODE: u8 = 0x09;
/// Compressed history blob: payload is a gzip-compressed sequence of normal
/// framed messages (each: `[1 byte type][4 bytes BE length][payload]`).
pub const AUDIO_MSG_HISTORY_COMPRESSED: u8 = 0x0a;

/// Maximum payload size for normal messages (1 MB).
const MAX_PAYLOAD_SIZE: u32 = 1_048_576;
/// Maximum payload size for the compressed history blob (16 MB).
/// A compressed 24-hour history on a busy channel can reach several MB.
const MAX_HISTORY_PAYLOAD_SIZE: u32 = 16_777_216;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioStreamInfo {
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_duration_ms: u16,
}

/// Write a length-prefixed audio message.
pub async fn write_audio_msg_buffered<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg_type: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    let len = payload.len() as u32;
    writer.write_u8(msg_type).await?;
    writer.write_u32(len).await?;
    writer.write_all(payload).await?;
    Ok(())
}

/// Write a length-prefixed audio message and flush the writer.
pub async fn write_audio_msg<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg_type: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    write_audio_msg_buffered(writer, msg_type, payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one length-prefixed audio message, returning `(type, payload)`.
pub async fn read_audio_msg<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<(u8, Vec<u8>)> {
    let msg_type = reader.read_u8().await?;
    let len = reader.read_u32().await?;
    let limit = if msg_type == AUDIO_MSG_HISTORY_COMPRESSED {
        MAX_HISTORY_PAYLOAD_SIZE
    } else {
        MAX_PAYLOAD_SIZE
    };
    if len > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("audio frame too large: {} bytes (type={:#04x})", len, msg_type),
        ));
    }
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).await?;
    Ok((msg_type, payload))
}
