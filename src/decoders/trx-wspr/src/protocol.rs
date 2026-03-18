// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

/// Decoded WSPR message payload.
#[derive(Debug, Clone)]
pub struct WsprProtocolMessage {
    pub message: String,
}

const POLY1: u32 = 0xF2D05351;
const POLY2: u32 = 0xE4613C47;
const NBITS: usize = 81; // 50 payload bits + 31 convolutional flush bits
const NSYMS: usize = 162;

/// Reverse the bits of an 8-bit value.
fn rev8(mut b: u8) -> u8 {
    let mut r = 0u8;
    for _ in 0..8 {
        r = (r << 1) | (b & 1);
        b >>= 1;
    }
    r
}

/// Extract hard data bits from 4-FSK symbols then deinterleave.
///
/// In WSPR: symbol = sync_bit + 2*data_bit, so data_bit = symbol >> 1.
/// The 162 data bits are reordered via bit-reversal of 8-bit indices.
///
/// The interleaving places coded bit p at transmitted position j = rev8(i)
/// (for each i in 0..255 where rev8(i) < 162).  Deinterleaving reverses
/// this: coded[p] = transmitted[j] >> 1.
fn deinterleave(symbols: &[u8]) -> [u8; NSYMS] {
    let mut out = [0u8; NSYMS];
    let mut p = 0usize;
    for i in 0u8..=255 {
        let j = rev8(i) as usize;
        if j < NSYMS {
            out[p] = symbols[j] >> 1;
            p += 1;
        }
    }
    out
}

/// Fano sequential decoder for the K=32, rate-1/2 convolutional code.
///
/// `coded[2*k]` and `coded[2*k+1]` are the two received bits for input bit k.
/// Returns the 81 decoded bits (first 50 are payload), or None if it cannot
/// converge within the iteration budget.
fn fano_decode(coded: &[u8; NSYMS]) -> Option<[u8; NBITS]> {
    const MAX_CYCLES: usize = 100_000;

    // Hard-decision branch metric: +1 per matching bit, -1 per mismatch.
    let bm = |pos: usize, state: u32, bit: u32| -> i32 {
        let ns = (state << 1) | bit;
        let c0 = ((ns & POLY1).count_ones() & 1) as u8;
        let c1 = ((ns & POLY2).count_ones() & 1) as u8;
        let r0 = coded[2 * pos];
        let r1 = coded[2 * pos + 1];
        (if c0 == r0 { 1 } else { -1 }) + (if c1 == r1 { 1 } else { -1 })
    };

    let mut bits = [0u8; NBITS];
    let mut states = [0u32; NBITS + 1];
    let mut metrics = [0i32; NBITS + 1];
    // 0 = not yet visited, 1 = tried best branch, 2 = tried both branches
    let mut visit = [0u8; NBITS];

    let mut pos: usize = 0;
    let mut threshold: i32 = 0;

    for _ in 0..MAX_CYCLES {
        if pos == NBITS {
            break;
        }

        let state = states[pos];
        let base = metrics[pos];
        let m0 = base + bm(pos, state, 0);
        let m1 = base + bm(pos, state, 1);

        // b0/mv0 = best branch; b1/mv1 = second-best
        let (b0, mv0, b1, mv1) = if m0 >= m1 {
            (0u8, m0, 1u8, m1)
        } else {
            (1u8, m1, 0u8, m0)
        };

        // Which branch to try at this visit count?
        let chosen = match visit[pos] {
            0 if mv0 >= threshold => Some((b0, mv0)),
            1 if mv1 >= threshold => Some((b1, mv1)),
            _ => None,
        };

        if let Some((b, m)) = chosen {
            visit[pos] = if visit[pos] == 0 { 1 } else { 2 };
            bits[pos] = b;
            states[pos + 1] = (state << 1) | b as u32;
            metrics[pos + 1] = m;
            pos += 1;
        } else {
            // Both branches at this position fail the threshold: backtrack.
            visit[pos] = 0;
            if pos == 0 {
                threshold -= 1;
            } else {
                pos -= 1;
                // Parent node (visit[pos] == 1 or 2) will try the next branch
                // on the following iteration, or backtrack further.
            }
        }
    }

    if pos < NBITS {
        return None;
    }
    Some(bits)
}

