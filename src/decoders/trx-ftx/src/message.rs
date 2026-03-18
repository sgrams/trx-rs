// SPDX-FileCopyrightText: 2026 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

//! FTx message pack/unpack logic.
//!
//! This is a pure Rust port of `ft8_lib/ft8/message.c`.

use crate::callsign_hash::{compute_callsign_hash, CallsignHashTable, HashType};
use crate::protocol::FTX_PAYLOAD_LENGTH_BYTES;
use crate::text::{charn, dd_to_int, int_to_dd, nchar, CharTable};

/// Maximum 22-bit hash value.
const MAX22: u32 = 4_194_304;

/// Number of special tokens before hashed callsigns.
const NTOKENS: u32 = 2_063_592;

/// Maximum encodable 4-character grid value.
const MAXGRID4: u16 = 32_400;

/// Maximum number of decoded message fields.
pub const FTX_MAX_MESSAGE_FIELDS: usize = 3;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// FTx message type classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtxMessageType {
    FreeText,
    Dxpedition,
    EuVhf,
    ArrlFd,
    Telemetry,
    Contesting,
    Standard,
    ArrlRtty,
    NonstdCall,
    Wwrof,
    Unknown,
}

/// Result codes for message encode/decode operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtxMessageRc {
    Ok,
    ErrorCallsign1,
    ErrorCallsign2,
    ErrorSuffix,
    ErrorGrid,
    ErrorType,
}

/// Field type classification for decoded message fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtxFieldType {
    Unknown,
    None,
    /// RRR, RR73, 73, DE, QRZ, CQ, etc.
    Token,
    /// CQ nnn, CQ abcd
    TokenWithArg,
    Call,
    Grid,
    Rst,
}

/// Offsets and types for decoded message fields.
#[derive(Debug, Clone)]
pub struct FtxMessageOffsets {
    pub types: [FtxFieldType; FTX_MAX_MESSAGE_FIELDS],
    pub offsets: [i16; FTX_MAX_MESSAGE_FIELDS],
}

impl Default for FtxMessageOffsets {
    fn default() -> Self {
        Self {
            types: [FtxFieldType::Unknown; FTX_MAX_MESSAGE_FIELDS],
            offsets: [-1; FTX_MAX_MESSAGE_FIELDS],
        }
    }
}

/// An FTx message holding 77 bits of payload data (in 10 bytes) and
/// a 16-bit hash for duplicate detection.
#[derive(Debug, Clone)]
pub struct FtxMessage {
    pub payload: [u8; FTX_PAYLOAD_LENGTH_BYTES],
    pub hash: u32,
}

impl Default for FtxMessage {
    fn default() -> Self {
        Self::new()
    }
}

impl FtxMessage {
    /// Create a new zeroed message.
    pub fn new() -> Self {
        Self {
            payload: [0u8; FTX_PAYLOAD_LENGTH_BYTES],
            hash: 0,
        }
    }

    /// Extract i3 (bits 74..76).
    pub fn get_i3(&self) -> u8 {
        (self.payload[9] >> 3) & 0x07
    }

    /// Extract n3 (bits 71..73).
    pub fn get_n3(&self) -> u8 {
        ((self.payload[8] << 2) & 0x04) | ((self.payload[9] >> 6) & 0x03)
    }

    /// Determine the message type from i3 and n3 fields.
    pub fn get_type(&self) -> FtxMessageType {
        let i3 = self.get_i3();
        match i3 {
            0 => {
                let n3 = self.get_n3();
                match n3 {
                    0 => FtxMessageType::FreeText,
                    1 => FtxMessageType::Dxpedition,
                    2 => FtxMessageType::EuVhf,
                    3 | 4 => FtxMessageType::ArrlFd,
                    5 => FtxMessageType::Telemetry,
                    _ => FtxMessageType::Unknown,
                }
            }
            1 | 2 => FtxMessageType::Standard,
            3 => FtxMessageType::ArrlRtty,
            4 => FtxMessageType::NonstdCall,
            5 => FtxMessageType::Wwrof,
            _ => FtxMessageType::Unknown,
        }
    }

