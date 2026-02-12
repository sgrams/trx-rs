// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::pin::Pin;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{timeout, Duration};
use tokio_serial::{ClearBuffer, SerialPort, SerialPortBuilderExt, SerialStream};

use trx_core::radio::freq::{Band, Freq};
use trx_core::rig::{
    Rig, RigAccessMethod, RigCapabilities, RigCat, RigInfo, RigStatusFuture, RigVfo, RigVfoEntry,
};
use trx_core::{DynResult, RigMode};

/// Backend for Yaesu FT-450D CAT control.
pub struct Ft450d {
    port: SerialStream,
    info: RigInfo,
    vfo_side: Ft450dVfoSide,
    vfo_a_freq: Option<Freq>,
    vfo_b_freq: Option<Freq>,
    vfo_a_mode: Option<RigMode>,
    vfo_b_mode: Option<RigMode>,
}

impl Ft450d {
    const READ_TIMEOUT: Duration = Duration::from_millis(800);

    pub fn new(path: &str, baud: u32) -> DynResult<Self> {
        let builder = tokio_serial::new(path, baud);
        let port = builder.open_native_async()?;
        let info = RigInfo {
            manufacturer: "Yaesu".to_string(),
            model: "FT-450D".to_string(),
            revision: "".to_string(),
            capabilities: RigCapabilities {
                supported_bands: vec![
                    // Transmit-capable amateur bands (HF + 6m)
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
                    // Receive-only coverage segments (general coverage)
                    Band {
                        low_hz: 30_000,
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
                        high_hz: 56_000_000,
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
            vfo_side: Ft450dVfoSide::Unknown,
            vfo_a_freq: None,
            vfo_b_freq: None,
            vfo_a_mode: None,
            vfo_b_mode: None,
        })
    }

    /// Query current status (frequency, mode, VFO) from FT-450D.
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
            Ft450dVfoSide::A if self.vfo_a_freq.is_some() => Some(0),
            Ft450dVfoSide::B if self.vfo_a_freq.is_some() => Some(1),
            Ft450dVfoSide::B if self.vfo_a_freq.is_none() && self.vfo_b_freq.is_some() => Some(0),
            _ => None,
        };
        let vfo = if entries.is_empty() {
            None
        } else {
            Some(RigVfo { entries, active })
        };
        Ok((freq, mode, vfo))
    }

    /// Query current frequency from FT-450D.
    pub async fn get_freq(&mut self) -> DynResult<Freq> {
        let (freq, _, _) = self.get_status().await?;
        Ok(freq)
    }

    /// Query current mode from FT-450D.
    pub async fn get_mode(&mut self) -> DynResult<RigMode> {
        let (_, mode, _) = self.get_status().await?;
        Ok(mode)
    }

    /// Send CAT command to set frequency on FT-450D.
    pub async fn set_freq(&mut self, freq: Freq) -> DynResult<()> {
        self.write_cmd(&format!("FA{:08};", freq.hz)).await?;
        self.update_vfo_freq(freq);
        Ok(())
    }

    /// Send CAT command to set mode on FT-450D.
    pub async fn set_mode(&mut self, mode: &RigMode) -> DynResult<()> {
        let mode_code = encode_mode(mode)?;
        self.write_cmd(&format!("MD0{};", mode_code)).await?;
        self.update_vfo_mode(mode.clone());
        Ok(())
    }

    /// Send CAT command to control PTT on FT-450D.
    pub async fn set_ptt(&mut self, ptt: bool) -> DynResult<()> {
        let cmd = if ptt { "TX1;" } else { "TX0;" };
        self.write_cmd(cmd).await?;
        Ok(())
    }

    /// Turn the radio on via CAT. The first frame is ignored while the CPU wakes,
    /// so send a dummy payload before issuing the actual command.
    pub async fn power_on(&mut self) -> DynResult<()> {
        self.write_cmd("PS1;").await?;
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        let _ = self.port.clear(ClearBuffer::Input);
        Ok(())
    }

    /// Turn the radio off via CAT.
    pub async fn power_off(&mut self) -> DynResult<()> {
        self.write_cmd("PS0;").await?;
        Ok(())
    }

    /// Toggle between VFO A/B.
    pub async fn toggle_vfo(&mut self) -> DynResult<()> {
        Err("VFO toggle not supported on FT-450D backend".into())
    }

    /// Enable front panel lock.
    pub async fn lock(&mut self) -> DynResult<()> {
        self.write_cmd("LK1;").await?;
        Ok(())
    }

    /// Disable front panel lock.
    pub async fn unlock(&mut self) -> DynResult<()> {
        self.write_cmd("LK0;").await?;
        Ok(())
    }

    /// Read the current signal strength meter (S-meter/PWR) from the radio.
    ///
    /// The returned value is the raw CAT meter byte (0-255). In receive it
    /// represents S-meter level; in transmit it reports power/ALC depending on
    /// rig state.
    pub async fn get_signal_strength(&mut self) -> DynResult<u8> {
        self.read_meter("SM0;").await
    }

    /// Read the current transmit power indication (raw meter value).
    ///
    /// The FT-450D reports the same meter byte for TX power as for the S-meter;
    /// callers should interpret based on current PTT state.
    pub async fn get_tx_power(&mut self) -> DynResult<u8> {
        self.read_meter("RM5;").await
    }

    async fn read_status(&mut self) -> DynResult<(u64, RigMode)> {
        let freq = self.read_freq().await?;
        let mode = self.read_mode().await?;
        Ok((freq, mode))
    }

    async fn read_meter(&mut self, cmd: &str) -> DynResult<u8> {
        let resp = self.query(cmd).await?;
        let digits: String = resp.chars().filter(|c| c.is_ascii_digit()).collect();
        let value: u16 = digits.parse().map_err(|_| "CAT meter parse failed")?;
        Ok(value.min(255) as u8)
    }

