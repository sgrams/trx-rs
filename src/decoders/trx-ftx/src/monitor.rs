// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Windowed FFT waterfall/spectrogram engine for FTx decoding.
//!
//! Replaces `monitor.c` from ft8_lib, using `realfft`/`rustfft` instead of KissFFT.

use num_complex::Complex32;
use realfft::RealFftPlanner;
use rustfft::FftPlanner;

use crate::protocol::FtxProtocol;

/// Waterfall element storing magnitude (dB) and phase (radians).
#[derive(Clone, Copy, Default)]
pub struct WfElem {
    pub mag: f32,
    pub phase: f32,
}

impl WfElem {
    pub fn mag_int(self) -> i32 {
        (2.0 * (self.mag + 120.0)) as i32
    }
}

/// Waterfall data collected during a message slot.
pub struct Waterfall {
    pub max_blocks: usize,
    pub num_blocks: usize,
    pub num_bins: usize,
    pub time_osr: usize,
    pub freq_osr: usize,
    pub mag: Vec<WfElem>,
    pub block_stride: usize,
    pub protocol: FtxProtocol,
}

impl Waterfall {
    pub fn new(max_blocks: usize, num_bins: usize, time_osr: usize, freq_osr: usize, protocol: FtxProtocol) -> Self {
        let block_stride = time_osr * freq_osr * num_bins;
        let mag = vec![WfElem::default(); max_blocks * block_stride];
        Self {
            max_blocks,
            num_blocks: 0,
            num_bins,
            time_osr,
            freq_osr,
            mag,
            block_stride,
            protocol,
        }
    }

    pub fn reset(&mut self) {
        self.num_blocks = 0;
    }
}

/// Monitor configuration.
pub struct MonitorConfig {
    pub f_min: f32,
    pub f_max: f32,
    pub sample_rate: i32,
    pub time_osr: i32,
    pub freq_osr: i32,
    pub protocol: FtxProtocol,
}

/// FTx monitor that manages DSP processing and prepares waterfall data.
pub struct Monitor {
    pub symbol_period: f32,
    pub min_bin: usize,
    pub max_bin: usize,
    pub block_size: usize,
    pub subblock_size: usize,
    pub nfft: usize,
    pub fft_norm: f32,
    window: Vec<f32>,
    last_frame: Vec<f32>,
    pub wf: Waterfall,
    pub max_mag: f32,
    // FFT planners/scratch
    fft_scratch: Vec<Complex32>,
    fft_output: Vec<Complex32>,
    fft_input: Vec<f32>,
    real_fft: std::sync::Arc<dyn realfft::RealToComplex<f32>>,
    // iFFT for resynthesis
    nifft: usize,
    ifft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    ifft_scratch: Vec<Complex32>,
}

fn hann_i(i: usize, n: usize) -> f32 {
    let x = (std::f32::consts::PI * i as f32 / n as f32).sin();
    x * x
}

impl Monitor {
    pub fn new(cfg: &MonitorConfig) -> Self {
        let symbol_period = cfg.protocol.symbol_period();
        let slot_time = cfg.protocol.slot_time();

        let block_size = (cfg.sample_rate as f32 * symbol_period) as usize;
        let subblock_size = block_size / cfg.time_osr as usize;
        let nfft = block_size * cfg.freq_osr as usize;
        let fft_norm = 2.0 / nfft as f32;

        let window: Vec<f32> = (0..nfft).map(|i| fft_norm * hann_i(i, nfft)).collect();
        let last_frame = vec![0.0f32; nfft];

        let min_bin = (cfg.f_min * symbol_period) as usize;
        let max_bin = (cfg.f_max * symbol_period) as usize + 1;
        let num_bins = max_bin - min_bin;
        let max_blocks = (slot_time / symbol_period) as usize;

        let wf = Waterfall::new(max_blocks, num_bins, cfg.time_osr as usize, cfg.freq_osr as usize, cfg.protocol);

        let mut real_planner = RealFftPlanner::<f32>::new();
        let real_fft = real_planner.plan_fft_forward(nfft);
        let fft_scratch = real_fft.make_scratch_vec();
        let fft_output = real_fft.make_output_vec();
        let fft_input = real_fft.make_input_vec();

        let nifft = 64;
        let mut fft_planner = FftPlanner::<f32>::new();
        let ifft = fft_planner.plan_fft_inverse(nifft);
        let ifft_scratch = vec![Complex32::new(0.0, 0.0); ifft.get_inplace_scratch_len()];

        Self {
            symbol_period,
            min_bin,
            max_bin,
            block_size,
            subblock_size,
            nfft,
            fft_norm,
            window,
            last_frame,
            wf,
            max_mag: -120.0,
            fft_scratch,
            fft_output,
            fft_input,
            real_fft,
            nifft,
            ifft,
            ifft_scratch,
        }
    }

