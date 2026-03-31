// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Audio streaming protocol types and framing helpers.
//!
//! Wire format: `[1 byte type][4 bytes BE length N][N bytes payload]`

use uuid::Uuid;

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

// ---------------------------------------------------------------------------
// Virtual-channel audio multiplexing (server → client)
// ---------------------------------------------------------------------------

/// Per-virtual-channel Opus frame: `[16 B UUID][opus_len B Opus]`.
/// Sent by the server for each virtual channel the client has subscribed to.
pub const AUDIO_MSG_RX_FRAME_CH: u8 = 0x0b;
/// Server → client: virtual channel audio subscription acknowledged.
/// Payload: 16-byte UUID of the newly activated channel slot.
pub const AUDIO_MSG_VCHAN_ALLOCATED: u8 = 0x0c;

// ---------------------------------------------------------------------------
// Virtual-channel audio multiplexing (client → server)
// ---------------------------------------------------------------------------

/// Client → server: create-or-subscribe to a virtual channel's audio.
/// Payload: JSON `{"uuid":"<uuid>","freq_hz":<u64>,"mode":"<mode>"}`.
/// If a channel with the given UUID already exists the server just subscribes;
/// otherwise it creates a new DSP pipeline at the given frequency/mode first.
pub const AUDIO_MSG_VCHAN_SUB: u8 = 0x0d;
/// Client → server: unsubscribe from a virtual channel's audio.
/// Payload: 16-byte UUID of the virtual channel on the server.
pub const AUDIO_MSG_VCHAN_UNSUB: u8 = 0x0e;
/// Client → server: update the dial frequency of a virtual channel.
/// Payload: JSON `{"uuid":"<uuid>","freq_hz":<u64>}`.
pub const AUDIO_MSG_VCHAN_FREQ: u8 = 0x0f;
/// Client → server: update the demodulation mode of a virtual channel.
/// Payload: JSON `{"uuid":"<uuid>","mode":"<mode>"}`.
pub const AUDIO_MSG_VCHAN_MODE: u8 = 0x10;
/// Client → server: remove a virtual channel (stops encoding and destroys the DSP pipeline).
/// Payload: 16-byte UUID of the virtual channel on the server.
pub const AUDIO_MSG_VCHAN_REMOVE: u8 = 0x11;
/// Server → client: a virtual channel was destroyed server-side (e.g. went out of bandwidth).
/// Payload: 16-byte UUID of the destroyed channel.
pub const AUDIO_MSG_VCHAN_DESTROYED: u8 = 0x12;
/// Client → server: update the audio filter bandwidth of an existing virtual channel.
/// Payload: JSON `{"uuid": "<uuid>", "bandwidth_hz": <u32>}`.
pub const AUDIO_MSG_VCHAN_BW: u8 = 0x13;
/// Server → client: FT4 decoded message (JSON `DecodedMessage::Ft4`).
pub const AUDIO_MSG_FT4_DECODE: u8 = 0x14;
/// Server → client: FT2 decoded message (JSON `DecodedMessage::Ft2`).
pub const AUDIO_MSG_FT2_DECODE: u8 = 0x15;
/// Server → client: Meteor-M LRPT image complete (JSON `DecodedMessage::LrptImage`).
pub const AUDIO_MSG_LRPT_IMAGE: u8 = 0x17;
/// Server → client: LRPT decode progress update (JSON `DecodedMessage::LrptProgress`).
pub const AUDIO_MSG_LRPT_PROGRESS: u8 = 0x18;

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
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub bitrate_bps: u32,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
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
            format!(
                "audio frame too large: {} bytes (type={:#04x})",
                len, msg_type
            ),
        ));
    }
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).await?;
    Ok((msg_type, payload))
}

// ---------------------------------------------------------------------------
// Virtual-channel frame helpers
// ---------------------------------------------------------------------------

/// Write a virtual-channel control frame (16-byte UUID payload only).
/// Used for `AUDIO_MSG_VCHAN_SUB`, `AUDIO_MSG_VCHAN_UNSUB`, and
/// `AUDIO_MSG_VCHAN_ALLOCATED`.
pub async fn write_vchan_uuid_msg<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg_type: u8,
    uuid: Uuid,
) -> std::io::Result<()> {
    write_audio_msg(writer, msg_type, uuid.as_bytes()).await
}

/// Write an `AUDIO_MSG_RX_FRAME_CH` frame: 16-byte UUID followed by Opus payload.
pub async fn write_vchan_audio_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    uuid: Uuid,
    opus: &[u8],
) -> std::io::Result<()> {
    let mut payload = Vec::with_capacity(16 + opus.len());
    payload.extend_from_slice(uuid.as_bytes());
    payload.extend_from_slice(opus);
    write_audio_msg(writer, AUDIO_MSG_RX_FRAME_CH, &payload).await
}

/// Parse a virtual-channel audio frame payload (`AUDIO_MSG_RX_FRAME_CH`).
/// Returns `(uuid, opus_bytes)` or an error if the payload is too short.
pub fn parse_vchan_audio_frame(payload: &[u8]) -> std::io::Result<(Uuid, &[u8])> {
    if payload.len() < 16 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "vchan audio frame payload too short",
        ));
    }
    let uuid = Uuid::from_bytes(payload[..16].try_into().unwrap());
    Ok((uuid, &payload[16..]))
}

/// Parse a 16-byte UUID control frame (SUB / UNSUB / ALLOCATED).
pub fn parse_vchan_uuid_msg(payload: &[u8]) -> std::io::Result<Uuid> {
    if payload.len() < 16 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "vchan uuid frame payload too short",
        ));
    }
    Ok(Uuid::from_bytes(payload[..16].try_into().unwrap()))
}
