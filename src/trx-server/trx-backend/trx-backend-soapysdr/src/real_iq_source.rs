// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Real SoapySDR device IQ source implementation.

use num_complex::Complex;
use soapysdr::SoapySDRError;
use std::ffi::CString;

use crate::dsp::IqSource;

/// Real SoapySDR device IQ source.
///
/// Reads IQ samples directly from a SoapySDR-compatible device.
pub struct RealIqSource {
    device: soapysdr::Device,
    stream: soapysdr::RxStream,
    buffer_size: usize,
}

impl RealIqSource {
    /// Create a new real IQ source from a SoapySDR device.
    ///
    /// # Parameters
    /// - `args`: SoapySDR device arguments string (e.g., `"driver=rtlsdr"`)
    /// - `center_freq_hz`: Center frequency in Hz
    /// - `sample_rate_hz`: IQ sample rate in Hz
    /// - `bandwidth_hz`: Hardware filter bandwidth in Hz
    /// - `gain_db`: RX gain in dB (used for manual gain mode)
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
        // Parse device arguments.
        let kwargs = Self::parse_device_args(args)?;

        // Create device.
        let device = soapysdr::Device::new(kwargs).map_err(|e| {
            format!(
                "Failed to open SoapySDR device (args={}): {}",
                args, e
            )
        })?;

        tracing::info!(
            "Opened SoapySDR device: {}",
            device
                .driver_key()
                .map(|k| k.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "unknown".to_string())
        );

        // Get RX antenna and print available options.
        if let Ok(antennas) = device.list_antennas(soapysdr::Direction::Rx, 0) {
            let antenna_list = antennas
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ");
            tracing::info!("Available RX antennas: {}", antenna_list);
        }

        // Set sample rate.
        device
            .set_sample_rate(soapysdr::Direction::Rx, 0, sample_rate_hz)
            .map_err(|e| format!("Failed to set sample rate: {}", e))?;

        let actual_rate = device
            .sample_rate(soapysdr::Direction::Rx, 0)
            .map_err(|e| format!("Failed to read sample rate: {}", e))?;
        tracing::info!(
            "Set sample rate to {} Hz (actual: {} Hz)",
            sample_rate_hz,
            actual_rate
        );

        // Set center frequency.
        device
            .set_frequency(soapysdr::Direction::Rx, 0, center_freq_hz)
            .map_err(|e| format!("Failed to set frequency: {}", e))?;

        let actual_freq = device
            .frequency(soapysdr::Direction::Rx, 0)
            .map_err(|e| format!("Failed to read frequency: {}", e))?;
        tracing::info!(
            "Set center frequency to {} Hz (actual: {} Hz)",
            center_freq_hz,
            actual_freq
        );

        // Set bandwidth.
        if bandwidth_hz > 0.0 {
            device
                .set_bandwidth(soapysdr::Direction::Rx, 0, bandwidth_hz)
                .map_err(|e| format!("Failed to set bandwidth: {}", e))?;

            let actual_bw = device
                .bandwidth(soapysdr::Direction::Rx, 0)
                .map_err(|e| format!("Failed to read bandwidth: {}", e))?;
            tracing::info!(
                "Set bandwidth to {} Hz (actual: {} Hz)",
                bandwidth_hz,
                actual_bw
            );
        }

        // Set gain.
        match device.set_gain(soapysdr::Direction::Rx, 0, gain_db) {
            Ok(_) => {
                let actual_gain = device
                    .gain(soapysdr::Direction::Rx, 0)
                    .unwrap_or(gain_db);
                tracing::info!("Set gain to {} dB (actual: {} dB)", gain_db, actual_gain);
            }
            Err(e) => {
                tracing::warn!("Failed to set gain: {}; using device default", e);
            }
        }

        // Create RX stream for complex f32 samples.
        let stream = device
            .rx_stream::<Complex<f32>>(
                &[0], // channel 0
            )
            .map_err(|e| format!("Failed to create RX stream: {}", e))?;

        // Activate stream.
        stream
            .activate(None)
            .map_err(|e| format!("Failed to activate RX stream: {}", e))?;

        let buffer_size = 4096; // Match IQ_BLOCK_SIZE from dsp.rs

        tracing::info!("RealIqSource initialized successfully");

        Ok(Self {
            device,
            stream,
            buffer_size,
        })
    }

    /// Parse SoapySDR device arguments string into a HashMap.
    ///
    /// Format: "key1=value1,key2=value2"
    fn parse_device_args(args: &str) -> Result<soapysdr::KwargsList, String> {
        let mut kwargs = soapysdr::KwargsList::new();

        if args.is_empty() {
            return Ok(kwargs);
        }

        for pair in args.split(',') {
            let parts: Vec<&str> = pair.split('=').collect();
            if parts.len() == 2 {
                let key = CString::new(parts[0].trim())
                    .map_err(|_| format!("Invalid device arg key: {}", parts[0]))?;
                let value = CString::new(parts[1].trim())
                    .map_err(|_| format!("Invalid device arg value: {}", parts[1]))?;
                kwargs.insert(key, value);
            } else if parts.len() == 1 && !parts[0].is_empty() {
                // Allow flag-style args without values
                let key = CString::new(parts[0].trim())
                    .map_err(|_| format!("Invalid device arg key: {}", parts[0]))?;
                kwargs.insert(key, CString::new("").unwrap());
            } else {
                return Err(format!("Invalid device args format: {}", args));
            }
        }

        Ok(kwargs)
    }
}

impl IqSource for RealIqSource {
    fn read_into(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, String> {
        let max_samples = buf.len().min(self.buffer_size);

        match self.stream.read(&[buf], 1000000) {
            Ok(n) => {
                if n > max_samples {
                    tracing::warn!(
                        "RX stream returned {} samples, buffer holds {}",
                        n,
                        max_samples
                    );
                    Ok(max_samples)
                } else {
                    Ok(n)
                }
            }
            Err(SoapySDRError::Timeout) => {
                tracing::warn!("RX stream read timeout");
                Ok(0)
            }
            Err(e) => Err(format!("RX stream read error: {}", e)),
        }
    }
}

impl Drop for RealIqSource {
    fn drop(&mut self) {
        // Deactivate stream on cleanup.
        if let Err(e) = self.stream.deactivate(None) {
            tracing::warn!("Failed to deactivate RX stream: {}", e);
        }
    }
}
