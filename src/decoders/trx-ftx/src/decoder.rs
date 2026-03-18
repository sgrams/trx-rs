// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Top-level FTx decoder matching the `trx-ft8` public API.

use crate::callsign_hash::CallsignHashTable;
use crate::decode::{ftx_decode_candidate, ftx_find_candidates, ftx_post_decode_snr, FtxMessage};
use crate::message;
use crate::monitor::{Monitor, MonitorConfig};
use crate::protocol::*;

const DEFAULT_F_MIN_HZ: f32 = 200.0;
const DEFAULT_F_MAX_HZ: f32 = 3000.0;
const DEFAULT_TIME_OSR: i32 = 2;
const DEFAULT_FREQ_OSR: i32 = 2;

const FT2_F_MIN_HZ: f32 = 200.0;
const FT2_F_MAX_HZ: f32 = 5000.0;
const FT2_TIME_OSR: i32 = 8;
const FT2_FREQ_OSR: i32 = 4;

const MAX_LDPC_ITERATIONS: usize = 20;
const MIN_CANDIDATE_SCORE: i32 = 10;
const MAX_CANDIDATES: usize = 120;

/// Decoded result from the FT8/FT4/FT2 decoder.
#[derive(Debug, Clone)]
pub struct Ft8DecodeResult {
    pub text: String,
    pub snr_db: f32,
    pub dt_s: f32,
    pub freq_hz: f32,
}

/// FTx decoder instance supporting FT8, FT4, and FT2 protocols.
pub struct Ft8Decoder {
    protocol: FtxProtocol,
    sample_rate: u32,
    block_size: usize,
    window_samples: usize,
    monitor: Monitor,
    callsign_hash: CallsignHashTable,
    // FT2-specific pipeline
    ft2_pipeline: Option<crate::ft2::Ft2Pipeline>,
}

// Ft8Decoder is not shared across threads, but may be moved between tasks.
unsafe impl Send for Ft8Decoder {}

impl Ft8Decoder {
    /// Create a new FT8 decoder.
    pub fn new(sample_rate: u32) -> Result<Self, String> {
        Self::new_with_protocol(sample_rate, FtxProtocol::Ft8)
    }

    /// Create a new FT4 decoder.
    pub fn new_ft4(sample_rate: u32) -> Result<Self, String> {
        Self::new_with_protocol(sample_rate, FtxProtocol::Ft4)
    }

    /// Create a new FT2 decoder.
    pub fn new_ft2(sample_rate: u32) -> Result<Self, String> {
        Self::new_with_protocol(sample_rate, FtxProtocol::Ft2)
    }

    fn new_with_protocol(sample_rate: u32, protocol: FtxProtocol) -> Result<Self, String> {
        let (f_min, f_max, time_osr, freq_osr) = match protocol {
            FtxProtocol::Ft2 => (FT2_F_MIN_HZ, FT2_F_MAX_HZ, FT2_TIME_OSR, FT2_FREQ_OSR),
            _ => (
                DEFAULT_F_MIN_HZ,
                DEFAULT_F_MAX_HZ,
                DEFAULT_TIME_OSR,
                DEFAULT_FREQ_OSR,
            ),
        };

        let cfg = MonitorConfig {
            f_min,
            f_max,
            sample_rate: sample_rate as i32,
            time_osr,
            freq_osr,
            protocol,
        };

        let monitor = Monitor::new(&cfg);
        let block_size = monitor.block_size;

        if block_size == 0 {
            return Err(format!("invalid {:?} block size", protocol));
        }

        let window_samples = match protocol {
            FtxProtocol::Ft2 => crate::ft2::FT2_NMAX,
            _ => {
                let slot_time = protocol.slot_time();
                (sample_rate as f32 * slot_time) as usize
            }
        };

        if window_samples == 0 {
            return Err(format!("invalid {:?} analysis window", protocol));
        }

        let ft2_pipeline = if protocol == FtxProtocol::Ft2 {
            Some(crate::ft2::Ft2Pipeline::new(sample_rate as i32))
        } else {
            None
        };

        Ok(Self {
            protocol,
            sample_rate,
            block_size,
            window_samples,
            monitor,
            callsign_hash: CallsignHashTable::new(),
            ft2_pipeline,
        })
    }

    /// Block size in samples for `process_block`.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// The sample rate this decoder was configured with.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Total analysis window in samples.
    pub fn window_samples(&self) -> usize {
        self.window_samples
    }

    /// Reset the decoder state for a new decode cycle.
    pub fn reset(&mut self) {
        self.monitor.reset();
        if let Some(ref mut pipe) = self.ft2_pipeline {
            pipe.reset();
        }
    }

    /// Feed one block of audio samples to the decoder.
    pub fn process_block(&mut self, block: &[f32]) {
        if block.len() < self.block_size {
            return;
        }

        if self.protocol == FtxProtocol::Ft2 {
            // FT2: accumulate raw audio and also feed the monitor
            if let Some(ref mut pipe) = self.ft2_pipeline {
                pipe.accumulate(block);
            }
        }

        self.monitor.process(block);
    }

