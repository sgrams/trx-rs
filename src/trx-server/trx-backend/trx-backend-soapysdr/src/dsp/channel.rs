// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use num_complex::Complex;
use tokio::sync::broadcast;
use trx_core::rig::state::{RdsData, RigMode};

use crate::demod::{DcBlocker, Demodulator, SoftAgc, WfmStereoDecoder};

use super::{BlockFirFilterPair, IQ_BLOCK_SIZE};

fn agc_for_mode(mode: &RigMode, audio_sample_rate: u32) -> SoftAgc {
    let sr = audio_sample_rate.max(1) as f32;
    match mode {
        RigMode::CW | RigMode::CWR => SoftAgc::new(sr, 1.0, 50.0, 0.5, 30.0),
        RigMode::AM => SoftAgc::new(sr, 500.0, 5_000.0, 0.5, 30.0),
        _ => SoftAgc::new(sr, 5.0, 500.0, 0.5, 30.0),
    }
}

fn iq_agc_for_mode(mode: &RigMode, sample_rate: u32) -> Option<SoftAgc> {
    let sr = sample_rate.max(1) as f32;
    match mode {
        RigMode::FM | RigMode::PKT => Some(SoftAgc::new(sr, 0.5, 150.0, 0.8, 12.0)),
        RigMode::WFM => None,
        _ => None,
    }
}

fn dc_for_mode(mode: &RigMode) -> Option<DcBlocker> {
    match mode {
        RigMode::WFM => None,
        _ => Some(DcBlocker::new(0.9999)),
    }
}

fn default_bandwidth_for_mode(mode: &RigMode) -> u32 {
    match mode {
        RigMode::LSB | RigMode::USB | RigMode::DIG => 3_000,
        RigMode::PKT => 25_000,
        RigMode::CW | RigMode::CWR => 500,
        RigMode::AM => 9_000,
        RigMode::FM => 12_500,
        RigMode::WFM => 180_000,
        RigMode::Other(_) => 3_000,
    }
}

/// Per-channel DSP state: mixer, FFT-FIR, decimator, demodulator, frame accumulator.
pub struct ChannelDsp {
    pub channel_if_hz: f64,
    pub demodulator: Demodulator,
    mode: RigMode,
    lpf_iq: BlockFirFilterPair,
    sdr_sample_rate: u32,
    audio_sample_rate: u32,
    audio_bandwidth_hz: u32,
    fir_taps: usize,
    wfm_deemphasis_us: u32,
    wfm_stereo: bool,
    wfm_denoise: bool,
    pub decim_factor: usize,
    output_channels: usize,
    pub frame_buf: Vec<f32>,
    frame_buf_offset: usize,
    pub frame_size: usize,
    pub pcm_tx: broadcast::Sender<Vec<f32>>,
    scratch_mixed_i: Vec<f32>,
    scratch_mixed_q: Vec<f32>,
    scratch_filtered_i: Vec<f32>,
    scratch_filtered_q: Vec<f32>,
    scratch_decimated: Vec<Complex<f32>>,
    pub mixer_phase: f64,
    pub mixer_phase_inc: f64,
    decim_counter: usize,
    resample_phase: f64,
    resample_phase_inc: f64,
    wfm_decoder: Option<WfmStereoDecoder>,
    iq_agc: Option<SoftAgc>,
    audio_agc: SoftAgc,
    audio_dc: Option<DcBlocker>,
}

impl ChannelDsp {
    fn clamp_bandwidth_for_mode(mode: &RigMode, bandwidth_hz: u32) -> u32 {
        let _ = mode;
        bandwidth_hz
    }

    pub fn set_channel_if_hz(&mut self, channel_if_hz: f64) {
        self.channel_if_hz = channel_if_hz;
        self.mixer_phase_inc = if self.sdr_sample_rate == 0 {
            0.0
        } else {
            2.0 * std::f64::consts::PI * channel_if_hz / self.sdr_sample_rate as f64
        };
    }

    fn pipeline_rates(
        mode: &RigMode,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        audio_bandwidth_hz: u32,
    ) -> (usize, u32) {
        if sdr_sample_rate == 0 {
            return (1, audio_sample_rate.max(1));
        }

        let target_rate = if *mode == RigMode::WFM {
            audio_bandwidth_hz.max(audio_sample_rate.saturating_mul(4))
        } else {
            audio_sample_rate.max(1)
        };
        let decim_factor = (sdr_sample_rate / target_rate.max(1)).max(1) as usize;
        let channel_sample_rate = (sdr_sample_rate / decim_factor as u32).max(1);
        (decim_factor, channel_sample_rate)
    }

