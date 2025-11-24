// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use crate::DynResult;

/// Encode frequency in Hz into 4 BCD bytes (10 Hz resolution) used by Yaesu CAT.
pub fn encode_freq_bcd(freq_hz: u64) -> DynResult<[u8; 4]> {
    if !freq_hz.is_multiple_of(10) {
        return Err("frequency must be a multiple of 10 Hz for CAT encoding".into());
    }

    let mut n = freq_hz / 10; // FT-817 uses 10 Hz units.
    if n > 99_999_999 {
        return Err("frequency out of range for CAT BCD encoding".into());
    }

    let mut digits = [0u8; 8];
    for i in (0..8).rev() {
        digits[i] = (n % 10) as u8;
        n /= 10;
    }

    let mut out = [0u8; 4];
    for i in 0..4 {
        out[i] = (digits[i * 2] << 4) | digits[i * 2 + 1];
    }

    Ok(out)
}

/// Decode 4 BCD bytes (10 Hz resolution) into frequency in Hz.
pub fn decode_freq_bcd(bytes: [u8; 4]) -> DynResult<u64> {
    let mut value = 0u64;

    for b in bytes {
        let high = (b >> 4) & 0x0F;
        let low = b & 0x0F;
        if high >= 10 || low >= 10 {
            return Err("invalid BCD digit in frequency".into());
        }

        value = value * 10 + u64::from(high);
        value = value * 10 + u64::from(low);
    }

    Ok(value * 10) // Convert back to Hz from 10 Hz units.
}
