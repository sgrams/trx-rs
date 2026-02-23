// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! APRS-IS IGate uplink — forwards RF-decoded APRS packets to APRS-IS (aprs.fi etc.).

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time::{self, Duration};
use tracing::{debug, info, warn};

use trx_core::decode::{AprsPacket, DecodedMessage};

use crate::config::AprsFiConfig;

/// Compute the APRS-IS passcode for a callsign.
///
/// Algorithm matches the canonical JS/Python reference implementations:
/// - Strip SSID (everything from `-` onwards)
/// - Take the first 10 characters, uppercased
/// - XOR hash initialised at 0x73E2, processed in 2-byte pairs
/// - Mask result with 0x7FFF
pub fn compute_passcode(callsign: &str) -> u16 {
    // Strip SSID
    let base = callsign.split('-').next().unwrap_or(callsign);
    // First 10 chars, uppercase
    let upper: String = base.chars().take(10).map(|c| c.to_ascii_uppercase()).collect();
    let bytes = upper.as_bytes();

    let mut hash: u16 = 0x73e2;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= (bytes[i] as u16) << 8;
        if i + 1 < bytes.len() {
            hash ^= bytes[i + 1] as u16;
        }
        i += 2;
    }
    hash & 0x7fff
}

/// Format an [`AprsPacket`] as a TNC2 line (CRLF-terminated) for APRS-IS.
fn format_tnc2(pkt: &AprsPacket) -> String {
    if pkt.path.is_empty() {
        format!("{}>{}:{}\r\n", pkt.src_call, pkt.dest_call, pkt.info)
    } else {
        format!(
            "{}>{},{}:{}\r\n",
            pkt.src_call, pkt.dest_call, pkt.path, pkt.info
        )
    }
}