/// Unpack 50 payload bits into a formatted WSPR message string.
///
/// Layout (MSB first):
///   bits  0-27  — N1 (28 bits): callsign
///   bits 28-42  — M1 (15 bits): Maidenhead grid
///   bits 43-49  — P  ( 7 bits): power code (dBm + 64)
fn unpack_message(bits: &[u8; NBITS]) -> Option<String> {
    // Accumulate N1, M1, and power code from the bit array.
    let mut n1 = 0u32;
    for &b in &bits[..28] {
        n1 = (n1 << 1) | b as u32;
    }
    let mut m1 = 0u32;
    for &b in &bits[28..43] {
        m1 = (m1 << 1) | b as u32;
    }
    let mut power_code = 0u32;
    for &b in &bits[43..50] {
        power_code = (power_code << 1) | b as u32;
    }

    // WSPR only permits specific power levels (dBm).
    const VALID_POWER: [i32; 19] = [
        0, 3, 7, 10, 13, 17, 20, 23, 27, 30, 33, 37, 40, 43, 47, 50, 53, 57, 60,
    ];
    let power_dbm = power_code as i32;
    if !VALID_POWER.contains(&power_dbm) {
        return None;
    }

    // Decode callsign from N1.
    // N1 = ((c0*36 + c1)*10 + c2)*27^3 + c3*27^2 + c4*27 + c5
    // c0,c1 ∈ charset37; c2 ∈ '0'-'9'; c3,c4,c5 ∈ charset27
    const CS37: &[u8] = b" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const CS27: &[u8] = b" ABCDEFGHIJKLMNOPQRSTUVWXYZ";

    let mut n = n1;
    let i5 = (n % 27) as usize;
    n /= 27;
    let i4 = (n % 27) as usize;
    n /= 27;
    let i3 = (n % 27) as usize;
    n /= 27;
    let i2 = (n % 10) as usize;
    n /= 10;
    let i1 = (n % 36) as usize;
    n /= 36;
    let i0 = n as usize;

    if i0 >= 37 || i1 >= 37 || i2 >= 10 || i3 >= 27 || i4 >= 27 || i5 >= 27 {
        return None;
    }

    let callsign = format!(
        "{}{}{}{}{}{}",
        CS37[i0] as char,
        CS37[i1] as char,
        (b'0' + i2 as u8) as char,
        CS27[i3] as char,
        CS27[i4] as char,
        CS27[i5] as char,
    )
    .trim()
    .to_string();

    // WSPR callsigns: after trimming, the digit (from position 2 of the
    // 6-char padded form) must appear at index 1 or 2.  The callsign must
    // also contain at least one letter and be at least 3 characters long.
    if callsign.len() < 3 || !callsign.chars().any(|c| c.is_alphabetic()) {
        return None;
    }
    let has_digit_at_1_or_2 = callsign.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
        || callsign.chars().nth(2).is_some_and(|c| c.is_ascii_digit());
    if !has_digit_at_1_or_2 {
        return None;
    }

    // Decode Maidenhead grid from M1.
    // M1 = (179 - 10*loc1 - loc3)*180 + 10*loc2 + loc4
    // loc1,loc2 ∈ 0-17 (A-R); loc3,loc4 ∈ 0-9
    if m1 > 32_399 {
        return None;
    }
    let hi = m1 / 180;
    let lo = m1 % 180;
    let t = 179u32.checked_sub(hi)?;
    let loc1 = t / 10; // longitude letter index
    let loc3 = t % 10; // longitude digit
    let loc2 = lo / 10; // latitude letter index
    let loc4 = lo % 10; // latitude digit

    if loc1 > 17 || loc2 > 17 {
        return None;
    }

    let grid = format!(
        "{}{}{}{}",
        (b'A' + loc1 as u8) as char,
        (b'A' + loc2 as u8) as char,
        (b'0' + loc3 as u8) as char,
        (b'0' + loc4 as u8) as char,
    );

    Some(format!("{} {} {}", callsign, grid, power_dbm))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a WSPR callsign+grid+power into N1/M1/power_code, then round-trip
    /// through `unpack_message` to verify the pack/unpack formulas are inverse.
    #[test]
    fn unpack_known_message() {
        // Callsign "K1JT", grid "FN20", power 37 dBm — a well-known WSPR beacon.
        // Encode callsign "K1JT  " (padded to 6 chars with trailing spaces).
        // charset37: ' '=0, '0'=1,..'9'=10, 'A'=11,..'Z'=36
        // charset27: ' '=0, 'A'=1,..'Z'=26
        let cs37 = b" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let cs27 = b" ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let idx37 = |c: u8| cs37.iter().position(|&x| x == c).unwrap() as u32;
        let idx27 = |c: u8| cs27.iter().position(|&x| x == c).unwrap() as u32;

        // "K1JT  " → c0='K'=21, c1='1'=2, c2='J'-no, wait: position 2 must be digit
        // Standard WSPR normalises so that position 2 is the digit.
        // "K1JT  " has digit at position 1, not 2 → needs prefix space: " K1JT "
        // " K1JT ": c0=' '=0, c1='K'=21, c2='1', c3='J'=10, c4='T'=20, c5=' '=0
        let c0 = idx37(b' ');
        let c1 = idx37(b'K');
        let c2 = 1u32; // '1'
        let c3 = idx27(b'J');
        let c4 = idx27(b'T');
        let c5 = idx27(b' ');

        let n1 = ((c0 * 36 + c1) * 10 + c2) * 27u32.pow(3) + c3 * 27u32.pow(2) + c4 * 27 + c5;

        // Grid "FN20": loc1='F'=5 (lon), loc2='N'=13 (lat), loc3='2', loc4='0'
        let loc1 = (b'F' - b'A') as u32; // 5
        let loc2 = (b'N' - b'A') as u32; // 13
        let loc3 = 2u32;
        let loc4 = 0u32;
        let m1 = (179 - 10 * loc1 - loc3) * 180 + 10 * loc2 + loc4;

        // Power 37 dBm → power_code = 37 (raw dBm value)
        let power_code = 37u32;

        // Pack into 50-bit array
        let mut bits = [0u8; NBITS];
        for i in (0..28).rev() {
            bits[27 - i] = ((n1 >> i) & 1) as u8;
        }
        for i in (0..15).rev() {
            bits[42 - i] = ((m1 >> i) & 1) as u8;
        }
        for i in (0..7).rev() {
            bits[49 - i] = ((power_code >> i) & 1) as u8;
        }

        let msg = unpack_message(&bits).expect("unpack_message should succeed");
        // Message should contain callsign, grid, and power
        assert!(msg.contains("K1JT"), "callsign not found in '{}'", msg);
        assert!(msg.contains("FN20"), "grid not found in '{}'", msg);
        assert!(msg.contains("37"), "power not found in '{}'", msg);
    }
}

/// Attempt protocol-level decode from 162 4-FSK symbols.
pub fn decode_symbols(symbols: &[u8]) -> Option<WsprProtocolMessage> {
    if symbols.len() < NSYMS {
        return None;
    }
    let coded = deinterleave(symbols);
    let bits = fano_decode(&coded)?;
    let message = unpack_message(&bits)?;
    Some(WsprProtocolMessage { message })
}