    /// Format message payload as hex string (for debug).
    pub fn to_hex_string(&self) -> String {
        self.payload
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

// ---------------------------------------------------------------------------
// Helper: string utilities (not in text.rs)
// ---------------------------------------------------------------------------

fn starts_with(s: &str, prefix: &str) -> bool {
    s.starts_with(prefix)
}

fn ends_with(s: &str, suffix: &str) -> bool {
    s.ends_with(suffix)
}

fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

fn is_letter(c: u8) -> bool {
    c.is_ascii_uppercase()
}

fn is_space(c: u8) -> bool {
    c == b' '
}

/// Copy the next whitespace-delimited token from `input` into a string,
/// returning the remainder of the input after the token (and any trailing
/// whitespace).
fn copy_token(input: &str) -> (&str, String) {
    let input = input.trim_start();
    let end = input
        .find(' ')
        .unwrap_or(input.len());
    let token = &input[..end];
    let rest = &input[end..].trim_start();
    (rest, token.to_string())
}

/// Trim leading occurrences of a specific character.
fn trim_front(s: &str, c: char) -> &str {
    s.trim_start_matches(c)
}

/// Add angle brackets around a callsign: `FOO` -> `<FOO>`.
fn add_brackets(callsign: &str) -> String {
    format!("<{}>", callsign)
}

// ---------------------------------------------------------------------------
// Internal: save_callsign / lookup_callsign
// ---------------------------------------------------------------------------

/// Compute hash values for a callsign and save it in the hash table.
/// Returns `(n22, n12, n10)` on success, or `None` if the callsign
/// contains invalid characters.
fn save_callsign(
    hash_table: Option<&mut CallsignHashTable>,
    callsign: &str,
) -> Option<(u32, u16, u16)> {
    let n22 = compute_callsign_hash(callsign)?;
    let n12 = (n22 >> 10) as u16;
    let n10 = (n22 >> 12) as u16;

    if let Some(ht) = hash_table {
        ht.add(callsign, n22);
    }

    Some((n22, n12, n10))
}

/// Look up a callsign by hash. Returns the callsign wrapped in angle
/// brackets if found, or `<...>` if not found.
fn lookup_callsign(
    hash_table: Option<&CallsignHashTable>,
    hash_type: HashType,
    hash: u32,
) -> String {
    if let Some(ht) = hash_table {
        if let Some(call) = ht.lookup(hash_type, hash) {
            return add_brackets(&call);
        }
    }
    "<...>".to_string()
}

// ---------------------------------------------------------------------------
// parse_cq_modifier
// ---------------------------------------------------------------------------

/// Parse a CQ modifier from a string like "CQ nnn" or "CQ abcd".
/// Returns the numeric value if it matches, otherwise `None`.
fn parse_cq_modifier(s: &str) -> Option<i32> {
    let bytes = s.as_bytes();
    if bytes.len() < 4 {
        return None;
    }

    let mut nnum = 0;
    let mut nlet = 0;
    let mut m: i32 = 0;

    for i in 3..8.min(bytes.len()) {
        let c = bytes[i];
        if c == b' ' || c == 0 {
            break;
        } else if c.is_ascii_digit() {
            nnum += 1;
        } else if c.is_ascii_uppercase() {
            nlet += 1;
            m = 27 * m + (c as i32 - b'A' as i32 + 1);
        } else {
            return None;
        }
    }

    if nnum == 3 && nlet == 0 {
        // "CQ nnn" - parse the 3-digit number
        let num_str: String = bytes[3..]
            .iter()
            .take_while(|&&c| c.is_ascii_digit())
            .map(|&c| c as char)
            .collect();
        if let Ok(v) = num_str.parse::<i32>() {
            return Some(v);
        }
        return None;
    } else if nnum == 0 && nlet > 0 && nlet <= 4 {
        return Some(1000 + m);
    }

    None
}

// ---------------------------------------------------------------------------
// pack_basecall
// ---------------------------------------------------------------------------

/// Pack a standard base callsign into a 28-bit integer.
/// Returns `None` if the callsign cannot be encoded in the standard way.
pub fn pack_basecall(callsign: &str) -> Option<i32> {
    let bytes = callsign.as_bytes();
    let length = bytes.len();

    if length <= 2 {
        return None;
    }

    let mut c6 = [b' '; 6];

    if starts_with(callsign, "3DA0") && length > 4 && length <= 7 {
        // Swaziland prefix: 3DA0XYZ -> 3D0XYZ
        c6[0] = b'3';
        c6[1] = b'D';
        c6[2] = b'0';
        for (i, &b) in bytes[4..].iter().enumerate() {
            if i + 3 < 6 {
                c6[i + 3] = b;
            }
        }
    } else if starts_with(callsign, "3X")
        && length > 2
        && is_letter(bytes[2])
        && length <= 7
    {
        // Guinea prefix: 3XA0XYZ -> QA0XYZ
        c6[0] = b'Q';
        for (i, &b) in bytes[2..].iter().enumerate() {
            if i + 1 < 6 {
                c6[i + 1] = b;
            }
        }
    } else if length > 2 && is_digit(bytes[2]) && length <= 6 {
        // AB0XYZ
        for (i, &b) in bytes.iter().enumerate() {
            if i < 6 {
                c6[i] = b;
            }
        }
    } else if length > 1 && is_digit(bytes[1]) && length <= 5 {
        // A0XYZ -> " A0XYZ"
        for (i, &b) in bytes.iter().enumerate() {
            if i + 1 < 6 {
                c6[i + 1] = b;
            }
        }
    } else {
        return None;
    }

    // Check for standard callsign encoding
    let i0 = nchar(c6[0] as char, CharTable::AlphanumSpace)?;
    let i1 = nchar(c6[1] as char, CharTable::Alphanum)?;
    let i2 = nchar(c6[2] as char, CharTable::Numeric)?;
    let i3 = nchar(c6[3] as char, CharTable::LettersSpace)?;
    let i4 = nchar(c6[4] as char, CharTable::LettersSpace)?;
    let i5 = nchar(c6[5] as char, CharTable::LettersSpace)?;

    let mut n = i0;
    n = n * 36 + i1;
    n = n * 10 + i2;
    n = n * 27 + i3;
    n = n * 27 + i4;
    n = n * 27 + i5;

    Some(n)
}

// ---------------------------------------------------------------------------
// pack28 / unpack28
// ---------------------------------------------------------------------------

/// Pack a special token, a 22-bit hash code, or a valid base call into a
/// 28-bit integer. Returns `(n28, ip)` on success, or `None` on error.
fn pack28(
    callsign: &str,
    hash_table: Option<&mut CallsignHashTable>,
) -> Option<(i32, u8)> {
    let mut ip: u8 = 0;

    // Check for special tokens
    if callsign == "DE" {
        return Some((0, 0));
    }
    if callsign == "QRZ" {
        return Some((1, 0));
    }
    if callsign == "CQ" {
        return Some((2, 0));
    }

    let length = callsign.len();

    if starts_with(callsign, "CQ ") && length < 8 {
        let v = parse_cq_modifier(callsign)?;
        return Some((3 + v, 0));
    }

    // Detect /R and /P suffix
    let length_base = if ends_with(callsign, "/P") || ends_with(callsign, "/R") {
        ip = 1;
        length - 2
    } else {
        length
    };

    let base = &callsign[..length_base];
    if let Some(n28) = pack_basecall(base) {
        // Standard basecall with optional /P or /R suffix
        save_callsign(hash_table, callsign)?;
        return Some(((NTOKENS + MAX22) as i32 + n28, ip));
    }

    if (3..=11).contains(&length) {
        // Non-standard callsign: compute 22-bit hash
        let (n22, _, _) = save_callsign(hash_table, callsign)?;
        ip = 0;
        return Some(((NTOKENS + n22) as i32, ip));
    }

    None
}

/// Unpack a callsign from a 28-bit field plus ip and i3 bits.
/// Returns `(callsign_string, field_type)` on success.
fn unpack28(
    n28: u32,
    ip: u8,
    i3: u8,
    hash_table: Option<&mut CallsignHashTable>,
) -> Option<(String, FtxFieldType)> {
    // Check for special tokens: DE, QRZ, CQ, CQ nnn, CQ a[bcd]
    if n28 < NTOKENS {
        if n28 <= 2 {
            let s = match n28 {
                0 => "DE",
                1 => "QRZ",
                _ => "CQ",
            };
            return Some((s.to_string(), FtxFieldType::Token));
        }
        if n28 <= 1002 {
            // CQ nnn with 3 digits
            let num = int_to_dd((n28 - 3) as i32, 3, false);
            return Some((format!("CQ {}", num), FtxFieldType::TokenWithArg));
        }
        if n28 <= 532443 {
            // CQ ABCD with up to 4 alphanumeric symbols
            let mut n = n28 - 1003;
            let mut aaaa = [b' '; 4];
            for i in (0..4).rev() {
                aaaa[i] = charn((n % 27) as i32, CharTable::LettersSpace) as u8;
                n /= 27;
            }
            let s: String = aaaa.iter().map(|&b| b as char).collect();
            let trimmed = trim_front(&s, ' ');
            return Some((format!("CQ {}", trimmed), FtxFieldType::TokenWithArg));
        }
        // unspecified
        return None;
    }

    let n28_adj = n28 - NTOKENS;
    if n28_adj < MAX22 {
        // 22-bit hashed callsign
        let call = lookup_callsign(
            hash_table.as_deref(),
            HashType::Hash22Bits,
            n28_adj,
        );
        return Some((call, FtxFieldType::Call));
    }

    // Standard callsign
    let mut n = n28_adj - MAX22;

    let mut callsign = [0u8; 7];
    callsign[6] = 0;
    callsign[5] = charn((n % 27) as i32, CharTable::LettersSpace) as u8;
    n /= 27;
    callsign[4] = charn((n % 27) as i32, CharTable::LettersSpace) as u8;
    n /= 27;
    callsign[3] = charn((n % 27) as i32, CharTable::LettersSpace) as u8;
    n /= 27;
    callsign[2] = charn((n % 10) as i32, CharTable::Numeric) as u8;
    n /= 10;
    callsign[1] = charn((n % 36) as i32, CharTable::Alphanum) as u8;
    n /= 36;
    callsign[0] = charn((n % 37) as i32, CharTable::AlphanumSpace) as u8;

    let raw: String = callsign[..6].iter().map(|&b| b as char).collect();

    let result = if raw.starts_with("3D0") && raw.len() > 3 && !is_space(raw.as_bytes()[3]) {
        // Swaziland prefix: 3D0XYZ -> 3DA0XYZ
        let suffix = raw[3..].trim();
        format!("3DA0{}", suffix)
    } else if raw.starts_with('Q') && raw.len() > 1 && is_letter(raw.as_bytes()[1]) {
        // Guinea prefix: QA0XYZ -> 3XA0XYZ
        let suffix = raw[1..].trim();
        format!("3X{}", suffix)
    } else {
        raw.trim().to_string()
    };

    if result.len() < 3 {
        return None; // callsign too short
    }

    // Append /R or /P suffix based on ip and i3
    let result = if ip != 0 {
        match i3 {
            1 => format!("{}/R", result),
            2 => format!("{}/P", result),
            _ => return None,
        }
    } else {
        result
    };

    // Save to hash table
    if let Some(ht) = hash_table {
        let _ = save_callsign(Some(ht), &result);
    }

    Some((result, FtxFieldType::Call))
}

// ---------------------------------------------------------------------------
// pack58 / unpack58
// ---------------------------------------------------------------------------

/// Pack a non-standard callsign into a 58-bit integer.
fn pack58(
    hash_table: Option<&mut CallsignHashTable>,
    callsign: &str,
) -> Option<u64> {
    let src = callsign.trim_start_matches('<').trim_end_matches('>');

    let mut result: u64 = 0;
    let mut c11 = String::with_capacity(12);
    let mut length = 0;

    for ch in src.chars() {
        if ch == '<' || length >= 11 {
            break;
        }
        c11.push(ch);
        let j = nchar(ch, CharTable::AlphanumSpaceSlash)?;
        result = result * 38 + j as u64;
        length += 1;
    }

    save_callsign(hash_table, &c11)?;

    Some(result)
}

/// Unpack a non-standard callsign from a 58-bit integer.
fn unpack58(
    n58: u64,
    hash_table: Option<&mut CallsignHashTable>,
) -> Option<String> {
    let mut c11 = [0u8; 11];
    let mut n = n58;

    for i in (0..11).rev() {
        c11[i] = charn((n % 38) as i32, CharTable::AlphanumSpaceSlash) as u8;
        n /= 38;
    }

    let raw: String = c11.iter().map(|&b| b as char).collect();
    let callsign = raw.trim().to_string();

    if callsign.len() >= 3 {
        let _ = save_callsign(hash_table, &callsign);
        Some(callsign)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// packgrid / unpackgrid
// ---------------------------------------------------------------------------

/// Pack a grid locator or signal report into a 16-bit value.
fn packgrid(grid4: &str) -> u16 {
    if grid4.is_empty() {
        return MAXGRID4 + 1;
    }

    // Special cases
    if grid4 == "RRR" {
        return MAXGRID4 + 2;
    }
    if grid4 == "RR73" {
        return MAXGRID4 + 3;
    }
    if grid4 == "73" {
        return MAXGRID4 + 4;
    }

    let bytes = grid4.as_bytes();

    // Check for standard 4-letter grid
    if bytes.len() >= 4
        && bytes[0] >= b'A'
        && bytes[0] <= b'R'
        && bytes[1] >= b'A'
        && bytes[1] <= b'R'
        && bytes[2].is_ascii_digit()
        && bytes[3].is_ascii_digit()
    {
        let mut igrid4: u16 = (bytes[0] - b'A') as u16;
        igrid4 = igrid4 * 18 + (bytes[1] - b'A') as u16;
        igrid4 = igrid4 * 10 + (bytes[2] - b'0') as u16;
        igrid4 = igrid4 * 10 + (bytes[3] - b'0') as u16;
        return igrid4;
    }

    // Parse report: +dd / -dd / R+dd / R-dd
    if bytes[0] == b'R' {
        let dd = dd_to_int(&grid4[1..]);
        let irpt = (35 + dd) as u16;
        // ir = 1
        (MAXGRID4 + irpt) | 0x8000
    } else {
        let dd = dd_to_int(grid4);
        let irpt = (35 + dd) as u16;
        // ir = 0
        MAXGRID4 + irpt
    }
}

/// Unpack a grid locator or signal report from a 16-bit value.
/// Returns `(extra_string, field_type)`.
fn unpackgrid(igrid4: u16, ir: u8) -> Option<(String, FtxFieldType)> {
    if igrid4 <= MAXGRID4 {
        // Standard 4-symbol grid locator
        let mut n = igrid4;
        let d3 = (n % 10) as u8;
        n /= 10;
        let d2 = (n % 10) as u8;
        n /= 10;
        let l1 = (n % 18) as u8;
        n /= 18;
        let l0 = (n % 18) as u8;

        let grid = format!(
            "{}{}{}{}",
            (b'A' + l0) as char,
            (b'A' + l1) as char,
            (b'0' + d2) as char,
            (b'0' + d3) as char,
        );

        let result = if ir > 0 {
            format!("R {}", grid)
        } else {
            grid
        };

        Some((result, FtxFieldType::Grid))
    } else {
        let irpt = (igrid4 - MAXGRID4) as i32;
        match irpt {
            1 => Some((String::new(), FtxFieldType::None)),
            2 => Some(("RRR".to_string(), FtxFieldType::Token)),
            3 => Some(("RR73".to_string(), FtxFieldType::Token)),
            4 => Some(("73".to_string(), FtxFieldType::Token)),
            _ => {
                // Signal report as +dd or -dd, optionally with R prefix
                let dd = irpt - 35;
                let dd_str = int_to_dd(dd, 2, true);
                let result = if ir > 0 {
                    format!("R{}", dd_str)
                } else {
                    dd_str
                };
                Some((result, FtxFieldType::Rst))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Encode functions
// ---------------------------------------------------------------------------

/// Encode a text message, guessing which message type to use.
///
/// Tries standard encoding first, then non-standard, then free text.
pub fn ftx_message_encode(
    msg: &mut FtxMessage,
    hash_table: &mut CallsignHashTable,
    message_text: &str,
) -> FtxMessageRc {
    let mut call_to: String;
    let mut parse_pos = message_text;
    let is_cq = starts_with(message_text, "CQ ");

    if is_cq {
        parse_pos = &parse_pos[3..];

        // Check for CQ modifier (CQ nnn or CQ abcd)
        let cq_modifier_v = parse_cq_modifier(message_text);
        if cq_modifier_v.is_some() {
            // Treat "CQ xxx" as a single token
            call_to = "CQ ".to_string();
            let (rest, token) = copy_token(parse_pos);
            call_to.push_str(&token);
            parse_pos = rest;
        } else {
            call_to = "CQ".to_string();
        }
    } else {
        let (rest, token) = copy_token(parse_pos);
        call_to = token;
        parse_pos = rest;
    }

    let (rest, token) = copy_token(parse_pos);
    let call_de: String = token;
    parse_pos = rest;

    let (rest, token) = copy_token(parse_pos);
    let extra: String = token;
    parse_pos = rest;

    // Check token lengths
    if call_to.len() > 11 {
        return FtxMessageRc::ErrorCallsign1;
    }
    if call_de.len() > 11 {
        return FtxMessageRc::ErrorCallsign2;
    }
    if extra.len() > 19 {
        return FtxMessageRc::ErrorGrid;
    }

    if parse_pos.is_empty() {
        // Up to 3 tokens with no leftovers
        let rc = ftx_message_encode_std(msg, hash_table, &call_to, &call_de, &extra);
        if rc == FtxMessageRc::Ok {
            return rc;
        }
        let rc = ftx_message_encode_nonstd(msg, hash_table, &call_to, &call_de, &extra);
        if rc == FtxMessageRc::Ok {
            return rc;
        }
    }

    ftx_message_encode_free(msg, message_text)
}

/// Encode a standard (type 1 or 2) message.
pub fn ftx_message_encode_std(
    msg: &mut FtxMessage,
    hash_table: &mut CallsignHashTable,
    call_to: &str,
    call_de: &str,
    extra: &str,
) -> FtxMessageRc {
    let (n28a, ipa) = match pack28(call_to, Some(hash_table)) {
        Some(v) => v,
        None => return FtxMessageRc::ErrorCallsign1,
    };
    if n28a < 0 {
        return FtxMessageRc::ErrorCallsign1;
    }

    let (n28b, ipb) = match pack28(call_de, Some(hash_table)) {
        Some(v) => v,
        None => return FtxMessageRc::ErrorCallsign2,
    };
    if n28b < 0 {
        return FtxMessageRc::ErrorCallsign2;
    }

    let mut i3: u8 = 1;
    if ends_with(call_to, "/P") || ends_with(call_de, "/P") {
        i3 = 2;
        if ends_with(call_to, "/R") || ends_with(call_de, "/R") {
            return FtxMessageRc::ErrorSuffix;
        }
    }

    let icq = call_to == "CQ" || starts_with(call_to, "CQ ");
    if let Some(slash_pos) = call_de.find('/') {
        if slash_pos >= 2
            && icq
            && !(call_de.ends_with("/P") || call_de.ends_with("/R"))
        {
            return FtxMessageRc::ErrorCallsign2;
        }
    }

    let igrid4 = packgrid(extra);

    // Shift in ipa and ipb bits
    let mut n29a = ((n28a as u32) << 1) | ipa as u32;
    let n29b = ((n28b as u32) << 1) | ipb as u32;

    if ends_with(call_to, "/R") {
        n29a |= 1;
    } else if ends_with(call_to, "/P") {
        n29a |= 1;
        i3 = 2;
    }

    // Pack into (28+1) + (28+1) + (1+15) + 3 bits
    msg.payload[0] = (n29a >> 21) as u8;
    msg.payload[1] = (n29a >> 13) as u8;
    msg.payload[2] = (n29a >> 5) as u8;
    msg.payload[3] = ((n29a << 3) as u8) | ((n29b >> 26) as u8);
    msg.payload[4] = (n29b >> 18) as u8;
    msg.payload[5] = (n29b >> 10) as u8;
    msg.payload[6] = (n29b >> 2) as u8;
    msg.payload[7] = ((n29b << 6) as u8) | ((igrid4 >> 10) as u8);
    msg.payload[8] = (igrid4 >> 2) as u8;
    msg.payload[9] = ((igrid4 << 6) as u8) | (i3 << 3);

    FtxMessageRc::Ok
}

/// Encode a non-standard (type 4) message.
pub fn ftx_message_encode_nonstd(
    msg: &mut FtxMessage,
    hash_table: &mut CallsignHashTable,
    call_to: &str,
    call_de: &str,
    extra: &str,
) -> FtxMessageRc {
    let i3: u8 = 4;

    let icq: u8 = if call_to == "CQ" || starts_with(call_to, "CQ ") {
        1
    } else {
        0
    };

    if icq == 0 && call_to.len() < 3 {
        return FtxMessageRc::ErrorCallsign1;
    }
    if call_de.len() < 3 {
        return FtxMessageRc::ErrorCallsign2;
    }

    let iflip: u8;
    let n12: u16;
    let call58: &str;

    if icq == 0 {
        // Choose which callsign to encode as plain-text (58 bits) or hash (12 bits)
        iflip = if call_de.starts_with('<') && call_de.ends_with('>') {
            1
        } else {
            0
        };

        let call12 = if iflip == 0 { call_to } else { call_de };
        call58 = if iflip == 0 { call_de } else { call_to };

        match save_callsign(Some(hash_table), call12) {
            Some((_, n12_val, _)) => n12 = n12_val,
            None => return FtxMessageRc::ErrorCallsign1,
        }
    } else {
        iflip = 0;
        n12 = 0;
        call58 = call_de;
    }

    let n58 = match pack58(Some(hash_table), call58) {
        Some(v) => v,
        None => return FtxMessageRc::ErrorCallsign2,
    };

    let nrpt: u8 = if icq != 0 {
        0
    } else if extra == "RRR" {
        1
    } else if extra == "RR73" {
        2
    } else if extra == "73" {
        3
    } else {
        0
    };

    // Pack into 12 + 58 + 1 + 2 + 1 + 3 == 77 bits
    msg.payload[0] = (n12 >> 4) as u8;
    msg.payload[1] = ((n12 << 4) as u8) | ((n58 >> 54) as u8);
    msg.payload[2] = (n58 >> 46) as u8;
    msg.payload[3] = (n58 >> 38) as u8;
    msg.payload[4] = (n58 >> 30) as u8;
    msg.payload[5] = (n58 >> 22) as u8;
    msg.payload[6] = (n58 >> 14) as u8;
    msg.payload[7] = (n58 >> 6) as u8;
    msg.payload[8] = ((n58 << 2) as u8) | (iflip << 1) | (nrpt >> 1);
    msg.payload[9] = (nrpt << 7) | (icq << 6) | (i3 << 3);

    FtxMessageRc::Ok
}

/// Encode a free text message (up to 13 characters).
pub fn ftx_message_encode_free(msg: &mut FtxMessage, text: &str) -> FtxMessageRc {
    let str_len = text.len();
    if str_len > 13 {
        return FtxMessageRc::ErrorType;
    }

    let mut b71 = [0u8; 9];

    for idx in 0..13 {
        let c = if idx < str_len {
            text.as_bytes()[idx] as char
        } else {
            ' '
        };

        let cid = match nchar(c, CharTable::Full) {
            Some(v) => v,
            None => return FtxMessageRc::ErrorType,
        };

        let mut rem = cid as u16;
        for i in (0..9).rev() {
            rem += b71[i] as u16 * 42;
            b71[i] = (rem & 0xff) as u8;
            rem >>= 8;
        }
    }

    let rc = ftx_message_encode_telemetry(msg, &b71);
    msg.payload[9] = 0; // i3.n3 = 0.0
    rc
}

/// Encode telemetry data (71 bits in 9 bytes).
pub fn ftx_message_encode_telemetry(msg: &mut FtxMessage, telemetry: &[u8]) -> FtxMessageRc {
    // Shift bits in telemetry left by 1 bit
    let mut carry: u8 = 0;
    for i in (0..9).rev() {
        msg.payload[i] = (telemetry[i] << 1) | (carry >> 7);
        carry = telemetry[i] & 0x80;
    }
    FtxMessageRc::Ok
}

// ---------------------------------------------------------------------------
// Decode functions
// ---------------------------------------------------------------------------

/// Decode an FTx message into a human-readable string.
///
/// Returns `(message_string, offsets, result_code)`.
pub fn ftx_message_decode(
    msg: &FtxMessage,
    hash_table: &mut CallsignHashTable,
) -> (String, FtxMessageOffsets, FtxMessageRc) {
    let mut offsets = FtxMessageOffsets::default();
    let msg_type = msg.get_type();

    let (field1, field2, field3, rc) = match msg_type {
        FtxMessageType::Standard => {
            match ftx_message_decode_std(msg, hash_table) {
                (Some(f1), Some(f2), Some(f3), types, rc) => {
                    offsets.types = types;
                    (Some(f1), Some(f2), Some(f3), rc)
                }
                (f1, f2, f3, types, rc) => {
                    offsets.types = types;
                    (f1, f2, f3, rc)
                }
            }
        }
        FtxMessageType::NonstdCall => {
            match ftx_message_decode_nonstd(msg, hash_table) {
                (Some(f1), Some(f2), Some(f3), types, rc) => {
                    offsets.types = types;
                    (Some(f1), Some(f2), Some(f3), rc)
                }
                (f1, f2, f3, types, rc) => {
                    offsets.types = types;
                    (f1, f2, f3, rc)
                }
            }
        }
        FtxMessageType::FreeText => {
            let text = ftx_message_decode_free(msg);
            (Some(text), None, None, FtxMessageRc::Ok)
        }
        FtxMessageType::Telemetry => {
            let hex = ftx_message_decode_telemetry_hex(msg);
            (Some(hex), None, None, FtxMessageRc::Ok)
        }
        _ => (None, None, None, FtxMessageRc::ErrorType),
    };

    // Build the message string
    let mut message = String::new();
    if let Some(ref f1) = field1 {
        offsets.offsets[0] = 0;
        message.push_str(f1);
        if let Some(ref f2) = field2 {
            message.push(' ');
            offsets.offsets[1] = message.len() as i16;
            message.push_str(f2);
            if let Some(ref f3) = field3 {
                if !f3.is_empty() {
                    message.push(' ');
                    offsets.offsets[2] = message.len() as i16;
                    message.push_str(f3);
                }
            }
        }
    }

    (message, offsets, rc)
}

/// Decode a standard (type 1 or 2) message.
///
/// Returns `(call_to, call_de, extra, field_types, result_code)`.
pub fn ftx_message_decode_std(
    msg: &FtxMessage,
    hash_table: &mut CallsignHashTable,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    [FtxFieldType; FTX_MAX_MESSAGE_FIELDS],
    FtxMessageRc,
) {
    let mut field_types = [FtxFieldType::Unknown; FTX_MAX_MESSAGE_FIELDS];

    // Extract packed fields
    let mut n29a: u32 = (msg.payload[0] as u32) << 21;
    n29a |= (msg.payload[1] as u32) << 13;
    n29a |= (msg.payload[2] as u32) << 5;
    n29a |= (msg.payload[3] as u32) >> 3;

    let mut n29b: u32 = ((msg.payload[3] & 0x07) as u32) << 26;
    n29b |= (msg.payload[4] as u32) << 18;
    n29b |= (msg.payload[5] as u32) << 10;
    n29b |= (msg.payload[6] as u32) << 2;
    n29b |= (msg.payload[7] as u32) >> 6;

    let ir = (msg.payload[7] & 0x20) >> 5;

    let mut igrid4: u16 = ((msg.payload[7] & 0x1F) as u16) << 10;
    igrid4 |= (msg.payload[8] as u16) << 2;
    igrid4 |= (msg.payload[9] as u16) >> 6;

    let i3 = (msg.payload[9] >> 3) & 0x07;

    // Unpack callsigns
    let (call_to, ft0) = match unpack28(n29a >> 1, (n29a & 1) as u8, i3, Some(hash_table)) {
        Some(v) => v,
        None => return (None, None, None, field_types, FtxMessageRc::ErrorCallsign1),
    };
    field_types[0] = ft0;

    let (call_de, ft1) = match unpack28(n29b >> 1, (n29b & 1) as u8, i3, Some(hash_table)) {
        Some(v) => v,
        None => return (Some(call_to), None, None, field_types, FtxMessageRc::ErrorCallsign2),
    };
    field_types[1] = ft1;

    let (extra, ft2) = match unpackgrid(igrid4, ir) {
        Some(v) => v,
        None => {
            return (
                Some(call_to),
                Some(call_de),
                None,
                field_types,
                FtxMessageRc::ErrorGrid,
            )
        }
    };
    field_types[2] = ft2;

    (
        Some(call_to),
        Some(call_de),
        Some(extra),
        field_types,
        FtxMessageRc::Ok,
    )
}

/// Decode a non-standard (type 4) message.
///
/// Returns `(call_to, call_de, extra, field_types, result_code)`.
pub fn ftx_message_decode_nonstd(
    msg: &FtxMessage,
    hash_table: &mut CallsignHashTable,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    [FtxFieldType; FTX_MAX_MESSAGE_FIELDS],
    FtxMessageRc,
) {
    let mut field_types = [FtxFieldType::Unknown; FTX_MAX_MESSAGE_FIELDS];

    let mut n12: u16 = (msg.payload[0] as u16) << 4;
    n12 |= (msg.payload[1] as u16) >> 4;

    let mut n58: u64 = ((msg.payload[1] & 0x0F) as u64) << 54;
    n58 |= (msg.payload[2] as u64) << 46;
    n58 |= (msg.payload[3] as u64) << 38;
    n58 |= (msg.payload[4] as u64) << 30;
    n58 |= (msg.payload[5] as u64) << 22;
    n58 |= (msg.payload[6] as u64) << 14;
    n58 |= (msg.payload[7] as u64) << 6;
    n58 |= (msg.payload[8] as u64) >> 2;

    let iflip = (msg.payload[8] >> 1) & 0x01;
    let mut nrpt: u16 = ((msg.payload[8] & 0x01) as u16) << 1;
    nrpt |= (msg.payload[9] >> 7) as u16;
    let icq = (msg.payload[9] >> 6) & 0x01;

    // Decode one call from 58-bit encoded string
    let call_decoded = unpack58(n58, Some(hash_table)).unwrap_or_else(|| "<...>".to_string());

    // Decode the other call from hash lookup table
    let call_3 = lookup_callsign(Some(hash_table), HashType::Hash12Bits, n12 as u32);

    // Possibly flip them
    let (call_1, call_2) = if iflip != 0 {
        (call_decoded.clone(), call_3)
    } else {
        (call_3, call_decoded.clone())
    };

    let call_to;
    let call_de;
    let extra;

    if icq == 0 {
        call_to = call_1;
        field_types[0] = FtxFieldType::Call;
        call_de = call_2;

        extra = match nrpt {
            1 => {
                field_types[2] = FtxFieldType::Token;
                "RRR".to_string()
            }
            2 => {
                field_types[2] = FtxFieldType::Token;
                "RR73".to_string()
            }
            3 => {
                field_types[2] = FtxFieldType::Token;
                "73".to_string()
            }
            _ => {
                field_types[2] = FtxFieldType::None;
                String::new()
            }
        };
    } else {
        call_to = "CQ".to_string();
        field_types[0] = FtxFieldType::Token;
        call_de = call_2;
        extra = String::new();
        field_types[2] = FtxFieldType::None;
    }
    field_types[1] = FtxFieldType::Call;

    (
        Some(call_to),
        Some(call_de),
        Some(extra),
        field_types,
        FtxMessageRc::Ok,
    )
}

/// Decode a free text message.
pub fn ftx_message_decode_free(msg: &FtxMessage) -> String {
    let mut b71 = ftx_message_decode_telemetry(msg);

    let mut c14 = [b' '; 13];
    for idx in (0..13).rev() {
        // Divide the long integer in b71 by 42
        let mut rem: u16 = 0;
        for i in 0..9 {
            rem = (rem << 8) | b71[i] as u16;
            b71[i] = (rem / 42) as u8;
            rem %= 42;
        }
        c14[idx] = charn(rem as i32, CharTable::Full) as u8;
    }

    let s: String = c14.iter().map(|&b| b as char).collect();
    s.trim().to_string()
}

/// Decode telemetry data as a hex string.
pub fn ftx_message_decode_telemetry_hex(msg: &FtxMessage) -> String {
    let b71 = ftx_message_decode_telemetry(msg);

    let mut hex = String::with_capacity(18);
    for &byte in &b71 {
        hex.push_str(&format!("{:02X}", byte));
    }
    hex
}

/// Decode telemetry data (71 bits in 9 bytes).
pub fn ftx_message_decode_telemetry(msg: &FtxMessage) -> [u8; 9] {
    let mut telemetry = [0u8; 9];
    let mut carry: u8 = 0;
    for i in 0..9 {
        telemetry[i] = (carry << 7) | (msg.payload[i] >> 1);
        carry = msg.payload[i] & 0x01;
    }
    telemetry
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_basecall_standard() {
        // Standard 6-char callsign
        let n = pack_basecall("W1AW");
        assert!(n.is_some());
        assert!(n.unwrap() >= 0);
    }

    #[test]
    fn test_pack_basecall_short() {
        // Too short
        assert!(pack_basecall("AB").is_none());
    }

    #[test]
    fn test_pack_basecall_3da0() {
        // Swaziland prefix
        let n = pack_basecall("3DA0XYZ");
        assert!(n.is_some());
    }

    #[test]
    fn test_pack_basecall_3x() {
        // Guinea prefix
        let n = pack_basecall("3XA0XY");
        assert!(n.is_some());
    }

    #[test]
    fn test_packgrid_round_trip() {
        let grid = "FN42";
        let packed = packgrid(grid);
        assert!(packed <= MAXGRID4);
        let (unpacked, ft) = unpackgrid(packed, 0).unwrap();
        assert_eq!(unpacked, grid);
        assert_eq!(ft, FtxFieldType::Grid);
    }

    #[test]
    fn test_packgrid_special_tokens() {
        assert_eq!(packgrid("RRR"), MAXGRID4 + 2);
        assert_eq!(packgrid("RR73"), MAXGRID4 + 3);
        assert_eq!(packgrid("73"), MAXGRID4 + 4);

        let (s, _) = unpackgrid(MAXGRID4 + 2, 0).unwrap();
        assert_eq!(s, "RRR");
        let (s, _) = unpackgrid(MAXGRID4 + 3, 0).unwrap();
        assert_eq!(s, "RR73");
        let (s, _) = unpackgrid(MAXGRID4 + 4, 0).unwrap();
        assert_eq!(s, "73");
    }

    #[test]
    fn test_packgrid_empty() {
        let packed = packgrid("");
        let (s, ft) = unpackgrid(packed, 0).unwrap();
        assert_eq!(s, "");
        assert_eq!(ft, FtxFieldType::None);
    }

    #[test]
    fn test_packgrid_report() {
        let packed = packgrid("+05");
        let (unpacked, ft) = unpackgrid(packed, 0).unwrap();
        assert_eq!(unpacked, "+05");
        assert_eq!(ft, FtxFieldType::Rst);
    }

    #[test]
    fn test_packgrid_report_with_r() {
        let packed = packgrid("R-10");
        assert!(packed & 0x8000 != 0);
        let igrid4 = packed & 0x7FFF;
        let (unpacked, ft) = unpackgrid(igrid4, 1).unwrap();
        assert_eq!(unpacked, "R-10");
        assert_eq!(ft, FtxFieldType::Rst);
    }

    #[test]
    fn test_packgrid_grid_with_r_prefix() {
        let grid = "FN42";
        let packed = packgrid(grid);
        let (unpacked, ft) = unpackgrid(packed, 1).unwrap();
        assert_eq!(unpacked, "R FN42");
        assert_eq!(ft, FtxFieldType::Grid);
    }

    #[test]
    fn test_encode_decode_std_cq() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_std(&mut msg, &mut ht, "CQ", "W1AW", "FN31");
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(msg.get_type(), FtxMessageType::Standard);

        let (text, offsets, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(text, "CQ W1AW FN31");
        assert_eq!(offsets.types[0], FtxFieldType::Token);
        assert_eq!(offsets.types[1], FtxFieldType::Call);
        assert_eq!(offsets.types[2], FtxFieldType::Grid);
    }

    #[test]
    fn test_encode_decode_std_call_to_call() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_std(&mut msg, &mut ht, "K1ABC", "W9XYZ", "-15");
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(msg.get_type(), FtxMessageType::Standard);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(text, "K1ABC W9XYZ -15");
    }

    #[test]
    fn test_encode_decode_std_rrr() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_std(&mut msg, &mut ht, "K1ABC", "W9XYZ", "RRR");
        assert_eq!(rc, FtxMessageRc::Ok);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(text, "K1ABC W9XYZ RRR");
    }

    #[test]
    fn test_encode_decode_std_rr73() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_std(&mut msg, &mut ht, "K1ABC", "W9XYZ", "RR73");
        assert_eq!(rc, FtxMessageRc::Ok);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(text, "K1ABC W9XYZ RR73");
    }

    #[test]
    fn test_encode_decode_std_73() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_std(&mut msg, &mut ht, "K1ABC", "W9XYZ", "73");
        assert_eq!(rc, FtxMessageRc::Ok);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(text, "K1ABC W9XYZ 73");
    }

    #[test]
    fn test_encode_decode_std_de() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_std(&mut msg, &mut ht, "DE", "W1AW", "FN31");
        assert_eq!(rc, FtxMessageRc::Ok);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(text, "DE W1AW FN31");
    }

    #[test]
    fn test_encode_decode_std_qrz() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_std(&mut msg, &mut ht, "QRZ", "W1AW", "");
        assert_eq!(rc, FtxMessageRc::Ok);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert!(text.starts_with("QRZ W1AW"));
    }

    #[test]
    fn test_encode_decode_std_cq_nnn() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_std(&mut msg, &mut ht, "CQ 123", "W1AW", "FN31");
        assert_eq!(rc, FtxMessageRc::Ok);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(text, "CQ 123 W1AW FN31");
    }

    #[test]
    fn test_encode_decode_std_cq_dx() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_std(&mut msg, &mut ht, "CQ DX", "W1AW", "FN31");
        assert_eq!(rc, FtxMessageRc::Ok);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(text, "CQ DX W1AW FN31");
    }

