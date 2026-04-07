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
use crate::pyramid::public_html::render::{
    esc, node_state_class, page, prov_footer, status_page,
};
use crate::pyramid::public_html::reserved::is_reserved_subpath;
use crate::pyramid::types::PyramidNode;
use crate::pyramid::PyramidState;
use std::sync::Arc;
use warp::filters::BoxedFilter;
use warp::{Filter, Rejection};

// Placeholder CSRF nonce until WS-A's `csrf_nonce` lands. The ask form needs
// *some* hidden field so WS-H can wire CSRF verification later without
// re-rendering the home template.
const CSRF_PLACEHOLDER: &str = "phase1_placeholder";

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Build the boxed filter chain for the WS-C read routes. The signature
/// matches the placeholder mount in `mod.rs` and the eventual real
/// `public_html::routes()` assembly: a single `Arc<PyramidState>` in,
/// `(warp::reply::Response,)` out.
pub fn read_routes(
    state: Arc<PyramidState>,
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
    let pyramid_home = warp::path("p")
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and_then(move |slug: String| {
            let state = state_home.clone();
            async move { Ok::<_, Rejection>(handle_pyramid_home(state, slug).await) }
        });

    let state_node = state.clone();
    let single_node = warp::path("p")
        .and(warp::path::param::<String>())
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and_then(move |slug: String, node_id: String| {
            let state = state_node.clone();
            async move {
                Ok::<_, Rejection>(handle_single_node(state, slug, node_id).await)
            }
        });

    index.or(pyramid_home).unify().or(single_node).unify().boxed()
}

// ---------------------------------------------------------------------------
// Tier check (inlined fallback for Phase 1; WS-A replaces this)
// ---------------------------------------------------------------------------

/// Anonymous tier gate: return `Some(404 page)` if the slug is not public.
/// Per the v3 plan, anonymous access to non-public pyramids returns 404
/// (anti-enumeration), not 401/403. WS-A's `enforce_public_tier` will
/// supersede this once cookies and operator tokens are wired.
async fn check_anon_tier(state: &PyramidState, slug: &str) -> Option<warp::reply::Response> {
    let conn = state.reader.lock().await;
    let (tier, _price, _circles) = match db::get_access_tier(&conn, slug) {
        Ok(v) => v,
        Err(_) => return Some(not_found_page()),
    };
    if tier == "public" {
        None
    } else {
        Some(not_found_page())
    }
}

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
    slug: String,
) -> warp::reply::Response {
    if slug.starts_with('_') {
        return not_found_page();
    }
    if let Some(deny) = check_anon_tier(&state, &slug).await {
        return deny;
    }

    let conn = state.reader.lock().await;

    let info = match db::get_slug(&conn, &slug) {
        Ok(Some(i)) => i,
        Ok(None) => return not_found_page(),
        Err(e) => return error_500(&format!("get_slug failed: {e}")),
    };

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
            prov = prov_footer(a),
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

    // Question form pinned to the bottom. CSRF placeholder until WS-A lands.
    body.push_str(&format!(
        "<form class=\"ask\" action=\"/p/{slug}/_ask\" method=\"post\">\n\
           <label for=\"q\">Ask the pyramid:</label>\n\
           <input id=\"q\" name=\"q\" type=\"text\" autocomplete=\"off\" required>\n\
           <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\n\
           <button type=\"submit\">ASK</button>\n\
         </form>\n",
        slug = esc(&slug),
        csrf = esc(CSRF_PLACEHOLDER),
    ));

    page(&title, &body, "no-cache, must-revalidate")
}

async fn handle_single_node(
    state: Arc<PyramidState>,
    slug: String,
    node_id: String,
) -> warp::reply::Response {
    if slug.starts_with('_') {
        return not_found_page();
    }
    // Reserved subpath check FIRST. The dedicated handlers for
    // tree/search/glossary/folio/_login/_ws/etc. live in other workstreams
    // and will be mounted earlier in the chain — but if for any reason this
    // catchall fires for one of those keywords, we 404 instead of trying to
    // load it as a node id.
    if is_reserved_subpath(&node_id) {
        return not_found_page();
    }
    if let Some(deny) = check_anon_tier(&state, &slug).await {
        return deny;
    }

    let conn = state.reader.lock().await;
    let node = match db::get_node(&conn, &slug, &node_id) {
        Ok(Some(n)) => n,
        Ok(None) => return not_found_page(),
        Err(e) => return error_500(&format!("get_node failed: {e}")),
    };

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

    // Cross-pyramid web edges: deferred to WS-G — render an empty placeholder
    // so the visual layout matches the eventual full template. WS-G replaces
    // this with the real query against `webbing.rs`.
    body.push_str("<!-- TODO(WS-G): cross-pyramid web edges -->\n");

    body.push_str(&prov_footer(&node));
    body.push_str("\n</article>\n");

    page(&title, &body, "no-cache, must-revalidate")
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
