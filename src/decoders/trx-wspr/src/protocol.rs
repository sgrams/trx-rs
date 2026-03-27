// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
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

// Fano decoder parameters (matching reference wsprd)
const FANO_DELTA: i32 = 60;
const FANO_MAX_CYCLES_PER_BIT: usize = 10_000;
const FANO_BIAS: f32 = 0.45;

/// Soft-decision metric table for the Fano decoder.
/// Es/No = 6 dB log-likelihood ratio table from WSJT-X reference (metric_tables[2]).
#[allow(clippy::approx_constant)]
#[rustfmt::skip]
const METRIC_TABLE: [f32; 256] = [
     0.9999,  0.9998,  0.9998,  0.9998,  0.9998,  0.9998,  0.9997,  0.9997,
     0.9997,  0.9997,  0.9997,  0.9996,  0.9996,  0.9996,  0.9995,  0.9995,
     0.9994,  0.9994,  0.9994,  0.9993,  0.9993,  0.9992,  0.9991,  0.9991,
     0.9990,  0.9989,  0.9988,  0.9988,  0.9988,  0.9986,  0.9985,  0.9984,
     0.9983,  0.9982,  0.9980,  0.9979,  0.9977,  0.9976,  0.9974,  0.9971,
     0.9969,  0.9968,  0.9965,  0.9962,  0.9960,  0.9957,  0.9953,  0.9950,
     0.9947,  0.9941,  0.9937,  0.9933,  0.9928,  0.9922,  0.9917,  0.9911,
     0.9904,  0.9897,  0.9890,  0.9882,  0.9874,  0.9863,  0.9855,  0.9843,
     0.9832,  0.9819,  0.9806,  0.9792,  0.9777,  0.9760,  0.9743,  0.9724,
     0.9704,  0.9683,  0.9659,  0.9634,  0.9609,  0.9581,  0.9550,  0.9516,
     0.9481,  0.9446,  0.9406,  0.9363,  0.9317,  0.9270,  0.9218,  0.9160,
     0.9103,  0.9038,  0.8972,  0.8898,  0.8822,  0.8739,  0.8647,  0.8554,
     0.8457,  0.8357,  0.8231,  0.8115,  0.7984,  0.7854,  0.7704,  0.7556,
     0.7391,  0.7210,  0.7038,  0.6840,  0.6633,  0.6408,  0.6174,  0.5939,
     0.5678,  0.5410,  0.5137,  0.4836,  0.4524,  0.4193,  0.3850,  0.3482,
     0.3132,  0.2733,  0.2315,  0.1891,  0.1435,  0.0980,  0.0493,  0.0000,
    -0.0510, -0.1052, -0.1593, -0.2177, -0.2759, -0.3374, -0.4005, -0.4599,
    -0.5266, -0.5935, -0.6626, -0.7328, -0.8051, -0.8757, -0.9498, -1.0271,
    -1.1019, -1.1816, -1.2642, -1.3459, -1.4295, -1.5077, -1.5958, -1.6818,
    -1.7647, -1.8548, -1.9387, -2.0295, -2.1152, -2.2154, -2.3011, -2.3904,
    -2.4820, -2.5786, -2.6730, -2.7652, -2.8616, -2.9546, -3.0526, -3.1445,
    -3.2445, -3.3416, -3.4357, -3.5325, -3.6324, -3.7313, -3.8225, -3.9209,
    -4.0248, -4.1278, -4.2261, -4.3193, -4.4220, -4.5262, -4.6214, -4.7242,
    -4.8234, -4.9245, -5.0298, -5.1250, -5.2232, -5.3267, -5.4332, -5.5342,
    -5.6431, -5.7270, -5.8401, -5.9350, -6.0407, -6.1418, -6.2363, -6.3384,
    -6.4536, -6.5429, -6.6582, -6.7433, -6.8438, -6.9478, -7.0789, -7.1894,
    -7.2714, -7.3815, -7.4810, -7.5575, -7.6852, -7.8071, -7.8580, -7.9724,
    -8.1000, -8.2207, -8.2867, -8.4017, -8.5287, -8.6347, -8.7082, -8.8319,
    -8.9448, -9.0355, -9.1885, -9.2095, -9.2863, -9.4186, -9.5064, -9.6386,
    -9.7207, -9.8286, -9.9453,-10.0701,-10.1735,-10.3001,-10.2858,-10.5427,
   -10.5982,-10.7361,-10.7042,-10.9212,-11.0097,-11.0469,-11.1155,-11.2812,
   -11.3472,-11.4988,-11.5327,-11.6692,-11.9376,-11.8606,-12.1372,-13.2539,
];