    pub fn reset(&mut self) {
        self.wf.reset();
        self.max_mag = -120.0;
        self.last_frame.fill(0.0);
    }

    /// Process one block of audio samples and update the waterfall.
    pub fn process(&mut self, frame: &[f32]) {
        if self.wf.num_blocks >= self.wf.max_blocks {
            return;
        }

        let mut offset = self.wf.num_blocks * self.wf.block_stride;
        let mut frame_pos = 0;

        for _time_sub in 0..self.wf.time_osr {
            // Shift new data into analysis frame
            let shift = self.nfft - self.subblock_size;
            self.last_frame.copy_within(self.subblock_size..self.nfft, 0);
            for pos in shift..self.nfft {
                self.last_frame[pos] = if frame_pos < frame.len() {
                    frame[frame_pos]
                } else {
                    0.0
                };
                frame_pos += 1;
            }

            // Windowed FFT
            for pos in 0..self.nfft {
                self.fft_input[pos] = self.window[pos] * self.last_frame[pos];
            }
            self.real_fft
                .process_with_scratch(&mut self.fft_input, &mut self.fft_output, &mut self.fft_scratch)
                .expect("FFT process failed");

            // Extract magnitude and phase for each frequency sub-bin
            for freq_sub in 0..self.wf.freq_osr {
                for bin in self.min_bin..self.max_bin {
                    let src_bin = bin * self.wf.freq_osr + freq_sub;
                    if src_bin < self.fft_output.len() {
                        let c = self.fft_output[src_bin];
                        let mag2 = c.re * c.re + c.im * c.im;
                        let db = 10.0 * (1e-12_f32 + mag2).log10();
                        let phase = c.im.atan2(c.re);

                        if offset < self.wf.mag.len() {
                            self.wf.mag[offset] = WfElem { mag: db, phase };
                        }
                        offset += 1;

                        if db > self.max_mag {
                            self.max_mag = db;
                        }
                    } else {
                        if offset < self.wf.mag.len() {
                            self.wf.mag[offset] = WfElem { mag: -120.0, phase: 0.0 };
                        }
                        offset += 1;
                    }
                }
            }
        }

        self.wf.num_blocks += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_block_size_ft8() {
        let cfg = MonitorConfig {
            f_min: 200.0,
            f_max: 3000.0,
            sample_rate: 12000,
            time_osr: 2,
            freq_osr: 2,
            protocol: FtxProtocol::Ft8,
        };
        let mon = Monitor::new(&cfg);
        assert_eq!(mon.block_size, 1920); // 12000 * 0.160
    }

    #[test]
    fn monitor_block_size_ft4() {
        let cfg = MonitorConfig {
            f_min: 200.0,
            f_max: 3000.0,
            sample_rate: 12000,
            time_osr: 2,
            freq_osr: 2,
            protocol: FtxProtocol::Ft4,
        };
        let mon = Monitor::new(&cfg);
        assert_eq!(mon.block_size, 576); // 12000 * 0.048
    }

    #[test]
    fn monitor_block_size_ft2() {
        let cfg = MonitorConfig {
            f_min: 200.0,
            f_max: 5000.0,
            sample_rate: 12000,
            time_osr: 8,
            freq_osr: 4,
            protocol: FtxProtocol::Ft2,
        };
        let mon = Monitor::new(&cfg);
        assert_eq!(mon.block_size, 288); // 12000 * 0.024
    }
}