    #[test]
    fn test_encode_decode_free_text() {
        let mut msg = FtxMessage::new();

        let rc = ftx_message_encode_free(&mut msg, "HELLO WORLD");
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(msg.get_type(), FtxMessageType::FreeText);

        let text = ftx_message_decode_free(&msg);
        assert_eq!(text, "HELLO WORLD");
    }

    #[test]
    fn test_encode_decode_free_text_full() {
        let mut msg = FtxMessage::new();

        let rc = ftx_message_encode_free(&mut msg, "0123456789ABC");
        assert_eq!(rc, FtxMessageRc::Ok);

        let text = ftx_message_decode_free(&msg);
        assert_eq!(text, "0123456789ABC");
    }

    #[test]
    fn test_encode_free_text_too_long() {
        let mut msg = FtxMessage::new();
        let rc = ftx_message_encode_free(&mut msg, "THIS IS TOO LONG");
        assert_eq!(rc, FtxMessageRc::ErrorType);
    }

    #[test]
    fn test_encode_decode_telemetry() {
        let mut msg = FtxMessage::new();
        let telemetry: [u8; 9] = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x12];

        let rc = ftx_message_encode_telemetry(&mut msg, &telemetry);
        assert_eq!(rc, FtxMessageRc::Ok);

        let decoded = ftx_message_decode_telemetry(&msg);
        assert_eq!(decoded, telemetry);
    }

    #[test]
    fn test_telemetry_hex_round_trip() {
        let mut msg = FtxMessage::new();
        let telemetry: [u8; 9] = [0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0x0A];

        ftx_message_encode_telemetry(&mut msg, &telemetry);
        // Set i3.n3 to 0.5 for telemetry type
        msg.payload[9] = (msg.payload[9] & 0xC0) | (5 << 3); // n3=5 requires special handling
        // Actually, telemetry is i3=0 n3=5: need bits 71..73 = 5, bits 74..76 = 0
        // n3 is in bits 71..73: payload[8] bit0 -> n3 bit2, payload[9] bits 7..6 -> n3 bits 1..0
        // i3 is in bits 74..76: payload[9] bits 5..3
        // For i3=0, n3=5 (binary 101): bit2=1, bit1=0, bit0=1
        msg.payload[8] = (msg.payload[8] & 0xFE) | 1; // n3 bit2 = 1
        msg.payload[9] = 0b01 << 6; // n3 bits 1..0 = 01, i3 = 0

        assert_eq!(msg.get_type(), FtxMessageType::Telemetry);

        let hex = ftx_message_decode_telemetry_hex(&msg);
        assert_eq!(hex.len(), 18);
    }

    #[test]
    fn test_encode_decode_nonstd() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_nonstd(&mut msg, &mut ht, "K1ABC", "PJ4/W9XYZ", "RR73");
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(msg.get_type(), FtxMessageType::NonstdCall);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert!(text.contains("PJ4/W9XYZ"));
        assert!(text.contains("RR73"));
    }

    #[test]
    fn test_encode_decode_nonstd_cq() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_nonstd(&mut msg, &mut ht, "CQ", "PJ4/W9XYZ", "");
        assert_eq!(rc, FtxMessageRc::Ok);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert!(text.starts_with("CQ "));
        assert!(text.contains("PJ4/W9XYZ"));
    }

    #[test]
    fn test_message_type_i3_n3() {
        let mut msg = FtxMessage::new();

        // Standard (i3=1)
        msg.payload[9] = 1 << 3;
        assert_eq!(msg.get_i3(), 1);
        assert_eq!(msg.get_type(), FtxMessageType::Standard);

        // Standard (i3=2)
        msg.payload[9] = 2 << 3;
        assert_eq!(msg.get_i3(), 2);
        assert_eq!(msg.get_type(), FtxMessageType::Standard);

        // Nonstd (i3=4)
        msg.payload[9] = 4 << 3;
        assert_eq!(msg.get_i3(), 4);
        assert_eq!(msg.get_type(), FtxMessageType::NonstdCall);

        // Free text (i3=0, n3=0)
        msg.payload[8] = 0;
        msg.payload[9] = 0;
        assert_eq!(msg.get_i3(), 0);
        assert_eq!(msg.get_n3(), 0);
        assert_eq!(msg.get_type(), FtxMessageType::FreeText);
    }

    #[test]
    fn test_pack28_special_tokens() {
        let mut ht = CallsignHashTable::new();

        let (n, ip) = pack28("DE", Some(&mut ht)).unwrap();
        assert_eq!(n, 0);
        assert_eq!(ip, 0);

        let (n, ip) = pack28("QRZ", Some(&mut ht)).unwrap();
        assert_eq!(n, 1);
        assert_eq!(ip, 0);

        let (n, ip) = pack28("CQ", Some(&mut ht)).unwrap();
        assert_eq!(n, 2);
        assert_eq!(ip, 0);
    }

    #[test]
    fn test_pack28_unpack28_standard() {
        let mut ht = CallsignHashTable::new();

        let (n28, ip) = pack28("W1AW", Some(&mut ht)).unwrap();
        assert!(n28 >= (NTOKENS + MAX22) as i32);

        let (call, ft) = unpack28(n28 as u32, ip, 1, Some(&mut ht)).unwrap();
        assert_eq!(call, "W1AW");
        assert_eq!(ft, FtxFieldType::Call);
    }

    #[test]
    fn test_pack28_unpack28_suffix_r() {
        let mut ht = CallsignHashTable::new();

        let (n28, ip) = pack28("W1AW/R", Some(&mut ht)).unwrap();
        assert_eq!(ip, 1);

        let (call, ft) = unpack28(n28 as u32, ip, 1, Some(&mut ht)).unwrap();
        assert_eq!(call, "W1AW/R");
        assert_eq!(ft, FtxFieldType::Call);
    }

    #[test]
    fn test_pack28_unpack28_suffix_p() {
        let mut ht = CallsignHashTable::new();

        let (n28, ip) = pack28("W1AW/P", Some(&mut ht)).unwrap();
        assert_eq!(ip, 1);

        let (call, ft) = unpack28(n28 as u32, ip, 2, Some(&mut ht)).unwrap();
        assert_eq!(call, "W1AW/P");
        assert_eq!(ft, FtxFieldType::Call);
    }

    #[test]
    fn test_pack58_unpack58_round_trip() {
        let mut ht = CallsignHashTable::new();

        let n58 = pack58(Some(&mut ht), "PJ4/W9XYZ").unwrap();
        let call = unpack58(n58, Some(&mut ht)).unwrap();
        assert_eq!(call, "PJ4/W9XYZ");
    }

    #[test]
    fn test_parse_cq_modifier_nnn() {
        assert_eq!(parse_cq_modifier("CQ 123"), Some(123));
        assert_eq!(parse_cq_modifier("CQ 000"), Some(0));
        assert_eq!(parse_cq_modifier("CQ 999"), Some(999));
    }

    #[test]
    fn test_parse_cq_modifier_letters() {
        let v = parse_cq_modifier("CQ DX");
        assert!(v.is_some());
        assert!(v.unwrap() >= 1000);
    }

    #[test]
    fn test_parse_cq_modifier_invalid() {
        assert!(parse_cq_modifier("CQ").is_none());
        assert!(parse_cq_modifier("CQ /X").is_none());
    }

    #[test]
    fn test_ftx_message_encode_auto() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        // Should encode as standard
        let rc = ftx_message_encode(&mut msg, &mut ht, "CQ W1AW FN31");
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(msg.get_type(), FtxMessageType::Standard);

        let (text, _, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(text, "CQ W1AW FN31");
    }

    #[test]
    fn test_ftx_message_encode_auto_free_text() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        // Use a string with 4+ tokens so it can't be parsed as std (3 tokens max)
        // and is at most 13 chars for free text encoding.
        let rc = ftx_message_encode(&mut msg, &mut ht, "HI THERE A B");
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(msg.get_type(), FtxMessageType::FreeText);

        let text = ftx_message_decode_free(&msg);
        assert_eq!(text.trim(), "HI THERE A B");
    }

    #[test]
    fn test_encode_decode_std_report_r() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_std(&mut msg, &mut ht, "K1ABC", "W9XYZ", "R-15");
        assert_eq!(rc, FtxMessageRc::Ok);

        let (text, offsets, rc) = ftx_message_decode(&msg, &mut ht);
        assert_eq!(rc, FtxMessageRc::Ok);
        assert_eq!(text, "K1ABC W9XYZ R-15");
        assert_eq!(offsets.types[2], FtxFieldType::Rst);
    }

    #[test]
    fn test_nonstd_short_callsign_rejected() {
        let mut msg = FtxMessage::new();
        let mut ht = CallsignHashTable::new();

        let rc = ftx_message_encode_nonstd(&mut msg, &mut ht, "AB", "CD", "");
        assert_ne!(rc, FtxMessageRc::Ok);
    }

    #[test]
    fn test_message_default() {
        let msg = FtxMessage::default();
        assert_eq!(msg.payload, [0u8; 10]);
        assert_eq!(msg.hash, 0);
    }

    #[test]
    fn test_offsets_default() {
        let off = FtxMessageOffsets::default();
        assert_eq!(off.types, [FtxFieldType::Unknown; 3]);
        assert_eq!(off.offsets, [-1, -1, -1]);
    }
}