    fn rebuild_filters(&mut self, reset_wfm_decoder: bool) {
        self.audio_bandwidth_hz =
            Self::clamp_bandwidth_for_mode(&self.mode, self.audio_bandwidth_hz);
        let (next_decim_factor, channel_sample_rate) = Self::pipeline_rates(
            &self.mode,
            self.sdr_sample_rate,
            self.audio_sample_rate,
            self.audio_bandwidth_hz,
        );
        let cutoff_hz = self
            .audio_bandwidth_hz
            .min(channel_sample_rate.saturating_sub(1)) as f32
            / 2.0;
        let cutoff_norm = if self.sdr_sample_rate == 0 {
            0.1
        } else {
            (cutoff_hz / self.sdr_sample_rate as f32).min(0.499)
        };
        self.lpf_iq = BlockFirFilterPair::new(cutoff_norm, self.fir_taps, IQ_BLOCK_SIZE);
        let rate_changed = self.decim_factor != next_decim_factor;
        self.decim_factor = next_decim_factor;
        self.decim_counter = 0;
        self.resample_phase = 0.0;
        self.resample_phase_inc = if self.sdr_sample_rate == 0 {
            1.0
        } else {
            self.audio_sample_rate as f64 / self.sdr_sample_rate as f64
        };
        if self.mode == RigMode::WFM {
            if reset_wfm_decoder || rate_changed || self.wfm_decoder.is_none() {
                self.wfm_decoder = Some(WfmStereoDecoder::new(
                    channel_sample_rate,
                    self.audio_sample_rate,
                    self.output_channels,
                    self.wfm_stereo,
                    self.wfm_deemphasis_us,
                ));
            }
        } else {
            self.wfm_decoder = None;
        }
        self.iq_agc = iq_agc_for_mode(&self.mode, channel_sample_rate);
        self.audio_agc = agc_for_mode(&self.mode, self.audio_sample_rate);
        self.audio_dc = dc_for_mode(&self.mode);
        self.frame_buf.clear();
        self.frame_buf_offset = 0;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel_if_hz: f64,
        mode: &RigMode,
        sdr_sample_rate: u32,
        audio_sample_rate: u32,
        output_channels: usize,
        frame_duration_ms: u16,
        audio_bandwidth_hz: u32,
        wfm_deemphasis_us: u32,
        wfm_stereo: bool,
        fir_taps: usize,
        pcm_tx: broadcast::Sender<Vec<f32>>,
    ) -> Self {
        let output_channels = output_channels.max(1);
        let audio_bandwidth_hz = Self::clamp_bandwidth_for_mode(mode, audio_bandwidth_hz);
        let frame_size = if audio_sample_rate == 0 || frame_duration_ms == 0 {
            960 * output_channels
        } else {
            (audio_sample_rate as usize * frame_duration_ms as usize * output_channels) / 1000
        };

        let taps = fir_taps.max(1);
        let (decim_factor, channel_sample_rate) =
            Self::pipeline_rates(mode, sdr_sample_rate, audio_sample_rate, audio_bandwidth_hz);
        let cutoff_hz = audio_bandwidth_hz.min(channel_sample_rate.saturating_sub(1)) as f32 / 2.0;
        let cutoff_norm = if sdr_sample_rate == 0 {
            0.1
        } else {
            (cutoff_hz / sdr_sample_rate as f32).min(0.499)
        };

        let mixer_phase_inc = if sdr_sample_rate == 0 {
            0.0
        } else {
            2.0 * std::f64::consts::PI * channel_if_hz / sdr_sample_rate as f64
        };

        Self {
            channel_if_hz,
            demodulator: Demodulator::for_mode(mode),
            mode: mode.clone(),
            lpf_iq: BlockFirFilterPair::new(cutoff_norm, taps, IQ_BLOCK_SIZE),
            sdr_sample_rate,
            audio_sample_rate,
            audio_bandwidth_hz,
            fir_taps: taps,
            wfm_deemphasis_us,
            wfm_stereo,
            wfm_denoise: true,
            decim_factor,
            output_channels,
            frame_buf: Vec::with_capacity(frame_size + output_channels),
            frame_buf_offset: 0,
            frame_size,
            pcm_tx,
            scratch_mixed_i: Vec::with_capacity(IQ_BLOCK_SIZE),
            scratch_mixed_q: Vec::with_capacity(IQ_BLOCK_SIZE),
            scratch_filtered_i: Vec::with_capacity(IQ_BLOCK_SIZE),
            scratch_filtered_q: Vec::with_capacity(IQ_BLOCK_SIZE),
            scratch_decimated: Vec::with_capacity(IQ_BLOCK_SIZE / decim_factor.max(1) + 1),
            mixer_phase: 0.0,
            mixer_phase_inc,
            decim_counter: 0,
            resample_phase: 0.0,
            resample_phase_inc: if sdr_sample_rate == 0 {
                1.0
            } else {
                audio_sample_rate as f64 / sdr_sample_rate as f64
            },
            wfm_decoder: if *mode == RigMode::WFM {
                Some(WfmStereoDecoder::new(
                    channel_sample_rate,
                    audio_sample_rate,
                    output_channels,
                    wfm_stereo,
                    wfm_deemphasis_us,
                ))
            } else {
                None
            },
            iq_agc: iq_agc_for_mode(mode, channel_sample_rate),
            audio_agc: agc_for_mode(mode, audio_sample_rate),
            audio_dc: dc_for_mode(mode),
        }
    }

