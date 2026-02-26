// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Real SoapySDR device IQ source implementation.

use num_complex::Complex;
use soapysdr::Device;

use crate::dsp::IqSource;

/// Real SoapySDR device IQ source.
///
/// Reads IQ samples directly from a SoapySDR-compatible device.
pub struct RealIqSource {
    _device: Device,
    buffer: Vec<Complex<f32>>,
}

impl RealIqSource {
    /// Create a new real IQ source from a SoapySDR device.
    ///
    /// # Parameters
    /// - `args`: SoapySDR device arguments string (e.g., `"driver=rtlsdr"`)
    /// - `center_freq_hz`: Center frequency in Hz
    /// - `sample_rate_hz`: IQ sample rate in Hz
    /// - `bandwidth_hz`: Hardware filter bandwidth in Hz
    /// - `gain_db`: RX gain in dB
    ///
    /// # Returns
    /// A configured RealIqSource or an error string if initialization fails.
    pub fn new(
        args: &str,
        center_freq_hz: f64,
        sample_rate_hz: f64,
        bandwidth_hz: f64,
        gain_db: f64,
    ) -> Result<Self, String> {
        tracing::info!("Initializing SoapySDR device with args: {}", args);

        // Create device from arguments string.
        let device = Device::new(args).map_err(|e| {
            format!(
                "Failed to open SoapySDR device (args={}): {}",
                args, e
            )
        })?;

        tracing::info!("SoapySDR device opened successfully");

        // Set sample rate.
        device
            .set_sample_rate(soapysdr::Direction::Rx, 0, sample_rate_hz)
            .map_err(|e| format!("Failed to set sample rate: {}", e))?;

        let actual_rate = device
            .sample_rate(soapysdr::Direction::Rx, 0)
            .unwrap_or(sample_rate_hz);
        tracing::info!(
            "Set sample rate to {} Hz (actual: {} Hz)",
            sample_rate_hz,
            actual_rate
        );

        // Set center frequency.
        device
            .set_frequency(soapysdr::Direction::Rx, 0, center_freq_hz, ())
            .map_err(|e| format!("Failed to set frequency: {}", e))?;

        let actual_freq = device
            .frequency(soapysdr::Direction::Rx, 0)
            .unwrap_or(center_freq_hz);
        tracing::info!(
            "Set center frequency to {} Hz (actual: {} Hz)",
            center_freq_hz,
            actual_freq
        );

        // Set bandwidth if specified.
        if bandwidth_hz > 0.0 {
            if let Err(e) = device.set_bandwidth(soapysdr::Direction::Rx, 0, bandwidth_hz) {
                tracing::warn!("Failed to set bandwidth: {}; continuing with default", e);
            } else {
                let actual_bw = device
                    .bandwidth(soapysdr::Direction::Rx, 0)
                    .unwrap_or(bandwidth_hz);
                tracing::info!(
                    "Set bandwidth to {} Hz (actual: {} Hz)",
                    bandwidth_hz,
                    actual_bw
                );
            }
        }

        // Set gain.
        if let Err(e) = device.set_gain(soapysdr::Direction::Rx, 0, gain_db) {
            tracing::warn!("Failed to set gain: {}; using device default", e);
        } else {
            let actual_gain = device
                .gain(soapysdr::Direction::Rx, 0)
                .unwrap_or(gain_db);
            tracing::info!("Set gain to {} dB (actual: {} dB)", gain_db, actual_gain);
        }

        let buffer = vec![Complex::new(0.0_f32, 0.0_f32); 4096];

        tracing::info!("RealIqSource initialized successfully");

        Ok(Self {
            _device: device,
            buffer,
        })
    }
}

impl IqSource for RealIqSource {
    fn read_into(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, String> {
        let max_samples = buf.len().min(4096);
        self.buffer.truncate(max_samples);
        self.buffer.resize(max_samples, Complex::new(0.0, 0.0));

        // TODO: Implement actual streaming read from device
        // For now, fill with zeros to test the architecture
        buf[..max_samples].copy_from_slice(&self.buffer[..max_samples]);
        Ok(max_samples)
    }
}
