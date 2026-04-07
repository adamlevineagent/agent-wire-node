//! Post-agents-retro WS-H — POST /p/{slug}/_ask preview-then-commit flow.
//!
//! Implements plan v3.3 §B1, §B2, §C3 (and Pillar 23):
//!
//! - `open` mode, any principal: direct synthesis — no preview, no cost
//!   token, no commit step.
//! - `absorb-all` / `absorb-selective` mode, `Anonymous` or `WebSession`:
//!   HARD DENY with a "Wire operator token required" 401 page. Per B2 and
//!   Pillar 13 we NEVER synthesize a Wire identity from a Supabase-backed
//!   WebSession, and we NEVER fall through to a free synthesis for an
//!   anonymous visitor on a questioner-pays pyramid.
//! - `absorb-all` / `absorb-selective` mode, `LocalOperator` or
//!   `WireOperator`: two-step flow. First POST renders the preview page
//!   (candidate nodes + estimated cost + model + a fresh `commit_token` in
//!   a hidden form field). Second POST with a valid `commit_token` runs
//!   the real rate-limit check (`check_absorption_rate_limit`) and then
//!   invokes the shared synthesis pipeline.
//!
//! Auth is resolved inline (Bearer `auth_token` → LocalOperator,
//! Bearer Wire JWT verified against `jwt_public_key` → WireOperator,
//! `wire_session` cookie → WebSession, otherwise Anonymous). Threading
//! `jwt_public_key` through `public_html::routes()` lets Wire-credentialed
//! visitors use the HTML surface for paid asks just like the JSON
//! `/navigate` endpoint.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use warp::filters::BoxedFilter;
use warp::http::StatusCode;
use warp::{Filter, Rejection};

use crate::http_utils::ct_eq;
use crate::pyramid::types::ContentType;
use crate::pyramid::PyramidState;
use crate::pyramid::public_html::auth::{
    ANON_SESSION_COOKIE, PublicAuthSource, WIRE_SESSION_COOKIE, csrf_nonce, enforce_public_tier,
    read_cookie, verify_csrf,
};
use crate::pyramid::public_html::rate_limit;
use crate::pyramid::public_html::render::{esc, page, status_page};
use crate::pyramid::public_html::web_sessions;

const FORM_BODY_LIMIT: u64 = 8 * 1024;
const QUESTION_MAX_LEN: usize = 2048;
const TOP_K: usize = 5;
const DEFAULT_COST_PER_CANDIDATE: u64 = 2;
const DEFAULT_MODEL_NAME: &str = "openrouter primary cascade";

// ── Slug sanity (mirrors routes_login::slug_is_safe) ────────────────────

fn slug_is_safe(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > 128 {
        return false;
    }
    if slug.starts_with('_') || slug.starts_with('.') {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ── HMAC-SHA256 (re-derived to avoid cross-module leakage) ──────────────

const HMAC_BLOCK_SIZE: usize = 64;

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut key_block = [0u8; HMAC_BLOCK_SIZE];
    if key.len() > HMAC_BLOCK_SIZE {
        let mut h = Sha256::new();
        h.update(key);
        let digest = h.finalize();
        key_block[..32].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut o_pad = [0x5cu8; HMAC_BLOCK_SIZE];
    let mut i_pad = [0x36u8; HMAC_BLOCK_SIZE];
    for i in 0..HMAC_BLOCK_SIZE {
        o_pad[i] ^= key_block[i];
        i_pad[i] ^= key_block[i];
    }
    let mut inner = Sha256::new();
    inner.update(i_pad);
    inner.update(msg);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(o_pad);
    outer.update(inner_digest);
    let outer_digest = outer.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer_digest);
    out
}

// ── commit_token — HMAC(secret, user_id:slug:sha256(question):window) ───

fn question_hash_hex(question: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(question.as_bytes());
    hex::encode(h.finalize())
}

fn epoch_minute_div5() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 60 / 5)
        .unwrap_or(0)
}

