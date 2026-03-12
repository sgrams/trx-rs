// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

const PKG_NAME: &str = env!("CARGO_PKG_NAME");
const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
const CLIENT_BUILD_DATE: &str = env!("TRX_CLIENT_BUILD_DATE");

const INDEX_HTML: &str = include_str!("../assets/web/index.html");
pub const STYLE_CSS: &str = include_str!("../assets/web/style.css");
pub const APP_JS: &str = include_str!("../assets/web/app.js");
pub const WEBGL_RENDERER_JS: &str = include_str!("../assets/web/webgl-renderer.js");
pub const LEAFLET_AIS_TRACKSYMBOL_JS: &str =
    include_str!("../assets/web/leaflet-ais-tracksymbol.js");
pub const AIS_JS: &str = include_str!("../assets/web/plugins/ais.js");
pub const VDES_JS: &str = include_str!("../assets/web/plugins/vdes.js");
pub const APRS_JS: &str = include_str!("../assets/web/plugins/aprs.js");
pub const HF_APRS_JS: &str = include_str!("../assets/web/plugins/hf-aprs.js");
pub const FT8_JS: &str = include_str!("../assets/web/plugins/ft8.js");
pub const WSPR_JS: &str = include_str!("../assets/web/plugins/wspr.js");
pub const CW_JS: &str = include_str!("../assets/web/plugins/cw.js");
pub const BOOKMARKS_JS: &str = include_str!("../assets/web/plugins/bookmarks.js");
pub const SCHEDULER_JS: &str = include_str!("../assets/web/plugins/scheduler.js");
pub const BACKGROUND_DECODE_JS: &str =
    include_str!("../assets/web/plugins/background-decode.js");
pub const VCHAN_JS: &str = include_str!("../assets/web/plugins/vchan.js");

pub fn index_html() -> String {
    INDEX_HTML
        .replace("{pkg}", PKG_NAME)
        .replace("{ver}", PKG_VERSION)
        .replace("{client_build_date}", CLIENT_BUILD_DATE)
}