/// Build the integer metric table for the soft-decision Fano decoder.
///
/// `mettab[0][rx]` = metric when expected coded bit is 0, received symbol is `rx`
/// `mettab[1][rx]` = metric when expected coded bit is 1, received symbol is `rx`
fn build_mettab() -> [[i32; 256]; 2] {
    let mut mettab = [[0i32; 256]; 2];
    for i in 0..256 {
        mettab[0][i] = (10.0 * (METRIC_TABLE[i] - FANO_BIAS)).round() as i32;
        mettab[1][i] = (10.0 * (METRIC_TABLE[255 - i] - FANO_BIAS)).round() as i32;
    }
    mettab
}

/// Reverse the bits of an 8-bit value.
fn rev8(mut b: u8) -> u8 {
    let mut r = 0u8;
    for _ in 0..8 {
        r = (r << 1) | (b & 1);
        b >>= 1;
    }
    r
}

/// Deinterleave soft symbols by permuting their order via bit-reversal of indices.
///
/// Unlike the old hard-decision version, this does NOT extract data bits — the
/// soft values (0-255, centered at 128) are preserved as-is. The Fano decoder
/// interprets them directly via the metric table.
fn deinterleave(symbols: &[u8]) -> [u8; NSYMS] {
    let mut out = [128u8; NSYMS]; // default to "no confidence"
    let mut p = 0usize;
    for i in 0u16..=255 {
        let j = rev8(i as u8) as usize;
        if j < NSYMS {
            out[p] = if j < symbols.len() { symbols[j] } else { 128 };
            p += 1;
        }
    }
    out
}

/// Compute the 2-bit convolutional encoder output for a given encoder state.
///
/// Returns a value 0-3 where:
///   bit 1 (2's place) = parity(state & POLY1)
///   bit 0 (1's place) = parity(state & POLY2)
fn encode_sym(state: u32) -> u32 {
    let p1 = (state & POLY1).count_ones() & 1;
    let p2 = (state & POLY2).count_ones() & 1;
    (p1 << 1) | p2
}

/// Result from the Fano decoder including quality metric.
struct FanoResult {
    bits: [u8; NBITS],
    /// Cumulative path metric — higher values indicate higher confidence.
    metric: i64,
}