fn commit_token_at(
    secret: &[u8; 32],
    user_id: &str,
    slug: &str,
    question: &str,
    window: u64,
) -> String {
    let msg = format!(
        "{}:{}:{}:{}",
        user_id,
        slug,
        question_hash_hex(question),
        window
    );
    hex::encode(hmac_sha256(secret, msg.as_bytes()))
}

/// Mint a `commit_token` binding the operator identity + slug + exact
/// question text + current 5-minute window. Consumed by the second POST to
/// prove the caller actually saw the preview page.
pub fn make_commit_token(
    secret: &[u8; 32],
    user_id: &str,
    slug: &str,
    question: &str,
) -> String {
    commit_token_at(secret, user_id, slug, question, epoch_minute_div5())
}

// ── One-shot commit-token consumption (P1-12) ───────────────────────────
//
// A commit_token is HMAC-bound to a 5–10 minute window, so the same operator
// could replay the second-step POST and trigger absorption_rate_limit charges
// repeatedly. We add a process-local consumed-set keyed on the token; the
// first successful commit inserts the token, subsequent commits with the
// same value are rejected. Entries are GC'd after 15 minutes (token TTL is
// at most ~10 minutes; the GC just bounds memory).

const COMMIT_TOKEN_TTL: Duration = Duration::from_secs(15 * 60);

static CONSUMED_TOKENS: OnceLock<StdMutex<HashMap<String, Instant>>> = OnceLock::new();

fn consumed_tokens() -> &'static StdMutex<HashMap<String, Instant>> {
    CONSUMED_TOKENS.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Try to mark a commit_token as consumed. Returns `true` if this is the
/// first time we've seen it (callers should proceed); `false` if it was
/// already consumed (callers should reject as a replay).
fn mark_token_consumed(token: &str) -> bool {
    let now = Instant::now();
    let mut map = match consumed_tokens().lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    // Lazy GC: drop expired entries while we hold the lock.
    map.retain(|_, ts| now.duration_since(*ts) < COMMIT_TOKEN_TTL);
    if map.contains_key(token) {
        return false;
    }
    map.insert(token.to_string(), now);
    true
}

/// Constant-time verification accepting the current OR previous 5-minute
/// window (so a preview rendered near a boundary still commits).
pub fn verify_commit_token(
    secret: &[u8; 32],
    token: &str,
    user_id: &str,
    slug: &str,
    question: &str,
) -> bool {
    let window = epoch_minute_div5();
    let cur = commit_token_at(secret, user_id, slug, question, window);
    if ct_eq(token, &cur) {
        return true;
    }
    let prev = commit_token_at(secret, user_id, slug, question, window.saturating_sub(1));
    ct_eq(token, &prev)
}

// ── Auth resolution (local subset — see module docstring) ───────────────

async fn resolve_auth(
    headers: &warp::http::HeaderMap,
    state: &PyramidState,
    jwt_public_key: &Arc<tokio::sync::RwLock<String>>,
) -> PublicAuthSource {
    // 1. Authorization: Bearer <...> → LocalOperator OR WireOperator.
    if let Some(h) = headers.get("authorization").and_then(|h| h.to_str().ok()) {
        if let Some(token) = h.strip_prefix("Bearer ") {
            let local = { state.config.read().await.auth_token.clone() };
            if !local.is_empty() && ct_eq(token, &local) {
                return PublicAuthSource::LocalOperator;
            }
            // Wire JWT: header.payload.signature → two dots.
            if token.matches('.').count() == 2 {
                let pk_str = jwt_public_key.read().await;
                if !pk_str.is_empty() {
                    if let Ok(claims) =
                        crate::server::verify_pyramid_query_jwt(token, &pk_str)
                    {
                        let operator_id = claims.operator_id.unwrap_or_default();
                        let circle_id = claims.circle_id;
                        return PublicAuthSource::WireOperator {
                            operator_id,
                            circle_id,
                        };
                    }
                }
            }
        }
    }
    // 2. wire_session cookie → WebSession (Supabase-backed).
    if let Some(wire_tok) = read_cookie(headers, WIRE_SESSION_COOKIE) {
        if !wire_tok.is_empty() {
            let sess_opt = {
                let conn = state.reader.lock().await;
                web_sessions::lookup(&conn, &wire_tok).ok().flatten()
            };
            if let Some(sess) = sess_opt {
                let anon_tok = read_cookie(headers, ANON_SESSION_COOKIE).unwrap_or_default();
                return PublicAuthSource::WebSession {
                    user_id: sess.supabase_user_id,
                    email: sess.email,
                    anon_session_token: anon_tok,
                };
            }
        }
    }
    // 3. Anonymous keyed by (empty) client_key; the real key is set
    //    upstream by WS-F bucketing when we call check_for_ask.
    PublicAuthSource::Anonymous {
        client_key: String::new(),
    }
}

