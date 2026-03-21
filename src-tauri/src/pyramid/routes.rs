// pyramid/routes.rs — Warp HTTP route handlers for the Knowledge Pyramid API
//
// All routes require bearer token authentication.
// Routes delegate to query:: and slug:: modules for actual logic.

use std::sync::Arc;
use warp::Filter;
use warp::Reply;
use serde::Deserialize;

use super::PyramidState;
use super::types::*;
use super::query;
use super::slug;

// ── Auth middleware ──────────────────────────────────────────────────

/// Validate bearer token and pass state through. Returns PyramidState on success.
fn with_auth_state(
    state: Arc<PyramidState>,
) -> impl Filter<Extract = (Arc<PyramidState>,), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("authorization")
        .and(warp::any().map(move || state.clone()))
        .and_then(|auth_header: Option<String>, state: Arc<PyramidState>| async move {
            let token = match auth_header {
                Some(h) => match h.strip_prefix("Bearer ") {
                    Some(t) => t.to_string(),
                    None => return Err(warp::reject::custom(Unauthorized)),
                },
                None => return Err(warp::reject::custom(Unauthorized)),
            };

            let api_key = {
                let config = state.config.read().await;
                config.api_key.clone()
            };
            if api_key.is_empty() || token != api_key {
                return Err(warp::reject::custom(Unauthorized));
            }

            Ok(state)
        })
}

#[derive(Debug)]
struct Unauthorized;
impl warp::reject::Reject for Unauthorized {}

// ── JSON reply helpers ──────────────────────────────────────────────

fn json_error(status: warp::http::StatusCode, msg: &str) -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"error": msg})),
        status,
    )
    .into_response()
}

fn json_ok<T: serde::Serialize>(val: &T) -> warp::reply::Response {
    warp::reply::json(val).into_response()
}

// ── Request body types ──────────────────────────────────────────────

#[derive(Deserialize)]
struct CreateSlugBody {
    slug: String,
    content_type: ContentType,
    source_path: String,
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

// ── Route definitions ───────────────────────────────────────────────

pub fn pyramid_routes(
    state: Arc<PyramidState>,
) -> warp::filters::BoxedFilter<(warp::reply::Response,)> {
    let prefix = warp::path("pyramid");

    // Helper macro: box each route to (Response,) to avoid nested Either types
    macro_rules! route {
        ($filter:expr) => {
            $filter.map(|r: warp::reply::Response| r).boxed()
        };
    }

    // GET /pyramid/slugs
    let list_slugs = route!(prefix
        .and(warp::path("slugs"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_list_slugs));

    // POST /pyramid/slugs
    let create_slug_route = route!(prefix
        .and(warp::path("slugs"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and(warp::body::json())
        .and_then(handle_create_slug));

    // GET /pyramid/:slug/build/status (must be before /pyramid/:slug/build)
    let build_status = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("build"))
        .and(warp::path("status"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_build_status));

    // POST /pyramid/:slug/build/cancel (must be before /pyramid/:slug/build)
    let build_cancel = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("build"))
        .and(warp::path("cancel"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_build_cancel));

    // POST /pyramid/:slug/build
    let build = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("build"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_build));

    // GET /pyramid/:slug/apex
    let apex = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("apex"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_apex));

