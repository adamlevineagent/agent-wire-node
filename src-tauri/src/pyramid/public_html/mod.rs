//! Public HTML surface for the pyramid web UI (Phase 0.5 skeleton).
//!
//! This module is the mount point for the post-agents-retro public web
//! surface. Phase 0.5 lands only a placeholder `routes()` that rejects
//! every request with `not_found`, so the parallel Phase 1 workstreams
//! (WS-A..F) can build against a stable module anchor without any
//! user-visible behavior change.

use crate::pyramid::PyramidState;
use std::sync::Arc;
use warp::{Filter, Rejection};

/// Placeholder route filter. Matches the `(Response,)` shape used by the
/// rest of `pyramid_routes()` in `routes.rs` so it can be chained with
/// `.or(...).unify().boxed()`.
pub fn routes(
    _state: Arc<PyramidState>,
) -> warp::filters::BoxedFilter<(warp::reply::Response,)> {
    warp::any()
        .and_then(|| async {
            Err::<warp::reply::Response, Rejection>(warp::reject::not_found())
        })
        .boxed()
}
