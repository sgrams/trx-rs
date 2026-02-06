// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::pin::Pin;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{timeout, Duration};
use tokio_serial::{ClearBuffer, SerialPort, SerialPortBuilderExt, SerialStream};

use trx_core::math::{decode_freq_bcd, encode_freq_bcd};
use trx_core::radio::freq::{Band, Freq};
use trx_core::rig::{
    Rig, RigAccessMethod, RigCapabilities, RigCat, RigInfo, RigStatusFuture, RigVfo, RigVfoEntry,
};
use trx_core::{DynResult, RigMode};

/// Backend for Yaesu FT-817 CAT control.
pub struct Ft817 {
    port: SerialStream,
    info: RigInfo,
    vfo_side: Ft817VfoSide,
    vfo_a_freq: Option<Freq>,
    vfo_b_freq: Option<Freq>,
    vfo_a_mode: Option<RigMode>,
    vfo_b_mode: Option<RigMode>,
}

impl Ft817 {
    const READ_TIMEOUT: Duration = Duration::from_millis(800);

    pub fn new(path: &str, baud: u32) -> DynResult<Self> {
        let builder = tokio_serial::new(path, baud);
        let port = builder.open_native_async()?;
        let info = RigInfo {
            manufacturer: "Yaesu".to_string(),
            model: "FT-817".to_string(),
            revision: "".to_string(),
            capabilities: RigCapabilities {
                supported_bands: vec![
                    // Transmit-capable amateur bands
                    Band {
                        low_hz: 1_800_000,
                        high_hz: 2_000_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 3_500_000,
                        high_hz: 4_000_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 5_250_000,
                        high_hz: 5_450_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 7_000_000,
                        high_hz: 7_300_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 10_100_000,
                        high_hz: 10_150_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 14_000_000,
                        high_hz: 14_350_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 18_068_000,
                        high_hz: 18_168_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 21_000_000,
                        high_hz: 21_450_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 24_890_000,
                        high_hz: 24_990_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 28_000_000,
                        high_hz: 29_700_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 50_000_000,
                        high_hz: 54_000_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 144_000_000,
                        high_hz: 148_000_000,
                        tx_allowed: true,
                    },
                    Band {
                        low_hz: 430_000_000,
                        high_hz: 450_000_000,
                        tx_allowed: true,
                    },
                    // Receive-only coverage segments
                    Band {
                        low_hz: 100_000,
                        high_hz: 1_799_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 2_000_001,
                        high_hz: 3_499_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 4_000_001,
                        high_hz: 5_249_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 5_450_001,
                        high_hz: 6_999_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 7_300_001,
                        high_hz: 10_099_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 10_150_001,
                        high_hz: 13_999_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 14_350_001,
                        high_hz: 18_067_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 18_168_001,
                        high_hz: 20_999_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 21_450_001,
                        high_hz: 24_889_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 24_990_001,
                        high_hz: 27_999_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 29_700_001,
                        high_hz: 49_999_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 54_000_001,
                        high_hz: 75_999_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 76_000_000,
                        high_hz: 107_999_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 108_000_000,
                        high_hz: 143_999_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 148_000_001,
                        high_hz: 429_999_999,
                        tx_allowed: false,
                    },
                    Band {
                        low_hz: 450_000_001,
                        high_hz: 470_000_000,
                        tx_allowed: false,
                    },
                ],
                supported_modes: vec![
                    RigMode::LSB,
                    RigMode::USB,
                    RigMode::CW,
                    RigMode::CWR,
                    RigMode::AM,
                    RigMode::WFM,
                    RigMode::FM,
                    RigMode::DIG,
                    RigMode::PKT,
                ],
                num_vfos: 2,
                // CAT only exposes lock and VFO toggle; the other features are panel-only.
                lockable: true,
                attenuator: false,
                preamp: false,
                rit: false,
                rpt: false,
                split: false,
                lock: true,
            },
            access: RigAccessMethod::Serial {
                path: path.to_string(),
                baud,
            },
        };
        Ok(Self {
            port,
            info,
            vfo_side: Ft817VfoSide::Unknown,
            vfo_a_freq: None,
            vfo_b_freq: None,
            vfo_a_mode: None,
            vfo_b_mode: None,
        })
    }