    // GET /pyramid/:slug/node/:id
    let node = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("node"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_node));

    // GET /pyramid/:slug/tree
    let tree = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("tree"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_tree));

    // GET /pyramid/:slug/drill/:id
    let drill = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("drill"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_drill));

    // GET /pyramid/:slug/search?q=term
    let search = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("search"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and(warp::query::<SearchQuery>())
        .and_then(handle_search));

    // GET /pyramid/:slug/entities
    let entities = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("entities"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_entities));

    // GET /pyramid/:slug/resolved
    let resolved = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("resolved"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_resolved));

    // GET /pyramid/:slug/corrections
    let corrections = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("corrections"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_corrections));

    // GET /pyramid/:slug/terms
    let terms = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("terms"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_terms));

    // DELETE /pyramid/:slug
    let delete_slug_route = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::delete())
        .and(with_auth_state(state.clone()))
        .and_then(handle_delete_slug));

    // Combine routes. Box in groups to keep the nested Either type manageable.
    // Each .or().unify() flattens a pair, and .boxed() erases the type.
    let r1 = list_slugs.or(create_slug_route).unify().boxed();
    let r2 = build_status.or(build_cancel).unify().boxed();
    let r3 = build.or(apex).unify().boxed();
    let r4 = node.or(tree).unify().boxed();
    let r5 = drill.or(search).unify().boxed();
    let r6 = entities.or(resolved).unify().boxed();
    let r7 = corrections.or(terms).unify().boxed();

    // Combine the groups (each is BoxedFilter<(Response,)>)
    let g1 = r1.or(r2).unify().boxed();
    let g2 = r3.or(r4).unify().boxed();
    let g3 = r5.or(r6).unify().boxed();
    let g4 = r7.or(delete_slug_route).unify().boxed();

    let h1 = g1.or(g2).unify().boxed();
    let h2 = g3.or(g4).unify().boxed();

    h1.or(h2).unify().boxed()
}

// ── Route handlers ──────────────────────────────────────────────────