/// Session token to bind CSRF nonces to: prefer wire_session, fall back to
/// anon_session, fall back to empty string. Mirrors the convention used
/// by routes_login / routes_read.
fn csrf_session_token(headers: &warp::http::HeaderMap) -> String {
    if let Some(t) = read_cookie(headers, WIRE_SESSION_COOKIE) {
        if !t.is_empty() {
            return t;
        }
    }
    read_cookie(headers, ANON_SESSION_COOKIE).unwrap_or_default()
}

// ── Response helpers ────────────────────────────────────────────────────

fn bad_request_page(message: &str) -> warp::reply::Response {
    status_page(
        400,
        "Bad request — Wire Node",
        &format!("<h1>400</h1>\n<p class=\"err\">{}</p>\n", esc(message)),
    )
}

fn not_found_page() -> warp::reply::Response {
    status_page(
        404,
        "Not found — Wire Node",
        "<h1>404</h1>\n<p class=\"empty\">Unknown pyramid.</p>\n",
    )
}

fn rate_limited_page(retry_after: u64) -> warp::reply::Response {
    let body = format!(
        "<h1>429</h1>\n<p class=\"empty\">Too many requests. Retry in {s}s.</p>\n",
        s = retry_after
    );
    let mut resp = status_page(429, "Rate limited — Wire Node", &body);
    if let Ok(hv) = warp::http::HeaderValue::from_str(&retry_after.to_string()) {
        resp.headers_mut().insert("retry-after", hv);
    }
    resp
}

fn operator_required_page(slug: &str) -> warp::reply::Response {
    let body = format!(
        "<h1>Wire operator token required</h1>\n\
         <p>This pyramid is in <code>questioner-pays</code> mode. \
         To ask questions here you need a Wire operator token.</p>\n\
         <ol>\n\
           <li>Visit your Wire dashboard and mint a query token.</li>\n\
           <li>Return here and provide it via the \
             <code>Authorization: Bearer &lt;jwt&gt;</code> header.</li>\n\
         </ol>\n\
         <p class=\"muted\">An anonymous or email-login session cannot be \
         billed as a Wire identity.</p>\n\
         <p><a href=\"/p/{slug}\">Back to pyramid</a></p>\n",
        slug = esc(slug)
    );
    let mut resp = status_page(401, "Operator token required", &body);
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    resp
}

// ── Preview + answer rendering ──────────────────────────────────────────

struct PreviewCandidate {
    id: String,
    headline: String,
    snippet: String,
}

