// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Character table lookup and string utility functions for FTx message
//! encoding/decoding.
//!
//! This is a pure Rust port of `ft8_lib/ft8/text.c`.

/// Character table variants used for encoding and decoding FTx messages.
///
/// Each variant defines a different subset of allowed characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharTable {
    /// `" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ+-./?"` (42 entries)
    Full,
    /// `" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ/"` (38 entries)
    AlphanumSpaceSlash,
    /// `" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"` (37 entries)
    AlphanumSpace,
    /// `" ABCDEFGHIJKLMNOPQRSTUVWXYZ"` (27 entries)
    LettersSpace,
    /// `"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"` (36 entries)
    Alphanum,
    /// `"0123456789"` (10 entries)
    Numeric,
}

/// Convert an integer index to an ASCII character according to the given
/// character table.
///
/// Returns `'_'` if the index is out of range (should not happen in normal
/// operation).
pub fn charn(mut c: i32, table: CharTable) -> char {
    // Tables that include a leading space
    if table != CharTable::Alphanum && table != CharTable::Numeric {
        if c == 0 {
            return ' ';
        }
        c -= 1;
    }

    // Digits (unless letters-space table which skips digits)
    if table != CharTable::LettersSpace {
        if c < 10 {
            return char::from(b'0' + c as u8);
        }
        c -= 10;
    }

    // Letters (unless numeric table which has no letters)
    if table != CharTable::Numeric {
        if c < 26 {
            return char::from(b'A' + c as u8);
        }
        c -= 26;
    }

    // Extra symbols
    match table {
        CharTable::Full => {
            const EXTRAS: [char; 5] = ['+', '-', '.', '/', '?'];
            if (c as usize) < EXTRAS.len() {
                return EXTRAS[c as usize];
            }
        }
        CharTable::AlphanumSpaceSlash => {
            if c == 0 {
                return '/';
            }
        }
        _ => {}
    }

    '_' // unknown character — should never get here
}

/// Look up the index of an ASCII character in the given character table.
///
/// Returns `None` if the character is not present in the table (the C version
/// returns -1).
pub fn nchar(c: char, table: CharTable) -> Option<i32> {
    let mut n: i32 = 0;

    // Leading space
    if table != CharTable::Alphanum && table != CharTable::Numeric {
        if c == ' ' {
            return Some(n);
        }
        n += 1;
    }

    // Digits
    if table != CharTable::LettersSpace {
        if c.is_ascii_digit() {
            return Some(n + (c as i32 - '0' as i32));
        }
        n += 10;
    }

    // Letters
    if table != CharTable::Numeric {
        if c.is_ascii_uppercase() {
            return Some(n + (c as i32 - 'A' as i32));
        }
        n += 26;
    }

    // Extra symbols
    match table {
        CharTable::Full => {
            match c {
                '+' => return Some(n),
                '-' => return Some(n + 1),
                '.' => return Some(n + 2),
                '/' => return Some(n + 3),
                '?' => return Some(n + 4),
                _ => {}
            }
        }
        CharTable::AlphanumSpaceSlash => {
            if c == '/' {
                return Some(n);
            }
        }
        _ => {}
    }

    None
}

/// Convert a character to uppercase ASCII. Non-letter characters are returned
/// unchanged.
pub fn to_upper(c: char) -> char {
    if c.is_ascii_lowercase() {
        char::from(c as u8 - b'a' + b'A')
    } else {
        c
    }
}

/// Format an FTx message string:
///   - replaces lowercase letters with uppercase
///   - collapses consecutive spaces into a single space
pub fn fmtmsg(msg_in: &str) -> String {
    let mut out = String::with_capacity(msg_in.len());
    let mut last_out: Option<char> = None;

    for c in msg_in.chars() {
        if c == ' ' && last_out == Some(' ') {
            continue;
        }
        let upper = to_upper(c);
        out.push(upper);
        last_out = Some(upper);
    }

    out
}