async fn handle_list_slugs(
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match slug::list_slugs(&conn) {
        Ok(slugs) => Ok(json_ok(&slugs)),
        Err(e) => Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn handle_create_slug(
    state: Arc<PyramidState>,
    body: CreateSlugBody,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.writer.lock().await;
    match slug::create_slug(&conn, &body.slug, &body.content_type, &body.source_path) {
        Ok(info) => Ok(warp::reply::with_status(
            warp::reply::json(&info),
            warp::http::StatusCode::CREATED,
        ).into_response()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") {
                Ok(json_error(warp::http::StatusCode::CONFLICT, &msg))
            } else {
                Ok(json_error(warp::http::StatusCode::BAD_REQUEST, &msg))
            }
        }
    }
}

async fn handle_apex(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::get_apex(&conn, &slug_name) {
        Ok(Some(node)) => Ok(json_ok(&node)),
        Ok(None) => Ok(json_error(warp::http::StatusCode::NOT_FOUND, "No apex node found")),
        Err(e) => Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn handle_node(
    slug_name: String,
    node_id: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::get_node(&conn, &slug_name, &node_id) {
        Ok(Some(node)) => Ok(json_ok(&node)),
        Ok(None) => Ok(json_error(warp::http::StatusCode::NOT_FOUND, "Node not found")),
        Err(e) => Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn handle_tree(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::get_tree(&conn, &slug_name) {
        Ok(tree) => Ok(json_ok(&tree)),
        Err(e) => Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn handle_drill(
    slug_name: String,
    node_id: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::drill(&conn, &slug_name, &node_id) {
        Ok(Some(result)) => Ok(json_ok(&result)),
        Ok(None) => Ok(json_error(warp::http::StatusCode::NOT_FOUND, "Node not found")),
        Err(e) => Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn handle_search(
    slug_name: String,
    state: Arc<PyramidState>,
    params: SearchQuery,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::search(&conn, &slug_name, &params.q) {
        Ok(hits) => Ok(json_ok(&hits)),
        Err(e) => Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn handle_entities(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::entities(&conn, &slug_name) {
        Ok(entries) => Ok(json_ok(&entries)),
        Err(e) => Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn handle_resolved(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::resolved(&conn, &slug_name) {
        Ok(entries) => Ok(json_ok(&entries)),
        Err(e) => Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn handle_corrections(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::corrections(&conn, &slug_name) {
        Ok(entries) => Ok(json_ok(&entries)),
        Err(e) => Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn handle_terms(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::terms(&conn, &slug_name) {
        Ok(entries) => Ok(json_ok(&entries)),
        Err(e) => Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

async fn handle_build(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Check if a build is already running
    {
        let active = state.active_build.read().await;
        if let Some(ref handle) = *active {
            if !handle.cancel.is_cancelled() {
                return Ok(json_error(
                    warp::http::StatusCode::CONFLICT,
                    &format!("Build already running for slug '{}'", handle.slug),
                ));
            }
        }
    }

    // Verify slug exists
    {
        let conn = state.reader.lock().await;
        match slug::get_slug(&conn, &slug_name) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Ok(json_error(warp::http::StatusCode::NOT_FOUND, "Slug not found"));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    }

    // Create build handle
    let cancel = tokio_util::sync::CancellationToken::new();
    let status = Arc::new(tokio::sync::RwLock::new(BuildStatus {
        slug: slug_name.clone(),
        status: "running".to_string(),
        progress: BuildProgress { done: 0, total: 0 },
        elapsed_seconds: 0.0,
    }));

    let handle = super::BuildHandle {
        slug: slug_name.clone(),
        cancel: cancel.clone(),
        status: status.clone(),
    };

    // Store the build handle
    {
        let mut active = state.active_build.write().await;
        *active = Some(handle);
    }

    // Spawn the build task
    let reader = state.reader.clone();
    let active_build = state.active_build.clone();
    let build_status = status.clone();

    tokio::spawn(async move {
        let start = std::time::Instant::now();

        // Read slug info to determine content type (for future use)
        let _content_type = {
            let conn = reader.lock().await;
            match super::slug::get_slug(&conn, &slug_name) {
                Ok(Some(info)) => info.content_type,
                _ => {
                    let mut s = build_status.write().await;
                    s.status = "failed".to_string();
                    s.elapsed_seconds = start.elapsed().as_secs_f64();
                    return;
                }
            }
        };

        // TODO: When build.rs is implemented, call the appropriate pipeline here:
        // match _content_type {
        //     ContentType::Conversation => build::build_conversation(...),
        //     ContentType::Code => build::build_code(...),
        //     ContentType::Document => build::build_docs(...),
        // }

        // For now, mark as complete (build.rs is a stub)
        {
            let mut s = build_status.write().await;
            if cancel.is_cancelled() {
                s.status = "cancelled".to_string();
            } else {
                s.status = "complete".to_string();
            }
            s.elapsed_seconds = start.elapsed().as_secs_f64();
        }

        // Clear the active build handle
        {
            let mut active = active_build.write().await;
            *active = None;
        }
    });

    // Return initial status
    let s = status.read().await;
    Ok(warp::reply::with_status(
        warp::reply::json(&*s),
        warp::http::StatusCode::ACCEPTED,
    )
    .into_response())
}

async fn handle_build_status(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let active = state.active_build.read().await;
    if let Some(ref handle) = *active {
        if handle.slug == slug_name {
            let s = handle.status.read().await;
            return Ok(json_ok(&*s));
        }
    }

    // No active build — return idle status
    Ok(json_ok(&BuildStatus {
        slug: slug_name,
        status: "idle".to_string(),
        progress: BuildProgress { done: 0, total: 0 },
        elapsed_seconds: 0.0,
    }))
}

async fn handle_build_cancel(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let active = state.active_build.read().await;
    if let Some(ref handle) = *active {
        if handle.slug == slug_name && !handle.cancel.is_cancelled() {
            handle.cancel.cancel();
            return Ok(json_ok(&serde_json::json!({"status": "cancelling"})));
        }
    }

    Ok(json_error(
        warp::http::StatusCode::NOT_FOUND,
        "No active build for this slug",
    ))
}

async fn handle_delete_slug(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Don't allow deleting a slug with an active build
    {
        let active = state.active_build.read().await;
        if let Some(ref handle) = *active {
            if handle.slug == slug_name && !handle.cancel.is_cancelled() {
                return Ok(json_error(
                    warp::http::StatusCode::CONFLICT,
                    "Cannot delete slug while build is running",
                ));
            }
        }
    }

    let conn = state.writer.lock().await;
    match slug::delete_slug(&conn, &slug_name) {
        Ok(()) => Ok(json_ok(&serde_json::json!({"deleted": slug_name}))),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                Ok(json_error(warp::http::StatusCode::NOT_FOUND, &msg))
            } else {
                Ok(json_error(warp::http::StatusCode::INTERNAL_SERVER_ERROR, &msg))
            }
        }
    }
}