fn render_preview_page(
    slug: &str,
    question: &str,
    candidates: &[PreviewCandidate],
    cost_credits: u64,
    model_name: &str,
    commit_token: &str,
    csrf: &str,
) -> warp::reply::Response {
    let mut cand_html = String::new();
    if candidates.is_empty() {
        cand_html.push_str("<p class=\"empty\">No matching nodes were found yet — \
             committing will still run a synthesis pass.</p>\n");
    } else {
        cand_html.push_str("<ul class=\"candidates\">\n");
        for c in candidates {
            cand_html.push_str(&format!(
                "<li><a href=\"/p/{slug}/{nid}\">{nid}</a> — {headline}<br><span class=\"snippet\">{snippet}</span></li>\n",
                slug = esc(slug),
                nid = esc(&c.id),
                headline = esc(&c.headline),
                snippet = esc(&c.snippet),
            ));
        }
        cand_html.push_str("</ul>\n");
    }

    let body = format!(
        "<h1>Preview — ask on <code>{slug}</code></h1>\n\
         <blockquote class=\"question\">{question}</blockquote>\n\
         <h2>Candidate nodes (top {n})</h2>\n\
         {cands}\n\
         <p class=\"cost\">Estimated cost: <strong>{cost}</strong> credits \
           · model: <code>{model}</code></p>\n\
         <form method=\"post\" action=\"/p/{slug}/_ask\">\n\
           <input type=\"hidden\" name=\"question\" value=\"{question_attr}\">\n\
           <input type=\"hidden\" name=\"commit_token\" value=\"{ct}\">\n\
           <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\n\
           <button type=\"submit\">ASK FOR REAL — costs {cost} credits</button>\n\
         </form>\n\
         <p class=\"muted\">To refine: go back and edit the question before committing. \
           A new preview will be generated.</p>\n\
         <p><a href=\"/p/{slug}\">Cancel and return to pyramid</a></p>\n",
        slug = esc(slug),
        question = esc(question),
        question_attr = esc(question),
        n = TOP_K,
        cands = cand_html,
        cost = cost_credits,
        model = esc(model_name),
        ct = esc(commit_token),
        csrf = esc(csrf),
    );
    page("Preview — Wire Node", &body, "no-store")
}

#[allow(dead_code)]
fn render_answer_page(
    slug: &str,
    question: &str,
    answer: &str,
    cited_nodes: &[String],
) -> warp::reply::Response {
    let mut cites_html = String::new();
    if !cited_nodes.is_empty() {
        cites_html.push_str("<h2>Cited nodes</h2>\n<ul>\n");
        for nid in cited_nodes {
            cites_html.push_str(&format!(
                "<li><a href=\"/p/{slug}/{nid}\">{nid}</a></li>\n",
                slug = esc(slug),
                nid = esc(nid),
            ));
        }
        cites_html.push_str("</ul>\n");
    }
    let body = format!(
        "<h1>Answer</h1>\n\
         <blockquote class=\"question\">{q}</blockquote>\n\
         <article class=\"answer\"><pre>{a}</pre></article>\n\
         {cites}\n\
         <p><a href=\"/p/{slug}\">Back to pyramid</a></p>\n",
        q = esc(question),
        a = esc(answer),
        cites = cites_html,
        slug = esc(slug),
    );
    page("Answer — Wire Node", &body, "no-store")
}

#[allow(dead_code)]
fn render_no_results_page(slug: &str, question: &str) -> warp::reply::Response {
    let body = format!(
        "<h1>No relevant nodes</h1>\n\
         <blockquote class=\"question\">{q}</blockquote>\n\
         <p class=\"empty\">No relevant nodes found for this question.</p>\n\
         <p><a href=\"/p/{slug}\">Back to pyramid</a></p>\n",
        q = esc(question),
        slug = esc(slug),
    );
    page("No results — Wire Node", &body, "no-store")
}

// ── Core synthesis (legacy — kept for reference; question-pyramid path
//    bypasses inline synthesis entirely). ───────────────────────────────

#[allow(dead_code)]
struct SynthesisOutput {
    answer: String,
    cited_nodes: Vec<String>,
    is_empty: bool,
}

