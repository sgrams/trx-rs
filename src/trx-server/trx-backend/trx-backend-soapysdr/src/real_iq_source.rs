// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Real SoapySDR device IQ source implementation.

use num_complex::Complex;
use soapysdr::Device;

use crate::dsp::IqSource;

/// Real SoapySDR device IQ source.
///
/// Reads IQ samples directly from a SoapySDR-compatible device via the
/// SoapySDR streaming API.  `RxStream<Complex<f32>>` is `Send` (the crate
/// provides `unsafe impl Send`) and `StreamSample` is implemented for
/// `num_complex::Complex<f32>`, so no type conversion is needed.
pub struct RealIqSource {
    /// Device is held here to keep it alive for the stream's lifetime.
    #[allow(dead_code)]
    device: Device,
    /// Active RX stream producing CF32 samples.
    stream: soapysdr::RxStream<Complex<f32>>,
    /// Indicates the stream is hardware-backed (blocks in read_into).
    pub is_blocking: bool,
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
    /// A configured `RealIqSource` or an error string if initialisation fails.
    pub fn new(
        args: &str,
        center_freq_hz: f64,
        sample_rate_hz: f64,
        bandwidth_hz: f64,
        gain_db: f64,
    ) -> Result<Self, String> {
        tracing::info!("Initializing SoapySDR device with args: {}", args);

        let device = match Device::new(args) {
            Ok(dev) => dev,
            Err(e) => {
                tracing::warn!(
                    "Failed to open device with args '{}': {}. Attempting fallback...",
                    args,
                    e
                );
                match Device::new("") {
                    Ok(dev) => {
                        tracing::warn!(
                            "Successfully opened a device with empty args (fallback). \
                             Note: this may not be the intended device. \
                             If this is incorrect, check SoapySDR environment variables and plugins."
                        );
                        dev
                    }
                    Err(fallback_err) => {
                        return Err(format!(
                            "Failed to open SoapySDR device:\n  \
                             Original args '{}': {}\n  \
                             Fallback (empty args): {}\n  \
                             Troubleshooting: Check that SoapySDR is installed and plugins are loaded. \
                             Try running SoapySDRUtil --probe to verify device availability.",
                            args, e, fallback_err
                        ));
                    }
                }
            }
        };

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
            let actual_gain = device.gain(soapysdr::Direction::Rx, 0).unwrap_or(gain_db);
            tracing::info!("Set gain to {} dB (actual: {} dB)", gain_db, actual_gain);
        }

        // Create RX stream.  CF32 = Complex<f32>, StreamSample is implemented
        // for num_complex::Complex<f32> so no conversion is needed.
        let mut stream = device
            .rx_stream::<Complex<f32>>(&[0])
            .map_err(|e| format!("Failed to create RX stream: {}", e))?;

        // Activate the stream (start hardware capture).
        stream
            .activate(None)
            .map_err(|e| format!("Failed to activate RX stream: {}", e))?;

        tracing::info!("RealIqSource: RX stream activated, streaming started");

        Ok(Self {
            device,
            stream,
            is_blocking: true,
        })
    }
}

impl IqSource for RealIqSource {
    fn read_into(&mut self, buf: &mut [Complex<f32>]) -> Result<usize, String> {
        // 1 second timeout; gives the recovery loop a chance to react without
        // busy-spinning when the device stalls.
        const TIMEOUT_US: i64 = 1_000_000;

        self.stream
            .read(&[buf], TIMEOUT_US)
            .map_err(|e| format!("Stream read error: {}", e))
    }

    fn is_blocking(&self) -> bool {
        self.is_blocking
    }

    fn set_center_freq(&mut self, hz: f64) -> Result<(), String> {
        self.device
            .set_frequency(soapysdr::Direction::Rx, 0, hz, ())
            .map_err(|e| format!("Failed to retune SDR center frequency: {}", e))
    }
}
