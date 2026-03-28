// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::sync::OnceLock;

const PKG_NAME: &str = env!("CARGO_PKG_NAME");
const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
const CLIENT_BUILD_DATE: &str = env!("TRX_CLIENT_BUILD_DATE");

const INDEX_HTML: &str = include_str!("../assets/web/index.html");
pub const STYLE_CSS: &str = include_str!("../assets/web/style.css");
pub const APP_JS: &str = include_str!("../assets/web/app.js");
pub const DECODE_HISTORY_WORKER_JS: &str = include_str!("../assets/web/decode-history-worker.js");
pub const WEBGL_RENDERER_JS: &str = include_str!("../assets/web/webgl-renderer.js");
pub const LEAFLET_AIS_TRACKSYMBOL_JS: &str =
    include_str!("../assets/web/leaflet-ais-tracksymbol.js");
pub const AIS_JS: &str = include_str!("../assets/web/plugins/ais.js");
pub const VDES_JS: &str = include_str!("../assets/web/plugins/vdes.js");
pub const APRS_JS: &str = include_str!("../assets/web/plugins/aprs.js");
pub const HF_APRS_JS: &str = include_str!("../assets/web/plugins/hf-aprs.js");
pub const FT8_JS: &str = include_str!("../assets/web/plugins/ft8.js");
pub const FT4_JS: &str = include_str!("../assets/web/plugins/ft4.js");
pub const FT2_JS: &str = include_str!("../assets/web/plugins/ft2.js");
pub const WSPR_JS: &str = include_str!("../assets/web/plugins/wspr.js");
pub const CW_JS: &str = include_str!("../assets/web/plugins/cw.js");
pub const SAT_JS: &str = include_str!("../assets/web/plugins/sat.js");
pub const BOOKMARKS_JS: &str = include_str!("../assets/web/plugins/bookmarks.js");
pub const SCHEDULER_JS: &str = include_str!("../assets/web/plugins/scheduler.js");
pub const SAT_SCHEDULER_JS: &str = include_str!("../assets/web/plugins/sat-scheduler.js");
pub const BACKGROUND_DECODE_JS: &str = include_str!("../assets/web/plugins/background-decode.js");
pub const VCHAN_JS: &str = include_str!("../assets/web/plugins/vchan.js");

/// Build version tag used for cache-busting asset URLs and ETag headers.
/// Computed once from `PKG_VERSION` + `CLIENT_BUILD_DATE`.
pub fn build_version_tag() -> &'static str {
    static TAG: OnceLock<String> = OnceLock::new();
    TAG.get_or_init(|| format!("{PKG_VERSION}-{CLIENT_BUILD_DATE}"))
}

/// Pre-computed index HTML with version/date placeholders resolved.
/// Computed once on first access, avoiding three `.replace()` calls per
/// request on the ~50 KB HTML template.
pub fn index_html() -> &'static str {
    static HTML: OnceLock<String> = OnceLock::new();
    HTML.get_or_init(|| {
        INDEX_HTML
            .replace("{pkg}", PKG_NAME)
            .replace("{ver}", PKG_VERSION)
            .replace("{client_build_date}", CLIENT_BUILD_DATE)
    })
}