#[allow(dead_code)]
async fn run_synthesis(
    state: &PyramidState,
    slug: &str,
    question: &str,
) -> Result<SynthesisOutput, String> {
    let llm_config = {
        let config = state.config.read().await;
        if config.api_key.is_empty() {
            return Err("LLM not configured on this Wire Node.".to_string());
        }
        config.clone()
    };

    let search_results = {
        let conn = state.reader.lock().await;
        match crate::pyramid::query::search(&conn, slug, question) {
            Ok(r) => r,
            Err(e) => return Err(format!("search failed: {}", e)),
        }
    };

    if search_results.is_empty() {
        return Ok(SynthesisOutput {
            answer: String::new(),
            cited_nodes: Vec::new(),
            is_empty: true,
        });
    }

    let top: Vec<_> = search_results.iter().take(TOP_K).collect();
    let mut node_contents: Vec<(String, String)> = Vec::new();
    {
        let conn = state.reader.lock().await;
        for hit in &top {
            if let Ok(Some(node)) = crate::pyramid::db::get_node(&conn, slug, &hit.node_id) {
                let mut distilled = node.distilled.clone();
                if distilled.len() > 800 {
                    let mut end = 800;
                    while end < distilled.len() && !distilled.is_char_boundary(end) {
                        end += 1;
                    }
                    distilled.truncate(end);
                }
                let content = format!("Node {}: {}\n{}", node.id, node.headline, distilled);
                node_contents.push((node.id.clone(), content));
            }
        }
    }

    if node_contents.is_empty() {
        return Ok(SynthesisOutput {
            answer: String::new(),
            cited_nodes: Vec::new(),
            is_empty: true,
        });
    }

    let system = "You answer questions using knowledge pyramid nodes. Cite the node ID (e.g. L1-xxx) that supports each claim. Be concise and direct. If the nodes don't contain enough information to fully answer, say what you can and note what's missing.";
    let user = format!(
        "Question: {}\n\nKnowledge nodes:\n{}",
        question,
        node_contents
            .iter()
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    );

    match crate::pyramid::llm::call_model_unified(&llm_config, system, &user, 0.2, 600, None).await
    {
        Ok(response) => {
            let cited: Vec<String> = node_contents
                .iter()
                .filter(|(id, _)| response.content.contains(id))
                .map(|(id, _)| id.clone())
                .collect();
            Ok(SynthesisOutput {
                answer: response.content,
                cited_nodes: cited,
                is_empty: false,
            })
        }
        Err(e) => Err(format!("LLM call failed: {}", e)),
    }
}

async fn load_preview_candidates(
    state: &PyramidState,
    slug: &str,
    question: &str,
) -> Vec<PreviewCandidate> {
    let hits = {
        let conn = state.reader.lock().await;
        crate::pyramid::query::search(&conn, slug, question)
            .ok()
            .unwrap_or_default()
    };
    hits.into_iter()
        .take(TOP_K)
        .map(|h| PreviewCandidate {
            id: h.node_id,
            headline: h.headline,
            snippet: h.snippet,
        })
        .collect()
}

fn estimated_cost_credits(candidate_count: usize) -> u64 {
    let base = (candidate_count as u64).saturating_mul(DEFAULT_COST_PER_CANDIDATE);
    base.max(1)
}

// ── Question-pyramid slug minting + redirect ────────────────────────────

/// Default question-pyramid build cost estimate (paid mode), used for the
/// rate-limit pre-check. Reflects roughly `10 * granularity * max_depth`
/// for the V1 defaults (granularity=3, max_depth=2).
const QUESTION_BUILD_COST_CREDITS: u64 = 60;

/// Default decomposed-build parameters for web-surface-initiated question
/// pyramids. The desktop UI can override these via the IPC path; the public
/// surface keeps them locked.
const QB_GRANULARITY: u32 = 3;
const QB_MAX_DEPTH: u32 = 2;
const QB_FROM_DEPTH: i64 = 0;

/// Truncate a kebab-case string to at most `max_chars`, preferring to cut
/// at the last hyphen so we don't slice a word in half. Returns the
/// truncated string (always non-empty if input was non-empty).
fn truncate_kebab(input: &str, max_chars: usize) -> String {
    if input.len() <= max_chars {
        return input.to_string();
    }
    let head = &input[..max_chars];
    if let Some(last_hyphen) = head.rfind('-') {
        if last_hyphen > 0 {
            return head[..last_hyphen].to_string();
        }
    }
    head.to_string()
}

