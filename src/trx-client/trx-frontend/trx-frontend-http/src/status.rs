// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

const PKG_NAME: &str = env!("CARGO_PKG_NAME");
const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

const INDEX_HTML: &str = include_str!("../assets/web/index.html");
pub const STYLE_CSS: &str = include_str!("../assets/web/style.css");
pub const APP_JS: &str = include_str!("../assets/web/app.js");
pub const APRS_JS: &str = include_str!("../assets/web/plugins/aprs.js");
pub const FT8_JS: &str = include_str!("../assets/web/plugins/ft8.js");
pub const CW_JS: &str = include_str!("../assets/web/plugins/cw.js");

pub fn index_html() -> String {
    INDEX_HTML
        .replace("{pkg}", PKG_NAME)
        .replace("{ver}", PKG_VERSION)
}
