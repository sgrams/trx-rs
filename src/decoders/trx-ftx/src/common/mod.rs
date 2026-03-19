// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Common types, constants, and shared functions used across all FTx protocols.

pub mod callsign_hash;
pub mod constants;
pub mod crc;
#[allow(dead_code, clippy::needless_range_loop)]
pub mod decode;
#[allow(clippy::needless_range_loop)]
pub mod encode;
#[allow(clippy::needless_range_loop)]
pub mod ldpc;
#[allow(clippy::explicit_counter_loop, clippy::needless_range_loop)]
pub mod message;
#[allow(dead_code)]
pub mod monitor;
#[allow(dead_code, clippy::needless_range_loop, clippy::too_many_arguments)]
pub mod osd;
pub mod protocol;
pub mod text;