    async fn read_freq(&mut self) -> DynResult<u64> {
        let resp = self.query("FA;").await?;
        let data = resp
            .strip_prefix("FA")
            .ok_or("CAT freq response missing FA")?;
        let digits: String = data.chars().filter(|c| c.is_ascii_digit()).collect();
        let freq: u64 = digits.parse().map_err(|_| "CAT freq parse failed")?;
        Ok(freq)
    }

    async fn read_mode(&mut self) -> DynResult<RigMode> {
        let resp = self.query("MD0;").await?;
        let data = resp
            .strip_prefix("MD")
            .ok_or("CAT mode response missing MD")?;
        let code = data.chars().last().ok_or("CAT mode parse failed")?;
        Ok(decode_mode(code))
    }

    async fn write_cmd(&mut self, cmd: &str) -> DynResult<()> {
        self.port.write_all(cmd.as_bytes()).await?;
        self.port.flush().await?;
        Ok(())
    }

    async fn read_response(&mut self) -> DynResult<String> {
        let mut buf = Vec::new();
        let read = async {
            loop {
                let mut byte = [0u8; 1];
                self.port.read_exact(&mut byte).await?;
                if byte[0] == b';' {
                    break;
                }
                buf.push(byte[0]);
            }
            Ok::<(), std::io::Error>(())
        };
        timeout(Self::READ_TIMEOUT, read)
            .await
            .map_err(|_| "CAT read timeout")??;
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    async fn query(&mut self, cmd: &str) -> DynResult<String> {
        let _ = self.port.clear(ClearBuffer::Input);
        self.write_cmd(cmd).await?;
        self.read_response().await
    }

    fn update_vfo_freq(&mut self, freq: Freq) {
        match self.vfo_side {
            Ft450dVfoSide::A => self.vfo_a_freq = Some(freq),
            Ft450dVfoSide::B => self.vfo_b_freq = Some(freq),
            Ft450dVfoSide::Unknown => {
                // Try to infer which VFO we are on using cached values; default to A only.
                if self.vfo_b_freq.map(|f| f.hz == freq.hz).unwrap_or(false)
                    && self.vfo_a_freq.is_none()
                {
                    self.vfo_side = Ft450dVfoSide::B;
                    self.vfo_b_freq = Some(freq);
                } else {
                    self.vfo_side = Ft450dVfoSide::A;
                    self.vfo_a_freq = Some(freq);
                }
            }
        }
    }

    fn update_vfo_mode(&mut self, mode: RigMode) {
        match self.vfo_side {
            Ft450dVfoSide::A => self.vfo_a_mode = Some(mode),
            Ft450dVfoSide::B => self.vfo_b_mode = Some(mode),
            Ft450dVfoSide::Unknown => {
                // Default to current VFO (assume A) when unknown.
                self.vfo_a_mode = Some(mode);
            }
        }
    }
}

impl Rig for Ft450d {
    fn info(&self) -> &RigInfo {
        &self.info
    }
}

impl RigCat for Ft450d {
    fn get_status<'a>(&'a mut self) -> RigStatusFuture<'a> {
        Box::pin(async move { self.get_status().await })
    }

    fn set_freq<'a>(
        &'a mut self,
        freq: Freq,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft450d::set_freq(self, freq).await })
    }

    fn set_mode<'a>(
        &'a mut self,
        mode: RigMode,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft450d::set_mode(self, &mode).await })
    }

    fn set_ptt<'a>(
        &'a mut self,
        ptt: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft450d::set_ptt(self, ptt).await })
    }

    fn power_on<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft450d::power_on(self).await })
    }

    fn power_off<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft450d::power_off(self).await })
    }

    fn get_signal_strength<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move { Ft450d::get_signal_strength(self).await })
    }

    fn get_tx_power<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move { Ft450d::get_tx_power(self).await })
    }

    fn get_tx_limit<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<u8>> + Send + 'a>> {
        Box::pin(async move { Err("TX limit query not supported on FT-450D".into()) })
    }

    fn set_tx_limit<'a>(
        &'a mut self,
        _limit: u8,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Err("TX limit setting not supported on FT-450D".into()) })
    }

    fn toggle_vfo<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft450d::toggle_vfo(self).await })
    }

    fn lock<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft450d::lock(self).await })
    }

    fn unlock<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = DynResult<()>> + Send + 'a>> {
        Box::pin(async move { Ft450d::unlock(self).await })
    }
}

#[derive(Clone, Copy)]
enum Ft450dVfoSide {
    A,
    B,
    Unknown,
}

fn encode_mode(mode: &RigMode) -> DynResult<char> {
    match mode {
        RigMode::LSB => Ok('1'),
        RigMode::USB => Ok('2'),
        RigMode::CW => Ok('3'),
        RigMode::FM => Ok('4'),
        RigMode::AM => Ok('5'),
        RigMode::DIG => Ok('6'),
        RigMode::CWR => Ok('7'),
        RigMode::PKT => Ok('9'),
        RigMode::WFM => Ok('4'),
        RigMode::Other(_) => Err("Unsupported mode for FT-450D".into()),
    }
}

fn decode_mode(code: char) -> RigMode {
    match code {
        '1' => RigMode::LSB,
        '2' => RigMode::USB,
        '3' => RigMode::CW,
        '4' => RigMode::FM,
        '5' => RigMode::AM,
        '6' => RigMode::DIG,
        '7' => RigMode::CWR,
        '8' => RigMode::DIG,
        '9' => RigMode::PKT,
        'B' | 'b' => RigMode::FM,
        'C' | 'c' => RigMode::DIG,
        other => RigMode::Other(format!("mode {}", other)),
    }
}
