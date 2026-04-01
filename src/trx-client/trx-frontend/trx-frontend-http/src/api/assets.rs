// SPDX-FileCopyrightText: 2026 Stan Grams <sjg@haxx.space>
//
// SPDX-License-Identifier: BSD-2-Clause

//! Static asset serving endpoints (HTML pages, JS, CSS, favicon, logo).

use actix_web::http::header;
use actix_web::{get, HttpRequest, HttpResponse, Responder};
use std::sync::OnceLock;

use super::{gz_cache_entry, static_asset_response, GzCacheEntry, FAVICON_BYTES, LOGO_BYTES};
use crate::server::status;

// ---------------------------------------------------------------------------
// Pre-compressed asset caches
// ---------------------------------------------------------------------------

macro_rules! define_gz_cache {
    ($fn_name:ident, $src:expr, $asset_name:literal) => {
        fn $fn_name() -> &'static GzCacheEntry {
            static CACHE: OnceLock<GzCacheEntry> = OnceLock::new();
            CACHE.get_or_init(|| gz_cache_entry($src.as_bytes(), $asset_name))
        }
    };
}

define_gz_cache!(gz_index_html, status::index_html(), "index.html");
define_gz_cache!(gz_style_css, status::STYLE_CSS, "style.css");
define_gz_cache!(gz_themes_css, status::THEMES_CSS, "themes.css");
define_gz_cache!(gz_app_js, status::APP_JS, "app.js");
define_gz_cache!(gz_map_core_js, status::MAP_CORE_JS, "map-core.js");
define_gz_cache!(gz_screenshot_js, status::SCREENSHOT_JS, "screenshot.js");
define_gz_cache!(
    gz_decode_history_worker_js,
    status::DECODE_HISTORY_WORKER_JS,
    "decode-history-worker.js"
);
define_gz_cache!(
    gz_webgl_renderer_js,
    status::WEBGL_RENDERER_JS,
    "webgl-renderer.js"
);
define_gz_cache!(
    gz_leaflet_ais_tracksymbol_js,
    status::LEAFLET_AIS_TRACKSYMBOL_JS,
    "leaflet-ais-tracksymbol.js"
);
define_gz_cache!(gz_ais_js, status::AIS_JS, "ais.js");
define_gz_cache!(gz_vdes_js, status::VDES_JS, "vdes.js");
define_gz_cache!(gz_aprs_js, status::APRS_JS, "aprs.js");
define_gz_cache!(gz_hf_aprs_js, status::HF_APRS_JS, "hf-aprs.js");
define_gz_cache!(gz_ft8_js, status::FT8_JS, "ft8.js");
define_gz_cache!(gz_ft4_js, status::FT4_JS, "ft4.js");
define_gz_cache!(gz_ft2_js, status::FT2_JS, "ft2.js");
define_gz_cache!(gz_wspr_js, status::WSPR_JS, "wspr.js");
define_gz_cache!(gz_cw_js, status::CW_JS, "cw.js");
define_gz_cache!(gz_sat_js, status::SAT_JS, "sat.js");
define_gz_cache!(gz_bookmarks_js, status::BOOKMARKS_JS, "bookmarks.js");
define_gz_cache!(gz_scheduler_js, status::SCHEDULER_JS, "scheduler.js");
define_gz_cache!(
    gz_sat_scheduler_js,
    status::SAT_SCHEDULER_JS,
    "sat-scheduler.js"
);
define_gz_cache!(
    gz_background_decode_js,
    status::BACKGROUND_DECODE_JS,
    "background-decode.js"
);
define_gz_cache!(gz_vchan_js, status::VCHAN_JS, "vchan.js");
define_gz_cache!(gz_bandplan_json, status::BANDPLAN_JSON, "bandplan.json");

// Vendored DSEG14 Classic font
// (binary woff2 — served directly, not through gz_cache)

// Vendored Leaflet 1.9.4
define_gz_cache!(gz_leaflet_js, status::LEAFLET_JS, "leaflet.js");
define_gz_cache!(gz_leaflet_css, status::LEAFLET_CSS, "leaflet.css");

// ---------------------------------------------------------------------------
// HTML page routes (all serve the SPA index)
// ---------------------------------------------------------------------------

#[get("/")]
pub(crate) async fn index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", c)
}

#[get("/map")]
pub(crate) async fn map_index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", c)
}

#[get("/digital-modes")]
pub(crate) async fn digital_modes_index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", c)
}

#[get("/recorder")]
pub(crate) async fn recorder_index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", c)
}

#[get("/settings")]
pub(crate) async fn settings_index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", c)
}

#[get("/about")]
pub(crate) async fn about_index(req: HttpRequest) -> impl Responder {
    let c = gz_index_html();
    static_asset_response(&req, "text/html; charset=utf-8", c)
}

// ---------------------------------------------------------------------------
// Favicon & logo
// ---------------------------------------------------------------------------

#[get("/favicon.ico")]
pub(crate) async fn favicon() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(FAVICON_BYTES)
}

#[get("/favicon.png")]
pub(crate) async fn favicon_png() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(FAVICON_BYTES)
}

#[get("/logo.png")]
pub(crate) async fn logo() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(LOGO_BYTES)
}

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

#[get("/style.css")]
pub(crate) async fn style_css(req: HttpRequest) -> impl Responder {
    let c = gz_style_css();
    static_asset_response(&req, "text/css; charset=utf-8", c)
}

