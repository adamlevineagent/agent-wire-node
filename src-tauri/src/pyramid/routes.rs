// pyramid/routes.rs — Warp HTTP route handlers for the Knowledge Pyramid API
//
// All routes require bearer token authentication.
// Routes delegate to query:: and slug:: modules for actual logic.
// Auto-stale endpoints (Phase 5/6) handle freeze, breaker, config, cost observatory.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use warp::Filter;
use warp::Reply;

use super::build::WriteOp;
use super::characterize;
use super::types::CharacterizationResult;
use super::db;
use super::delta;
use super::faq;
use super::ingest;
use super::meta;
use super::query;
use super::slug;
use super::stale_engine;
use super::types::*;
use super::vine;
use super::webbing;
use super::wire_import;
use super::wire_publish;
use super::PyramidState;
use crate::http_utils::{ct_eq, json_error, json_ok, Unauthorized};
use std::path::PathBuf;

// ── Auth middleware ──────────────────────────────────────────────────

/// Validate bearer token and pass state through. Returns PyramidState on success.
fn with_auth_state(
    state: Arc<PyramidState>,
) -> impl Filter<Extract = (Arc<PyramidState>,), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("authorization")
        .and(warp::any().map(move || state.clone()))
        .and_then(
            |auth_header: Option<String>, state: Arc<PyramidState>| async move {
                let token = match auth_header {
                    Some(h) => match h.strip_prefix("Bearer ") {
                        Some(t) => t.to_string(),
                        None => return Err(warp::reject::custom(Unauthorized)),
                    },
                    None => return Err(warp::reject::custom(Unauthorized)),
                };

                // Auth token is set in pyramid_config.json (field: "auth_token")
                // or via the desktop app's Settings → API Key flow which writes to the same file.
                // Location: ~/Library/Application Support/wire-node/pyramid_config.json
                // All HTTP API calls require: Authorization: Bearer <auth_token>
                let auth_token = {
                    let config = state.config.read().await;
                    config.auth_token.clone()
                };
                if auth_token.is_empty() || !ct_eq(&token, &auth_token) {
                    return Err(warp::reject::custom(Unauthorized));
                }

                Ok(state)
            },
        )
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
struct AnnotateBody {
    node_id: String,
    annotation_type: String,
    content: String,
    question_context: Option<String>,
    author: Option<String>,
}

#[derive(Deserialize)]
struct AnnotationsQuery {
    node_id: Option<String>,
}

#[derive(Deserialize)]
struct FaqMatchQuery {
    q: String,
}

#[derive(Deserialize)]
struct VineBuildBody {
    vine_slug: String,
    jsonl_dirs: Vec<String>,
}

#[derive(Deserialize)]
struct ConfigBody {
    openrouter_api_key: Option<String>,
    primary_model: Option<String>,
    fallback_model_1: Option<String>,
    fallback_model_2: Option<String>,
    use_ir_executor: Option<bool>,
}

#[derive(Deserialize)]
struct UsageQuery {
    limit: Option<i64>,
}

// ── Phase 5 & 6: Auto-update request/response types ─────────────────

#[derive(Deserialize)]
struct AutoUpdateConfigBody {
    debounce_minutes: Option<i32>,
    min_changed_files: Option<i32>,
    runaway_threshold: Option<f64>,
    auto_update: Option<bool>,
}

#[derive(Deserialize)]
struct StaleLogQuery {
    layer: Option<i32>,
    stale: Option<String>, // "yes" or "no"
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct QuestionBuildBody {
    question: String,
    #[serde(default = "default_granularity")]
    granularity: u32,
    #[serde(default = "default_max_depth")]
    max_depth: u32,
    #[serde(default)]
    from_depth: Option<i64>,
    /// Optional pre-computed characterization. If provided, the build skips
    /// automatic characterization and uses this directly.
    #[serde(default)]
    characterization: Option<CharacterizationResult>,
}

#[derive(Deserialize)]
struct CharacterizeBody {
    question: String,
    #[serde(default)]
    source_path: Option<String>,
}

fn default_granularity() -> u32 {
    3
}
fn default_max_depth() -> u32 {
    3
}

#[cfg(test)]
mod question_build_body_tests {
    use super::QuestionBuildBody;

    #[test]
    fn question_build_body_defaults_without_from_depth() {
        let body: QuestionBuildBody =
            serde_json::from_str(r#"{"question":"What matters here?"}"#).unwrap();

        assert_eq!(body.question, "What matters here?");
        assert_eq!(body.granularity, 3);
        assert_eq!(body.max_depth, 3);
        assert_eq!(body.from_depth, None);
    }

    #[test]
    fn question_build_body_accepts_from_depth() {
        let body: QuestionBuildBody = serde_json::from_str(
            r#"{"question":"What matters here?","granularity":2,"max_depth":4,"from_depth":1}"#,
        )
        .unwrap();

        assert_eq!(body.granularity, 2);
        assert_eq!(body.max_depth, 4);
        assert_eq!(body.from_depth, Some(1));
    }
}

#[derive(Deserialize)]
struct CostQuery {
    window: Option<String>, // "24h", "7d", "30d"
}

#[derive(Deserialize)]
struct ChainImportBody {
    contribution_id: String,
    /// "chain" or "question_set" — defaults to "chain"
    import_type: Option<String>,
}

#[derive(Serialize)]
struct ChainImportResponse {
    ok: bool,
    contribution_id: String,
    title: String,
    content_type: Option<String>,
    import_type: String,
}

// ── Phase 4.3: Publication boundary types ────────────────────────────

#[derive(Deserialize)]
struct PublishQuestionSetBody {
    /// Optional human-readable description of the question set.
    description: Option<String>,
}

#[derive(Serialize)]
struct AutoUpdateStatusResponse {
    auto_update: bool,
    frozen: bool,
    breaker_tripped: bool,
    pending_mutations_by_layer: std::collections::HashMap<i32, i64>,
    last_check_at: Option<String>,
}

// ── Agent ID filter ─────────────────────────────────────────────────

fn with_agent_id() -> impl Filter<Extract = (Option<String>,), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("x-agent-id")
}

// ── Usage logging helper (non-blocking) ─────────────────────────────

fn log_query_usage(
    writer: Arc<Mutex<Connection>>,
    slug: String,
    query_type: String,
    query_params: String,
    result_node_ids: Vec<String>,
    agent_id: Option<String>,
) {
    tokio::spawn(async move {
        let conn = writer.lock().await;
        let entry = UsageLogEntry {
            id: 0,
            slug,
            query_type,
            query_params,
            result_node_ids: serde_json::to_string(&result_node_ids).unwrap_or_default(),
            agent_id,
            created_at: String::new(),
        };
        if let Err(e) = db::log_usage(&conn, &entry) {
            tracing::warn!("[usage] Failed to log query: {}", e);
        }
    });
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

    // POST /pyramid/:slug/build?from_depth=N
    let build = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("build"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(with_auth_state(state.clone()))
        .and_then(handle_build));

    // GET /pyramid/:slug/apex
    let apex = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("apex"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and(with_agent_id())
        .and_then(handle_apex));

    // GET /pyramid/:slug/node/:id
    let node = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("node"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and(with_agent_id())
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
        .and(with_agent_id())
        .and_then(handle_drill));

    // GET /pyramid/:slug/search?q=term
    let search = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("search"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and(warp::query::<SearchQuery>())
        .and(with_agent_id())
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

    // GET /pyramid/:slug/threads
    let threads = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("threads"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_threads));

    // POST /pyramid/:slug/annotate
    let annotate = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("annotate"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and(warp::body::json())
        .and_then(handle_annotate));

    // GET /pyramid/:slug/annotations?node_id=...
    let annotations = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("annotations"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and(warp::query::<AnnotationsQuery>())
        .and_then(handle_annotations));

    // GET /pyramid/:slug/edges
    let edges = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("edges"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_edges));

