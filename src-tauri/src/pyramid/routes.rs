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
use super::ingest;
use super::build::{self, WriteOp};
use super::db;

// ── Auth middleware ──────────────────────────────────────────────────

/// Constant-time string comparison to prevent timing attacks on auth tokens.
fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

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

            let auth_token = {
                let config = state.config.read().await;
                config.auth_token.clone()
            };
            if auth_token.is_empty() || !ct_eq(&token, &auth_token) {
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

#[derive(Deserialize)]
struct ConfigBody {
    openrouter_api_key: Option<String>,
    auth_token: Option<String>,
    primary_model: Option<String>,
    fallback_model_1: Option<String>,
    fallback_model_2: Option<String>,
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

    // POST /pyramid/:slug/ingest
    let ingest_route = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("ingest"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_ingest));

    // POST /pyramid/config
    let config_route = route!(prefix
        .and(warp::path("config"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and(warp::body::json())
        .and_then(handle_config));

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
    let r8 = ingest_route.or(config_route).unify().boxed();

    // Combine the groups (each is BoxedFilter<(Response,)>)
    let g1 = r1.or(r2).unify().boxed();
    let g2 = r3.or(r4).unify().boxed();
    let g3 = r5.or(r6).unify().boxed();
    let g4 = r7.or(delete_slug_route).unify().boxed();
    let g5 = r8;

    let h1 = g1.or(g2).unify().boxed();
    let h2 = g3.or(g4).unify().boxed();

    let top = h1.or(h2).unify().boxed();
    top.or(g5).unify().boxed()
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
    match db::get_node(&conn, &slug_name, &node_id) {
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
    // Verify slug exists before taking the write lock
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

    // Use write lock for atomic check-and-set (prevents TOCTOU race)
    let cancel = tokio_util::sync::CancellationToken::new();
    let status = Arc::new(tokio::sync::RwLock::new(BuildStatus {
        slug: slug_name.clone(),
        status: "running".to_string(),
        progress: BuildProgress { done: 0, total: 0 },
        elapsed_seconds: 0.0,
        failures: 0,
    }));

    {
        let mut active = state.active_build.write().await;
        if let Some(ref handle) = *active {
            if !handle.cancel.is_cancelled() {
                return Ok(json_error(
                    warp::http::StatusCode::CONFLICT,
                    &format!("Build already running for slug '{}'", handle.slug),
                ));
            }
        }

        let handle = super::BuildHandle {
            slug: slug_name.clone(),
            cancel: cancel.clone(),
            status: status.clone(),
        };
        *active = Some(handle);
    }

    // Spawn the build task
    let reader = state.reader.clone();
    let writer = state.writer.clone();
    let config = state.config.clone();
    let active_build = state.active_build.clone();
    let build_status = status.clone();

    tokio::spawn(async move {
        let start = std::time::Instant::now();

        // Read slug info to determine content type
        let content_type = {
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

        // Snapshot LLM config
        let llm_config = {
            let cfg = config.read().await;
            cfg.clone()
        };

        // Create mpsc channel for WriteOps
        let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<WriteOp>(256);

        // Spawn the writer task that consumes WriteOps using the writer connection
        let writer_handle = {
            let writer_conn = writer.clone();
            tokio::spawn(async move {
                while let Some(op) = write_rx.recv().await {
                    let result = {
                        let conn = writer_conn.lock().await;
                        match op {
                            WriteOp::SaveNode { ref node, ref topics_json } => {
                                db::save_node(&conn, node, topics_json.as_deref())
                            }
                            WriteOp::SaveStep {
                                ref slug, ref step_type, chunk_index, depth,
                                ref node_id, ref output_json, ref model, elapsed,
                            } => {
                                db::save_step(&conn, slug, step_type, chunk_index, depth, node_id, output_json, model, elapsed)
                            }
                            WriteOp::UpdateParent { ref slug, ref node_id, ref parent_id } => {
                                db::update_parent(&conn, slug, node_id, parent_id)
                            }
                            WriteOp::UpdateStats { ref slug } => {
                                db::update_slug_stats(&conn, slug)
                            }
                        }
                    };
                    if let Err(e) = result {
                        tracing::error!("WriteOp failed: {e}");
                    }
                }
            })
        };

        // Create progress channel — forward updates into the build status
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<BuildProgress>(64);
        let progress_status = build_status.clone();
        let progress_start = start;
        let progress_handle = tokio::spawn(async move {
            while let Some(prog) = progress_rx.recv().await {
                let mut s = progress_status.write().await;
                s.progress = prog;
                s.elapsed_seconds = progress_start.elapsed().as_secs_f64();
            }
        });

        // Call the appropriate build pipeline
        let result = match content_type {
            ContentType::Conversation => {
                build::build_conversation(
                    reader.clone(), &write_tx, &llm_config, &slug_name, &cancel, &progress_tx,
                ).await
            }
            ContentType::Code => {
                build::build_code(
                    reader.clone(), &write_tx, &llm_config, &slug_name, &cancel, &progress_tx,
                ).await
            }
            ContentType::Document => {
                build::build_docs(
                    reader.clone(), &write_tx, &llm_config, &slug_name, &cancel, &progress_tx,
                ).await
            }
        };

        // Drop the write sender so the writer task can finish
        drop(write_tx);
        drop(progress_tx);
        let _ = writer_handle.await;
        let _ = progress_handle.await;

        // Update final status
        {
            let mut s = build_status.write().await;
            if cancel.is_cancelled() {
                s.status = "cancelled".to_string();
            } else {
                match result {
                    Ok(failures) => {
                        s.failures = failures;
                        if failures > 0 {
                            s.status = "complete_with_errors".to_string();
                            tracing::warn!(
                                "Build completed for '{}' with {failures} node failure(s)",
                                slug_name
                            );
                        } else {
                            s.status = "complete".to_string();
                        }
                    }
                    Err(ref e) => {
                        s.status = "failed".to_string();
                        tracing::error!("Build failed for '{}': {e}", slug_name);
                    }
                }
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
        failures: 0,
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

async fn handle_ingest(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Look up slug info to get source_path and content_type
    let slug_info = {
        let conn = state.reader.lock().await;
        match slug::get_slug(&conn, &slug_name) {
            Ok(Some(info)) => info,
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
    };

    let source_path = slug_info.source_path.clone();
    let content_type = slug_info.content_type.clone();
    let slug_clone = slug_name.clone();

    // Parse source_path as JSON array, falling back to single-path for backward compat
    let paths: Vec<String> = serde_json::from_str(&source_path)
        .unwrap_or_else(|_| vec![source_path.clone()]);

    // Run synchronous ingest on a blocking thread
    let writer = state.writer.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = writer.blocking_lock();
        for path_str in &paths {
            let path = std::path::Path::new(path_str);
            match content_type {
                ContentType::Code => { ingest::ingest_code(&conn, &slug_clone, path)?; }
                ContentType::Conversation => { ingest::ingest_conversation(&conn, &slug_clone, path)?; }
                ContentType::Document => { ingest::ingest_docs(&conn, &slug_clone, path)?; }
            }
        }
        Ok::<String, anyhow::Error>(slug_clone)
    })
    .await;

    match result {
        Ok(Ok(_slug)) => {
            // Count chunks to return
            let conn = state.reader.lock().await;
            let chunk_count = db::count_chunks(&conn, &slug_name).unwrap_or(0);
            Ok(json_ok(&serde_json::json!({
                "slug": slug_name,
                "chunks": chunk_count,
                "status": "ingested"
            })))
        }
        Ok(Err(e)) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Ingest task panicked: {e}"),
        )),
    }
}

async fn handle_config(
    state: Arc<PyramidState>,
    body: ConfigBody,
) -> Result<warp::reply::Response, warp::Rejection> {
    let mut config = state.config.write().await;

    if let Some(ref key) = body.openrouter_api_key {
        config.api_key = key.clone();
    }
    if let Some(ref token) = body.auth_token {
        config.auth_token = token.clone();
    }
    if let Some(ref model) = body.primary_model {
        config.primary_model = model.clone();
    }
    if let Some(ref model) = body.fallback_model_1 {
        config.fallback_model_1 = model.clone();
    }
    if let Some(ref model) = body.fallback_model_2 {
        config.fallback_model_2 = model.clone();
    }

    // Persist to config file if data_dir is set
    if let Some(ref data_dir) = state.data_dir {
        let pyramid_config = super::PyramidConfig {
            openrouter_api_key: config.api_key.clone(),
            auth_token: config.auth_token.clone(),
            primary_model: config.primary_model.clone(),
            fallback_model_1: config.fallback_model_1.clone(),
            fallback_model_2: config.fallback_model_2.clone(),
        };
        if let Err(e) = pyramid_config.save(data_dir) {
            tracing::error!("Failed to save pyramid config: {e}");
        }
    }

    Ok(json_ok(&serde_json::json!({
        "status": "updated",
        "primary_model": config.primary_model,
        "fallback_model_1": config.fallback_model_1,
        "fallback_model_2": config.fallback_model_2,
    })))
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

