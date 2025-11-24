// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::error::Error;

/// Detect the specific CAT decode error for invalid BCD digits.
pub fn is_invalid_bcd_error(err: &(dyn Error + 'static)) -> bool {
    if err.to_string().contains("invalid BCD digit in frequency") {
        return true;
    }
    err.source().map(is_invalid_bcd_error).unwrap_or(false)
}