    // POST /pyramid/:slug/meta (run all meta passes)
    let meta_run = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("meta"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_meta_run));

    // GET /pyramid/:slug/meta (read meta nodes)
    let meta_read = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("meta"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_meta_read));

    // GET /pyramid/:slug/usage?limit=100
    let usage = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("usage"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and(warp::query::<UsageQuery>())
        .and_then(handle_usage));

    // GET /pyramid/:slug/faq — list all FAQ nodes for the slug
    let faq_list = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("faq"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_list_faq));

    // GET /pyramid/:slug/faq/match?q=<question> — find best matching FAQ
    let faq_match = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("faq"))
        .and(warp::path("match"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and(warp::query::<FaqMatchQuery>())
        .and_then(handle_match_faq));

    // GET /pyramid/:slug/faq/directory — FAQ directory (flat or hierarchical)
    let faq_directory = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("faq"))
        .and(warp::path("directory"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_faq_directory));

    // GET /pyramid/:slug/faq/category/:id — drill into a FAQ category
    let faq_category_drill = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("faq"))
        .and(warp::path("category"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_faq_category_drill));

    // DELETE /pyramid/:slug
    let delete_slug_route = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::delete())
        .and(with_auth_state(state.clone()))
        .and_then(handle_delete_slug));

    // ── Phase 5: Breaker & Freeze routes ────────────────────────────

    // GET /pyramid/:slug/auto-update/config
    let auto_update_config_get = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("auto-update"))
        .and(warp::path("config"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_auto_update_config_get));

    // POST /pyramid/:slug/auto-update/config
    let auto_update_config_post = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("auto-update"))
        .and(warp::path("config"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and(warp::body::json())
        .and_then(handle_auto_update_config_post));

    // POST /pyramid/:slug/auto-update/freeze
    let auto_update_freeze = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("auto-update"))
        .and(warp::path("freeze"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_auto_update_freeze));

    // POST /pyramid/:slug/auto-update/unfreeze
    let auto_update_unfreeze = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("auto-update"))
        .and(warp::path("unfreeze"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_auto_update_unfreeze));

    // POST /pyramid/:slug/auto-update/l0-sweep
    let auto_update_l0_sweep = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("auto-update"))
        .and(warp::path("l0-sweep"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_auto_update_l0_sweep));

    // POST /pyramid/:slug/auto-update/breaker/resume
    let breaker_resume = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("auto-update"))
        .and(warp::path("breaker"))
        .and(warp::path("resume"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_breaker_resume));

    // POST /pyramid/:slug/auto-update/breaker/build-new
    let breaker_build_new = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("auto-update"))
        .and(warp::path("breaker"))
        .and(warp::path("build-new"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_breaker_build_new));

    // GET /pyramid/:slug/auto-update/status
    let auto_update_status = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("auto-update"))
        .and(warp::path("status"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_auto_update_status));

    // GET /pyramid/:slug/stale-log
    let stale_log = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("stale-log"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and(warp::query::<StaleLogQuery>())
        .and_then(handle_stale_log));

    // ── Phase 6: Cost Observatory route ─────────────────────────────

    // GET /pyramid/:slug/cost
    let cost = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("cost"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and(warp::query::<CostQuery>())
        .and_then(handle_cost));

    // ── P3.3: Crystallization chain pattern routes ────────────────────

    // POST /pyramid/:slug/crystallize — manually trigger a delta check
    let crystallize_trigger = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("crystallize"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and(warp::body::json())
        .and_then(handle_crystallize_trigger));

    // GET /pyramid/:slug/crystallize/status — show crystallization cascade status
    let crystallize_status = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("crystallize"))
        .and(warp::path("status"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_crystallize_status));

    // ── Vine Conversation System routes ─────────────────────────────

    // POST /pyramid/vine/build — must come BEFORE :slug param routes
    let vine_build = route!(prefix
        .and(warp::path("vine"))
        .and(warp::path("build"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and(warp::body::json())
        .and_then(handle_vine_build));

    // GET /pyramid/:slug/vine/bunches
    let vine_bunches = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("vine"))
        .and(warp::path("bunches"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_vine_bunches));

    // GET /pyramid/:slug/vine/eras
    let vine_eras = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("vine"))
        .and(warp::path("eras"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_vine_eras));

    // GET /pyramid/:slug/vine/decisions
    let vine_decisions = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("vine"))
        .and(warp::path("decisions"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_vine_decisions));

    // GET /pyramid/:slug/vine/entities
    let vine_entities = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("vine"))
        .and(warp::path("entities"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_vine_entities));

    // GET /pyramid/:slug/vine/threads
    let vine_threads = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("vine"))
        .and(warp::path("threads"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_vine_threads));

    // GET /pyramid/:slug/vine/drill
    let vine_drill = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("vine"))
        .and(warp::path("drill"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_vine_drill));

    // POST /pyramid/:slug/vine/rebuild-upper
    let vine_rebuild_upper = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("vine"))
        .and(warp::path("rebuild-upper"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_vine_rebuild_upper));

    // POST /pyramid/:slug/vine/integrity
    let vine_integrity = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("vine"))
        .and(warp::path("integrity"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_vine_integrity));

    // GET /pyramid/:slug/vine/build/status
    let vine_build_status = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("vine"))
        .and(warp::path("build"))
        .and(warp::path("status"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_vine_build_status));

    // POST /pyramid/:slug/build/question — decomposed question build (P2.2)
    let question_build = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("build"))
        .and(warp::path("question"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(warp::body::json::<QuestionBuildBody>())
        .and(with_auth_state(state.clone()))
        .and_then(handle_question_build));

    // POST /pyramid/:slug/build/preview — preview decomposition without building (P2.2)
    let question_preview = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("build"))
        .and(warp::path("preview"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::body::json::<QuestionBuildBody>())
        .and(with_auth_state(state.clone()))
        .and_then(handle_question_preview));

    // POST /pyramid/:slug/characterize — characterize source material before build (P1.1)
    let characterize_route = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("characterize"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::body::json::<CharacterizeBody>())
        .and(with_auth_state(state.clone()))
        .and_then(handle_characterize));

    // POST /pyramid/:slug/publish — publish pyramid to Wire (P4.3)
    let publish_pyramid = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("publish"))
        .and(warp::path::end())
        .and(warp::post())
        .and(with_auth_state(state.clone()))
        .and_then(handle_publish_pyramid));

    // POST /pyramid/:slug/publish/question-set — publish question set to Wire (P4.3)
    let publish_question_set = route!(prefix
        .and(warp::path::param::<String>())
        .and(warp::path("publish"))
        .and(warp::path("question-set"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::body::json::<PublishQuestionSetBody>())
        .and(with_auth_state(state.clone()))
        .and_then(handle_publish_question_set));

    // POST /pyramid/chain/import — import a chain or question set from the Wire (P4.2)
    let chain_import = route!(prefix
        .and(warp::path("chain"))
        .and(warp::path("import"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::body::json::<ChainImportBody>())
        .and(with_auth_state(state.clone()))
        .and_then(handle_chain_import));

    // Combine routes. Box in groups to keep the nested Either type manageable.
    // Each .or().unify() flattens a pair, and .boxed() erases the type.
    let r1 = list_slugs.or(create_slug_route).unify().boxed();
    let r2 = build_status.or(build_cancel).unify().boxed();
    // Question build/preview/characterize routes must come before generic build (more specific paths)
    let r2a = question_build
        .or(question_preview)
        .unify()
        .or(characterize_route)
        .unify()
        .boxed();
    let r3 = build.or(apex).unify().boxed();
    let r4 = node.or(tree).unify().boxed();
    let r5 = drill.or(search).unify().boxed();
    let r6 = entities.or(resolved).unify().boxed();
    let r7 = corrections.or(terms).unify().boxed();
    let r8 = ingest_route.or(config_route).unify().boxed();
    let r9 = threads.or(delete_slug_route).unify().boxed();
    let r10 = annotate.or(annotations).unify().boxed();
    let r11 = edges.or(usage).unify().boxed();
    let r12 = meta_run.or(meta_read).unify().boxed();
    let r13 = faq_match.or(faq_list).unify().boxed();
    let r19 = faq_directory.or(faq_category_drill).unify().boxed();
    // Phase 5 & 6 routes
    let r14 = auto_update_config_get
        .or(auto_update_config_post)
        .unify()
        .boxed();
    let r15 = auto_update_freeze.or(auto_update_unfreeze).unify().boxed();
    let r16 = breaker_resume.or(breaker_build_new).unify().boxed();
    let r17 = auto_update_status.or(stale_log).unify().boxed();
    let r20 = auto_update_l0_sweep;
    let r18 = cost;
    // Crystallization routes (P3.3)
    let r26 = crystallize_status.or(crystallize_trigger).unify().boxed();
    // Vine routes
    let r21 = vine_build.or(vine_bunches).unify().boxed();
    let r22 = vine_eras.or(vine_decisions).unify().boxed();
    let r23 = vine_entities.or(vine_threads).unify().boxed();
    let r24 = vine_drill.or(vine_rebuild_upper).unify().boxed();
    let r25 = vine_integrity.or(vine_build_status).unify().boxed();

    // Combine the groups (each is BoxedFilter<(Response,)>)
    let g1 = r1.or(r2).unify().boxed();
    let g1a = r2a.or(r3).unify().boxed();
    let g2 = g1a.or(r4).unify().boxed();
    let g3 = r5.or(r6).unify().boxed();
    let g4 = r7.or(r8).unify().boxed();
    let g5 = r9.or(r10).unify().boxed();
    let g6 = r11.or(r12).unify().boxed();
    let g7 = r13.or(r14).unify().boxed();
    let g8 = r15.or(r16).unify().boxed();
    let g9 = r17.or(r18).unify().boxed();
    let g10 = r19.or(r20).unify().boxed();
    let g11 = r21.or(r22).unify().boxed();
    let g12 = r23.or(r24).unify().boxed();
    let g13 = r25.or(r26).unify().boxed();

    let h1 = g1.or(g2).unify().boxed();
    let h2 = g3.or(g4).unify().boxed();
    let h3 = g5.or(g6).unify().boxed();
    let h4 = g7.or(g8).unify().boxed();
    let h5 = g9.or(g10).unify().boxed();
    let h6 = g11.or(g12).unify().boxed();
    let h7 = g13;

    // Publication routes (P4.3) — slug-parameterized
    let r27 = publish_pyramid.or(publish_question_set).unify().boxed();

    // Chain import route (P4.2) — literal "chain" path must be before slug-parameterized routes
    let h8 = chain_import;

    // CRITICAL: Vine routes (h6, h7) and chain import (h8) with literal path segments MUST come
    // BEFORE slug-parameterized routes, otherwise "vine"/"chain" gets captured as a :slug param.
    let top = h6.or(h7).unify().boxed(); // Vine routes first (literal paths)
    let top = top.or(h8).unify().boxed(); // Chain import (literal paths)
    let top2 = top.or(h1).unify().boxed(); // Then everything else
    let top3 = top2.or(h2).unify().boxed();
    let top4 = top3.or(h3).unify().boxed();
    let top5 = top4.or(h4).unify().boxed();
    let top6 = top5.or(h5).unify().boxed();
    let top7 = top6.or(r27).unify().boxed(); // Publication routes (P4.3)
    top7
}

// ── Route handlers ──────────────────────────────────────────────────

async fn handle_list_slugs(
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match slug::list_slugs(&conn) {
        Ok(slugs) => Ok(json_ok(&slugs)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_create_slug(
    state: Arc<PyramidState>,
    body: CreateSlugBody,
) -> Result<warp::reply::Response, warp::Rejection> {
    let normalized_source_path = match slug::normalize_and_validate_source_path(
        &body.source_path,
        &body.content_type,
        state.data_dir.as_deref(),
    ) {
        Ok(path) => path,
        Err(e) => {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                &e.to_string(),
            ));
        }
    };
    let conn = state.writer.lock().await;
    match slug::create_slug(
        &conn,
        &body.slug,
        &body.content_type,
        &normalized_source_path,
    ) {
        Ok(info) => Ok(warp::reply::with_status(
            warp::reply::json(&info),
            warp::http::StatusCode::CREATED,
        )
        .into_response()),
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
    agent_id: Option<String>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::get_apex_with_edges(&conn, &slug_name) {
        Ok(Some(node)) => {
            let response = json_ok(&node);
            log_query_usage(
                state.writer.clone(),
                slug_name,
                "apex".to_string(),
                "{}".to_string(),
                vec![node.node.id.clone()],
                agent_id,
            );
            Ok(response)
        }
        Ok(None) => Ok(json_error(
            warp::http::StatusCode::NOT_FOUND,
            "No apex node found",
        )),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_node(
    slug_name: String,
    node_id: String,
    state: Arc<PyramidState>,
    agent_id: Option<String>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::get_node_with_edges(&conn, &slug_name, &node_id) {
        Ok(Some(node)) => {
            let response = json_ok(&node);
            log_query_usage(
                state.writer.clone(),
                slug_name,
                "node".to_string(),
                serde_json::json!({"node_id": node_id}).to_string(),
                vec![node.node.id.clone()],
                agent_id,
            );
            Ok(response)
        }
        Ok(None) => Ok(json_error(
            warp::http::StatusCode::NOT_FOUND,
            "Node not found",
        )),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_tree(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::get_tree(&conn, &slug_name) {
        Ok(tree) => Ok(json_ok(&tree)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_drill(
    slug_name: String,
    node_id: String,
    state: Arc<PyramidState>,
    agent_id: Option<String>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::drill(&conn, &slug_name, &node_id) {
        Ok(Some(result)) => {
            let response = json_ok(&result);
            let mut ids = vec![result.node.id.clone()];
            for child in &result.children {
                ids.push(child.id.clone());
            }
            log_query_usage(
                state.writer.clone(),
                slug_name,
                "drill".to_string(),
                serde_json::json!({"node_id": node_id}).to_string(),
                ids,
                agent_id,
            );
            Ok(response)
        }
        Ok(None) => Ok(json_error(
            warp::http::StatusCode::NOT_FOUND,
            "Node not found",
        )),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_search(
    slug_name: String,
    state: Arc<PyramidState>,
    params: SearchQuery,
    agent_id: Option<String>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::search(&conn, &slug_name, &params.q) {
        Ok(hits) => {
            let response = json_ok(&hits);
            let ids: Vec<String> = hits.iter().map(|h| h.node_id.clone()).collect();
            log_query_usage(
                state.writer.clone(),
                slug_name,
                "search".to_string(),
                serde_json::json!({"q": params.q}).to_string(),
                ids,
                agent_id,
            );
            Ok(response)
        }
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_usage(
    slug_name: String,
    state: Arc<PyramidState>,
    params: UsageQuery,
) -> Result<warp::reply::Response, warp::Rejection> {
    let limit = params.limit.unwrap_or(100);
    let conn = state.reader.lock().await;
    match db::get_usage_log(&conn, &slug_name, limit) {
        Ok(entries) => Ok(json_ok(&entries)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_entities(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::entities(&conn, &slug_name) {
        Ok(entries) => Ok(json_ok(&entries)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_resolved(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::resolved(&conn, &slug_name) {
        Ok(entries) => Ok(json_ok(&entries)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_corrections(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::corrections(&conn, &slug_name) {
        Ok(entries) => Ok(json_ok(&entries)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_terms(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match query::terms(&conn, &slug_name) {
        Ok(entries) => Ok(json_ok(&entries)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_build(
    slug_name: String,
    query: std::collections::HashMap<String, String>,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let from_depth: i64 = query
        .get("from_depth")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // Verify slug exists before taking the write lock
    {
        let conn = state.reader.lock().await;
        match slug::get_slug(&conn, &slug_name) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    "Slug not found",
                ));
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
        if let Some(handle) = active.get(&slug_name) {
            let s = handle.status.read().await;
            let is_terminal = s.is_terminal();
            drop(s);
            if !handle.cancel.is_cancelled() && !is_terminal {
                return Ok(json_error(
                    warp::http::StatusCode::CONFLICT,
                    "Build already running for this slug",
                ));
            }
        }

        let handle = super::BuildHandle {
            slug: slug_name.clone(),
            cancel: cancel.clone(),
            status: status.clone(),
        };
        active.insert(slug_name.clone(), handle);
    }

    // Spawn the build task
    let build_state = state.clone();
    let writer = state.writer.clone();
    let build_status = status.clone();

    tokio::spawn(async move {
        let start = std::time::Instant::now();

        // Create mpsc channel for WriteOps (used by legacy path)
        let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<WriteOp>(256);

        // Spawn the writer task that consumes WriteOps using the writer connection
        let writer_handle = {
            let writer_conn = writer.clone();
            tokio::spawn(async move {
                while let Some(op) = write_rx.recv().await {
                    let result = {
                        let conn = writer_conn.lock().await;
                        match op {
                            WriteOp::SaveNode {
                                ref node,
                                ref topics_json,
                            } => db::save_node(&conn, node, topics_json.as_deref()),
                            WriteOp::SaveStep {
                                ref slug,
                                ref step_type,
                                chunk_index,
                                depth,
                                ref node_id,
                                ref output_json,
                                ref model,
                                elapsed,
                            } => db::save_step(
                                &conn,
                                slug,
                                step_type,
                                chunk_index,
                                depth,
                                node_id,
                                output_json,
                                model,
                                elapsed,
                            ),
                            WriteOp::UpdateParent {
                                ref slug,
                                ref node_id,
                                ref parent_id,
                            } => db::update_parent(&conn, slug, node_id, parent_id),
                            WriteOp::UpdateStats { ref slug } => db::update_slug_stats(&conn, slug),
                            WriteOp::Flush { done } => {
                                let _ = done.send(());
                                Ok(())
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

        // Unified build dispatch — chain engine or legacy based on feature flag
        let result = super::build_runner::run_build_from(
            &build_state,
            &slug_name,
            from_depth,
            &cancel,
            Some(progress_tx.clone()),
            &write_tx,
        )
        .await;

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
                    Ok((_apex_id, failures)) => {
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
                        s.progress = super::types::BuildProgress {
                            done: s.progress.total,
                            total: s.progress.total,
                        };
                    }
                    Err(ref e) => {
                        s.status = "failed".to_string();
                        s.progress = super::types::BuildProgress {
                            done: s.progress.total,
                            total: s.progress.total,
                        };
                        tracing::error!("Build failed for '{}': {e}", slug_name);
                    }
                }
            }
            s.elapsed_seconds = start.elapsed().as_secs_f64();
        }
    });

    // Return initial status
    let s = status.read().await;
    Ok(
        warp::reply::with_status(warp::reply::json(&*s), warp::http::StatusCode::ACCEPTED)
            .into_response(),
    )
}

async fn handle_build_status(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let active = state.active_build.read().await;
    if let Some(handle) = active.get(&slug_name) {
        let s = handle.status.read().await;
        return Ok(json_ok(&*s));
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
    let maybe_handle = {
        let active = state.active_build.read().await;
        active
            .get(&slug_name)
            .map(|handle| (handle.cancel.clone(), handle.status.clone()))
    };

    if let Some((cancel, status)) = maybe_handle {
        let s = status.read().await;
        if s.is_running() && !cancel.is_cancelled() {
            drop(s);
            cancel.cancel();
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
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    "Slug not found",
                ));
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
    let paths = match slug::resolve_validated_source_paths(
        &source_path,
        &content_type,
        state.data_dir.as_deref(),
    ) {
        Ok(paths) => paths,
        Err(e) => {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                &e.to_string(),
            ));
        }
    };

    // Run synchronous ingest on a blocking thread
    let writer = state.writer.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = writer.blocking_lock();
        for path in &paths {
            match content_type {
                ContentType::Code => {
                    let _ = ingest::ingest_code(&conn, &slug_clone, path)?;
                }
                ContentType::Conversation => {
                    ingest::ingest_conversation(&conn, &slug_clone, path)?;
                }
                ContentType::Document => {
                    let _ = ingest::ingest_docs(&conn, &slug_clone, path)?;
                }
                ContentType::Vine => {
                    return Err(anyhow::anyhow!(
                        "Use POST /pyramid/vine/build for vine ingestion"
                    ));
                }
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
    if let Some(ref model) = body.primary_model {
        config.primary_model = model.clone();
    }
    if let Some(ref model) = body.fallback_model_1 {
        config.fallback_model_1 = model.clone();
    }
    if let Some(ref model) = body.fallback_model_2 {
        config.fallback_model_2 = model.clone();
    }

    if let Some(use_ir) = body.use_ir_executor {
        state
            .use_ir_executor
            .store(use_ir, std::sync::atomic::Ordering::Relaxed);
        tracing::info!("IR executor toggled to: {use_ir}");
    }

    // Persist to config file if data_dir is set
    if let Some(ref data_dir) = state.data_dir {
        // Load existing config to preserve fields not managed by this endpoint
        let mut pyramid_config = super::PyramidConfig::load(data_dir);
        pyramid_config.openrouter_api_key = config.api_key.clone();
        pyramid_config.primary_model = config.primary_model.clone();
        pyramid_config.fallback_model_1 = config.fallback_model_1.clone();
        pyramid_config.fallback_model_2 = config.fallback_model_2.clone();
        pyramid_config.use_ir_executor = state
            .use_ir_executor
            .load(std::sync::atomic::Ordering::Relaxed);
        if let Err(e) = pyramid_config.save(data_dir) {
            tracing::error!("Failed to save pyramid config: {e}");
        }
    }

    Ok(json_ok(&serde_json::json!({
        "status": "updated",
        "primary_model": config.primary_model,
        "fallback_model_1": config.fallback_model_1,
        "fallback_model_2": config.fallback_model_2,
        "use_ir_executor": state.use_ir_executor.load(std::sync::atomic::Ordering::Relaxed),
    })))
}

async fn handle_delete_slug(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Don't allow deleting a slug with an active build
    {
        let active = state.active_build.read().await;
        if let Some(handle) = active.get(&slug_name) {
            let s = handle.status.read().await;
            if s.is_running() && !handle.cancel.is_cancelled() {
                return Ok(json_error(
                    warp::http::StatusCode::CONFLICT,
                    "Cannot delete slug while build is running",
                ));
            }
        }
    }

    let conn = state.writer.lock().await;
    let result = slug::delete_slug(&conn, &slug_name);
    drop(conn);

    match result {
        Ok(()) => {
            let mut active = state.active_build.write().await;
            active.remove(&slug_name);
            Ok(json_ok(&serde_json::json!({"deleted": slug_name})))
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                Ok(json_error(warp::http::StatusCode::NOT_FOUND, &msg))
            } else {
                Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &msg,
                ))
            }
        }
    }
}

async fn handle_threads(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match db::get_threads(&conn, &slug_name) {
        Ok(threads) => Ok(json_ok(&threads)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_annotate(
    slug_name: String,
    state: Arc<PyramidState>,
    body: AnnotateBody,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Validate slug and node exist
    {
        let conn = state.reader.lock().await;
        match slug::get_slug(&conn, &slug_name) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    "Slug not found",
                ));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
        match db::get_node(&conn, &slug_name, &body.node_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    &format!("Node '{}' not found in slug '{}'", body.node_id, slug_name),
                ));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    }

    let annotation = PyramidAnnotation {
        id: 0, // will be set by DB
        slug: slug_name,
        node_id: body.node_id,
        annotation_type: AnnotationType::from_str(&body.annotation_type),
        content: body.content,
        question_context: body.question_context,
        author: body.author.unwrap_or_else(|| "system".to_string()),
        created_at: String::new(), // will be set by DB default
    };

    let saved = {
        let conn = state.writer.lock().await;
        match db::save_annotation(&conn, &annotation) {
            Ok(saved_annotation) => saved_annotation,
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    };

    // Post-save hook: process annotation in background (non-blocking)
    let annotation_clone = saved.clone();
    let writer_clone = state.writer.clone();
    let reader_clone = state.reader.clone();
    let api_key = { state.config.read().await.api_key.clone() };
    let model = { state.config.read().await.primary_model.clone() };
    let slug_clone = saved.slug.clone();

    tokio::spawn(async move {
        if let Err(e) = process_annotation_hook(
            &reader_clone,
            &writer_clone,
            &slug_clone,
            &annotation_clone,
            &api_key,
            &model,
        )
        .await
        {
            tracing::warn!("[annotation] post-save hook failed: {}", e);
        }
    });

    Ok(
        warp::reply::with_status(warp::reply::json(&saved), warp::http::StatusCode::CREATED)
            .into_response(),
    )
}

/// Background hook that runs after an annotation is saved.
/// Correction annotations create deltas on the matching thread.
/// Other types are logged for future FAQ/review processing.
async fn process_annotation_hook(
    reader: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    writer: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    slug: &str,
    annotation: &PyramidAnnotation,
    api_key: &str,
    model: &str,
) -> anyhow::Result<()> {
    match annotation.annotation_type {
        AnnotationType::Correction => {
            // Correction annotations create deltas on the relevant thread
            let threads = {
                let conn = reader.lock().await;
                db::get_threads(&conn, slug)?
            };

            // Find the thread whose canonical node matches the annotated node
            let target_thread = threads
                .iter()
                .find(|t| t.current_canonical_id == annotation.node_id);

            if let Some(thread) = target_thread {
                let delta_content = format!(
                    "CORRECTION (from annotation #{}): {}",
                    annotation.id, annotation.content
                );

                delta::create_delta(
                    reader,
                    writer,
                    slug,
                    &thread.thread_id,
                    &delta_content,
                    Some(&annotation.node_id),
                    api_key,
                    model,
                )
                .await?;

                tracing::info!(
                    "[annotation] correction annotation #{} created delta on thread '{}'",
                    annotation.id,
                    thread.thread_id
                );
            } else {
                tracing::info!("[annotation] correction annotation #{} on node '{}' — no matching thread found, skipping delta",
                    annotation.id, annotation.node_id);
            }
        }

        AnnotationType::Observation | AnnotationType::Idea => {
            // Observations and ideas flag the thread for review
            tracing::info!(
                "[annotation] {} annotation #{} on node '{}' — logged for FAQ processing",
                annotation.annotation_type.as_str(),
                annotation.id,
                annotation.node_id
            );
        }

        AnnotationType::Question => {
            // Questions get processed by the FAQ system (separate workstream)
            tracing::info!(
                "[annotation] question annotation #{} on node '{}' — logged for FAQ processing",
                annotation.id,
                annotation.node_id
            );
        }

        AnnotationType::Friction => {
            // Friction is logged but doesn't trigger deltas
            tracing::info!(
                "[annotation] friction annotation #{} on node '{}' — logged",
                annotation.id,
                annotation.node_id
            );
        }

        AnnotationType::Era => {
            // ERA annotations mark project phase boundaries on vine nodes
            tracing::info!(
                "[annotation] ERA annotation #{} on node '{}' — vine intelligence",
                annotation.id,
                annotation.node_id
            );
        }

        AnnotationType::Transition => {
            // Transition annotations classify phase shifts between ERAs
            tracing::info!(
                "[annotation] transition annotation #{} on node '{}' — vine intelligence",
                annotation.id,
                annotation.node_id
            );
        }

        AnnotationType::HealthCheck => {
            // Health check results from vine integrity pass
            tracing::info!(
                "[annotation] health_check annotation #{} on node '{}' — vine integrity",
                annotation.id,
                annotation.node_id
            );
        }

        AnnotationType::Directory => {
            // Sub-apex directory wiring for vine navigation
            tracing::info!(
                "[annotation] directory annotation #{} on node '{}' — vine directory",
                annotation.id,
                annotation.node_id
            );
        }
    }

    // FAQ processing — for any annotation with question_context
    if annotation.question_context.is_some() {
        match faq::process_annotation(reader, writer, slug, annotation, api_key, model).await {
            Ok(Some(faq_node)) => {
                tracing::info!(
                    "[annotation] FAQ processed: annotation #{} → FAQ '{}'",
                    annotation.id,
                    faq_node.id
                );
            }
            Ok(None) => {
                tracing::debug!(
                    "[annotation] no FAQ generated for annotation #{}",
                    annotation.id
                );
            }
            Err(e) => {
                tracing::warn!(
                    "[annotation] FAQ processing failed for annotation #{}: {}",
                    annotation.id,
                    e
                );
            }
        }
    }

    Ok(())
}

async fn handle_annotations(
    slug_name: String,
    state: Arc<PyramidState>,
    params: AnnotationsQuery,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    let result = if let Some(ref node_id) = params.node_id {
        db::get_annotations(&conn, &slug_name, node_id)
    } else {
        db::get_all_annotations(&conn, &slug_name)
    };
    match result {
        Ok(annotations) => Ok(json_ok(&annotations)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_edges(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match webbing::get_active_edges(&conn, &slug_name, 0.1) {
        Ok(edges) => Ok(json_ok(&edges)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_meta_run(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Verify slug exists
    {
        let conn = state.reader.lock().await;
        match slug::get_slug(&conn, &slug_name) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    "Slug not found",
                ));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    }

    // Get LLM config
    let (api_key, model) = {
        let config = state.config.read().await;
        (config.api_key.clone(), config.primary_model.clone())
    };

    let reader = state.reader.clone();
    let writer = state.writer.clone();

    match meta::run_all_meta_passes(&reader, &writer, &slug_name, &api_key, &model).await {
        Ok(quickstart) => Ok(json_ok(&serde_json::json!({
            "slug": slug_name,
            "status": "complete",
            "quickstart": quickstart,
        }))),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_meta_read(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match meta::get_meta_nodes(&conn, &slug_name) {
        Ok(nodes) => Ok(json_ok(&nodes)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

// ── FAQ route handlers ──────────────────────────────────────────────

async fn handle_list_faq(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match db::get_faq_nodes(&conn, &slug_name) {
        Ok(faqs) => Ok(json_ok(&faqs)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_match_faq(
    slug_name: String,
    state: Arc<PyramidState>,
    params: FaqMatchQuery,
) -> Result<warp::reply::Response, warp::Rejection> {
    let config = state.config.read().await;
    let api_key = config.api_key.clone();
    let model = config.primary_model.clone();
    drop(config);

    match faq::match_faq(
        &state.reader,
        &state.writer,
        &slug_name,
        &params.q,
        &api_key,
        &model,
    )
    .await
    {
        Ok(Some(faq_node)) => Ok(json_ok(&faq_node)),
        Ok(None) => Ok(json_ok(&serde_json::json!(null))),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

// ── FAQ Directory route handlers ─────────────────────────────────────

async fn handle_faq_directory(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let config = state.config.read().await;
    let api_key = config.api_key.clone();
    let model = config.primary_model.clone();
    drop(config);

    match faq::get_faq_directory(&state.reader, &state.writer, &slug_name, &api_key, &model).await {
        Ok(directory) => Ok(json_ok(&directory)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_faq_category_drill(
    slug_name: String,
    category_id: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    match faq::drill_faq_category(&state.reader, &slug_name, &category_id).await {
        Ok(entry) => Ok(json_ok(&entry)),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                Ok(json_error(warp::http::StatusCode::NOT_FOUND, &msg))
            } else {
                Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &msg,
                ))
            }
        }
    }
}

// ── Phase 5: Breaker & Freeze route handlers ────────────────────────

/// Helper: load AutoUpdateConfig from DB for a given slug.
fn load_auto_update_config_from_db(conn: &Connection, slug: &str) -> Option<AutoUpdateConfig> {
    conn.query_row(
        "SELECT slug, auto_update, debounce_minutes, min_changed_files,
                runaway_threshold, breaker_tripped, breaker_tripped_at, frozen, frozen_at
         FROM pyramid_auto_update_config WHERE slug = ?1",
        rusqlite::params![slug],
        |row| {
            Ok(AutoUpdateConfig {
                slug: row.get(0)?,
                auto_update: row.get::<_, i32>(1)? != 0,
                debounce_minutes: row.get(2)?,
                min_changed_files: row.get(3)?,
                runaway_threshold: row.get(4)?,
                breaker_tripped: row.get::<_, i32>(5)? != 0,
                breaker_tripped_at: row.get(6)?,
                frozen: row.get::<_, i32>(7)? != 0,
                frozen_at: row.get(8)?,
            })
        },
    )
    .ok()
}

/// GET /pyramid/:slug/auto-update/config
async fn handle_auto_update_config_get(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match load_auto_update_config_from_db(&conn, &slug_name) {
        Some(config) => Ok(json_ok(&config)),
        None => Ok(json_error(
            warp::http::StatusCode::NOT_FOUND,
            &format!("No auto-update config for slug '{}'", slug_name),
        )),
    }
}

/// POST /pyramid/:slug/auto-update/config
async fn handle_auto_update_config_post(
    slug_name: String,
    state: Arc<PyramidState>,
    body: AutoUpdateConfigBody,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.writer.lock().await;

    // Build a dynamic UPDATE query from supplied fields
    let mut sets: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(d) = body.debounce_minutes {
        if d < 1 {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                "debounce_minutes must be >= 1",
            ));
        }
        sets.push(format!("debounce_minutes = ?{}", params.len() + 1));
        params.push(Box::new(d));
    }
    if let Some(m) = body.min_changed_files {
        sets.push(format!("min_changed_files = ?{}", params.len() + 1));
        params.push(Box::new(m));
    }
    if let Some(r) = body.runaway_threshold {
        if r <= 0.0 || r > 1.0 {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                "runaway_threshold must be > 0.0 and <= 1.0",
            ));
        }
        sets.push(format!("runaway_threshold = ?{}", params.len() + 1));
        params.push(Box::new(r));
    }
    if let Some(a) = body.auto_update {
        sets.push(format!("auto_update = ?{}", params.len() + 1));
        params.push(Box::new(if a { 1i32 } else { 0i32 }));
    }

    if sets.is_empty() {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            "No fields to update",
        ));
    }

    let slug_idx = params.len() + 1;
    params.push(Box::new(slug_name.clone()));
    let sql = format!(
        "UPDATE pyramid_auto_update_config SET {} WHERE slug = ?{}",
        sets.join(", "),
        slug_idx
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    match conn.execute(&sql, param_refs.as_slice()) {
        Ok(0) => Ok(json_error(
            warp::http::StatusCode::NOT_FOUND,
            &format!("No auto-update config for slug '{}'", slug_name),
        )),
        Ok(_) => {
            // Return the updated config
            match load_auto_update_config_from_db(&conn, &slug_name) {
                Some(config) => Ok(json_ok(&config)),
                None => Ok(json_ok(&serde_json::json!({"status": "updated"}))),
            }
        }
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

/// POST /pyramid/:slug/auto-update/freeze
async fn handle_auto_update_freeze(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let mut engines = state.stale_engines.lock().await;
    if let Some(engine) = engines.get_mut(&slug_name) {
        engine.freeze();
    } else {
        // No engine in memory — update DB directly
        let conn = state.writer.lock().await;
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let _ = conn.execute(
            "UPDATE pyramid_auto_update_config SET frozen = 1, frozen_at = ?1 WHERE slug = ?2",
            rusqlite::params![now, slug_name],
        );
        let _ = conn.execute(
            "UPDATE pyramid_pending_mutations SET processed = 1 WHERE processed = 0 AND slug = ?1",
            rusqlite::params![slug_name],
        );
    }
    // Pause file watcher
    let mut watchers = state.file_watchers.lock().await;
    if let Some(watcher) = watchers.get_mut(&slug_name) {
        watcher.pause();
    }

    Ok(json_ok(
        &serde_json::json!({"status": "frozen", "slug": slug_name}),
    ))
}

/// POST /pyramid/:slug/auto-update/unfreeze
/// Unfreezes the engine and triggers a hash rescan of all watched files.
async fn handle_auto_update_unfreeze(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Unfreeze the engine
    let mut engines = state.stale_engines.lock().await;
    if let Some(engine) = engines.get_mut(&slug_name) {
        engine.unfreeze();
    } else {
        // No engine in memory — update DB directly
        let conn = state.writer.lock().await;
        let _ = conn.execute(
            "UPDATE pyramid_auto_update_config SET frozen = 0, frozen_at = NULL WHERE slug = ?1",
            rusqlite::params![slug_name],
        );
    }
    drop(engines);

    // Resume file watcher (repopulates caches from DB)
    let db_path = state
        .data_dir
        .as_ref()
        .expect("data_dir not set")
        .join("pyramid.db")
        .to_string_lossy()
        .to_string();
    let mut watchers = state.file_watchers.lock().await;
    if let Some(watcher) = watchers.get_mut(&slug_name) {
        watcher.resume(&db_path);
    }
    drop(watchers);

    // Hash rescan: read all files in pyramid_file_hashes, compute current hashes,
    // compare, write mutations for any differences.
    let mutations_written = {
        let conn = state.writer.lock().await;
        hash_rescan(&conn, &slug_name)
    };

    // Notify the engine about new mutations so it restarts timers
    if mutations_written > 0 {
        let mut engines = state.stale_engines.lock().await;
        if let Some(engine) = engines.get_mut(&slug_name) {
            engine.notify_mutation(0);
        }
    }

    Ok(json_ok(&serde_json::json!({
        "status": "unfrozen",
        "slug": slug_name,
        "mutations_from_rescan": mutations_written,
    })))
}

/// Rescan all tracked files for a slug, comparing current hashes against stored hashes.
/// Writes `file_change` mutations for any differences. Returns count of mutations written.
fn hash_rescan(conn: &Connection, slug: &str) -> i64 {
    use hex;
    use sha2::{Digest, Sha256};

    let mut stmt =
        match conn.prepare("SELECT file_path, hash FROM pyramid_file_hashes WHERE slug = ?1") {
            Ok(s) => s,
            Err(_) => return 0,
        };

    let rows: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![slug], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .ok()
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut count = 0i64;

    for (file_path, stored_hash) in &rows {
        let current_hash = match std::fs::read(file_path) {
            Ok(data) => {
                let mut hasher = Sha256::new();
                hasher.update(&data);
                hex::encode(hasher.finalize())
            }
            Err(_) => {
                // File was deleted during freeze — write deleted_file mutation
                let _ = conn.execute(
                    "INSERT INTO pyramid_pending_mutations
                     (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at)
                     VALUES (?1, 0, 'deleted_file', ?2, 'Detected during unfreeze rescan', 0, ?3)",
                    rusqlite::params![slug, file_path, now],
                );
                count += 1;
                continue;
            }
        };

        if current_hash != *stored_hash {
            let _ = conn.execute(
                "INSERT INTO pyramid_pending_mutations
                 (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at)
                 VALUES (?1, 0, 'file_change', ?2, 'Detected during unfreeze rescan', 0, ?3)",
                rusqlite::params![slug, file_path, now],
            );
            count += 1;
        }
    }

    count
}

/// Force a full L0 sweep by enqueueing one pending mutation for every tracked file
/// that is not already waiting in the WAL.
pub fn enqueue_full_l0_sweep(conn: &Connection, slug: &str) -> (i64, i64, i64) {
    let mut stmt = match conn
        .prepare("SELECT file_path FROM pyramid_file_hashes WHERE slug = ?1 ORDER BY file_path ASC")
    {
        Ok(stmt) => stmt,
        Err(_) => return (0, 0, 0),
    };

    let file_paths: Vec<String> = stmt
        .query_map(rusqlite::params![slug], |row| row.get::<_, String>(0))
        .ok()
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut enqueued = 0i64;
    let mut already_pending = 0i64;

    for file_path in &file_paths {
        let pending_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_pending_mutations
                 WHERE slug = ?1 AND layer = 0 AND processed = 0
                   AND target_ref = ?2
                   AND mutation_type IN ('file_change', 'deleted_file')",
                rusqlite::params![slug, file_path],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if pending_count > 0 {
            already_pending += 1;
            continue;
        }

        let exists_on_disk = std::path::Path::new(file_path).exists();
        let mutation_type = if exists_on_disk {
            "file_change"
        } else {
            "deleted_file"
        };
        let detail = if exists_on_disk {
            "Forced full L0 sweep"
        } else {
            "Forced full L0 sweep (file missing)"
        };

        let _ = conn.execute(
            "INSERT INTO pyramid_pending_mutations
             (slug, layer, mutation_type, target_ref, detail, cascade_depth, detected_at, processed)
             VALUES (?1, 0, ?2, ?3, ?4, 0, ?5, 0)",
            rusqlite::params![slug, mutation_type, file_path, detail, now],
        );
        enqueued += 1;
    }

    (file_paths.len() as i64, enqueued, already_pending)
}

/// POST /pyramid/:slug/auto-update/breaker/resume
async fn handle_breaker_resume(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let mut engines = state.stale_engines.lock().await;
    if let Some(engine) = engines.get_mut(&slug_name) {
        engine.resume_breaker();
        Ok(json_ok(
            &serde_json::json!({"status": "resumed", "slug": slug_name}),
        ))
    } else {
        // No engine in memory — update DB directly
        let conn = state.writer.lock().await;
        let _ = conn.execute(
            "UPDATE pyramid_auto_update_config SET breaker_tripped = 0, breaker_tripped_at = NULL WHERE slug = ?1",
            rusqlite::params![slug_name],
        );
        Ok(json_ok(
            &serde_json::json!({"status": "resumed", "slug": slug_name, "note": "No active engine, breaker cleared in DB"}),
        ))
    }
}

/// POST /pyramid/:slug/auto-update/breaker/build-new
/// Creates a new slug `{slug}-{YYYYMMDD}`, archives the old one, triggers full build on new.
async fn handle_breaker_build_new(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Get old slug info
    let slug_info = {
        let conn = state.reader.lock().await;
        match slug::get_slug(&conn, &slug_name) {
            Ok(Some(info)) => info,
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    "Slug not found",
                ));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    };

    // Freeze the old pyramid
    {
        let mut engines = state.stale_engines.lock().await;
        if let Some(engine) = engines.get_mut(&slug_name) {
            engine.freeze();
        }
        // Also remove it from active watchers (archived = excluded from watcher)
        let mut watchers = state.file_watchers.lock().await;
        if let Some(watcher) = watchers.get_mut(&slug_name) {
            watcher.stop();
        }
        watchers.remove(&slug_name);
        engines.remove(&slug_name);
    }

    // Create new slug with date suffix
    let date_suffix = chrono::Utc::now().format("%Y%m%d").to_string();
    let new_slug = format!("{}-{}", slug_name, date_suffix);

    {
        let conn = state.writer.lock().await;
        match slug::create_slug(
            &conn,
            &new_slug,
            &slug_info.content_type,
            &slug_info.source_path,
        ) {
            Ok(_) => {}
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
        // Create auto-update config for the new slug with defaults
        let _ = conn.execute(
            "INSERT OR IGNORE INTO pyramid_auto_update_config (slug) VALUES (?1)",
            rusqlite::params![new_slug],
        );
    }

    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "status": "created",
            "old_slug": slug_name,
            "new_slug": new_slug,
            "note": "Old pyramid archived (frozen + no watcher). Trigger POST /pyramid/{new_slug}/build to start full build."
        })),
        warp::http::StatusCode::CREATED,
    )
    .into_response())
}

/// GET /pyramid/:slug/auto-update/status
async fn handle_auto_update_status(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    let config = match load_auto_update_config_from_db(&conn, &slug_name) {
        Some(c) => c,
        None => {
            return Ok(json_error(
                warp::http::StatusCode::NOT_FOUND,
                &format!("No auto-update config for slug '{}'", slug_name),
            ));
        }
    };

    // Count pending mutations by layer
    let mut pending_by_layer = std::collections::HashMap::new();
    for layer in 0..=3 {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pyramid_pending_mutations
                 WHERE processed = 0 AND slug = ?1 AND layer = ?2",
                rusqlite::params![slug_name, layer],
                |row| row.get(0),
            )
            .unwrap_or(0);
        pending_by_layer.insert(layer, count);
    }

    // Get last check time
    let last_check_at: Option<String> = conn
        .query_row(
            "SELECT MAX(checked_at) FROM pyramid_stale_check_log WHERE slug = ?1",
            rusqlite::params![slug_name],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    let response = AutoUpdateStatusResponse {
        auto_update: config.auto_update,
        frozen: config.frozen,
        breaker_tripped: config.breaker_tripped,
        pending_mutations_by_layer: pending_by_layer,
        last_check_at,
    };

    Ok(json_ok(&response))
}

/// GET /pyramid/:slug/stale-log
async fn handle_stale_log(
    slug_name: String,
    state: Arc<PyramidState>,
    params: StaleLogQuery,
) -> Result<warp::reply::Response, warp::Rejection> {
    let limit = params.limit.unwrap_or(100);
    let offset = params.offset.unwrap_or(0);
    let conn = state.reader.lock().await;

    // Bug 3 fix: Delegate to db::get_stale_log instead of duplicating the query inline.
    match db::get_stale_log(
        &conn,
        &slug_name,
        params.layer,
        params.stale.as_deref(),
        limit,
        offset,
    ) {
        Ok(rows) => Ok(json_ok(&rows)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

// ── Phase 6: Cost Observatory route handler ─────────────────────────

/// GET /pyramid/:slug/cost
async fn handle_cost(
    slug_name: String,
    state: Arc<PyramidState>,
    params: CostQuery,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;

    // Parse time window
    let window_clause = match params.window.as_deref() {
        Some("24h") => "AND created_at >= datetime('now', '-1 day')",
        Some("7d") => "AND created_at >= datetime('now', '-7 days')",
        Some("30d") => "AND created_at >= datetime('now', '-30 days')",
        _ => "", // all time
    };

    // Total spend and calls
    let (total_spend, total_calls): (f64, i64) = conn
        .query_row(
            &format!(
                "SELECT COALESCE(SUM(estimated_cost), 0.0), COUNT(*) FROM pyramid_cost_log
                 WHERE slug = ?1 {}",
                window_clause
            ),
            rusqlite::params![slug_name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((0.0, 0));

    // By source (manual vs auto_stale)
    let by_source = {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT COALESCE(source, 'manual'), COALESCE(SUM(estimated_cost), 0.0), COUNT(*)
             FROM pyramid_cost_log WHERE slug = ?1 {}
             GROUP BY COALESCE(source, 'manual')",
                window_clause
            ))
            .unwrap();
        let rows: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![slug_name], |row| {
                Ok(serde_json::json!({
                    "source": row.get::<_, String>(0)?,
                    "spend": row.get::<_, f64>(1)?,
                    "calls": row.get::<_, i64>(2)?,
                }))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        rows
    };

    // By check_type
    let by_check_type = {
        let mut stmt = conn
            .prepare(&format!(
            "SELECT COALESCE(check_type, 'unknown'), COALESCE(SUM(estimated_cost), 0.0), COUNT(*)
             FROM pyramid_cost_log WHERE slug = ?1 {}
             GROUP BY COALESCE(check_type, 'unknown')", window_clause
        ))
            .unwrap();
        let rows: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![slug_name], |row| {
                Ok(serde_json::json!({
                    "check_type": row.get::<_, String>(0)?,
                    "spend": row.get::<_, f64>(1)?,
                    "calls": row.get::<_, i64>(2)?,
                }))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        rows
    };

    // By layer
    let by_layer = {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT COALESCE(layer, -1), COALESCE(SUM(estimated_cost), 0.0), COUNT(*)
             FROM pyramid_cost_log WHERE slug = ?1 {}
             GROUP BY COALESCE(layer, -1)",
                window_clause
            ))
            .unwrap();
        let rows: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![slug_name], |row| {
                Ok(serde_json::json!({
                    "layer": row.get::<_, i32>(0)?,
                    "spend": row.get::<_, f64>(1)?,
                    "calls": row.get::<_, i64>(2)?,
                }))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        rows
    };

    // Recent calls (last 20)
    let recent_calls = {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id, operation, model, input_tokens, output_tokens, estimated_cost,
                    COALESCE(source, 'manual'), layer, check_type, created_at,
                    chain_id, step_name, tier, latency_ms, generation_id, estimated_cost_usd
             FROM pyramid_cost_log WHERE slug = ?1 {}
             ORDER BY created_at DESC LIMIT 20",
                window_clause
            ))
            .unwrap();
        let rows: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![slug_name], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, i64>(0)?,
                    "operation": row.get::<_, String>(1)?,
                    "model": row.get::<_, String>(2)?,
                    "input_tokens": row.get::<_, i64>(3)?,
                    "output_tokens": row.get::<_, i64>(4)?,
                    "cost_usd": row.get::<_, f64>(5)?,
                    "source": row.get::<_, String>(6)?,
                    "layer": row.get::<_, Option<i32>>(7)?,
                    "check_type": row.get::<_, Option<String>>(8)?,
                    "created_at": row.get::<_, String>(9)?,
                    "chain_id": row.get::<_, Option<String>>(10)?,
                    "step_name": row.get::<_, Option<String>>(11)?,
                    "tier": row.get::<_, Option<String>>(12)?,
                    "latency_ms": row.get::<_, Option<i64>>(13)?,
                    "generation_id": row.get::<_, Option<String>>(14)?,
                    "estimated_cost_usd": row.get::<_, Option<f64>>(15)?,
                }))
            })
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        rows
    };

    Ok(json_ok(&serde_json::json!({
        "slug": slug_name,
        "total_spend": total_spend,
        "total_calls": total_calls,
        "by_source": by_source,
        "by_check_type": by_check_type,
        "by_layer": by_layer,
        "recent_calls": recent_calls,
    })))
}

/// POST /pyramid/:slug/auto-update/l0-sweep
///
/// Enqueue every tracked L0 file for a fresh stale check, then immediately
/// drain layers 0..=3 so the full cascade runs without waiting for the poll loop.
async fn handle_auto_update_l0_sweep(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let (tracked_files, enqueued, already_pending) = {
        let conn = state.writer.lock().await;
        enqueue_full_l0_sweep(&conn, &slug_name)
    };

    let engine_data = {
        let engines = state.stale_engines.lock().await;
        engines.get(&slug_name).map(|engine| {
            (
                engine.db_path.clone(),
                engine.api_key.clone(),
                engine.model.clone(),
                engine.concurrent_helpers.clone(),
                engine.current_phase.clone(),
                engine.phase_detail.clone(),
                engine.last_result_summary.clone(),
            )
        })
    };

    let dispatch_status =
        if let Some((db_path, api_key, model, semaphore, phase_arc, detail_arc, summary_arc)) =
            engine_data
        {
            for layer in 0..=3 {
                let _ = stale_engine::drain_and_dispatch(
                    &slug_name,
                    layer,
                    0,
                    &db_path,
                    semaphore.clone(),
                    &api_key,
                    &model,
                    phase_arc.clone(),
                    detail_arc.clone(),
                    summary_arc.clone(),
                )
                .await;
            }
            "completed"
        } else {
            "enqueued_only"
        };

    Ok(json_ok(&serde_json::json!({
        "status": dispatch_status,
        "slug": slug_name,
        "tracked_files": tracked_files,
        "enqueued": enqueued,
        "already_pending": already_pending,
    })))
}

// ── Vine Conversation System handlers ────────────────────────────────────────

async fn handle_vine_build(
    state: Arc<PyramidState>,
    body: VineBuildBody,
) -> Result<warp::reply::Response, warp::Rejection> {
    let vine_slug = slug::slugify(&body.vine_slug);
    if let Err(e) = slug::validate_slug(&vine_slug) {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            &format!("Invalid vine slug: {}", e),
        ));
    }
    let jsonl_dirs: Vec<PathBuf> = body.jsonl_dirs.iter().map(PathBuf::from).collect();

    if jsonl_dirs.is_empty() {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            "jsonl_dirs must not be empty",
        ));
    }

    // Validate all directories exist
    for dir in &jsonl_dirs {
        if !dir.is_dir() {
            return Ok(json_error(
                warp::http::StatusCode::BAD_REQUEST,
                &format!("Directory does not exist: {}", dir.display()),
            ));
        }
    }

    // Check if vine_slug already exists — if so, check it's a vine type
    {
        let conn = state.reader.lock().await;
        if let Ok(Some(existing)) = slug::get_slug(&conn, &vine_slug) {
            if existing.content_type != ContentType::Vine {
                return Ok(json_error(
                    warp::http::StatusCode::CONFLICT,
                    &format!(
                        "Slug '{}' exists but is not a vine (type: {:?})",
                        vine_slug, existing.content_type
                    ),
                ));
            }
        }
    }

    // Check for concurrent vine build on this slug
    {
        let builds = state.vine_builds.lock().await;
        if let Some(handle) = builds.get(&vine_slug) {
            if handle.status == "running" {
                return Ok(json_error(
                    warp::http::StatusCode::CONFLICT,
                    &format!("A vine build is already running for '{}'", vine_slug),
                ));
            }
        }
    }

    // Spawn build in background with its own cancellation token (NOT the global active_build)
    let cancel = tokio_util::sync::CancellationToken::new();

    // Register the vine build
    {
        let mut builds = state.vine_builds.lock().await;
        builds.insert(
            vine_slug.clone(),
            super::VineBuildHandle {
                cancel: cancel.clone(),
                status: "running".to_string(),
                error: None,
            },
        );
    }

    let state_clone = state.clone();
    let slug_clone = vine_slug.clone();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        let (final_status, error_msg) =
            match vine::build_vine(&state_clone, &slug_clone, &jsonl_dirs, &cancel_clone).await {
                Ok(apex_id) => {
                    tracing::info!("Vine build complete for '{}': apex={}", slug_clone, apex_id);
                    ("complete".to_string(), None)
                }
                Err(e) => {
                    let msg = format!("{:#}", e);
                    tracing::error!("Vine build failed for '{}': {}", slug_clone, msg);
                    ("failed".to_string(), Some(msg))
                }
            };
        // Update status when build finishes
        let mut builds = state_clone.vine_builds.lock().await;
        if let Some(handle) = builds.get_mut(&slug_clone) {
            handle.status = final_status;
            handle.error = error_msg;
        }
    });

    Ok(json_ok(&serde_json::json!({
        "status": "started",
        "vine_slug": vine_slug,
        "jsonl_dirs": body.jsonl_dirs,
    })))
}

async fn handle_vine_bunches(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match db::list_vine_bunches(&conn, &slug_name) {
        Ok(bunches) => Ok(json_ok(&bunches)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_vine_eras(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match db::get_annotations_by_type(&conn, &slug_name, "era") {
        Ok(annotations) => Ok(json_ok(&annotations)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_vine_decisions(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match db::get_faq_nodes_by_prefix(&conn, &slug_name, "FAQ-vine-decision-") {
        Ok(faqs) => Ok(json_ok(&faqs)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_vine_entities(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    match db::get_faq_nodes_by_prefix(&conn, &slug_name, "FAQ-vine-entity-") {
        Ok(faqs) => Ok(json_ok(&faqs)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_vine_threads(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    let threads = db::get_threads(&conn, &slug_name);
    let edges = webbing::get_active_edges(&conn, &slug_name, 0.1);
    match (threads, edges) {
        (Ok(t), Ok(e)) => Ok(json_ok(&serde_json::json!({
            "threads": t,
            "web_edges": e,
        }))),
        (Err(e), _) | (_, Err(e)) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_vine_drill(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let conn = state.reader.lock().await;
    // Read Directory annotations on sub-apex nodes and return as navigation structure
    let directory_annotations = db::get_annotations_by_type(&conn, &slug_name, "directory");
    match directory_annotations {
        Ok(dirs) => {
            // Build navigation structure from directory annotations
            let mut nav: Vec<serde_json::Value> = Vec::new();
            for ann in &dirs {
                // Parse the content as JSON if possible (directory annotations store structured data)
                let content_val: serde_json::Value = serde_json::from_str(&ann.content)
                    .unwrap_or_else(|_| serde_json::Value::String(ann.content.clone()));
                nav.push(serde_json::json!({
                    "node_id": ann.node_id,
                    "content": content_val,
                    "author": ann.author,
                    "created_at": ann.created_at,
                }));
            }
            Ok(json_ok(&serde_json::json!({
                "vine_slug": slug_name,
                "directory_count": nav.len(),
                "directories": nav,
            })))
        }
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_vine_rebuild_upper(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let cancel = tokio_util::sync::CancellationToken::new();
    let state_clone = state.clone();
    let slug_clone = slug_name.clone();

    tokio::spawn(async move {
        match vine::force_rebuild_vine_upper(&state_clone, &slug_clone, &cancel).await {
            Ok(apex_id) => {
                tracing::info!(
                    "Vine upper rebuild complete for '{}': apex={}",
                    slug_clone,
                    apex_id
                );
            }
            Err(e) => {
                tracing::error!("Vine upper rebuild failed for '{}': {}", slug_clone, e);
            }
        }
    });

    Ok(json_ok(&serde_json::json!({
        "status": "started",
        "vine_slug": slug_name,
        "operation": "rebuild-upper",
    })))
}

async fn handle_vine_integrity(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    match vine::run_integrity_check(&state, &slug_name).await {
        Ok(summary) => Ok(json_ok(&serde_json::json!({
            "vine_slug": slug_name,
            "summary": summary,
        }))),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_vine_build_status(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let builds = state.vine_builds.lock().await;
    match builds.get(&slug_name) {
        Some(handle) => Ok(json_ok(&serde_json::json!({
            "vine_slug": slug_name,
            "status": handle.status,
            "error": handle.error,
        }))),
        None => Ok(json_ok(&serde_json::json!({
            "vine_slug": slug_name,
            "status": "not_found",
        }))),
    }
}

// ── Characterization route (P1.1) ─────────────────────────────────────────────

/// POST /pyramid/:slug/characterize
///
/// Characterize source material before building a knowledge pyramid.
/// Returns a CharacterizationResult that the caller can review/modify
/// before passing into the question build endpoint.
async fn handle_characterize(
    slug_name: String,
    body: CharacterizeBody,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Validate slug exists and get source_path
    let source_path = {
        let conn = state.reader.lock().await;
        match slug::get_slug(&conn, &slug_name) {
            Ok(Some(s)) => {
                // Use provided source_path or fall back to slug's source_path
                body.source_path.unwrap_or(s.source_path)
            }
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    "Slug not found",
                ));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    };

    if body.question.trim().is_empty() {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            "question cannot be empty",
        ));
    }

    let llm_config = state.config.read().await.clone();

    match characterize::characterize(&source_path, &body.question, &llm_config).await {
        Ok(result) => Ok(json_ok(&result)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Characterization failed: {}", e),
        )),
    }
}

// ── Question decomposition routes (P2.2) ─────────────────────────────────────

/// POST /pyramid/:slug/build/question
///
/// Start a decomposed question build. Decomposes the apex question into sub-questions,
/// compiles to IR, and executes through the standard executor.
async fn handle_question_build(
    slug_name: String,
    query: std::collections::HashMap<String, String>,
    body: QuestionBuildBody,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let from_depth = query
        .get("from_depth")
        .and_then(|s| s.parse().ok())
        .or(body.from_depth)
        .unwrap_or(0);

    // Validate slug exists
    {
        let conn = state.reader.lock().await;
        match slug::get_slug(&conn, &slug_name) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    "Slug not found",
                ));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    }

    if body.question.trim().is_empty() {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            "question cannot be empty",
        ));
    }

    // Check for existing active build
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
        if let Some(handle) = active.get(&slug_name) {
            let s = handle.status.read().await;
            let is_terminal = s.is_terminal();
            drop(s);
            if !handle.cancel.is_cancelled() && !is_terminal {
                return Ok(json_error(
                    warp::http::StatusCode::CONFLICT,
                    "Build already running for this slug",
                ));
            }
        }

        let handle = super::BuildHandle {
            slug: slug_name.clone(),
            cancel: cancel.clone(),
            status: status.clone(),
        };
        active.insert(slug_name.clone(), handle);
    }

    // Spawn the build task
    let build_state = state.clone();
    let build_status = status.clone();
    let question = body.question.clone();
    let granularity = body.granularity;
    let max_depth = body.max_depth;
    let from_depth_for_build = from_depth;
    let characterization = body.characterization.clone();
    let response_slug = slug_name.clone();

    tokio::spawn(async move {
        let start = std::time::Instant::now();

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

        let result = super::build_runner::run_decomposed_build(
            &build_state,
            &slug_name,
            &question,
            granularity,
            max_depth,
            from_depth_for_build,
            characterization,
            &cancel,
            Some(progress_tx.clone()),
        )
        .await;

        drop(progress_tx);
        let _ = progress_handle.await;

        // Update final status
        {
            let mut s = build_status.write().await;
            if cancel.is_cancelled() {
                s.status = "cancelled".to_string();
            } else {
                match result {
                    Ok((_apex, failures)) => {
                        s.status = "complete".to_string();
                        s.failures = failures;
                    }
                    Err(e) => {
                        s.status = format!("error: {}", e);
                        s.failures = -1;
                    }
                }
            }
            s.elapsed_seconds = start.elapsed().as_secs_f64();
        }
    });

    Ok(json_ok(&serde_json::json!({
        "status": "started",
        "slug": response_slug,
        "build_type": "question_decomposition",
        "question": body.question,
        "granularity": body.granularity,
        "max_depth": body.max_depth,
        "from_depth": from_depth,
    })))
}

/// POST /pyramid/:slug/build/preview
///
/// Preview what a decomposed question build would produce without actually building.
/// Returns the question tree, estimated node counts, estimated LLM calls, and cost.
async fn handle_question_preview(
    slug_name: String,
    body: QuestionBuildBody,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Validate slug exists
    {
        let conn = state.reader.lock().await;
        match slug::get_slug(&conn, &slug_name) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    "Slug not found",
                ));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    }

    if body.question.trim().is_empty() {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            "question cannot be empty",
        ));
    }

    match super::build_runner::preview_decomposed_build(
        &state,
        &slug_name,
        &body.question,
        body.granularity,
        body.max_depth,
    )
    .await
    {
        Ok((tree, preview)) => Ok(json_ok(&serde_json::json!({
            "slug": slug_name,
            "question": body.question,
            "preview": preview,
            "question_tree": tree,
        }))),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

// ── P3.3: Crystallization route handlers ────────────────────────────────

/// Request body for POST /pyramid/:slug/crystallize
#[derive(Debug, Deserialize)]
struct CrystallizeTriggerBody {
    /// List of L0 node IDs that changed (e.g., ["L0-001", "L0-005"]).
    changed_node_ids: Vec<String>,
}

/// POST /pyramid/:slug/crystallize — manually trigger a delta check
async fn handle_crystallize_trigger(
    slug_name: String,
    state: Arc<PyramidState>,
    body: CrystallizeTriggerBody,
) -> Result<warp::reply::Response, warp::Rejection> {
    use super::crystallization;

    // Load config and build subscriptions while holding the lock, then release
    let subscriptions = {
        let conn = state.reader.lock().await;
        let config = crystallization::load_config(&conn, &slug_name).unwrap_or_default();
        crystallization::build_crystallization_subscriptions(&config)
    };

    // Register subscriptions in-memory only (no DB persistence from route handler)
    for sub in subscriptions {
        let _ = state.event_bus.subscribe_memory_only(sub).await;
    }

    // Emit StaleDetected event directly (avoids holding &Connection across awaits)
    let event = super::event_chain::PyramidEvent::StaleDetected {
        slug: slug_name.clone(),
        node_ids: body.changed_node_ids.clone(),
        layer: 0,
    };
    match state.event_bus.emit_memory_only(event).await {
        Ok(invocation_ids) => Ok(json_ok(&serde_json::json!({
            "slug": slug_name,
            "triggered": true,
            "changed_node_ids": body.changed_node_ids,
            "invocation_ids": invocation_ids,
        }))),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

/// GET /pyramid/:slug/crystallize/status — show crystallization cascade status
async fn handle_crystallize_status(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    use super::crystallization;

    let status = crystallization::get_crystallization_status(&state.event_bus, &slug_name).await;
    Ok(json_ok(&status))
}

/// POST /pyramid/chain/import — import a chain or question set from the Wire (P4.2)
async fn handle_chain_import(
    body: ChainImportBody,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let import_type = body.import_type.as_deref().unwrap_or("chain");
    let contribution_id = body.contribution_id.trim();

    if contribution_id.is_empty() {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            "contribution_id is required",
        ));
    }

    // Read Wire config from pyramid config
    let config = state.config.read().await;
    let wire_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://api.callmeplayful.com".to_string());
    let wire_auth = config.auth_token.clone();
    drop(config);

    if wire_auth.is_empty() {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            "auth_token not configured — set via POST /pyramid/config",
        ));
    }

    let client = wire_import::WireImportClient::new(wire_url, wire_auth, None);

    match import_type {
        "chain" => {
            match client.fetch_chain(contribution_id).await {
                Ok(chain) => {
                    // Persist to SQLite (tables created at startup in init_pyramid_db)
                    let writer = state.writer.lock().await;
                    if let Err(e) = wire_import::save_imported_chain(&writer, &chain) {
                        tracing::error!(error = %e, "failed to persist imported chain");
                        return Ok(json_error(
                            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                            &format!("failed to persist chain: {}", e),
                        ));
                    }
                    drop(writer);

                    let resp = ChainImportResponse {
                        ok: true,
                        contribution_id: chain.id,
                        title: chain.title,
                        content_type: chain.content_type,
                        import_type: "chain".into(),
                    };
                    Ok(json_ok(&resp))
                }
                Err(e) => {
                    let msg = format!("failed to import chain: {}", e);
                    tracing::warn!(contribution_id, error = %e, "chain import failed");
                    Ok(json_error(warp::http::StatusCode::BAD_GATEWAY, &msg))
                }
            }
        }
        "question_set" => {
            match client.fetch_question_set(contribution_id).await {
                Ok(qs) => {
                    // Persist to SQLite (tables created at startup in init_pyramid_db)
                    let writer = state.writer.lock().await;
                    if let Err(e) = wire_import::save_imported_question_set(&writer, &qs) {
                        tracing::error!(error = %e, "failed to persist imported question set");
                        return Ok(json_error(
                            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                            &format!("failed to persist question set: {}", e),
                        ));
                    }
                    drop(writer);

                    let resp = ChainImportResponse {
                        ok: true,
                        contribution_id: qs.id,
                        title: qs.title,
                        content_type: None,
                        import_type: "question_set".into(),
                    };
                    Ok(json_ok(&resp))
                }
                Err(e) => {
                    let msg = format!("failed to import question set: {}", e);
                    tracing::warn!(contribution_id, error = %e, "question set import failed");
                    Ok(json_error(warp::http::StatusCode::BAD_GATEWAY, &msg))
                }
            }
        }
        other => Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            &format!(
                "invalid import_type '{}': expected 'chain' or 'question_set'",
                other
            ),
        )),
    }
}

// ── P4.3: Publication handlers ──────────────────────────────────────

/// POST /pyramid/:slug/publish — publish all pyramid nodes to the Wire
async fn handle_publish_pyramid(
    slug_name: String,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Validate slug exists
    {
        let conn = state.reader.lock().await;
        match db::get_slug(&conn, &slug_name) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    &format!("slug '{}' not found", slug_name),
                ));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    }

    // Read Wire config
    let config = state.config.read().await;
    let wire_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://api.callmeplayful.com".to_string());
    let wire_auth = config.auth_token.clone();
    drop(config);

    if wire_auth.is_empty() {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            "auth_token not configured — set via POST /pyramid/config",
        ));
    }

    let publisher = wire_publish::PyramidPublisher::new(wire_url, wire_auth);

    // Phase 1: Load all nodes from DB (synchronous, scoped lock)
    let nodes_by_depth = {
        let conn = state.reader.lock().await;
        let slug_info = match db::get_slug(&conn, &slug_name) {
            Ok(Some(info)) => info,
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    &format!("slug '{}' not found", slug_name),
                ));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        };
        let mut result = Vec::new();
        for depth in 0..=slug_info.max_depth {
            match db::get_nodes_at_depth(&conn, &slug_name, depth) {
                Ok(nodes) => {
                    if !nodes.is_empty() {
                        result.push((depth, nodes));
                    }
                }
                Err(e) => {
                    return Ok(json_error(
                        warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("failed to load nodes at depth {}: {}", depth, e),
                    ));
                }
            }
        }
        result
    };

    if nodes_by_depth.is_empty() {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            &format!("no nodes found for slug '{}'", slug_name),
        ));
    }

    // Phase 2: Publish nodes via HTTP (async, no DB lock held)
    match publisher.publish_pyramid(&slug_name, &nodes_by_depth).await {
        Ok(result) => {
            // Phase 3: Persist ID mappings (scoped write lock)
            {
                let writer = state.writer.lock().await;
                if let Err(e) = wire_publish::init_id_map_table(&writer) {
                    tracing::warn!(error = %e, "failed to init id_map table");
                }
                for mapping in &result.id_mappings {
                    let uuid = mapping.wire_uuid.as_deref().unwrap_or(&mapping.wire_handle_path);
                    if let Err(e) = wire_publish::save_id_mapping(
                        &writer,
                        &slug_name,
                        &mapping.local_id,
                        uuid,
                    ) {
                        tracing::warn!(
                            local_id = %mapping.local_id,
                            error = %e,
                            "failed to persist ID mapping"
                        );
                    }
                }
            }
            tracing::info!(
                slug = %slug_name,
                node_count = result.node_count,
                apex_uuid = ?result.apex_wire_uuid,
                "pyramid published to Wire"
            );
            Ok(json_ok(&result))
        }
        Err(e) => {
            let msg = format!("failed to publish pyramid: {}", e);
            tracing::warn!(slug = %slug_name, error = %e, "publish failed");
            Ok(json_error(warp::http::StatusCode::BAD_GATEWAY, &msg))
        }
    }
}

/// POST /pyramid/:slug/publish/question-set — publish a question set to the Wire
async fn handle_publish_question_set(
    slug_name: String,
    body: PublishQuestionSetBody,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Validate slug exists and get its content type
    let content_type = {
        let conn = state.reader.lock().await;
        match db::get_slug(&conn, &slug_name) {
            Ok(Some(info)) => info.content_type,
            Ok(None) => {
                return Ok(json_error(
                    warp::http::StatusCode::NOT_FOUND,
                    &format!("slug '{}' not found", slug_name),
                ));
            }
            Err(e) => {
                return Ok(json_error(
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                    &e.to_string(),
                ));
            }
        }
    };

    // Load the question set YAML for this content type
    let chains_dir = state
        .data_dir
        .as_ref()
        .map(|d| d.join("chains"))
        .unwrap_or_else(|| PathBuf::from("chains"));

    let qs_path = chains_dir
        .join("questions")
        .join(format!("{}.yaml", content_type.as_str()));

    let qs_yaml = match std::fs::read_to_string(&qs_path) {
        Ok(yaml) => yaml,
        Err(e) => {
            return Ok(json_error(
                warp::http::StatusCode::NOT_FOUND,
                &format!(
                    "question set not found for content type '{}': {}",
                    content_type.as_str(),
                    e
                ),
            ));
        }
    };

    let question_set: super::question_yaml::QuestionSet = match serde_yaml::from_str(&qs_yaml) {
        Ok(qs) => qs,
        Err(e) => {
            return Ok(json_error(
                warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to parse question set YAML: {}", e),
            ));
        }
    };

    // Read Wire config
    let config = state.config.read().await;
    let wire_url =
        std::env::var("WIRE_URL").unwrap_or_else(|_| "https://api.callmeplayful.com".to_string());
    let wire_auth = config.auth_token.clone();
    drop(config);

    if wire_auth.is_empty() {
        return Ok(json_error(
            warp::http::StatusCode::BAD_REQUEST,
            "auth_token not configured — set via POST /pyramid/config",
        ));
    }

    let publisher = wire_publish::PyramidPublisher::new(wire_url, wire_auth);
    let description = body.description.unwrap_or_else(|| {
        format!(
            "Question set for {} content type ({} questions, v{})",
            question_set.r#type,
            question_set.questions.len(),
            question_set.version,
        )
    });

    match publisher
        .publish_question_set(&question_set, &description)
        .await
    {
        Ok(result) => {
            tracing::info!(
                slug = %slug_name,
                wire_uuid = %result.wire_uuid,
                "question set published to Wire"
            );
            Ok(json_ok(&result))
        }
        Err(e) => {
            let msg = format!("failed to publish question set: {}", e);
            tracing::warn!(slug = %slug_name, error = %e, "question set publish failed");
            Ok(json_error(warp::http::StatusCode::BAD_GATEWAY, &msg))
        }
    }
}
