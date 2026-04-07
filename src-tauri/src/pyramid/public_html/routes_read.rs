//! Read-route handlers for the post-agents-retro public web surface (WS-C).
//!
//! Mounts:
//! - `GET  /p/`                       — index of public pyramids on this node
//! - `GET  /p/{slug}`                 — pyramid home (apex + topic TOC + ask form)
//! - `GET  /p/{slug}/{node_id}`       — single node view
//!
//! All three handlers run with `PublicAuthSource::Anonymous` for V1; the real
//! `with_public_or_session_auth` filter (WS-A) plugs in at the assembly site
//! in `mod.rs` once it lands. Tier enforcement is inlined here as a basic
//! "anonymous + non-public => 404" check so the read path works against the
//! Phase 0.5 skeleton without WS-A's helper.

use crate::pyramid::db;
use crate::pyramid::public_html::auth::{
    csrf_nonce, enforce_public_tier, issue_anon_session_cookie, read_cookie, ANON_SESSION_COOKIE,
    PublicAuthSource, WIRE_SESSION_COOKIE,
};
use crate::pyramid::public_html::rate_limit;
use crate::pyramid::public_html::etag::{
    etag_for_node, etag_for_pyramid, matches_inm, not_modified,
};
use crate::pyramid::public_html::render::{
    esc, node_state_class, page, page_with_etag, prov_footer, status_page,
};
use crate::pyramid::public_html::reserved::is_reserved_subpath;
use crate::pyramid::types::PyramidNode;
use crate::pyramid::PyramidState;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use warp::filters::BoxedFilter;
use warp::{Filter, Rejection};

// WS-G caps (plan v3.2 §Verification).
const SEARCH_QUERY_MAX: usize = 256;
const SEARCH_RESULT_CAP: usize = 50;
const TREE_NODE_CAP: usize = 500;
const TREE_DEPTH_CAP: i64 = 4;
const FOLIO_NODE_CAP: usize = 500;
const FOLIO_DEPTH_DEFAULT: i64 = 2;
const FOLIO_DEPTH_MAX: i64 = 4;