/// Mint a unique question-pyramid slug from the question text. Probes
/// `{base}`, `{base}-1`, ..., `{base}-99`. If all collide, appends a
/// 6-char random hex suffix instead.
fn mint_question_slug(conn: &rusqlite::Connection, question: &str) -> String {
    let raw = crate::pyramid::slug::slugify(question);
    let trimmed = truncate_kebab(&raw, 60);
    let base = if trimmed.trim_matches('-').is_empty() {
        "question".to_string()
    } else {
        trimmed.trim_matches('-').to_string()
    };

    // Try the base name first.
    if crate::pyramid::db::get_slug(conn, &base)
        .ok()
        .flatten()
        .is_none()
    {
        return base;
    }

    // Probe `-1`..`-99`.
    for i in 1..=99u32 {
        let candidate = format!("{base}-{i}");
        if crate::pyramid::db::get_slug(conn, &candidate)
            .ok()
            .flatten()
            .is_none()
        {
            return candidate;
        }
    }

    // Fallback: append a 6-char random hex suffix.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let suffix = format!("{:06x}", nanos & 0xff_ff_ff);
    format!("{base}-{suffix}")
}

/// Build a 302 redirect response to the question-pyramid live page.
fn redirect_to_question(source_slug: &str, question_slug: &str) -> warp::reply::Response {
    let target = format!("/p/{}/q/{}", source_slug, question_slug);
    let mut resp = warp::reply::Response::new(warp::hyper::Body::empty());
    *resp.status_mut() = StatusCode::FOUND;
    if let Ok(hv) = warp::http::HeaderValue::from_str(&target) {
        resp.headers_mut().insert("location", hv);
    }
    resp
}

/// Mint slug, persist source reference, and spawn the background build.
/// Returns a 302 to the live answer page on success, or an error response.
async fn create_question_pyramid_and_redirect(
    state: &Arc<PyramidState>,
    source_slug: &str,
    question: &str,
) -> warp::reply::Response {
    // Step 1: mint a unique slug under the writer connection so collision
    // probing + insert happen atomically (well, serialized — sqlite writer
    // mutex on PyramidState ensures only one minter at a time).
    let writer = state.writer.lock().await;
    let question_slug = mint_question_slug(&writer, question);

    // Step 2: create the slug row.
    if let Err(e) = crate::pyramid::db::create_slug(
        &writer,
        &question_slug,
        &ContentType::Question,
        "",
    ) {
        let body = format!(
            "<h1>Failed to create question pyramid</h1>\n<p class=\"err\">{}</p>\n",
            esc(&e.to_string())
        );
        return status_page(500, "Error — Wire Node", &body);
    }

    // Step 3: link the source. If this fails we leave the orphaned slug
    // (sweeper concern) — log a warning and proceed.
    if let Err(e) = crate::pyramid::db::save_slug_references(
        &writer,
        &question_slug,
        &[source_slug.to_string()],
    ) {
        tracing::warn!(
            question_slug = %question_slug,
            source_slug = %source_slug,
            error = %e,
            "save_slug_references failed for question pyramid; slug persists with no source link"
        );
    }
    drop(writer);

    // Step 4: spawn the background build. Don't await — return the redirect
    // immediately so the browser can start streaming WS events.
    let state_for_build = state.clone();
    let qslug = question_slug.clone();
    let q = question.to_string();
    tokio::spawn(async move {
        if let Err(e) = crate::pyramid::question_build::spawn_question_build(
            &state_for_build,
            qslug.clone(),
            q,
            QB_GRANULARITY,
            QB_MAX_DEPTH,
            QB_FROM_DEPTH,
            None,
        )
        .await
        {
            tracing::error!(
                question_slug = %qslug,
                error = %e,
                "spawn_question_build failed from web surface"
            );
        }
    });

    redirect_to_question(source_slug, &question_slug)
}

// ── Main handler ────────────────────────────────────────────────────────