/// Run the APRS-IS IGate uplink task.
///
/// Subscribes to the decoded-message broadcast channel and forwards every
/// CRC-valid APRS packet to the configured APRS-IS server as a TNC2 line.
/// Reconnects automatically with exponential backoff (1 s → 2 s → … → 60 s).
pub async fn run_aprsfi_uplink(
    cfg: AprsFiConfig,
    callsign: String,
    mut decode_rx: broadcast::Receiver<DecodedMessage>,
) {
    let passcode: u16 = if cfg.passcode == -1 {
        compute_passcode(&callsign)
    } else {
        (cfg.passcode as u16) & 0x7fff
    };

    let mut stats_received: u64 = 0;
    let mut stats_forwarded: u64 = 0;
    let mut stats_skipped: u64 = 0;
    let mut stats_write_errors: u64 = 0;
    let mut stats_reconnects: u64 = 0;
    let mut backoff_secs: u64 = 1;

    'reconnect: loop {
        // ----------------------------------------------------------------
        // TCP connect
        // ----------------------------------------------------------------
        let stream = match TcpStream::connect((cfg.host.as_str(), cfg.port)).await {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "APRS-IS IGate: connection to {}:{} failed: {}, retrying in {}s",
                    cfg.host, cfg.port, e, backoff_secs
                );
                time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(60);
                stats_reconnects += 1;
                continue 'reconnect;
            }
        };

        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // ----------------------------------------------------------------
        // Login
        // ----------------------------------------------------------------
        let login = format!(
            "user {} pass {} vers trx-server {}\r\n",
            callsign,
            passcode,
            env!("CARGO_PKG_VERSION")
        );
        if let Err(e) = write_half.write_all(login.as_bytes()).await {
            warn!(
                "APRS-IS IGate: login write to {}:{} failed: {}, retrying in {}s",
                cfg.host, cfg.port, e, backoff_secs
            );
            time::sleep(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(60);
            stats_reconnects += 1;
            continue 'reconnect;
        }

        // ----------------------------------------------------------------
        // Read logresp (up to 10 lines)
        // ----------------------------------------------------------------
        let mut verified = false;
        let mut got_logresp = false;
        let mut line = String::new();
        for _ in 0..10 {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    warn!("APRS-IS IGate: connection closed before logresp from {}:{}", cfg.host, cfg.port);
                    break;
                }
                Ok(_) => {
                    if line.starts_with("# logresp") {
                        verified = !line.contains("unverified");
                        got_logresp = true;
                        break;
                    }
                }
                Err(e) => {
                    warn!("APRS-IS IGate: error reading logresp from {}:{}: {}", cfg.host, cfg.port, e);
                    break;
                }
            }
        }

        if !got_logresp {
            warn!(
                "APRS-IS IGate: no logresp from {}:{}, retrying in {}s",
                cfg.host, cfg.port, backoff_secs
            );
            time::sleep(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(60);
            stats_reconnects += 1;
            continue 'reconnect;
        }

        info!(
            "APRS-IS IGate connected ({}:{} as {}, {})",
            cfg.host,
            cfg.port,
            callsign,
            if verified { "verified" } else { "unverified" }
        );

        // Successful connection — reset backoff
        backoff_secs = 1;

        // ----------------------------------------------------------------
        // Forward loop
        // ----------------------------------------------------------------
        let period = Duration::from_secs(60);
        let first_at = time::Instant::now() + period;
        let mut keepalive_tick = time::interval_at(first_at, period);
        let mut stats_tick = time::interval_at(first_at, period);

        'forward: loop {
            tokio::select! {
                _ = keepalive_tick.tick() => {
                    if let Err(e) = write_half.write_all(b"# trx-server keepalive\r\n").await {
                        warn!("APRS-IS IGate: keepalive write failed: {}", e);
                        stats_write_errors += 1;
                        break 'forward;
                    }
                }

                _ = stats_tick.tick() => {
                    info!(
                        "APRS-IS stats: received={}, forwarded={}, skipped={}, write_errors={}, reconnects={}",
                        stats_received, stats_forwarded, stats_skipped,
                        stats_write_errors, stats_reconnects
                    );
                }

                recv = decode_rx.recv() => {
                    match recv {
                        Ok(DecodedMessage::Aprs(pkt)) => {
                            stats_received += 1;
                            if !pkt.crc_ok {
                                stats_skipped += 1;
                                continue 'forward;
                            }
                            let tnc2 = format_tnc2(&pkt);
                            debug!("APRS-IS: forwarded {}>{},...", pkt.src_call, pkt.dest_call);
                            if let Err(e) = write_half.write_all(tnc2.as_bytes()).await {
                                warn!("APRS-IS IGate: packet write failed: {}", e);
                                stats_write_errors += 1;
                                break 'forward;
                            }
                            stats_forwarded += 1;
                        }
                        Ok(_) => {
                            // Non-APRS messages (FT8, WSPR, CW) are silently skipped
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("APRS-IS IGate: dropped {} decode events (channel lagged)", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            return;
                        }
                    }
                }
            }
        }

        // Forward loop exited due to a write error — reconnect with backoff
        stats_reconnects += 1;
        warn!(
            "APRS-IS IGate: disconnected from {}:{}, reconnecting in {}s",
            cfg.host, cfg.port, backoff_secs
        );
        time::sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(60);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trx_core::decode::AprsPacket;

    fn make_pkt(src: &str, dest: &str, path: &str, info: &str, crc_ok: bool) -> AprsPacket {
        AprsPacket {
            src_call: src.to_string(),
            dest_call: dest.to_string(),
            path: path.to_string(),
            info: info.to_string(),
            info_bytes: vec![],
            packet_type: "Unknown".to_string(),
            crc_ok,
            lat: None,
            lon: None,
            symbol_table: None,
            symbol_code: None,
        }
    }

    #[test]
    fn passcode_result_in_valid_range() {
        assert!(compute_passcode("N0CALL") <= 0x7fff);
        assert!(compute_passcode("W1AW") <= 0x7fff);
        assert!(compute_passcode("SP2SJG") <= 0x7fff);
    }

    #[test]
    fn passcode_strips_ssid() {
        assert_eq!(compute_passcode("N0CALL"), compute_passcode("N0CALL-9"));
        assert_eq!(compute_passcode("W1AW"), compute_passcode("W1AW-5"));
        assert_eq!(compute_passcode("SP2SJG"), compute_passcode("SP2SJG-15"));
    }

    #[test]
    fn passcode_case_insensitive() {
        assert_eq!(compute_passcode("n0call"), compute_passcode("N0CALL"));
        assert_eq!(compute_passcode("sp2sjg"), compute_passcode("SP2SJG"));
    }

    #[test]
    fn passcode_truncates_to_ten_chars() {
        // Callsigns are at most 10 chars after stripping SSID; extra chars must be ignored
        assert_eq!(
            compute_passcode("ABCDEFGHIJ"),
            compute_passcode("ABCDEFGHIJKL")
        );
    }

    #[test]
    fn tnc2_with_path() {
        let pkt = make_pkt(
            "N0CALL-9",
            "APRS",
            "WIDE1-1,WIDE2-1",
            "!1234.56N/01234.56E-Test",
            true,
        );
        assert_eq!(
            format_tnc2(&pkt),
            "N0CALL-9>APRS,WIDE1-1,WIDE2-1:!1234.56N/01234.56E-Test\r\n"
        );
    }

    #[test]
    fn tnc2_without_path() {
        let pkt = make_pkt("W1AW", "BEACON", "", ">Test status", true);
        assert_eq!(format_tnc2(&pkt), "W1AW>BEACON:>Test status\r\n");
    }
}