/// Resolve the public auth identity for an incoming read. Mirrors
/// `routes_ask::resolve_auth` and `auth::with_public_or_session_auth`. Used
/// by `gate()` so per-IP rate limits are keyed on a real client identifier
/// (P1-4: previously every anonymous reader shared a single empty bucket).
async fn resolve_auth(
    headers: &warp::http::HeaderMap,
    peer: Option<std::net::SocketAddr>,
    state: &PyramidState,
    jwt_public_key: &Arc<tokio::sync::RwLock<String>>,
) -> PublicAuthSource {
    use crate::http_utils::ct_eq;
    if let Some(h) = headers.get("authorization").and_then(|h| h.to_str().ok()) {
        if let Some(token) = h.strip_prefix("Bearer ") {
            let local = { state.config.read().await.auth_token.clone() };
            if !local.is_empty() && ct_eq(token, &local) {
                return PublicAuthSource::LocalOperator;
            }
            if token.matches('.').count() == 2 {
                let pk_str = jwt_public_key.read().await;
                if !pk_str.is_empty() {
                    if let Ok(claims) = crate::server::verify_pyramid_query_jwt(token, &pk_str) {
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
    if let Some(wire_tok) = read_cookie(headers, WIRE_SESSION_COOKIE) {
        if !wire_tok.is_empty() {
            let sess_opt = {
                let conn = state.reader.lock().await;
                crate::pyramid::public_html::web_sessions::lookup(&conn, &wire_tok)
                    .ok()
                    .flatten()
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
    PublicAuthSource::Anonymous {
        client_key: crate::pyramid::public_html::auth::client_key(headers, peer),
    }
}

async fn gate(
    state: &Arc<PyramidState>,
    slug: &str,
    auth: &PublicAuthSource,
) -> Result<(), warp::reply::Response> {
    if enforce_public_tier(state, slug, auth).await.is_err() {
        return Err(not_found_page());
    }
    let rl = rate_limit::global();
    if let Err(e) = rate_limit::check_for_reads(&rl, auth).await {
        return Err(rate_limited_page(e.retry_after));
    }
    Ok(())
}

fn rate_limited_page(retry_after: u64) -> warp::reply::Response {
    let body = format!(
        "<h1>429</h1>\n\
         <p class=\"empty\">Too many requests. Retry in {s}s.</p>\n",
        s = retry_after
    );
    let mut resp = status_page(429, "Rate limited — Wire Node", &body);
    resp.headers_mut().insert(
        "retry-after",
        warp::http::HeaderValue::from(retry_after),
    );
    resp
}

/// Resolve the session token to bind a CSRF nonce against. Mirrors
/// `routes_ask::csrf_session_token` exactly: prefer wire_session, fall back
/// to anon_session, fall back to empty string. The verifier in routes_ask
/// uses the same selection — it MUST stay in sync.
fn csrf_session_token_for_form(headers: &warp::http::HeaderMap) -> String {
    if let Some(t) = read_cookie(headers, WIRE_SESSION_COOKIE) {
        if !t.is_empty() {
            return t;
        }
    }
    read_cookie(headers, ANON_SESSION_COOKIE).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Build the boxed filter chain for the WS-C read routes. Each handler
/// resolves auth inline via `resolve_auth(headers, peer, state, jwt_pk)`
/// (P1-4) so per-IP rate limits key on real client identifiers, not a
/// single shared empty bucket.
pub fn read_routes(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
) -> BoxedFilter<(warp::reply::Response,)> {
    let state_idx = state.clone();
    let index = warp::path("p")
        .and(warp::path::end())
        .and(warp::get())
        .and_then(move || {
            let state = state_idx.clone();
            async move { Ok::<_, Rejection>(handle_index(state).await) }
        });

    let state_home = state.clone();
    let jwt_home = jwt_public_key.clone();
    let pyramid_home = warp::path("p")
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::filters::addr::remote())
        .and(warp::header::headers_cloned())
        .and_then(
            move |slug: String,
                  peer: Option<std::net::SocketAddr>,
                  headers: warp::http::HeaderMap| {
                let state = state_home.clone();
                let jwt_pk = jwt_home.clone();
                async move {
                    Ok::<_, Rejection>(
                        handle_pyramid_home(state, jwt_pk, slug, peer, headers).await,
                    )
                }
            },
        );

    let state_search = state.clone();
    let jwt_search = jwt_public_key.clone();
    let search = warp::path("p")
        .and(warp::path::param::<String>())
        .and(warp::path("search"))
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::filters::addr::remote())
        .and(warp::header::headers_cloned())
        .and(warp::query::<HashMap<String, String>>())
        .and_then(
            move |slug: String,
                  peer: Option<std::net::SocketAddr>,
                  headers: warp::http::HeaderMap,
                  q: HashMap<String, String>| {
                let state = state_search.clone();
                let jwt_pk = jwt_search.clone();
                async move {
                    Ok::<_, Rejection>(handle_search(state, jwt_pk, slug, peer, headers, q).await)
                }
            },
        );

    let state_tree = state.clone();
    let jwt_tree = jwt_public_key.clone();
    let tree = warp::path("p")
        .and(warp::path::param::<String>())
        .and(warp::path("tree"))
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::filters::addr::remote())
        .and(warp::header::headers_cloned())
        .and_then(
            move |slug: String,
                  peer: Option<std::net::SocketAddr>,
                  headers: warp::http::HeaderMap| {
                let state = state_tree.clone();
                let jwt_pk = jwt_tree.clone();
                async move {
                    Ok::<_, Rejection>(handle_tree(state, jwt_pk, slug, peer, headers).await)
                }
            },
        );

    let state_glossary = state.clone();
    let jwt_glossary = jwt_public_key.clone();
    let glossary = warp::path("p")
        .and(warp::path::param::<String>())
        .and(warp::path("glossary"))
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::filters::addr::remote())
        .and(warp::header::headers_cloned())
        .and_then(
            move |slug: String,
                  peer: Option<std::net::SocketAddr>,
                  headers: warp::http::HeaderMap| {
                let state = state_glossary.clone();
                let jwt_pk = jwt_glossary.clone();
                async move {
                    Ok::<_, Rejection>(handle_glossary(state, jwt_pk, slug, peer, headers).await)
                }
            },
        );

    let state_folio = state.clone();
    let jwt_folio = jwt_public_key.clone();
    let folio = warp::path("p")
        .and(warp::path::param::<String>())
        .and(warp::path("folio"))
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::filters::addr::remote())
        .and(warp::header::headers_cloned())
        .and(warp::query::<HashMap<String, String>>())
        .and_then(
            move |slug: String,
                  peer: Option<std::net::SocketAddr>,
                  headers: warp::http::HeaderMap,
                  q: HashMap<String, String>| {
                let state = state_folio.clone();
                let jwt_pk = jwt_folio.clone();
                async move {
                    Ok::<_, Rejection>(handle_folio(state, jwt_pk, slug, peer, headers, q).await)
                }
            },
        );

    let state_node = state.clone();
    let jwt_node = jwt_public_key.clone();
    let single_node = warp::path("p")
        .and(warp::path::param::<String>())
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::filters::addr::remote())
        .and(warp::header::headers_cloned())
        .and_then(
            move |slug: String,
                  node_id: String,
                  peer: Option<std::net::SocketAddr>,
                  headers: warp::http::HeaderMap| {
                let state = state_node.clone();
                let jwt_pk = jwt_node.clone();
                async move {
                    Ok::<_, Rejection>(
                        handle_single_node(state, jwt_pk, slug, node_id, peer, headers).await,
                    )
                }
            },
        );

    // Literal sub-paths (search/tree/glossary/folio) MUST be ordered before
    // the `{slug}/{node_id}` catchall so they win the match.
    index
        .or(pyramid_home)
        .unify()
        .or(search)
        .unify()
        .or(tree)
        .unify()
        .or(glossary)
        .unify()
        .or(folio)
        .unify()
        .or(single_node)
        .unify()
        .boxed()
}

// ---------------------------------------------------------------------------
// Tier check (inlined fallback for Phase 1; WS-A replaces this)
// ---------------------------------------------------------------------------

// (Removed `check_anon_tier`: superseded by `gate()` which calls
// `enforce_public_tier` with the resolved auth identity. P1-4.)

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn handle_index(state: Arc<PyramidState>) -> warp::reply::Response {
    let conn = state.reader.lock().await;
    let slugs = match db::list_slugs(&conn) {
        Ok(v) => v,
        Err(e) => return error_500(&format!("list_slugs failed: {e}")),
    };

    // Walk each slug's tier inline so we don't surface non-public pyramids on
    // the anonymous index. The desktop UI / operator surfaces have their own
    // listings; this one is anti-enumeration.
    let mut items = String::new();
    for info in &slugs {
        let tier = db::get_access_tier(&conn, &info.slug)
            .map(|(t, _, _)| t)
            .unwrap_or_else(|_| "public".to_string());
        if tier != "public" {
            continue;
        }
        // Try to read the apex headline (depth = max_depth) for a nicer card.
        let apex_headline = apex_headline_for(&conn, &info.slug, info.max_depth);
        items.push_str(&format!(
            "<li class=\"slug-card\">\
               <a href=\"/p/{slug_attr}\"><strong>{slug_text}</strong></a>\
               <div class=\"apex\">{headline}</div>\
             </li>\n",
            slug_attr = esc(&info.slug),
            slug_text = esc(&info.slug),
            headline = esc(&apex_headline.unwrap_or_else(|| "(empty pyramid)".to_string())),
        ));
    }

    drop(conn);

    let body = if items.is_empty() {
        "<h1>WIRE NODE</h1>\n\
         <p class=\"empty\">No public pyramids on this node yet.</p>\n"
            .to_string()
    } else {
        format!(
            "<h1>WIRE NODE</h1>\n\
             <p class=\"sub\">Public pyramids on this node:</p>\n\
             <ul class=\"slug-list\">\n{items}</ul>\n"
        )
    };

    page("Wire Node — Public Pyramids", &body, "no-cache, must-revalidate")
}

async fn handle_pyramid_home(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
    slug: String,
    peer: Option<std::net::SocketAddr>,
    headers: warp::http::HeaderMap,
) -> warp::reply::Response {
    if slug.starts_with('_') {
        return not_found_page();
    }
    let auth = resolve_auth(&headers, peer, &state, &jwt_public_key).await;
    if let Err(resp) = gate(&state, &slug, &auth).await {
        return resp;
    }

    let conn = state.reader.lock().await;

    let info = match db::get_slug(&conn, &slug) {
        Ok(Some(i)) => i,
        Ok(None) => return not_found_page(),
        Err(e) => return error_500(&format!("get_slug failed: {e}")),
    };

    // Pyramid-level ETag (A10): slug + pyramid_slugs.updated_at. The
    // Phase 0.5 skeleton guarantees the column exists via an idempotent
    // ALTER TABLE; if the lookup fails, fall back to created_at so the
    // ETag is still deterministic and cache-safe.
    let pyramid_updated_at: String = conn
        .query_row(
            "SELECT updated_at FROM pyramid_slugs WHERE slug = ?1",
            rusqlite::params![&slug],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| info.created_at.clone());
    let etag = etag_for_pyramid(&slug, &pyramid_updated_at);
    if matches_inm(&headers, &etag) {
        return not_modified(&etag);
    }

    // Apex node = the highest-depth (most distilled) live node.
    let apex_nodes = match db::get_nodes_at_depth(&conn, &slug, info.max_depth) {
        Ok(v) => v,
        Err(e) => return error_500(&format!("get_nodes_at_depth failed: {e}")),
    };
    let apex = apex_nodes.into_iter().next();

    // Children of apex (depth-1 topics) form the table of contents.
    let depth_minus_one = if info.max_depth > 0 { info.max_depth - 1 } else { 0 };
    let toc_nodes = if let Some(ref a) = apex {
        // Prefer apex.children for ordering; fall back to depth scan.
        let mut found = Vec::new();
        for child_id in &a.children {
            if let Ok(Some(n)) = db::get_node(&conn, &slug, child_id) {
                found.push(n);
            }
        }
        if found.is_empty() {
            db::get_nodes_at_depth(&conn, &slug, depth_minus_one).unwrap_or_default()
        } else {
            found
        }
    } else {
        db::get_nodes_at_depth(&conn, &slug, depth_minus_one).unwrap_or_default()
    };

    drop(conn);

    let title = format!("{} — Wire Node", slug);

    // Lookup apex's wire handle path (if published) for prov footer.
    let apex_wire_handle: Option<String> = if let Some(ref a) = apex {
        let conn2 = state.reader.lock().await;
        db::get_wire_handle_path(&conn2, &slug, &a.id)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    let mut body = String::new();
    body.push_str(&format!("<h1>{}</h1>\n", esc(&slug)));
    if let Some(ref a) = apex {
        body.push_str(&format!(
            "<article class=\"node node--{state}\">\n\
               <h2>{headline}</h2>\n\
               <p class=\"distilled\">{distilled}</p>\n\
               {prov}\n\
             </article>\n",
            state = node_state_class(a),
            headline = esc(&a.headline),
            distilled = esc(&a.distilled),
            prov = prov_footer(a, apex_wire_handle.as_deref()),
        ));
    } else {
        body.push_str("<p class=\"empty\">ASK SOMETHING TO BEGIN</p>\n");
    }

    if !toc_nodes.is_empty() {
        body.push_str("<nav class=\"toc\"><h3>Topics</h3><ul>\n");
        for child in &toc_nodes {
            body.push_str(&format!(
                "<li><a href=\"/p/{slug}/{nid}\">{headline}</a></li>\n",
                slug = esc(&slug),
                nid = esc(&child.id),
                headline = esc(&child.headline),
            ));
        }
        body.push_str("</ul></nav>\n");
    }

    // Real CSRF nonce bound to (session token, slug, current 5-min window).
    // If the visitor has no anon_session cookie yet, mint one and pipe its
    // Set-Cookie header through with the response. The verifier in
    // routes_ask::handle_ask_post uses the EXACT same csrf_session_token
    // selection (wire_session → anon_session → empty), so the nonce we
    // issue here will round-trip cleanly.
    let mut set_anon_cookie: Option<String> = None;
    let mut sess_tok = csrf_session_token_for_form(&headers);
    if sess_tok.is_empty() {
        let (tok, header) = issue_anon_session_cookie();
        sess_tok = tok;
        set_anon_cookie = Some(header);
    }
    let real_nonce = csrf_nonce(&state.csrf_secret, &sess_tok, &slug);

    body.push_str(&format!(
        "<form class=\"ask\" action=\"/p/{slug}/_ask\" method=\"post\">\n\
           <label for=\"q\">Ask the pyramid:</label>\n\
           <input id=\"q\" name=\"question\" type=\"text\" autocomplete=\"off\" required>\n\
           <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\n\
           <button type=\"submit\">ASK</button>\n\
         </form>\n",
        slug = esc(&slug),
        csrf = esc(&real_nonce),
    ));

    let banner = crate::pyramid::public_html::ascii_art::get_banner_for_slug(&state, &slug).await;
    let mut resp = page_with_etag(
        &title,
        &body,
        "no-cache, must-revalidate",
        Some(&etag),
        banner.as_deref(),
    );
    if let Some(cookie) = set_anon_cookie {
        if let Ok(hv) = warp::http::HeaderValue::from_str(&cookie) {
            resp.headers_mut().append(warp::http::header::SET_COOKIE, hv);
        }
    }
    resp
}

async fn handle_single_node(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
    slug: String,
    node_id: String,
    peer: Option<std::net::SocketAddr>,
    headers: warp::http::HeaderMap,
) -> warp::reply::Response {
    if slug.starts_with('_') {
        return not_found_page();
    }
    if is_reserved_subpath(&node_id) {
        return not_found_page();
    }
    let auth = resolve_auth(&headers, peer, &state, &jwt_public_key).await;
    if let Err(resp) = gate(&state, &slug, &auth).await {
        return resp;
    }

    let conn = state.reader.lock().await;
    let node = match db::get_node(&conn, &slug, &node_id) {
        Ok(Some(n)) => n,
        Ok(None) => return not_found_page(),
        Err(e) => return error_500(&format!("get_node failed: {e}")),
    };

    // Per-node ETag (A10). Because PyramidNode has no updated_at column
    // today, etag_for_node() hashes the rendered fields. A client whose
    // cache entry matches the current computed tag gets a bare 304 with
    // no body. There's a small race window where the node could change
    // between get_node() and the render below — we accept it: the ETag
    // correctly describes the revision we actually serve on THIS render,
    // and any subsequent write will flip the tag on the next request.
    let etag = etag_for_node(&node);
    if matches_inm(&headers, &etag) {
        return not_modified(&etag);
    }

    // Resolve the children for the in-page TOC.
    let mut child_nodes: Vec<PyramidNode> = Vec::new();
    for cid in &node.children {
        if let Ok(Some(n)) = db::get_node(&conn, &slug, cid) {
            child_nodes.push(n);
        }
    }

    drop(conn);

    let title = format!("{} — {}", node.headline, slug);
    let mut body = String::new();
    body.push_str(&format!(
        "<nav class=\"crumbs\"><a href=\"/p/{slug}\">{slug_text}</a></nav>\n",
        slug = esc(&slug),
        slug_text = esc(&slug),
    ));
    body.push_str(&format!(
        "<article class=\"node node--{state}\">\n\
           <h1>{headline}</h1>\n\
           <p class=\"distilled\">{distilled}</p>\n",
        state = node_state_class(&node),
        headline = esc(&node.headline),
        distilled = esc(&node.distilled),
    ));

    if !node.terms.is_empty() {
        body.push_str("<section class=\"terms\"><h3>Terms</h3><dl>\n");
        for t in &node.terms {
            body.push_str(&format!(
                "<dt>{}</dt><dd>{}</dd>\n",
                esc(&t.term),
                esc(&t.definition),
            ));
        }
        body.push_str("</dl></section>\n");
    }

    if !child_nodes.is_empty() {
        body.push_str("<nav class=\"children\"><h3>Children</h3><ul>\n");
        for c in &child_nodes {
            body.push_str(&format!(
                "<li><a href=\"/p/{slug}/{nid}\">{headline}</a></li>\n",
                slug = esc(&slug),
                nid = esc(&c.id),
                headline = esc(&c.headline),
            ));
        }
        body.push_str("</ul></nav>\n");
    }

    // Cross-pyramid web edges: V2 scope (not rendered in V1).

    // P1-6: prefer Wire handle path when this node has been published.
    let wire_handle = {
        let conn2 = state.reader.lock().await;
        db::get_wire_handle_path(&conn2, &slug, &node.id)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
    };
    body.push_str(&prov_footer(&node, wire_handle.as_deref()));
    body.push_str("\n</article>\n");

    let banner = crate::pyramid::public_html::ascii_art::get_banner_for_slug(&state, &slug).await;
    page_with_etag(
        &title,
        &body,
        "no-cache, must-revalidate",
        Some(&etag),
        banner.as_deref(),
    )
}

// ---------------------------------------------------------------------------
// WS-G handlers: search, tree, glossary, folio
// ---------------------------------------------------------------------------

async fn handle_search(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
    slug: String,
    peer: Option<std::net::SocketAddr>,
    headers: warp::http::HeaderMap,
    query: HashMap<String, String>,
) -> warp::reply::Response {
    if slug.starts_with('_') {
        return not_found_page();
    }
    let auth = resolve_auth(&headers, peer, &state, &jwt_public_key).await;
    if let Err(resp) = gate(&state, &slug, &auth).await {
        return resp;
    }

    let raw_q = query.get("q").map(|s| s.as_str()).unwrap_or("").trim();
    let q: String = raw_q.chars().take(SEARCH_QUERY_MAX).collect();

    let title = format!("search: {} — {}", q, slug);
    let mut body = String::new();
    body.push_str(&format!(
        "<nav class=\"crumbs\"><a href=\"/p/{slug}\">{slug_text}</a> / search</nav>\n",
        slug = esc(&slug),
        slug_text = esc(&slug),
    ));
    body.push_str(&format!(
        "<form class=\"search\" action=\"/p/{slug}/search\" method=\"get\">\n\
           <label for=\"q\">Search:</label>\n\
           <input id=\"q\" name=\"q\" type=\"text\" value=\"{qv}\" autocomplete=\"off\" maxlength=\"256\">\n\
           <button type=\"submit\">GO</button>\n\
         </form>\n",
        slug = esc(&slug),
        qv = esc(&q),
    ));

    if q.is_empty() {
        body.push_str("<p class=\"empty\">Type a query above to search this pyramid.</p>\n");
        let banner =
            crate::pyramid::public_html::ascii_art::get_banner_for_slug(&state, &slug).await;
        return page_with_etag(
            &title,
            &body,
            "no-cache, must-revalidate",
            None,
            banner.as_deref(),
        );
    }

    let conn = state.reader.lock().await;
    let hits = match crate::pyramid::query::search(&conn, &slug, &q) {
        Ok(v) => v,
        Err(e) => return error_500(&format!("search failed: {e}")),
    };
    drop(conn);

    let total = hits.len();
    let shown = hits.iter().take(SEARCH_RESULT_CAP);

    body.push_str(&format!(
        "<h1>results for <q>{qe}</q></h1>\n",
        qe = esc(&q),
    ));

    if total == 0 {
        body.push_str("<p class=\"empty\">No matches.</p>\n");
    } else {
        if total > SEARCH_RESULT_CAP {
            body.push_str(&format!(
                "<p class=\"sub\">showing first {cap} of {total} matches</p>\n",
                cap = SEARCH_RESULT_CAP,
                total = total,
            ));
        } else {
            body.push_str(&format!("<p class=\"sub\">{total} match(es)</p>\n"));
        }
        body.push_str("<ul class=\"search-results\">\n");
        for hit in shown {
            body.push_str(&format!(
                "<li><article class=\"search-result\">\
                   <a href=\"/p/{slug}/{nid}\"><strong>{headline}</strong></a>\
                   <p class=\"snippet\">{snippet}</p>\
                 </article></li>\n",
                slug = esc(&slug),
                nid = esc(&hit.node_id),
                headline = esc(&hit.headline),
                snippet = esc(&hit.snippet),
            ));
        }
        body.push_str("</ul>\n");
    }

    let banner = crate::pyramid::public_html::ascii_art::get_banner_for_slug(&state, &slug).await;
    page_with_etag(
        &title,
        &body,
        "no-cache, must-revalidate",
        None,
        banner.as_deref(),
    )
}

async fn handle_tree(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
    slug: String,
    peer: Option<std::net::SocketAddr>,
    headers: warp::http::HeaderMap,
) -> warp::reply::Response {
    if slug.starts_with('_') {
        return not_found_page();
    }
    let auth = resolve_auth(&headers, peer, &state, &jwt_public_key).await;
    if let Err(resp) = gate(&state, &slug, &auth).await {
        return resp;
    }

    let conn = state.reader.lock().await;
    let info = match db::get_slug(&conn, &slug) {
        Ok(Some(i)) => i,
        Ok(None) => return not_found_page(),
        Err(e) => return error_500(&format!("get_slug failed: {e}")),
    };
    let all = match db::get_all_live_nodes(&conn, &slug) {
        Ok(v) => v,
        Err(e) => return error_500(&format!("get_all_live_nodes failed: {e}")),
    };
    drop(conn);

    // Enforce depth cap: show only top (TREE_DEPTH_CAP + 1) layers.
    let min_depth = info.max_depth.saturating_sub(TREE_DEPTH_CAP).max(0);

    // Build id -> node lookup; filter by depth window.
    let mut by_id: HashMap<String, PyramidNode> = HashMap::new();
    for n in all.into_iter() {
        if n.depth >= min_depth {
            by_id.insert(n.id.clone(), n);
        }
    }

    // Find roots at max_depth (apex layer within the window).
    let mut roots: Vec<&PyramidNode> = by_id
        .values()
        .filter(|n| n.depth == info.max_depth)
        .collect();
    roots.sort_by(|a, b| a.headline.cmp(&b.headline));

    let total_in_window = by_id.len();

    // Walk and render, cap at TREE_NODE_CAP.
    let mut rendered: usize = 0;
    let mut body = String::new();
    body.push_str(&format!(
        "<nav class=\"crumbs\"><a href=\"/p/{s}\">{st}</a> / tree</nav>\n",
        s = esc(&slug),
        st = esc(&slug),
    ));
    body.push_str("<h1>tree</h1>\n");

    if roots.is_empty() {
        body.push_str("<p class=\"empty\">This pyramid has no nodes.</p>\n");
        let banner =
            crate::pyramid::public_html::ascii_art::get_banner_for_slug(&state, &slug).await;
        return page_with_etag(
            &format!("{} — tree", slug),
            &body,
            "no-cache, must-revalidate",
            None,
            banner.as_deref(),
        );
    }

    body.push_str("<ul class=\"toc\">\n");
    let mut truncated = false;
    for root in &roots {
        if rendered >= TREE_NODE_CAP {
            truncated = true;
            break;
        }
        render_tree_node(
            root,
            &by_id,
            &slug,
            min_depth,
            &mut body,
            &mut rendered,
            &mut truncated,
        );
    }
    body.push_str("</ul>\n");

    if truncated || total_in_window > TREE_NODE_CAP {
        body.push_str(&format!(
            "<p class=\"sub\">(showing first {cap} of {total} nodes — narrow with <a href=\"/p/{slug}/search\">search</a>)</p>\n",
            cap = TREE_NODE_CAP,
            total = total_in_window,
            slug = esc(&slug),
        ));
    }

    let banner = crate::pyramid::public_html::ascii_art::get_banner_for_slug(&state, &slug).await;
    page_with_etag(
        &format!("{} — tree", slug),
        &body,
        "no-cache, must-revalidate",
        None,
        banner.as_deref(),
    )
}

fn render_tree_node(
    node: &PyramidNode,
    by_id: &HashMap<String, PyramidNode>,
    slug: &str,
    min_depth: i64,
    body: &mut String,
    rendered: &mut usize,
    truncated: &mut bool,
) {
    if *rendered >= TREE_NODE_CAP {
        *truncated = true;
        return;
    }
    *rendered += 1;

    body.push_str(&format!(
        "<li class=\"node--{state}\"><a href=\"/p/{slug}/{nid}\">{headline}</a>",
        state = node_state_class(node),
        slug = esc(slug),
        nid = esc(&node.id),
        headline = esc(&node.headline),
    ));

    // Recurse into children that are inside the depth window.
    let child_nodes: Vec<&PyramidNode> = node
        .children
        .iter()
        .filter_map(|cid| by_id.get(cid))
        .filter(|c| c.depth >= min_depth)
        .collect();

    if !child_nodes.is_empty() && *rendered < TREE_NODE_CAP {
        body.push_str("<ul>\n");
        for c in child_nodes {
            if *rendered >= TREE_NODE_CAP {
                *truncated = true;
                break;
            }
            render_tree_node(c, by_id, slug, min_depth, body, rendered, truncated);
        }
        body.push_str("</ul>\n");
    }
    body.push_str("</li>\n");
}

async fn handle_glossary(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
    slug: String,
    peer: Option<std::net::SocketAddr>,
    headers: warp::http::HeaderMap,
) -> warp::reply::Response {
    if slug.starts_with('_') {
        return not_found_page();
    }
    let auth = resolve_auth(&headers, peer, &state, &jwt_public_key).await;
    if let Err(resp) = gate(&state, &slug, &auth).await {
        return resp;
    }

    let conn = state.reader.lock().await;
    let all = match db::get_all_live_nodes(&conn, &slug) {
        Ok(v) => v,
        Err(e) => return error_500(&format!("get_all_live_nodes failed: {e}")),
    };
    drop(conn);

    // Collect terms. Prefer the deepest (most distilled) node's definition
    // when duplicates appear. Sort nodes by depth DESC so last-write-wins ==
    // shallowest; we flip that by iterating shallow->deep and overwriting.
    let mut sorted_nodes = all;
    sorted_nodes.sort_by_key(|n| n.depth);
    let mut by_lower: HashMap<String, (String, String)> = HashMap::new(); // lower -> (term, def)
    for n in &sorted_nodes {
        for t in &n.terms {
            let lower = t.term.trim().to_lowercase();
            if lower.is_empty() {
                continue;
            }
            by_lower.insert(lower, (t.term.clone(), t.definition.clone()));
        }
    }

    let mut entries: Vec<(String, String)> = by_lower.into_values().collect();
    entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

    let mut body = String::new();
    body.push_str(&format!(
        "<nav class=\"crumbs\"><a href=\"/p/{s}\">{st}</a> / glossary</nav>\n",
        s = esc(&slug),
        st = esc(&slug),
    ));
    body.push_str("<h1>glossary</h1>\n");

    if entries.is_empty() {
        body.push_str("<p class=\"empty\">this pyramid has no terms yet.</p>\n");
    } else {
        body.push_str("<dl class=\"glossary\">\n");
        for (term, def) in &entries {
            body.push_str(&format!(
                "<dt>{}</dt><dd>{}</dd>\n",
                esc(term),
                esc(def),
            ));
        }
        body.push_str("</dl>\n");
    }

    let banner = crate::pyramid::public_html::ascii_art::get_banner_for_slug(&state, &slug).await;
    page_with_etag(
        &format!("{} — glossary", slug),
        &body,
        "no-cache, must-revalidate",
        None,
        banner.as_deref(),
    )
}

async fn handle_folio(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
    slug: String,
    peer: Option<std::net::SocketAddr>,
    headers: warp::http::HeaderMap,
    query: HashMap<String, String>,
) -> warp::reply::Response {
    if slug.starts_with('_') {
        return not_found_page();
    }
    let auth = resolve_auth(&headers, peer, &state, &jwt_public_key).await;
    if let Err(resp) = gate(&state, &slug, &auth).await {
        return resp;
    }

    // Parse ?depth. Accept 0..=FOLIO_DEPTH_MAX; default 2; reject garbage with
    // a soft default-to-2 (rather than 400 — keeps URL-sharing forgiving).
    let depth_req: i64 = query
        .get("depth")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(FOLIO_DEPTH_DEFAULT);
    let depth = depth_req.clamp(0, FOLIO_DEPTH_MAX);

    let conn = state.reader.lock().await;
    let info = match db::get_slug(&conn, &slug) {
        Ok(Some(i)) => i,
        Ok(None) => return not_found_page(),
        Err(e) => return error_500(&format!("get_slug failed: {e}")),
    };
    let all = match db::get_all_live_nodes(&conn, &slug) {
        Ok(v) => v,
        Err(e) => return error_500(&format!("get_all_live_nodes failed: {e}")),
    };
    drop(conn);

    let mut by_id: HashMap<String, PyramidNode> = HashMap::new();
    for n in all.into_iter() {
        by_id.insert(n.id.clone(), n);
    }

    let apex_id: Option<String> = by_id
        .values()
        .filter(|n| n.depth == info.max_depth)
        .map(|n| n.id.clone())
        .next();

    let min_allowed_depth = info.max_depth.saturating_sub(depth).max(0);

    let mut body = String::new();
    body.push_str(&format!(
        "<nav class=\"crumbs\"><a href=\"/p/{s}\">{st}</a> / folio</nav>\n",
        s = esc(&slug),
        st = esc(&slug),
    ));
    body.push_str(&format!(
        "<h1>folio — {s}</h1>\n\
         <p class=\"sub\">depth: {d} \
           (<a href=\"/p/{se}/folio?depth=0\">0</a> \
            <a href=\"/p/{se}/folio?depth=1\">1</a> \
            <a href=\"/p/{se}/folio?depth=2\">2</a> \
            <a href=\"/p/{se}/folio?depth=3\">3</a> \
            <a href=\"/p/{se}/folio?depth=4\">4</a>)</p>\n",
        s = esc(&slug),
        se = esc(&slug),
        d = depth,
    ));

    let apex = match apex_id.and_then(|id| by_id.get(&id).cloned()) {
        Some(n) => n,
        None => {
            body.push_str("<p class=\"empty\">This pyramid has no nodes.</p>\n");
            let banner =
                crate::pyramid::public_html::ascii_art::get_banner_for_slug(&state, &slug).await;
            return page_with_etag(
                &format!("{} — folio", slug),
                &body,
                "no-cache, must-revalidate",
                None,
                banner.as_deref(),
            );
        }
    };

    let mut rendered: usize = 0;
    let mut truncated = false;
    let mut seen: HashSet<String> = HashSet::new();
    render_folio_node(
        &apex,
        &by_id,
        &slug,
        min_allowed_depth,
        &mut body,
        &mut rendered,
        &mut truncated,
        &mut seen,
    );

    if truncated {
        body.push_str(&format!(
            "<p class=\"sub\">(truncated at {cap} nodes)</p>\n",
            cap = FOLIO_NODE_CAP,
        ));
    }

    let banner = crate::pyramid::public_html::ascii_art::get_banner_for_slug(&state, &slug).await;
    page_with_etag(
        &format!("{} — folio", slug),
        &body,
        "no-cache, must-revalidate",
        None,
        banner.as_deref(),
    )
}

fn render_folio_node(
    node: &PyramidNode,
    by_id: &HashMap<String, PyramidNode>,
    slug: &str,
    min_allowed_depth: i64,
    body: &mut String,
    rendered: &mut usize,
    truncated: &mut bool,
    seen: &mut HashSet<String>,
) {
    if *rendered >= FOLIO_NODE_CAP {
        *truncated = true;
        return;
    }
    if !seen.insert(node.id.clone()) {
        return;
    }
    *rendered += 1;

    body.push_str(&format!(
        "<section>\n\
         <article id=\"{nid}\" class=\"node node--{state}\">\n\
           <h2>{headline}</h2>\n\
           <p class=\"distilled\">{distilled}</p>\n",
        nid = esc(&node.id),
        state = node_state_class(node),
        headline = esc(&node.headline),
        distilled = esc(&node.distilled),
    ));

    if !node.topics.is_empty() {
        body.push_str("<ul class=\"topics\">\n");
        for t in &node.topics {
            body.push_str(&format!(
                "<li><strong>{}</strong>: {}</li>\n",
                esc(&t.name),
                esc(&t.current),
            ));
        }
        body.push_str("</ul>\n");
    }

    // P1-6: render-only prov footer for folio uses local: fallback (avoiding
    // an async DB hop inside this sync recursive helper). The dedicated node
    // page exposes the resolved Wire handle path.
    body.push_str(&prov_footer(node, None));
    body.push_str("\n</article>\n");

    if node.depth > min_allowed_depth {
        for cid in &node.children {
            if *rendered >= FOLIO_NODE_CAP {
                *truncated = true;
                break;
            }
            if let Some(child) = by_id.get(cid) {
                render_folio_node(
                    child,
                    by_id,
                    slug,
                    min_allowed_depth,
                    body,
                    rendered,
                    truncated,
                    seen,
                );
            }
        }
    }

    body.push_str("</section>\n");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Best-effort apex headline lookup for the index card. Returns None when
/// the pyramid has no nodes yet.
fn apex_headline_for(
    conn: &rusqlite::Connection,
    slug: &str,
    max_depth: i64,
) -> Option<String> {
    let nodes = db::get_nodes_at_depth(conn, slug, max_depth).ok()?;
    nodes.into_iter().next().map(|n| n.headline)
}

fn not_found_page() -> warp::reply::Response {
    status_page(
        404,
        "Not found — Wire Node",
        "<h1>404</h1>\n\
         <p class=\"empty\">No such page.</p>\n\
         <p><a href=\"/p/\">&larr; Back to public pyramids</a></p>\n",
    )
}

fn error_500(msg: &str) -> warp::reply::Response {
    tracing::error!(public_html_read_error = %msg);
    status_page(
        500,
        "Server error — Wire Node",
        "<h1>500</h1>\n\
         <p class=\"empty\">Something went wrong rendering this page.</p>\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_query_xss_is_escaped() {
        // The search query is the highest-risk XSS surface. Simulate the
        // two places `q` is interpolated into the search results page: the
        // <q> in the title/headline and the form value attribute.
        let payload = "<script>alert('x')</script>";
        let escaped = esc(payload);
        assert!(!escaped.contains("<script>"));
        assert!(escaped.contains("&lt;script&gt;"));
        assert!(escaped.contains("&#x27;"));

        // And a mocked-up results fragment the handler would render:
        let fragment = format!(
            "<h1>results for <q>{qe}</q></h1>\n\
             <input value=\"{qv}\">",
            qe = esc(payload),
            qv = esc(payload),
        );
        assert!(!fragment.contains("<script>"));
        assert!(fragment.contains("&lt;script&gt;alert(&#x27;x&#x27;)&lt;/script&gt;"));
    }

    #[test]
    fn search_result_snippet_xss_is_escaped() {
        // A snippet returned from the search fn could contain user content
        // from the indexed pyramid. Make sure it round-trips escaped.
        let snippet = "he said <img src=x onerror=alert(1)>";
        let rendered = format!("<p class=\"snippet\">{}</p>", esc(snippet));
        assert!(!rendered.contains("<img"));
        assert!(rendered.contains("&lt;img src=x onerror=alert(1)&gt;"));
    }

    #[test]
    fn search_input_value_attribute_breakout_is_blocked() {
        // An attacker crafts ?q=" onfocus=alert(1) x=" trying to break out of
        // the <input value="..."> attribute. esc() must escape the `"` so the
        // attribute boundary is preserved.
        let payload = "\" onfocus=alert(1) x=\"";
        let rendered = format!("<input value=\"{}\">", esc(payload));
        // The only literal `"` chars remaining must be the two that wrap value.
        let quote_count = rendered.matches('"').count();
        assert_eq!(
            quote_count, 2,
            "attribute breakout possible: {}",
            rendered
        );
        assert!(rendered.contains("&quot;"));
        // The literal `onfocus=` survives as inert text inside the escaped
        // attribute value — what matters is that the `"` boundary holds and
        // the browser never sees it as a new attribute.
    }

    #[test]
    fn folio_depth_parser_clamps_and_tolerates_garbage() {
        // Mirrors handle_folio's parse logic. Any of these must land in 0..=4
        // without panicking.
        let cases: &[(&str, i64)] = &[
            ("2", 2),
            ("0", 0),
            ("4", 4),
            ("99", FOLIO_DEPTH_MAX),
            ("-1", 0),
            ("foo", FOLIO_DEPTH_DEFAULT),
            ("", FOLIO_DEPTH_DEFAULT),
            ("2; DROP TABLE", FOLIO_DEPTH_DEFAULT),
        ];
        for (raw, expected) in cases {
            let parsed = raw
                .parse::<i64>()
                .ok()
                .unwrap_or(FOLIO_DEPTH_DEFAULT)
                .clamp(0, FOLIO_DEPTH_MAX);
            assert_eq!(parsed, *expected, "input {:?}", raw);
        }
    }

    #[test]
    fn glossary_case_insensitive_dedupe_deepest_wins() {
        // Mirrors handle_glossary's dedupe logic: iterate shallow -> deep,
        // overwriting by lowercased key, so the deepest occurrence wins.
        use crate::pyramid::types::Term;
        struct Fake {
            depth: i64,
            terms: Vec<Term>,
        }
        let nodes = vec![
            Fake {
                depth: 0,
                terms: vec![Term {
                    term: "Foo".into(),
                    definition: "shallow".into(),
                }],
            },
            Fake {
                depth: 2,
                terms: vec![Term {
                    term: "foo".into(),
                    definition: "deep".into(),
                }],
            },
        ];
        let mut sorted = nodes;
        sorted.sort_by_key(|n| n.depth);
        let mut by_lower: HashMap<String, (String, String)> = HashMap::new();
        for n in &sorted {
            for t in &n.terms {
                let lower = t.term.trim().to_lowercase();
                by_lower.insert(lower, (t.term.clone(), t.definition.clone()));
            }
        }
        assert_eq!(by_lower.len(), 1);
        let (_, def) = by_lower.get("foo").unwrap();
        assert_eq!(def, "deep", "deepest definition should win");
    }
}