#[get("/themes.css")]
pub(crate) async fn themes_css(req: HttpRequest) -> impl Responder {
    let c = gz_themes_css();
    static_asset_response(&req, "text/css; charset=utf-8", c)
}

// ---------------------------------------------------------------------------
// JavaScript assets
// ---------------------------------------------------------------------------

#[get("/app.js")]
pub(crate) async fn app_js(req: HttpRequest) -> impl Responder {
    let c = gz_app_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/map-core.js")]
pub(crate) async fn map_core_js(req: HttpRequest) -> impl Responder {
    let c = gz_map_core_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/screenshot.js")]
pub(crate) async fn screenshot_js(req: HttpRequest) -> impl Responder {
    let c = gz_screenshot_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/decode-history-worker.js")]
pub(crate) async fn decode_history_worker_js(req: HttpRequest) -> impl Responder {
    let c = gz_decode_history_worker_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/webgl-renderer.js")]
pub(crate) async fn webgl_renderer_js(req: HttpRequest) -> impl Responder {
    let c = gz_webgl_renderer_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/leaflet-ais-tracksymbol.js")]
pub(crate) async fn leaflet_ais_tracksymbol_js(req: HttpRequest) -> impl Responder {
    let c = gz_leaflet_ais_tracksymbol_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/aprs.js")]
pub(crate) async fn aprs_js(req: HttpRequest) -> impl Responder {
    let c = gz_aprs_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/hf-aprs.js")]
pub(crate) async fn hf_aprs_js(req: HttpRequest) -> impl Responder {
    let c = gz_hf_aprs_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/ais.js")]
pub(crate) async fn ais_js(req: HttpRequest) -> impl Responder {
    let c = gz_ais_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/vdes.js")]
pub(crate) async fn vdes_js(req: HttpRequest) -> impl Responder {
    let c = gz_vdes_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/ft8.js")]
pub(crate) async fn ft8_js(req: HttpRequest) -> impl Responder {
    let c = gz_ft8_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/ft4.js")]
pub(crate) async fn ft4_js(req: HttpRequest) -> impl Responder {
    let c = gz_ft4_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/ft2.js")]
pub(crate) async fn ft2_js(req: HttpRequest) -> impl Responder {
    let c = gz_ft2_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/wspr.js")]
pub(crate) async fn wspr_js(req: HttpRequest) -> impl Responder {
    let c = gz_wspr_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/cw.js")]
pub(crate) async fn cw_js(req: HttpRequest) -> impl Responder {
    let c = gz_cw_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/sat.js")]
pub(crate) async fn sat_js(req: HttpRequest) -> impl Responder {
    let c = gz_sat_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/bookmarks.js")]
pub(crate) async fn bookmarks_js(req: HttpRequest) -> impl Responder {
    let c = gz_bookmarks_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/scheduler.js")]
pub(crate) async fn scheduler_js(req: HttpRequest) -> impl Responder {
    let c = gz_scheduler_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/sat-scheduler.js")]
pub(crate) async fn sat_scheduler_js(req: HttpRequest) -> impl Responder {
    let c = gz_sat_scheduler_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/background-decode.js")]
pub(crate) async fn background_decode_js(req: HttpRequest) -> impl Responder {
    let c = gz_background_decode_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/vchan.js")]
pub(crate) async fn vchan_js(req: HttpRequest) -> impl Responder {
    let c = gz_vchan_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/bandplan.json")]
pub(crate) async fn bandplan_json(req: HttpRequest) -> impl Responder {
    let c = gz_bandplan_json();
    static_asset_response(&req, "application/json; charset=utf-8", c)
}

// ---------------------------------------------------------------------------
// Vendored DSEG14 Classic font
// ---------------------------------------------------------------------------

#[get("/vendor/dseg14-classic-latin-400-normal.woff2")]
pub(crate) async fn dseg14_classic_woff2() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "font/woff2"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(status::DSEG14_CLASSIC_WOFF2)
}

// ---------------------------------------------------------------------------
// Vendored Leaflet 1.9.4
// ---------------------------------------------------------------------------

#[get("/vendor/leaflet.js")]
pub(crate) async fn leaflet_js(req: HttpRequest) -> impl Responder {
    let c = gz_leaflet_js();
    static_asset_response(
        &req,
        "application/javascript; charset=utf-8",
        c,
    )
}

#[get("/vendor/leaflet.css")]
pub(crate) async fn leaflet_css(req: HttpRequest) -> impl Responder {
    let c = gz_leaflet_css();
    static_asset_response(&req, "text/css; charset=utf-8", c)
}

#[get("/vendor/marker-icon.png")]
pub(crate) async fn leaflet_marker_icon() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(status::LEAFLET_MARKER_ICON)
}

#[get("/vendor/marker-icon-2x.png")]
pub(crate) async fn leaflet_marker_icon_2x() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(status::LEAFLET_MARKER_ICON_2X)
}

#[get("/vendor/marker-shadow.png")]
pub(crate) async fn leaflet_marker_shadow() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(status::LEAFLET_MARKER_SHADOW)
}

#[get("/vendor/layers.png")]
pub(crate) async fn leaflet_layers() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(status::LEAFLET_LAYERS)
}

#[get("/vendor/layers-2x.png")]
pub(crate) async fn leaflet_layers_2x() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "image/png"))
        .insert_header((header::CACHE_CONTROL, "public, max-age=604800, immutable"))
        .body(status::LEAFLET_LAYERS_2X)
}