async fn handle_ask_post(
    slug: String,
    headers: warp::http::HeaderMap,
    form: HashMap<String, String>,
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
) -> Result<warp::reply::Response, Rejection> {
    if !slug_is_safe(&slug) {
        return Ok(not_found_page());
    }

    // Parse body.
    let question = form
        .get("question")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let csrf = form.get("csrf").cloned().unwrap_or_default();
    let commit_token = form.get("commit_token").cloned().unwrap_or_default();

    if question.is_empty() {
        return Ok(bad_request_page("Missing question."));
    }
    if question.len() > QUESTION_MAX_LEN {
        return Ok(bad_request_page("Question is too long."));
    }

    // Resolve auth.
    let auth = resolve_auth(&headers, &state, &jwt_public_key).await;

    // CSRF (bound to wire_session | anon_session | empty + slug).
    let sess_tok = csrf_session_token(&headers);
    if !verify_csrf(&state.csrf_secret, &csrf, &sess_tok, &slug) {
        return Ok(bad_request_page("Session expired — please reload the pyramid page."));
    }

    // Tier gate: Anonymous/WebSession on a non-public pyramid → 404.
    if enforce_public_tier(&state, &slug, &auth).await.is_err() {
        return Ok(not_found_page());
    }

    // Rate limit (always skips LocalOperator / WireOperator per C2).
    let rl = rate_limit::global();
    if let Err(e) = rate_limit::check_for_ask(&rl, &auth).await {
        return Ok(rate_limited_page(e.retry_after));
    }

    // Absorption mode.
    let mode = {
        let conn = state.reader.lock().await;
        match crate::pyramid::db::get_absorption_mode(&conn, &slug) {
            Ok((m, _chain)) => m,
            Err(_) => return Ok(not_found_page()),
        }
    };

    let is_paid_mode = matches!(mode.as_str(), "absorb-all" | "absorb-selective");

    // B2: paid modes hard-deny Anonymous + WebSession before any synthesis.
    if is_paid_mode {
        match &auth {
            PublicAuthSource::Anonymous { .. } | PublicAuthSource::WebSession { .. } => {
                return Ok(operator_required_page(&slug));
            }
            _ => {}
        }
    }

    // Extract operator_id for the second-step rate-limit + token binding.
    // Per Pillar 13, NEVER use a WebSession.user_id here.
    let operator_id: Option<String> = match &auth {
        PublicAuthSource::LocalOperator => Some("local".to_string()),
        PublicAuthSource::WireOperator { operator_id, .. } => Some(operator_id.clone()),
        _ => None,
    };

    // ── Open mode → mint a question pyramid + redirect to live page. ──
    if !is_paid_mode {
        return Ok(create_question_pyramid_and_redirect(&state, &slug, &question).await);
    }

    // ── Paid mode + operator identity → preview-or-commit ──────────────
    let Some(op_id) = operator_id else {
        // Should be unreachable given the B2 guard above, but defend anyway.
        return Ok(operator_required_page(&slug));
    };

    if commit_token.is_empty() {
        // STEP 1 — render preview.
        let candidates = load_preview_candidates(&state, &slug, &question).await;
        let cost = estimated_cost_credits(candidates.len().max(1));
        let ct = make_commit_token(&state.csrf_secret, &op_id, &slug, &question);
        let next_csrf = csrf_nonce(&state.csrf_secret, &sess_tok, &slug);
        return Ok(render_preview_page(
            &slug,
            &question,
            &candidates,
            cost,
            DEFAULT_MODEL_NAME,
            &ct,
            &next_csrf,
        ));
    }

    // STEP 2 — verify commit_token (question must match exactly).
    if !verify_commit_token(&state.csrf_secret, &commit_token, &op_id, &slug, &question) {
        return Ok(bad_request_page(
            "Commit token expired or does not match this question — please ask again.",
        ));
    }

    // P1-12: one-shot consumption. Reject token replay within the
    // 5-10 minute HMAC window so the same preview cannot be re-submitted
    // multiple times to charge absorption_rate_limit repeatedly.
    if !mark_token_consumed(&commit_token) {
        return Ok(bad_request_page(
            "This preview was already submitted — re-ask to get a fresh preview.",
        ));
    }

    // Re-check the absorption rate limit with the question-build cost,
    // using the REAL operator_id (Pillar 13). Anonymous/WebSession never
    // reach here. Reject BEFORE we mint a slug so we don't leak orphaned
    // empty pyramids on rate-limit denial.
    if let Err(e) = crate::pyramid::build_runner::check_absorption_rate_limit(
        &state,
        &slug,
        &op_id,
        QUESTION_BUILD_COST_CREDITS,
    )
    .await
    {
        let body = format!(
            "<h1>429</h1>\n<p class=\"err\">{msg}</p>\n<p><a href=\"/p/{slug}\">Back</a></p>\n",
            msg = esc(&e.to_string()),
            slug = esc(&slug),
        );
        return Ok(status_page(429, "Rate limited — Wire Node", &body));
    }

    Ok(create_question_pyramid_and_redirect(&state, &slug, &question).await)
}