/// Soft-decision Fano sequential decoder for K=32, rate-1/2 convolutional code.
///
/// Closely follows the reference implementation from WSJT-X (fano.c by Phil Karn, KA9Q).
///
/// Input: 162 deinterleaved soft-decision symbols (0-255, 128=no confidence).
/// Symbols are read in pairs: `symbols[2k]` and `symbols[2k+1]` are the two
/// coded bits for input bit k.
///
/// Output: decoded bits and cumulative path metric, or None on timeout.
fn fano_decode(symbols: &[u8; NSYMS]) -> Option<FanoResult> {
    let mettab = build_mettab();
    let max_cycles = FANO_MAX_CYCLES_PER_BIT * NBITS;
    let tail_start = NBITS - 31; // position 50: first tail bit

    // Precompute all 4 branch metrics for each bit position.
    // metrics[k][sym_pair] where sym_pair encodes (expected_bit0, expected_bit1):
    //   0 = (0,0), 1 = (0,1), 2 = (1,0), 3 = (1,1)
    let mut metrics = [[0i32; 4]; NBITS];
    for k in 0..NBITS {
        let s0 = symbols[2 * k] as usize;
        let s1 = symbols[2 * k + 1] as usize;
        metrics[k][0] = mettab[0][s0] + mettab[0][s1];
        metrics[k][1] = mettab[0][s0] + mettab[1][s1];
        metrics[k][2] = mettab[1][s0] + mettab[0][s1];
        metrics[k][3] = mettab[1][s0] + mettab[1][s1];
    }

    // Per-node state
    let mut encstate = [0u32; NBITS + 1];
    let mut gamma = [0i64; NBITS + 1]; // cumulative path metric
    let mut tm = [[0i32; 2]; NBITS]; // sorted branch metrics [best, second]
    let mut branch_i = [0u8; NBITS]; // 0 = trying best branch, 1 = trying second

    let mut pos: usize = 0;
    let mut t: i64 = 0; // threshold

    // Initialize root node: compute and sort branch metrics
    let lsym = encode_sym(encstate[0]) as usize;
    let m0 = metrics[0][lsym];
    let m1 = metrics[0][3 ^ lsym];
    if m0 > m1 {
        tm[0] = [m0, m1];
    } else {
        tm[0] = [m1, m0];
        encstate[0] |= 1; // 1-branch is better; encode choice in LSB
    }
    branch_i[0] = 0;

    for _cycle in 0..max_cycles {
        if pos >= NBITS {
            break;
        }

        // Look forward: try current branch
        let ngamma = gamma[pos] + tm[pos][branch_i[pos] as usize] as i64;
        if ngamma >= t {
            // Acceptable — tighten threshold if this is a first visit
            if gamma[pos] < t + FANO_DELTA as i64 {
                while ngamma >= t + FANO_DELTA as i64 {
                    t += FANO_DELTA as i64;
                }
            }

            // Move forward
            gamma[pos + 1] = ngamma;
            encstate[pos + 1] = encstate[pos] << 1;
            pos += 1;

            if pos >= NBITS {
                break; // Done!
            }

            // Compute and sort metrics at the new position
            let lsym = encode_sym(encstate[pos]) as usize;
            if pos >= tail_start {
                // Tail must be all zeros — only consider 0-branch
                tm[pos] = [metrics[pos][lsym], i32::MIN];
            } else {
                let m0 = metrics[pos][lsym];
                let m1 = metrics[pos][3 ^ lsym];
                if m0 > m1 {
                    tm[pos] = [m0, m1];
                } else {
                    tm[pos] = [m1, m0];
                    encstate[pos] |= 1; // mark 1-branch as better
                }
            }
            branch_i[pos] = 0;
            continue;
        }

        // Threshold violated — look backward
        loop {
            if pos == 0 || gamma[pos - 1] < t {
                // Can't back up (at root, or parent's metric below threshold).
                // Relax threshold and reset to best branch at current position.
                t -= FANO_DELTA as i64;
                if branch_i[pos] != 0 {
                    branch_i[pos] = 0;
                    encstate[pos] ^= 1;
                }
                break;
            }
            // Back up to parent
            pos -= 1;
            if pos < tail_start && branch_i[pos] != 1 {
                // Try second branch at this position
                branch_i[pos] = 1;
                encstate[pos] ^= 1;
                break;
            }
            // Already tried both branches (or in tail) — keep backing up
        }
    }

    if pos < NBITS {
        return None; // Timeout
    }

    // Extract decoded bits from encoder states.
    // At each position k, the LSB of encstate[k] is the chosen input bit.
    let mut bits = [0u8; NBITS];
    for k in 0..NBITS {
        bits[k] = (encstate[k] & 1) as u8;
    }
    Some(FanoResult {
        bits,
        metric: gamma[NBITS],
    })
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

/// Minimum Fano cumulative path metric to accept a decode.
///
/// The Fano decoder can sometimes converge on random noise, producing bits
/// that happen to unpack into a valid-looking message. The cumulative path
/// metric reflects how well the received symbols matched the best trellis
/// path. Real WSPR signals at decodable SNR produce metrics well above this
/// threshold; noise-induced decodes have metrics near or below zero.
const FANO_MIN_METRIC: i64 = 20;

/// Attempt protocol-level decode from 162 soft-decision symbols.
///
/// Input: 162 bytes where each value is a soft-decision symbol (0-255):
///   0   = high confidence that data bit is 0
///   128 = no confidence
///   255 = high confidence that data bit is 1
pub fn decode_symbols(symbols: &[u8]) -> Option<WsprProtocolMessage> {
    if symbols.len() < NSYMS {
        return None;
    }
    let coded = deinterleave(symbols);
    let result = fano_decode(&coded)?;

    // Reject low-confidence decodes that are likely false positives from noise
    if result.metric < FANO_MIN_METRIC {
        return None;
    }

    let message = unpack_message(&result.bits)?;
    Some(WsprProtocolMessage { message })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::SYNC_VECTOR;

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

    /// Convolutionally encode 81 bits → 162 coded bits (for testing).
    fn convolutional_encode(input: &[u8; NBITS]) -> [u8; NSYMS] {
        let mut coded = [0u8; NSYMS];
        let mut encstate: u32 = 0;
        for k in 0..NBITS {
            encstate = (encstate << 1) | input[k] as u32;
            coded[2 * k] = ((encstate & POLY1).count_ones() & 1) as u8;
            coded[2 * k + 1] = ((encstate & POLY2).count_ones() & 1) as u8;
        }
        coded
    }

    /// Interleave coded bits (inverse of deinterleave).
    fn interleave(coded: &[u8; NSYMS]) -> [u8; NSYMS] {
        let mut out = [0u8; NSYMS];
        let mut p = 0usize;
        for i in 0u16..=255 {
            let j = rev8(i as u8) as usize;
            if j < NSYMS {
                out[j] = coded[p];
                p += 1;
            }
        }
        out
    }

    /// End-to-end test: encode K1JT FN20 37, produce perfect soft symbols,
    /// and verify round-trip decode.
    #[test]
    fn roundtrip_encode_decode() {
        let cs37 = b" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let cs27 = b" ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let idx37 = |c: u8| cs37.iter().position(|&x| x == c).unwrap() as u32;
        let idx27 = |c: u8| cs27.iter().position(|&x| x == c).unwrap() as u32;

        let c0 = idx37(b' ');
        let c1 = idx37(b'K');
        let c2 = 1u32;
        let c3 = idx27(b'J');
        let c4 = idx27(b'T');
        let c5 = idx27(b' ');
        let n1 = ((c0 * 36 + c1) * 10 + c2) * 27u32.pow(3) + c3 * 27u32.pow(2) + c4 * 27 + c5;
        let m1 = (179 - 10 * 5 - 2) * 180 + 10 * 13 + 0; // FN20
        let power_code = 37u32;

        let mut input_bits = [0u8; NBITS];
        for i in (0..28).rev() {
            input_bits[27 - i] = ((n1 >> i) & 1) as u8;
        }
        for i in (0..15).rev() {
            input_bits[42 - i] = ((m1 >> i) & 1) as u8;
        }
        for i in (0..7).rev() {
            input_bits[49 - i] = ((power_code >> i) & 1) as u8;
        }
        // bits 50..80 are tail (zeros), already set

        // Convolutional encode
        let coded = convolutional_encode(&input_bits);

        // Interleave
        let interleaved = interleave(&coded);

        // Create channel symbols: symbol[i] = sync[i] + 2*data_bit[i]
        let channel_syms: Vec<u8> = (0..NSYMS)
            .map(|i| SYNC_VECTOR[i] + 2 * interleaved[i])
            .collect();

        // Create perfect soft symbols from channel symbols.
        // data_bit = channel_sym >> 1. Soft: 0 if data=0, 255 if data=1.
        let soft: Vec<u8> = channel_syms
            .iter()
            .map(|&cs| if cs >> 1 == 1 { 255u8 } else { 0u8 })
            .collect();

        // Decode
        let result = decode_symbols(&soft);
        assert!(result.is_some(), "decode_symbols should succeed");
        let msg = result.unwrap().message;
        assert!(msg.contains("K1JT"), "callsign not found in '{msg}'");
        assert!(msg.contains("FN20"), "grid not found in '{msg}'");
        assert!(msg.contains("37"), "power not found in '{msg}'");
    }

    /// Verify deinterleave is the inverse of interleave.
    #[test]
    fn interleave_deinterleave_roundtrip() {
        // Create a sequence of distinguishable values
        let mut original = [0u8; NSYMS];
        for i in 0..NSYMS {
            original[i] = (i % 256) as u8;
        }

        let interleaved = interleave(&original);
        let recovered = deinterleave(&interleaved);
        assert_eq!(original, recovered, "deinterleave should invert interleave");
    }

    /// Verify that the Fano decoder can decode a convolutionally-encoded message
    /// with perfect soft symbols (0 and 255).
    #[test]
    fn fano_decode_perfect_soft_symbols() {
        // Create a simple 81-bit message (50 payload + 31 tail zeros)
        let mut input_bits = [0u8; NBITS];
        // Set some payload bits to a recognizable pattern
        input_bits[0] = 1;
        input_bits[5] = 1;
        input_bits[10] = 1;
        input_bits[15] = 1;
        input_bits[20] = 1;

        // Encode
        let coded = convolutional_encode(&input_bits);

        // Convert to perfect soft symbols: coded_bit=0 → 0, coded_bit=1 → 255
        let mut soft = [0u8; NSYMS];
        for i in 0..NSYMS {
            soft[i] = if coded[i] == 1 { 255 } else { 0 };
        }

        // Fano decode (already in coded order, no interleaving needed)
        let result = fano_decode(&soft);
        assert!(result.is_some(), "Fano decoder should succeed");
        let result = result.unwrap();
        assert_eq!(
            &result.bits[..NBITS],
            &input_bits[..NBITS],
            "Decoded bits should match input"
        );
        assert!(
            result.metric > 0,
            "Path metric should be positive for perfect symbols"
        );
    }
}
