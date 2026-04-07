//! WS-D — static asset routes for the public retro web surface.
//!
//! Serves:
//!   GET /p/_assets/{file}   — content-hashed (immutable) or unhashed (no-cache)
//!   GET /robots.txt
//!   GET /favicon.ico
//!
//! Asset bytes are baked into the binary at compile time via `include_bytes!`
//! through the manifest emitted by `build.rs`.
//!
//! Dual-resolution: the hashed name (e.g. `app.deadbeef.css`) and the unhashed
//! name (`app.css`) both resolve to the same bytes. The hashed variant gets
//! `Cache-Control: public, max-age=31536000, immutable`; the unhashed variant
//! gets `Cache-Control: no-cache` so a deploy-on-the-fly font reference inside
//! the (also-hashed) CSS file still picks up the latest bytes after a reload.

use std::sync::Arc;
use warp::http::{header, Response, StatusCode};
use warp::reply::Response as WarpResponse;
use warp::Filter;

include!(concat!(env!("OUT_DIR"), "/asset_manifest.rs"));

/// Build the asset routes filter. Returns the same shape as the rest of
/// `pyramid_routes()` so it can be `.or(...).unify().boxed()`-chained by WS-C.
pub fn asset_routes(
    _state: Arc<crate::pyramid::PyramidState>,
) -> warp::filters::BoxedFilter<(WarpResponse,)> {
    // GET /p/_assets/{file}
    let assets = warp::get()
        .and(warp::path("p"))
        .and(warp::path("_assets"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .map(|file: String| serve_asset(&file))
        .boxed();

    // GET /robots.txt
    let robots = warp::get()
        .and(warp::path("robots.txt"))
        .and(warp::path::end())
        .map(|| serve_asset("robots.txt"))
        .boxed();

    // GET /favicon.ico
    let favicon = warp::get()
        .and(warp::path("favicon.ico"))
        .and(warp::path::end())
        .map(|| serve_asset("favicon.ico"))
        .boxed();

    assets.or(robots).unify().boxed().or(favicon).unify().boxed()
}

/// Serve an asset by either its hashed or unhashed basename.
///
/// Returns 404 if no matching entry is found in the manifest.
fn serve_asset(name: &str) -> WarpResponse {
    for entry in ASSETS.iter() {
        if name == entry.hashed_name {
            return build_response(entry, /* immutable */ true);
        }
        if name == entry.name {
            return build_response(entry, /* immutable */ false);
        }
    }
    not_found()
}

fn build_response(entry: &AssetEntry, immutable: bool) -> WarpResponse {
    let cache = if immutable {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, entry.mime)
        .header(header::CONTENT_LENGTH, entry.bytes.len())
        .header(header::CACHE_CONTROL, cache)
        // Strong validator: the hash IS the etag for hashed responses;
        // for the unhashed alias we still expose it so conditional GETs work.
        .header(header::ETAG, format!("\"{}\"", entry.hashed_name))
        .header("X-Content-Type-Options", "nosniff")
        .body(entry.bytes.into())
        .unwrap_or_else(|_| not_found())
}

fn not_found() -> WarpResponse {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body("not found".into())
        .unwrap()
}

/// Convenience accessor used by WS-C/WS-J when assembling the HTML page —
/// they need the hashed path of `app.css` to put in a `<link rel=stylesheet>`.
#[allow(dead_code)]
pub fn hashed_path(name: &str) -> Option<&'static str> {
    ASSETS
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.hashed_path)
}