    /// Check if enough data has been collected and run the decode.
    /// Returns decoded messages, or empty if not ready yet.
    pub fn decode_if_ready(&mut self, max_results: usize) -> Vec<Ft8DecodeResult> {
        if self.protocol == FtxProtocol::Ft2 {
            return self.decode_ft2(max_results);
        }

        // FT8/FT4: waterfall-based decode
        if self.monitor.wf.num_blocks < self.monitor.wf.max_blocks {
            return Vec::new();
        }

        self.decode_waterfall(max_results)
    }

    /// Waterfall-based decode for FT8/FT4.
    fn decode_waterfall(&mut self, max_results: usize) -> Vec<Ft8DecodeResult> {
        let candidates =
            ftx_find_candidates(&self.monitor.wf, MAX_CANDIDATES, MIN_CANDIDATE_SCORE);

        let mut results = Vec::new();
        let mut seen: Vec<u16> = Vec::new();

        for cand in &candidates {
            if results.len() >= max_results {
                break;
            }

            let msg = match ftx_decode_candidate(&self.monitor.wf, cand, MAX_LDPC_ITERATIONS) {
                Some(m) => m,
                None => continue,
            };

            // Dedup by hash
            if seen.contains(&msg.hash) {
                continue;
            }
            seen.push(msg.hash);

            // Unpack message text
            let text = match self.unpack_message(&msg) {
                Some(t) => t,
                None => continue,
            };

            // Compute SNR
            let snr_db = ftx_post_decode_snr(&self.monitor.wf, cand, &msg);

            // Compute time offset
            let symbol_period = self.protocol.symbol_period();
            let dt_s =
                (cand.time_offset as f32 + cand.time_sub as f32 / self.monitor.wf.time_osr as f32)
                    * symbol_period
                    - 0.5;

            // Compute frequency
            let freq_hz = (self.monitor.min_bin as f32 + cand.freq_offset as f32
                + cand.freq_sub as f32 / self.monitor.wf.freq_osr as f32)
                / symbol_period;

            results.push(Ft8DecodeResult {
                text,
                snr_db,
                dt_s,
                freq_hz,
            });
        }

        results
    }

    /// FT2-specific decode pipeline.
    fn decode_ft2(&mut self, max_results: usize) -> Vec<Ft8DecodeResult> {
        let pipe = match self.ft2_pipeline.as_ref() {
            Some(p) => p,
            None => return Vec::new(),
        };

        if !pipe.is_ready() {
            return Vec::new();
        }

        let ft2_results = pipe.decode(max_results);
        let mut results = Vec::new();

        for r in ft2_results {
            let text = match self.unpack_message(&r.message) {
                Some(t) => t,
                None => continue,
            };

            results.push(Ft8DecodeResult {
                text,
                snr_db: r.snr_db,
                dt_s: r.dt_s,
                freq_hz: r.freq_hz,
            });
        }

        results
    }

    /// Unpack a decoded FtxMessage into a human-readable string.
    fn unpack_message(&mut self, msg: &FtxMessage) -> Option<String> {
        let m = message::FtxMessage {
            payload: msg.payload,
            hash: msg.hash as u32,
        };
        let (text, _offsets, _rc) =
            message::ftx_message_decode(&m, &mut self.callsign_hash);
        if text.is_empty() {
            return None;
        }
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ft8_decoder_creates() {
        let dec = Ft8Decoder::new(12_000).expect("ft8 decoder");
        assert_eq!(dec.block_size(), 1920); // 12000 * 0.160
        assert_eq!(dec.sample_rate(), 12_000);
    }

    #[test]
    fn ft4_decoder_creates() {
        let dec = Ft8Decoder::new_ft4(12_000).expect("ft4 decoder");
        assert_eq!(dec.block_size(), 576); // 12000 * 0.048
    }

    #[test]
    fn ft2_uses_distinct_block_size() {
        let ft4 = Ft8Decoder::new_ft4(12_000).expect("ft4 decoder");
        let ft2 = Ft8Decoder::new_ft2(12_000).expect("ft2 decoder");

        assert!(ft2.block_size() < ft4.block_size());
        assert_eq!(ft4.block_size(), 576);
        assert_eq!(ft2.block_size(), 288);
        assert_eq!(ft2.window_samples(), 45_000);
    }

    #[test]
    fn decoder_reset() {
        let mut dec = Ft8Decoder::new(12_000).expect("ft8 decoder");
        dec.reset();
        // Should not panic
    }

    #[test]
    fn decode_empty_returns_nothing() {
        let mut dec = Ft8Decoder::new(12_000).expect("ft8 decoder");
        let results = dec.decode_if_ready(10);
        assert!(results.is_empty());
    }
}