#[allow(dead_code)]
async fn synthesize_and_render(
    state: &PyramidState,
    slug: &str,
    question: &str,
) -> warp::reply::Response {
    match run_synthesis(state, slug, question).await {
        Ok(out) if out.is_empty => render_no_results_page(slug, question),
        Ok(out) => render_answer_page(slug, question, &out.answer, &out.cited_nodes),
        Err(msg) => {
            let body = format!(
                "<h1>Synthesis failed</h1>\n<p class=\"err\">{}</p>\n",
                esc(&msg)
            );
            status_page(500, "Error — Wire Node", &body)
        }
    }
}

// ── Filter assembly ─────────────────────────────────────────────────────

fn with_state(
    state: Arc<PyramidState>,
) -> impl Filter<Extract = (Arc<PyramidState>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || state.clone())
}

/// `POST /p/{slug}/_ask` — the only route owned by WS-H.
pub fn ask_routes(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
) -> BoxedFilter<(warp::reply::Response,)> {
    let jwt_pk = jwt_public_key.clone();
    warp::path!("p" / String / "_ask")
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::content_length_limit(FORM_BODY_LIMIT))
        .and(warp::body::form::<HashMap<String, String>>())
        .and(with_state(state))
        .and(warp::any().map(move || jwt_pk.clone()))
        .and_then(handle_ask_post)
        .boxed()
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_token_round_trip() {
        let secret = [7u8; 32];
        let t = make_commit_token(&secret, "op-1", "slug-a", "what is a pyramid?");
        assert!(verify_commit_token(
            &secret,
            &t,
            "op-1",
            "slug-a",
            "what is a pyramid?"
        ));
    }

    #[test]
    fn commit_token_rejects_different_question() {
        let secret = [7u8; 32];
        let t = make_commit_token(&secret, "op-1", "slug-a", "question A");
        assert!(!verify_commit_token(
            &secret, &t, "op-1", "slug-a", "question B"
        ));
    }

    #[test]
    fn commit_token_rejects_different_operator() {
        let secret = [7u8; 32];
        let t = make_commit_token(&secret, "op-1", "slug-a", "Q");
        assert!(!verify_commit_token(&secret, &t, "op-2", "slug-a", "Q"));
    }

    #[test]
    fn commit_token_rejects_different_slug() {
        let secret = [7u8; 32];
        let t = make_commit_token(&secret, "op-1", "slug-a", "Q");
        assert!(!verify_commit_token(&secret, &t, "op-1", "slug-b", "Q"));
    }

    #[test]
    fn commit_token_rejects_garbage() {
        let secret = [7u8; 32];
        assert!(!verify_commit_token(
            &secret, "deadbeef", "op-1", "slug-a", "Q"
        ));
    }

    #[test]
    fn commit_token_rejects_different_secret() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let t = make_commit_token(&a, "op-1", "slug-a", "Q");
        assert!(!verify_commit_token(&b, &t, "op-1", "slug-a", "Q"));
    }

    #[test]
    fn slug_is_safe_rejects_reserved_and_bad_chars() {
        assert!(slug_is_safe("foo"));
        assert!(slug_is_safe("foo-bar_2"));
        assert!(!slug_is_safe(""));
        assert!(!slug_is_safe("_ask"));
        assert!(!slug_is_safe("foo/bar"));
        assert!(!slug_is_safe(".hidden"));
    }

    #[test]
    fn estimated_cost_floor_is_one() {
        assert_eq!(estimated_cost_credits(0), 1);
        assert_eq!(estimated_cost_credits(1), 2);
        assert_eq!(estimated_cost_credits(5), 10);
    }
}