    /// Query current status (frequency, mode, VFO) from FT-817.
    pub async fn get_status(&mut self) -> DynResult<(Freq, RigMode, Option<RigVfo>)> {
        let (hz, mode) = self.read_status().await?;
        let freq = Freq { hz };
        self.update_vfo_freq(freq);
        self.update_vfo_mode(mode.clone());
        let mut entries = Vec::new();
        if let Some(a) = self.vfo_a_freq {
            entries.push(RigVfoEntry {
                name: "A".to_string(),
                freq: a,
                mode: self.vfo_a_mode.clone(),
            });
        }
        if let Some(b) = self.vfo_b_freq {
            entries.push(RigVfoEntry {
                name: "B".to_string(),
                freq: b,
                mode: self.vfo_b_mode.clone(),
            });
        }
        let active = match self.vfo_side {
            Ft817VfoSide::A if self.vfo_a_freq.is_some() => Some(0),
            Ft817VfoSide::B if self.vfo_a_freq.is_some() => Some(1),
            Ft817VfoSide::B if self.vfo_a_freq.is_none() && self.vfo_b_freq.is_some() => Some(0),
            _ => None,
        };
        let vfo = if entries.is_empty() {
            None
        } else {
            Some(RigVfo { entries, active })
        };
        Ok((freq, mode, vfo))
    }

    /// Query current frequency from FT-817.
    pub async fn get_freq(&mut self) -> DynResult<Freq> {
        let (freq, _, _) = self.get_status().await?;
        Ok(freq)
    }

    /// Query current mode from FT-817.
    pub async fn get_mode(&mut self) -> DynResult<RigMode> {
        let (_, mode, _) = self.get_status().await?;
        Ok(mode)
    }

    /// Send CAT command to set frequency on FT-817.
    pub async fn set_freq(&mut self, freq: Freq) -> DynResult<()> {
        let bcd = encode_freq_bcd(freq.hz)?;
        let frame = [bcd[0], bcd[1], bcd[2], bcd[3], CMD_SET_FREQ];
        self.write_frame(&frame).await?;
        self.update_vfo_freq(freq);
        Ok(())
    }

    /// Send CAT command to set mode on FT-817.
    pub async fn set_mode(&mut self, mode: &RigMode) -> DynResult<()> {
        // Ensure panel is unlocked and drop any stale bytes before sending.
        let _ = self.unlock().await;
        let _ = self.port.clear(ClearBuffer::Input);

        // Data byte 1 = mode, data bytes 2-4 = 0x00, command = 0x07.
        let mode_code = encode_mode(mode);
        tracing::debug!("FT-817 set_mode -> code 0x{:02X} ({:?})", mode_code, mode);
        let frame = [mode_code, 0x00, 0x00, 0x00, CMD_SET_MODE];
        self.write_frame(&frame).await?;
        self.port.flush().await?;
        // Some rigs occasionally miss the first frame; send a second time after a short delay.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        self.write_frame(&frame).await?;
        self.port.flush().await?;
        self.update_vfo_mode(mode.clone());
        Ok(())
    }

    /// Send CAT command to control PTT on FT-817.
    pub async fn set_ptt(&mut self, ptt: bool) -> DynResult<()> {
        let opcode = if ptt { CMD_PTT_ON } else { CMD_PTT_OFF };
        // PTT on/off does not take a payload; CAT uses separate opcodes.
        let frame = [0x00, 0x00, 0x00, 0x00, opcode];
        self.write_frame(&frame).await?;
        Ok(())
    }

    /// Turn the radio on via CAT. The first frame is ignored while the CPU wakes,
    /// so send a dummy payload before issuing the actual command.
    pub async fn power_on(&mut self) -> DynResult<()> {
        const POWER_ON_DUMMY: [u8; 5] = [0x00, 0x00, 0x00, 0x00, 0x00];
        self.port.write_all(&POWER_ON_DUMMY).await?;
        self.port.flush().await?;
        // Give the radio a moment to wake up and lock onto CAT framing.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        let frame = [0x00, 0x00, 0x00, 0x00, CMD_POWER_ON];
        self.write_frame(&frame).await?;
        self.port.flush().await?;
        // Drop any boot noise that might remain in the input buffer before we start polling.
        let _ = self.port.clear(ClearBuffer::Input);
        Ok(())
    }

