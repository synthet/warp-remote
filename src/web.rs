//! The embedded web/mobile remote-control app. Everything is compiled into the binary so the
//! server is a single self-contained file with no CDN or runtime asset directory.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::server::static_response;

const NO_CACHE: &str = "no-cache";
const IMMUTABLE: &str = "public, max-age=86400";

pub async fn index() -> Response {
    static_response(
        "text/html; charset=utf-8",
        include_bytes!("../web/index.html"),
        NO_CACHE,
    )
}

pub async fn manifest() -> Response {
    static_response(
        "application/manifest+json",
        include_bytes!("../web/manifest.webmanifest"),
        NO_CACHE,
    )
}

pub async fn service_worker() -> Response {
    static_response(
        "text/javascript; charset=utf-8",
        include_bytes!("../web/sw.js"),
        NO_CACHE,
    )
}

pub async fn asset(Path(path): Path<String>) -> Response {
    let (content_type, body, cache): (&'static str, &'static [u8], &'static str) =
        match path.as_str() {
            "app.js" => (
                "text/javascript; charset=utf-8",
                include_bytes!("../web/app.js"),
                NO_CACHE,
            ),
            "app.css" => (
                "text/css; charset=utf-8",
                include_bytes!("../web/app.css"),
                NO_CACHE,
            ),
            "icon.svg" => (
                "image/svg+xml",
                include_bytes!("../web/icon.svg"),
                IMMUTABLE,
            ),
            "vendor/xterm.js" => (
                "text/javascript; charset=utf-8",
                include_bytes!("../web/vendor/xterm.js"),
                IMMUTABLE,
            ),
            "vendor/xterm.css" => (
                "text/css; charset=utf-8",
                include_bytes!("../web/vendor/xterm.css"),
                IMMUTABLE,
            ),
            "vendor/addon-fit.js" => (
                "text/javascript; charset=utf-8",
                include_bytes!("../web/vendor/addon-fit.js"),
                IMMUTABLE,
            ),
            _ => return StatusCode::NOT_FOUND.into_response(),
        };
    static_response(content_type, body, cache)
}
