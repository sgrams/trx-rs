// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

/// Normalize a name to lowercase alphanumeric.
pub fn normalize_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_name() {
        assert_eq!(normalize_name("FT-817"), "ft817");
        assert_eq!(normalize_name("HTTP-JSON"), "httpjson");
        assert_eq!(normalize_name("foo_bar-baz"), "foobarbaz");
    }
}