    /// Turn the radio off via CAT.
    pub async fn power_off(&mut self) -> DynResult<()> {
        let frame = [0x00, 0x00, 0x00, 0x00, CMD_POWER_OFF];
        self.write_frame(&frame).await?;
        Ok(())
    }

    /// Toggle between VFO A/B.
    pub async fn toggle_vfo(&mut self) -> DynResult<()> {
        let frame = [0x00, 0x00, 0x00, 0x00, CMD_TOGGLE_VFO];
        self.write_frame(&frame).await?;
        self.vfo_side = self.vfo_side.other();
        Ok(())
    }

    /// Enable front panel lock.
    pub async fn lock(&mut self) -> DynResult<()> {
        let frame = [0x00, 0x00, 0x00, 0x00, CMD_LOCK];
        self.write_frame(&frame).await?;
        let mut buf = [0u8; 1];
        if let Err(e) = self.port.read_exact(&mut buf).await {
            tracing::warn!("LOCK read failed: {:?}", e);
        } else {
            tracing::debug!("LOCK response: 0x{:02X}", buf[0]);
        }
        Ok(())
    }

    /// Disable front panel lock.
    pub async fn unlock(&mut self) -> DynResult<()> {
        let frame = [0x00, 0x00, 0x00, 0x00, CMD_UNLOCK];
        self.write_frame(&frame).await?;
        let mut buf = [0u8; 1];
        if let Err(e) = self.port.read_exact(&mut buf).await {
            tracing::warn!("UNLOCK read failed: {:?}", e);
        } else {
            tracing::debug!("UNLOCK response: 0x{:02X}", buf[0]);
        }
        Ok(())
    }

    /// Read the current signal strength meter (S-meter/PWR) from the radio.
    ///
    /// The returned value is the raw CAT meter byte (0-255). In receive it
    /// represents S-meter level; in transmit it reports power/ALC depending on
    /// rig state.
    pub async fn get_signal_strength(&mut self) -> DynResult<u8> {
        self.read_meter().await
    }

    /// Read the current transmit power indication (raw meter value).
    ///
    /// The FT-817 reports the same meter byte for TX power as for the S-meter;
    /// callers should interpret based on current PTT state.
    pub async fn get_tx_power(&mut self) -> DynResult<u8> {
        self.read_meter().await
    }

    async fn read_status(&mut self) -> DynResult<(u64, RigMode)> {
        // Status request returns frequency (4 BCD bytes, LSB first) and mode code.
        let _ = self.port.clear(ClearBuffer::Input);
        let frame = [0x00, 0x00, 0x00, 0x00, CMD_READ_STATUS];
        self.write_frame(&frame).await?;

        let mut buf = [0u8; 5];
        timeout(Self::READ_TIMEOUT, self.port.read_exact(&mut buf))
            .await
            .map_err(|_| "CAT status read timeout")??;

        let freq = decode_freq_bcd([buf[0], buf[1], buf[2], buf[3]])?;
        let mode = decode_mode(buf[4]);
        Ok((freq, mode))
    }

    async fn read_meter(&mut self) -> DynResult<u8> {
        let frame = [0x00, 0x00, 0x00, 0x00, CMD_READ_METER];
        self.write_frame(&frame).await?;

        let mut buf = [0u8; 1];
        timeout(Self::READ_TIMEOUT, self.port.read_exact(&mut buf))
            .await
            .map_err(|_| "CAT meter read timeout")??;
        Ok(buf[0])
    }

    async fn write_frame(&mut self, frame: &[u8; 5]) -> DynResult<()> {
        self.port.write_all(frame).await?;
        self.port.flush().await?;
        Ok(())
    }

    fn update_vfo_freq(&mut self, freq: Freq) {
        match self.vfo_side {
            Ft817VfoSide::A => self.vfo_a_freq = Some(freq),
            Ft817VfoSide::B => self.vfo_b_freq = Some(freq),
            Ft817VfoSide::Unknown => {
                // Try to infer which VFO we are on using cached values; default to A only.
                if self.vfo_b_freq.map(|f| f.hz == freq.hz).unwrap_or(false)
                    && self.vfo_a_freq.is_none()
                {
                    self.vfo_side = Ft817VfoSide::B;
                    self.vfo_b_freq = Some(freq);
                } else {
                    self.vfo_side = Ft817VfoSide::A;
                    self.vfo_a_freq = Some(freq);
                }
            }
        }
    }