/// Parse a signed integer from a string slice.
///
/// Handles optional leading `+` or `-` sign, followed by decimal digits.
/// Stops at the first non-digit character (or end of string).
pub fn dd_to_int(s: &str) -> i32 {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return 0;
    }

    let (negative, start) = match bytes[0] {
        b'-' => (true, 1),
        b'+' => (false, 1),
        _ => (false, 0),
    };

    let mut result: i32 = 0;
    for &b in &bytes[start..] {
        if !b.is_ascii_digit() {
            break;
        }
        result = result * 10 + (b - b'0') as i32;
    }

    if negative {
        -result
    } else {
        result
    }
}

/// Format an integer into a fixed-width decimal string.
///
/// * `value`     – the integer value to format
/// * `width`     – number of digit positions (excluding sign)
/// * `full_sign` – if `true`, a `+` is prepended for non-negative values
pub fn int_to_dd(value: i32, width: usize, full_sign: bool) -> String {
    let mut out = String::with_capacity(width + 1);

    let abs_value = if value < 0 {
        out.push('-');
        (-value) as u32
    } else {
        if full_sign {
            out.push('+');
        }
        value as u32
    };

    if width == 0 {
        return out;
    }

    let mut divisor: u32 = 1;
    for _ in 0..width - 1 {
        divisor *= 10;
    }

    let mut remaining = abs_value;
    while divisor >= 1 {
        let digit = remaining / divisor;
        out.push(char::from(b'0' + digit as u8));
        remaining -= digit * divisor;
        divisor /= 10;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // charn / nchar round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn full_table_round_trip() {
        let expected = " 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ+-./?";
        for (i, ch) in expected.chars().enumerate() {
            assert_eq!(charn(i as i32, CharTable::Full), ch, "charn({i})");
            assert_eq!(
                nchar(ch, CharTable::Full),
                Some(i as i32),
                "nchar('{ch}')"
            );
        }
    }

    #[test]
    fn alphanum_space_slash_round_trip() {
        let expected = " 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ/";
        for (i, ch) in expected.chars().enumerate() {
            assert_eq!(
                charn(i as i32, CharTable::AlphanumSpaceSlash),
                ch,
                "charn({i})"
            );
            assert_eq!(
                nchar(ch, CharTable::AlphanumSpaceSlash),
                Some(i as i32),
                "nchar('{ch}')"
            );
        }
    }

    #[test]
    fn alphanum_space_round_trip() {
        let expected = " 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        for (i, ch) in expected.chars().enumerate() {
            assert_eq!(
                charn(i as i32, CharTable::AlphanumSpace),
                ch,
                "charn({i})"
            );
            assert_eq!(
                nchar(ch, CharTable::AlphanumSpace),
                Some(i as i32),
                "nchar('{ch}')"
            );
        }
    }

    #[test]
    fn letters_space_round_trip() {
        let expected = " ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        for (i, ch) in expected.chars().enumerate() {
            assert_eq!(
                charn(i as i32, CharTable::LettersSpace),
                ch,
                "charn({i})"
            );
            assert_eq!(
                nchar(ch, CharTable::LettersSpace),
                Some(i as i32),
                "nchar('{ch}')"
            );
        }
    }

    #[test]
    fn alphanum_round_trip() {
        let expected = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        for (i, ch) in expected.chars().enumerate() {
            assert_eq!(charn(i as i32, CharTable::Alphanum), ch, "charn({i})");
            assert_eq!(
                nchar(ch, CharTable::Alphanum),
                Some(i as i32),
                "nchar('{ch}')"
            );
        }
    }

    #[test]
    fn numeric_round_trip() {
        let expected = "0123456789";
        for (i, ch) in expected.chars().enumerate() {
            assert_eq!(charn(i as i32, CharTable::Numeric), ch, "charn({i})");
            assert_eq!(
                nchar(ch, CharTable::Numeric),
                Some(i as i32),
                "nchar('{ch}')"
            );
        }
    }

    #[test]
    fn nchar_returns_none_for_unknown() {
        assert_eq!(nchar('!', CharTable::Full), None);
        assert_eq!(nchar('a', CharTable::Full), None); // lowercase not in table
        assert_eq!(nchar(' ', CharTable::Alphanum), None);
        assert_eq!(nchar('A', CharTable::Numeric), None);
        assert_eq!(nchar('0', CharTable::LettersSpace), None);
    }

    #[test]
    fn charn_returns_underscore_for_out_of_range() {
        assert_eq!(charn(42, CharTable::Full), '_');
        assert_eq!(charn(38, CharTable::AlphanumSpaceSlash), '_');
        assert_eq!(charn(10, CharTable::Numeric), '_');
    }

    // -----------------------------------------------------------------------
    // to_upper
    // -----------------------------------------------------------------------

    #[test]
    fn to_upper_converts_lowercase() {
        assert_eq!(to_upper('a'), 'A');
        assert_eq!(to_upper('z'), 'Z');
        assert_eq!(to_upper('m'), 'M');
    }

    #[test]
    fn to_upper_preserves_non_lower() {
        assert_eq!(to_upper('A'), 'A');
        assert_eq!(to_upper('5'), '5');
        assert_eq!(to_upper(' '), ' ');
        assert_eq!(to_upper('/'), '/');
    }

    // -----------------------------------------------------------------------
    // fmtmsg
    // -----------------------------------------------------------------------

    #[test]
    fn fmtmsg_uppercases_and_collapses_spaces() {
        assert_eq!(fmtmsg("cq dx  de  ab1cd"), "CQ DX DE AB1CD");
    }

    #[test]
    fn fmtmsg_preserves_single_spaces() {
        assert_eq!(fmtmsg("CQ DX"), "CQ DX");
    }

    #[test]
    fn fmtmsg_empty() {
        assert_eq!(fmtmsg(""), "");
    }

    #[test]
    fn fmtmsg_all_spaces() {
        assert_eq!(fmtmsg("     "), " ");
    }

    // -----------------------------------------------------------------------
    // dd_to_int
    // -----------------------------------------------------------------------

    #[test]
    fn dd_to_int_positive() {
        assert_eq!(dd_to_int("42"), 42);
        assert_eq!(dd_to_int("+42"), 42);
    }

    #[test]
    fn dd_to_int_negative() {
        assert_eq!(dd_to_int("-7"), -7);
    }

    #[test]
    fn dd_to_int_stops_at_non_digit() {
        assert_eq!(dd_to_int("12abc"), 12);
    }

    #[test]
    fn dd_to_int_empty() {
        assert_eq!(dd_to_int(""), 0);
    }

    #[test]
    fn dd_to_int_sign_only() {
        assert_eq!(dd_to_int("-"), 0);
        assert_eq!(dd_to_int("+"), 0);
    }

    // -----------------------------------------------------------------------
    // int_to_dd
    // -----------------------------------------------------------------------

    #[test]
    fn int_to_dd_positive_no_sign() {
        assert_eq!(int_to_dd(7, 2, false), "07");
    }

    #[test]
    fn int_to_dd_positive_with_sign() {
        assert_eq!(int_to_dd(7, 2, true), "+07");
    }

    #[test]
    fn int_to_dd_negative() {
        assert_eq!(int_to_dd(-15, 2, false), "-15");
    }

    #[test]
    fn int_to_dd_zero() {
        assert_eq!(int_to_dd(0, 2, false), "00");
        assert_eq!(int_to_dd(0, 2, true), "+00");
    }

    #[test]
    fn int_to_dd_width_3() {
        assert_eq!(int_to_dd(123, 3, false), "123");
        assert_eq!(int_to_dd(5, 3, true), "+005");
    }
}