    pub fn set_mode(&mut self, mode: &RigMode) {
        self.mode = mode.clone();
        if *mode != RigMode::WFM {
            self.audio_bandwidth_hz = default_bandwidth_for_mode(mode);
        }
        self.demodulator = Demodulator::for_mode(mode);
        self.rebuild_filters(true);
    }

    pub fn set_filter(&mut self, bandwidth_hz: u32, taps: usize) {
        self.audio_bandwidth_hz = Self::clamp_bandwidth_for_mode(&self.mode, bandwidth_hz);
        self.fir_taps = taps.max(1);
        self.rebuild_filters(false);
    }

    pub fn set_wfm_deemphasis(&mut self, deemphasis_us: u32) {
        self.wfm_deemphasis_us = deemphasis_us;
        self.rebuild_filters(true);
    }

    pub fn set_wfm_stereo(&mut self, enabled: bool) {
        self.wfm_stereo = enabled;
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.set_stereo_enabled(enabled);
        }
    }

    pub fn set_wfm_denoise(&mut self, enabled: bool) {
        self.wfm_denoise = enabled;
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.set_denoise_enabled(enabled);
        }
    }

    pub fn rds_data(&self) -> Option<RdsData> {
        self.wfm_decoder
            .as_ref()
            .and_then(WfmStereoDecoder::rds_data)
    }

    pub fn wfm_stereo_detected(&self) -> bool {
        self.wfm_decoder
            .as_ref()
            .map(WfmStereoDecoder::stereo_detected)
            .unwrap_or(false)
    }

    pub fn reset_rds(&mut self) {
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.reset_rds();
        }
    }

    pub fn reset_wfm_state(&mut self) {
        if let Some(decoder) = &mut self.wfm_decoder {
            decoder.reset_state();
        }
    }

    pub fn process_block(&mut self, block: &[Complex<f32>]) {
        let n = block.len();
        if n == 0 {
            return;
        }

        self.scratch_mixed_i.resize(n, 0.0);
        self.scratch_mixed_q.resize(n, 0.0);
        let mixed_i = &mut self.scratch_mixed_i;
        let mixed_q = &mut self.scratch_mixed_q;

        let phase_start = self.mixer_phase;
        let phase_inc = self.mixer_phase_inc;
        let (mut sin_phase, mut cos_phase) = phase_start.sin_cos();
        let (sin_inc, cos_inc) = phase_inc.sin_cos();
        for (idx, &sample) in block.iter().enumerate() {
            let lo_re = cos_phase as f32;
            let lo_im = -(sin_phase as f32);
            mixed_i[idx] = sample.re * lo_re - sample.im * lo_im;
            mixed_q[idx] = sample.re * lo_im + sample.im * lo_re;
            let next_sin = sin_phase * cos_inc + cos_phase * sin_inc;
            let next_cos = cos_phase * cos_inc - sin_phase * sin_inc;
            sin_phase = next_sin;
            cos_phase = next_cos;
        }
        self.mixer_phase = (phase_start + n as f64 * phase_inc).rem_euclid(std::f64::consts::TAU);

        self.lpf_iq.filter_block_into(
            mixed_i,
            mixed_q,
            &mut self.scratch_filtered_i,
            &mut self.scratch_filtered_q,
        );
        let filtered_i = &self.scratch_filtered_i;
        let filtered_q = &self.scratch_filtered_q;

        let capacity = n / self.decim_factor + 1;
        self.scratch_decimated.clear();
        if self.scratch_decimated.capacity() < capacity {
            self.scratch_decimated
                .reserve(capacity - self.scratch_decimated.capacity());
        }
        let decimated = &mut self.scratch_decimated;
        if self.wfm_decoder.is_some() {
            for idx in 0..n {
                self.decim_counter += 1;
                if self.decim_counter >= self.decim_factor {
                    self.decim_counter = 0;
                    let fi = filtered_i.get(idx).copied().unwrap_or(0.0);
                    let fq = filtered_q.get(idx).copied().unwrap_or(0.0);
                    decimated.push(Complex::new(fi, fq));
                }
            }
        } else {
            for idx in 0..n {
                self.resample_phase += self.resample_phase_inc;
                if self.resample_phase >= 1.0 {
                    self.resample_phase -= 1.0;
                    let fi = filtered_i.get(idx).copied().unwrap_or(0.0);
                    let fq = filtered_q.get(idx).copied().unwrap_or(0.0);
                    decimated.push(Complex::new(fi, fq));
                }
            }
        }

        if decimated.is_empty() {
            return;
        }

        if let Some(iq_agc) = &mut self.iq_agc {
            for sample in decimated.iter_mut() {
                *sample = iq_agc.process_complex(*sample);
            }
        }

        if self.wfm_decoder.is_some() {
            for sample in decimated.iter_mut() {
                let mag = (sample.re * sample.re + sample.im * sample.im).sqrt();
                if mag > 1.0 {
                    *sample /= mag;
                }
            }
        }

        const WFM_OUTPUT_GAIN: f32 = 0.10;
        let audio = if let Some(decoder) = self.wfm_decoder.as_mut() {
            let mut out = decoder.process_iq(decimated);
            for sample in &mut out {
                *sample = (*sample * WFM_OUTPUT_GAIN).clamp(-1.0, 1.0);
            }
            out
        } else {
            let mut raw = self.demodulator.demodulate(decimated);
            for sample in &mut raw {
                if let Some(dc) = &mut self.audio_dc {
                    *sample = dc.process(*sample);
                }
                *sample = self.audio_agc.process(*sample);
            }
            if self.output_channels >= 2 {
                let mut stereo = Vec::with_capacity(raw.len() * self.output_channels);
                for sample in raw {
                    stereo.push(sample);
                    stereo.push(sample);
                }
                stereo
            } else {
                raw
            }
        };

        self.frame_buf.extend_from_slice(&audio);
        while self.frame_buf.len().saturating_sub(self.frame_buf_offset) >= self.frame_size {
            let start = self.frame_buf_offset;
            let end = start + self.frame_size;
            let frame = self.frame_buf[start..end].to_vec();
            self.frame_buf_offset = end;
            let _ = self.pcm_tx.send(frame);
        }
        if self.frame_buf_offset > 0 && self.frame_buf_offset * 2 >= self.frame_buf.len() {
            self.frame_buf.copy_within(self.frame_buf_offset.., 0);
            self.frame_buf
                .truncate(self.frame_buf.len() - self.frame_buf_offset);
            self.frame_buf_offset = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_dsp_processes_silence() {
        let (pcm_tx, _pcm_rx) = broadcast::channel::<Vec<f32>>(8);
        let mut dsp = ChannelDsp::new(
            0.0,
            &RigMode::USB,
            48_000,
            8_000,
            1,
            20,
            3000,
            75,
            true,
            31,
            pcm_tx,
        );
        let block = vec![Complex::new(0.0_f32, 0.0_f32); 4096];
        dsp.process_block(&block);
    }

    #[test]
    fn channel_dsp_set_mode() {
        let (pcm_tx, _) = broadcast::channel::<Vec<f32>>(8);
        let mut dsp = ChannelDsp::new(
            0.0,
            &RigMode::USB,
            48_000,
            8_000,
            1,
            20,
            3000,
            75,
            true,
            31,
            pcm_tx,
        );
        assert_eq!(dsp.demodulator, Demodulator::Usb);
        dsp.set_mode(&RigMode::FM);
        assert_eq!(dsp.demodulator, Demodulator::Fm);
    }
}
