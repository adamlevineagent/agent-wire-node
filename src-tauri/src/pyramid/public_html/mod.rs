//! Public HTML surface for the pyramid web UI (post-agents-retro web).
//!
//! WS-C owns this assembly file. Each Phase-1 sibling workstream contributes
//! its own filter function:
//!   - WS-A `auth`           — auth filter helpers (used by other WS, no
//!                             standalone routes)
//!   - WS-B `routes_ws`      — `GET /p/{slug}/_ws` build-event stream
//!   - WS-C `routes_read`    — `GET /p/`, `/p/{slug}`, `/p/{slug}/{node_id}`
//!   - WS-D `routes_assets`  — `GET /p/_assets/{file}`, robots.txt, favicon
//!   - WS-E `routes_login`   — `_login`, `_verify`, `_logout`
//!   - WS-F `rate_limit`     — middleware (no standalone routes)
//!
//! Order matters per A9/B11: literal `_*` and reserved subpaths must match
//! BEFORE the catchall `/p/{slug}/{node_id}` (which lives inside
//! `routes_read`). The chain order below puts the literal-prefix filters
//! (assets, login, ws) ahead of the read routes for that reason.

pub mod ascii_art;
pub mod auth;
pub mod etag;
pub mod rate_limit;
pub mod render;
pub mod reserved;
pub mod routes_ask;
pub mod routes_assets;
pub mod routes_login;
pub mod routes_read;
pub mod routes_ws;
pub mod web_sessions; // WS-L (Phase 3)

#[cfg(test)]
mod integration_tests; // Phase 4

use crate::pyramid::PyramidState;
use std::sync::Arc;
use warp::Filter;

/// Public-surface route filter. Mounted by `pyramid_routes()` in `routes.rs`
/// at the `// === public_html mount point ===` anchor (single-edit rule per
/// A5). Returns `(Response,)` so the caller can chain it with
/// `.or().unify().boxed()`.
pub fn routes(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
) -> warp::filters::BoxedFilter<(warp::reply::Response,)> {
    let assets = routes_assets::asset_routes(state.clone());
    let login = routes_login::login_routes(state.clone());
    let ws = routes_ws::ws_route(state.clone(), jwt_public_key.clone());
    let ask = routes_ask::ask_routes(state.clone(), jwt_public_key.clone());
    let read = routes_read::read_routes(state.clone(), jwt_public_key.clone());

    // Literal-prefix matches first, catchall last. Each `.or()` is followed
    // by `.unify()` to keep the tuple shape `(Response,)` flat. `_ask` is a
    // literal segment and must precede the catchall `read_routes`.
    assets
        .or(login)
        .unify()
        .or(ws)
        .unify()
        .or(ask)
        .unify()
        .or(read)
        .unify()
        .boxed()
}
