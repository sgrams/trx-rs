// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

/// Decoded WSPR message payload.
#[derive(Debug, Clone)]
pub struct WsprProtocolMessage {
    pub message: String,
}

/// Attempt protocol-level decode from 162 4-FSK symbols.
///
/// This boundary keeps DSP and protocol concerns separated while the
/// native Rust decoder is implemented incrementally.
pub fn decode_symbols(_symbols: &[u8]) -> Option<WsprProtocolMessage> {
    None
}