    fn update_vfo_mode(&mut self, mode: RigMode) {
        match self.vfo_side {
            Ft817VfoSide::A => self.vfo_a_mode = Some(mode),
            Ft817VfoSide::B => self.vfo_b_mode = Some(mode),
            Ft817VfoSide::Unknown => {
                // Default to current VFO (assume A) when unknown.
                self.vfo_a_mode = Some(mode);
            }
        }
    }
}

impl Rig for Ft817 {
    fn info(&self) -> &RigInfo {
        &self.info
    }
}

impl RigCat for Ft817 {
    fn get_status<'a>(&'a mut self) -> RigStatusFuture<'a> {
        Box::pin(async move { self.get_status().await })
    }

    fn set_freq<'a>(
        &'a mut self,
        freq: Freq,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft817::set_freq(self, freq).await })
    }

    fn set_mode<'a>(
        &'a mut self,
        mode: RigMode,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft817::set_mode(self, &mode).await })
    }

    fn set_ptt<'a>(
        &'a mut self,
        ptt: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft817::set_ptt(self, ptt).await })
    }

    fn power_on<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft817::power_on(self).await })
    }

    fn power_off<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft817::power_off(self).await })
    }

    fn get_signal_strength<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move { Ft817::get_signal_strength(self).await })
    }

    fn get_tx_power<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move { Ft817::get_tx_power(self).await })
    }

    fn get_tx_limit<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move { Err("TX limit query not supported on FT-817".into()) })
    }

    fn set_tx_limit<'a>(
        &'a mut self,
        _limit: u8,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Err("TX limit setting not supported on FT-817".into()) })
    }

    fn toggle_vfo<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft817::toggle_vfo(self).await })
    }

    fn lock<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft817::lock(self).await })
    }

    fn unlock<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft817::unlock(self).await })
    }
}

#[derive(Clone, Copy)]
enum Ft817VfoSide {
    A,
    B,
    Unknown,
}

impl Ft817VfoSide {
    fn other(self) -> Self {
        match self {
            Ft817VfoSide::A => Ft817VfoSide::B,
            Ft817VfoSide::B => Ft817VfoSide::A,
            Ft817VfoSide::Unknown => Ft817VfoSide::A,
        }
    }
}

// Command codes per Yaesu CAT protocol.
const CMD_SET_FREQ: u8 = 0x01;
const CMD_READ_STATUS: u8 = 0x03;
const CMD_SET_MODE: u8 = 0x07;
const CMD_PTT_ON: u8 = 0x08;
const CMD_PTT_OFF: u8 = 0x88;
const CMD_POWER_ON: u8 = 0x0F;
const CMD_POWER_OFF: u8 = 0x8F;
const CMD_TOGGLE_VFO: u8 = 0x81;
const CMD_LOCK: u8 = 0x00;
const CMD_UNLOCK: u8 = 0x80;
const CMD_READ_METER: u8 = 0xE7;

fn encode_mode(mode: &RigMode) -> u8 {
    match mode {
        RigMode::LSB => 0x00,
        RigMode::USB => 0x01,
        RigMode::CW => 0x02,
        RigMode::CWR => 0x03,
        RigMode::AM => 0x04,
        RigMode::WFM => 0x06,
        RigMode::FM => 0x08,
        RigMode::DIG => 0x0A,
        RigMode::PKT => 0x0C,
        RigMode::Other(_) => 0x00,
    }
}

fn decode_mode(code: u8) -> RigMode {
    match code {
        0x00 => RigMode::LSB,
        0x01 => RigMode::USB,
        0x02 => RigMode::CW,
        0x03 => RigMode::CWR,
        0x04 => RigMode::AM,
        0x06 => RigMode::WFM,
        0x08 => RigMode::FM,
        0x0A => RigMode::DIG,
        0x0C => RigMode::PKT,
        other => RigMode::Other(format!("0x{:02X}", other)),
    }
}
