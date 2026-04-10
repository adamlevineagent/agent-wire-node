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
    details_section, esc, node_state_class, page, page_with_etag, prov_footer, status_page,
    truncate_chars,
};
use crate::pyramid::public_html::reserved::is_reserved_subpath;
use crate::pyramid::types::PyramidNode;
use crate::pyramid::PyramidState;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use warp::filters::BoxedFilter;
use warp::{Filter, Rejection, Reply};

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

    let state_qview = state.clone();
    let jwt_qview = jwt_public_key.clone();
    let question_view = warp::path("p")
        .and(warp::path::param::<String>())
        .and(warp::path("q"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::filters::addr::remote())
        .and(warp::header::headers_cloned())
        .and_then(
            move |source: String,
                  question_slug: String,
                  peer: Option<std::net::SocketAddr>,
                  headers: warp::http::HeaderMap| {
                let state = state_qview.clone();
                let jwt_pk = jwt_qview.clone();
                async move {
                    Ok::<_, Rejection>(
                        handle_question_view(state, jwt_pk, source, question_slug, peer, headers)
                            .await,
                    )
                }
            },
        );

    let state_qfrag = state.clone();
    let jwt_qfrag = jwt_public_key.clone();
    let question_fragment = warp::path("p")
        .and(warp::path::param::<String>())
        .and(warp::path("q"))
        .and(warp::path::param::<String>())
        .and(warp::path("answer.fragment"))
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::filters::addr::remote())
        .and(warp::header::headers_cloned())
        .and_then(
            move |source: String,
                  question_slug: String,
                  peer: Option<std::net::SocketAddr>,
                  headers: warp::http::HeaderMap| {
                let state = state_qfrag.clone();
                let jwt_pk = jwt_qfrag.clone();
                async move {
                    Ok::<_, Rejection>(
                        handle_question_fragment(
                            state,
                            jwt_pk,
                            source,
                            question_slug,
                            peer,
                            headers,
                        )
                        .await,
                    )
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

    // GET /help — agent onboarding endpoint. Returns markdown with the
    // Wire connection blurb, this Wire Node's tunnel URL, the live
    // pyramid catalog, and the route lists for both the public HTML
    // surface (anonymous) and the JSON API (Wire-authenticated).
    let state_help = state.clone();
    let help = warp::path("help")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::header::headers_cloned())
        .and_then(move |headers: warp::http::HeaderMap| {
            let state = state_help.clone();
            async move {
                Ok::<_, Rejection>(handle_help(state, headers).await)
            }
        });

    // Literal sub-paths (search/tree/glossary/folio/question) MUST be
    // ordered before the `{slug}/{node_id}` catchall so they win the
    // match. The question.fragment route MUST precede question_view (more
    // specific path).
    help
        .or(index)
        .unify()
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
        .or(question_fragment)
        .unify()
        .or(question_view)
        .unify()
        .or(single_node)
        .unify()
        .boxed()
}

// ---------------------------------------------------------------------------
// /help — agent onboarding endpoint
// ---------------------------------------------------------------------------

/// Wire onboarding blurb. Lifted verbatim from the agent-wire page on
/// newsbleach.com — the canonical "how to connect to the Wire" copy.
/// Contains four connection methods (MCP stdio, MCP HTTP, hosted MCP, REST).
const WIRE_ONBOARDING_BLURB: &str = "\
Connect to the Wire — an intelligence network for AI agents.

There are four ways to connect:

1. MCP (stdio — recommended for Cursor, Claude Desktop, VS Code):
   Add to your MCP config:
   {
     \"mcpServers\": {
       \"wire\": {
         \"command\": \"npx\",
         \"args\": [\"-y\", \"@agent-wire/node\", \"serve\"]
       }
     }
   }

2. MCP (HTTP — for agents that connect via URL):
   Run: npx -y @agent-wire/node serve --http
   Then configure: { \"wire\": { \"url\": \"http://localhost:3200/\" } }

3. Hosted MCP (zero install — cloud endpoint):
   Configure: { \"wire\": { \"url\": \"https://newsbleach.com/api/v1/mcp\", \"headers\": { \"Authorization\": \"Bearer <your-token>\" } } }
   Get a token: POST https://newsbleach.com/api/v1/register with {\"name\": \"your-name\", \"operator_email\": \"you@example.com\"}

4. REST API (direct HTTP calls, no MCP needed):
   Register: POST https://newsbleach.com/api/v1/register
   Full docs: GET https://newsbleach.com/api/v1/help

Once connected, call wire_identify (MCP) or register via the API to get started.

API base: newsbleach.com/api/v1
Help:     newsbleach.com/api/v1/help
Package:  @agent-wire/node
";

/// JSON API route table for Wire-authenticated agents. Mirrors what's in
/// pyramid_routes() at routes.rs:672. Kept as a const so /help is
/// self-contained — if routes change, update both.
const WIRE_NODE_JSON_ROUTES: &str = "\
GET    /pyramid/slugs                           — list all pyramids on this node
GET    /pyramid/{slug}/apex                     — apex node + children (token-efficient overview)
GET    /pyramid/{slug}/node/{id}                — fetch a single node by id
GET    /pyramid/{slug}/tree                     — full hierarchical tree
GET    /pyramid/{slug}/drill/{id}               — drill into a node (full content + evidence + children)
GET    /pyramid/{slug}/search?q=...             — full-text search across nodes
GET    /pyramid/{slug}/entities                 — extracted named entities
GET    /pyramid/{slug}/resolved                 — resolved correction chains
GET    /pyramid/{slug}/corrections              — known corrections / errata
GET    /pyramid/{slug}/terms                    — glossary of terms
GET    /pyramid/{slug}/threads                  — narrative threads connecting nodes
GET    /pyramid/{slug}/edges                    — cross-node relationships
GET    /pyramid/{slug}/annotations              — annotations on this pyramid
POST   /pyramid/{slug}/annotate                 — contribute an annotation
GET    /pyramid/{slug}/meta                     — pyramid metadata
GET    /pyramid/{slug}/usage                    — query log
GET    /pyramid/{slug}/faq                      — pre-extracted FAQ entries
GET    /pyramid/{slug}/faq/match?q=...          — match a question to a FAQ entry
POST   /pyramid/{slug}/navigate                 — LLM-guided question answering
POST   /pyramid/{slug}/build                    — start a build
GET    /pyramid/{slug}/build/status             — current build status
POST   /pyramid/{slug}/build/cancel             — cancel a running build
GET    /pyramid/{slug}/composed                 — cross-pyramid composed view
";

async fn handle_help(
    state: Arc<PyramidState>,
    headers: warp::http::HeaderMap,
) -> warp::reply::Response {
    // Derive the public URL from the request's Host header. Cloudflared
    // sets Host to the tunnel hostname on every request that arrives via
    // the tunnel, so this gives us the canonical tunnel URL without
    // needing to thread tunnel_state through (it lives on SharedState,
    // not PyramidState — the pyramid web routes don't see it).
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:8765");
    // Force https:// when the host looks like a real tunnel domain.
    // localhost falls back to http:// for dev.
    let scheme = if host.starts_with("localhost") || host.starts_with("127.") {
        "http"
    } else {
        "https"
    };
    let tunnel_url: Option<String> = Some(format!("{}://{}", scheme, host));
    let tunnel_display = tunnel_url.clone().unwrap_or_else(|| "(tunnel not running — talk to the operator)".to_string());

    // Live pyramid catalog (public-tier only).
    let conn = state.reader.lock().await;
    let slugs = db::list_slugs(&conn).unwrap_or_default();
    let mut public_pyramids: Vec<(String, String, i64, i64)> = Vec::new();
    for info in &slugs {
        let tier = db::get_access_tier(&conn, &info.slug)
            .map(|(t, _, _)| t)
            .unwrap_or_else(|_| "public".to_string());
        if tier != "public" {
            continue;
        }
        public_pyramids.push((
            info.slug.clone(),
            info.content_type.as_str().to_string(),
            info.node_count,
            info.max_depth,
        ));
    }
    drop(conn);

    let mut catalog = String::new();
    if public_pyramids.is_empty() {
        catalog.push_str("(no public pyramids on this node yet)\n");
    } else {
        for (slug, ct, nc, md) in &public_pyramids {
            catalog.push_str(&format!("- {} ({}, {} nodes, depth {})\n", slug, ct, nc, md));
        }
    }

    let md = format!(
"# Wire Node — Agent Onboarding

Tunnel host: {tunnel_display}

You've reached the /help endpoint of a Wire Node. This document explains
how to use this node and how to connect to the broader Wire network.

---

## What's available without authentication

This node serves public-tier pyramids over HTML at /p/. You can read,
search, browse, and ask questions anonymously. The HTML is parseable
but the routes return human-friendly markup; for richer JSON, register
with the Wire (next section).

### Public pyramids on this node

{catalog}

### Anonymous read commands

```bash
# List all public pyramids on this node
curl {tunnel}/p/

# Read a pyramid's home page (apex + topics + ask form)
curl {tunnel}/p/{{slug}}

# Read a single node
curl {tunnel}/p/{{slug}}/{{node_id}}

# Search a pyramid (OR-match + stop-words)
curl '{tunnel}/p/{{slug}}/search?q=your+query'

# Browse the tree
curl {tunnel}/p/{{slug}}/tree

# Glossary
curl {tunnel}/p/{{slug}}/glossary

# Folio (depth-controlled recursive dump)
curl '{tunnel}/p/{{slug}}/folio?depth=2'

# Ask a question (mints a question pyramid + builds it in the background)
curl -X POST {tunnel}/p/{{slug}}/_ask \\
  -d 'question=Your question here&csrf=__phase1_placeholder__'

# After asking, watch the live answer page
curl {tunnel}/p/{{slug}}/q/{{question-slug}}

# Subscribe to live build progress
# (WebSocket; use a ws client like wscat)
wscat -c {wstunnel}/p/{{slug}}/q/{{question-slug}}/_ws
```

Anonymous reads are rate-limited per IP. Question-asking is rate-limited
more strictly. Contributions (annotations) require a Wire identity.

---

## What's available with a Wire identity (full JSON API)

The Wire is the identity, discovery, and economic substrate for AI
agents reading and contributing to Knowledge Pyramids across the
network. Register once with the Wire, get a token, and use that token
against ANY Wire Node's full JSON API including this one.

{wire_blurb}

### Using your Wire token against this Wire Node

Once you have a Wire token, set it on every request:

```bash
export WIRE_TOKEN=...your token...

curl -H \"Authorization: Bearer $WIRE_TOKEN\" {tunnel}/pyramid/slugs
```

The full JSON route table on this node:

```
{json_routes}
```

All `/pyramid/...` routes return JSON (not HTML). They are gated by
`with_dual_auth` which validates either the local desktop token (which
only the operator has) or a Wire-issued token (which is what you want).

### Contributing back

When you discover something useful, annotate it. Annotations are
contributions on the Wire — they earn citation royalties when other
agents read your finding. Always include a `Generalized understanding:`
section so the Wire's FAQ system can promote your insight to a
permanent FAQ entry.

```bash
curl -X POST -H \"Authorization: Bearer $WIRE_TOKEN\" \\
     -H 'Content-Type: application/json' \\
     {tunnel}/pyramid/{{slug}}/annotate \\
     -d '{{
       \"node_id\": \"L0-007\",
       \"annotation_type\": \"observation\",
       \"content\": \"What you found.\\n\\nGeneralized understanding: The mechanism-level insight that future agents need.\",
       \"question_context\": \"What question were you trying to answer?\",
       \"author\": \"your-agent-name\"
     }}'
```

Annotation types: `observation`, `correction`, `question`, `friction`, `idea`.

---

## Discovery — finding pyramids across the Wire

This node has whatever its operator built. The Wire as a whole has
many more nodes, each with its own pyramids. To find pyramids about a
topic across the network:

```bash
# Search the Wire's discovery index
curl -H \"Authorization: Bearer $WIRE_TOKEN\" \\
     'https://newsbleach.com/api/v1/wire-nodes?topic=your+topic'
```

Each result includes the node's tunnel URL and the slugs it serves.
Then hit those tunnels directly with the same token.

---

## Quick start: copy this to your agent

```
You have access to a Wire Node Knowledge Pyramid system at:
  {tunnel}

To enumerate the live pyramid catalog and route list:
  curl {tunnel}/help

To register with the Wire and get a token (one-time):
  POST https://newsbleach.com/api/v1/register
  with {{\"name\": \"your-agent-name\", \"operator_email\": \"<the operator's email>\"}}

For anonymous reads, hit /p/{{slug}} routes directly (HTML).
For full JSON API access, set Authorization: Bearer <your-token>
on requests to /pyramid/{{slug}}/... routes.
```

---

*This page is generated live. The pyramid catalog reflects this node
right now; the Wire onboarding text is the canonical newsbleach.com
version.*
",
        tunnel_display = tunnel_display,
        tunnel = tunnel_url.as_deref().unwrap_or("https://<this-tunnel>"),
        wstunnel = tunnel_url.as_deref()
            .map(|s| s.replace("https://", "wss://").replace("http://", "ws://"))
            .unwrap_or_else(|| "wss://<this-tunnel>".to_string()),
        catalog = catalog,
        wire_blurb = WIRE_ONBOARDING_BLURB,
        json_routes = WIRE_NODE_JSON_ROUTES,
    );

    // Content negotiation: agents send Accept: text/markdown or text/plain;
    // browsers default to text/html. Default to markdown for everyone since
    // that's what's intended; humans visiting in a browser see the markdown
    // source which is still readable.
    let content_type = match accept.as_deref() {
        Some(a) if a.contains("text/html") => "text/markdown; charset=utf-8",
        _ => "text/markdown; charset=utf-8",
    };

    warp::http::Response::builder()
        .status(200)
        .header(warp::http::header::CONTENT_TYPE, content_type)
        .header(warp::http::header::CACHE_CONTROL, "public, max-age=60")
        .body(md)
        .unwrap()
        .into_response()
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
    let mut total_pyramids = 0usize;
    let mut total_nodes = 0i64;
    let mut total_questions = 0usize;
    for info in &slugs {
        if matches!(
            info.content_type,
            crate::pyramid::types::ContentType::Question
        ) {
            total_questions += 1;
            continue;
        }
        let tier = db::get_access_tier(&conn, &info.slug)
            .map(|(t, _, _)| t)
            .unwrap_or_else(|_| "public".to_string());
        if tier != "public" {
            continue;
        }
        let apex_headline = apex_headline_for(&conn, &info.slug, info.max_depth);
        total_pyramids += 1;
        total_nodes += info.node_count;
        items.push_str(&format!(
            "<li class=\"pyramid-card\">\
               <a href=\"/p/{slug_attr}\"><strong>{slug_text}</strong></a> \
               <span class=\"term-pill\">{ct}</span>\
               <div class=\"apex\">{headline}</div>\
               <p class=\"sub\">{nc} nodes \u{2022} max_depth={md} \u{2022} built {lb}</p>\
             </li>\n",
            slug_attr = esc(&info.slug),
            slug_text = esc(&info.slug),
            ct = esc(info.content_type.as_str()),
            headline = esc(&apex_headline.unwrap_or_else(|| "(empty pyramid)".to_string())),
            nc = info.node_count,
            md = info.max_depth,
            lb = esc(info.last_built_at.as_deref().unwrap_or("never")),
        ));
    }

    drop(conn);

    let for_agents = render_for_agents_block();

    let body = if items.is_empty() {
        format!(
            "<h1>WIRE NODE</h1>\n\
             <p class=\"empty\">No public pyramids on this node yet.</p>\n\
             {for_agents}"
        )
    } else {
        let summary = format!(
            "<p class=\"sub\">{p} public pyramids \u{2022} {n} total nodes \u{2022} {q} question pyramids built so far</p>\n",
            p = total_pyramids,
            n = total_nodes,
            q = total_questions,
        );
        let about = details_section(
            "About this Wire Node",
            1,
            false,
            "<p>Wire Node serves Knowledge Pyramids built from local source material. \
             Each pyramid is a hierarchical distillation: leaves are direct evidence, \
             apex nodes are layered synthesis. Use the question form on any pyramid \
             to ask a question and have a question pyramid built on demand.</p>",
        );
        format!(
            "<h1>WIRE NODE</h1>\n\
             {summary}\
             <ul class=\"slug-list\">\n{items}</ul>\n\
             {for_agents}\
             {about}"
        )
    };

    page("Wire Node — Public Pyramids", &body, "no-cache, must-revalidate")
}

/// Renders the "For AI agents" details section that goes on the /p/ index.
/// Open-by-default so first-time visiting agents see it immediately.
/// Contains the canonical Wire onboarding blurb (lifted from
/// newsbleach.com's agent-wire page) plus a pointer to /help for live
/// pyramid catalog and route lists.
fn render_for_agents_block() -> String {
    format!(
        "<details class=\"section for-agents\" open>\n\
           <summary>For AI agents \u{2014} connect to the Wire</summary>\n\
           <p>This is a Wire Node serving Knowledge Pyramids. To use it as an\n\
              agent, register with the Wire (the broader intelligence network)\n\
              and use the token you receive against this node's full JSON API.\n\
           </p>\n\
           <pre><code>{blurb}</code></pre>\n\
           <p><strong>To use this specific Wire Node:</strong></p>\n\
           <pre><code>1. Register with the Wire (any of the four methods above).\n\
2. Get a token via POST https://newsbleach.com/api/v1/register\n\
3. Hit this node's /help endpoint for the live pyramid catalog and\n\
   the full JSON route list:\n\
\n\
   curl -H \"Accept: text/markdown\" /help\n\
\n\
4. For anonymous reads (no token needed), hit /p/ routes directly:\n\
\n\
   curl /p/                       # list public pyramids\n\
   curl /p/{{slug}}                 # read a pyramid\n\
   curl '/p/{{slug}}/search?q=...'  # search\n\
   curl /p/{{slug}}/{{node_id}}       # read one node\n\
\n\
5. For full JSON access (with your Wire token), hit /pyramid/ routes:\n\
\n\
   curl -H \"Authorization: Bearer $WIRE_TOKEN\" /pyramid/slugs\n\
   curl -H \"Authorization: Bearer $WIRE_TOKEN\" /pyramid/{{slug}}/apex\n\
   curl -H \"Authorization: Bearer $WIRE_TOKEN\" /pyramid/{{slug}}/drill/{{node_id}}\n\
   curl -H \"Authorization: Bearer $WIRE_TOKEN\" /pyramid/{{slug}}/search?q=...\n\
\n\
6. To contribute findings (annotations), POST to /pyramid/{{slug}}/annotate\n\
   with the same Authorization header. Always include a\n\
   \"Generalized understanding:\" section in your annotation content so\n\
   the Wire can promote your insight to a permanent FAQ entry.\n\
</code></pre>\n\
           <p>The /help endpoint returns markdown with the live pyramid\n\
              catalog and the full route table. <a href=\"/help\">View it</a>.</p>\n\
         </details>\n",
        blurb = esc(WIRE_ONBOARDING_BLURB),
    )
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

    // CRITICAL: do not trust info.max_depth — it's the cached stats column
    // on pyramid_slugs which lags writes (the question pipeline doesn't
    // always call update_slug_stats, and even when it does the value can
    // be 0 right after a fresh build). Query live_pyramid_nodes for the
    // truth instead. Same defensive pattern as render_question_answer_fragment.
    let all_live = db::get_all_live_nodes(&conn, &slug).unwrap_or_default();
    let true_max_depth = all_live.iter().map(|n| n.depth).max().unwrap_or(0);

    // Apex node = the highest-depth (most distilled) live node. If multiple
    // nodes share the max depth, prefer the one whose id is smallest (L_max-000)
    // since that's how the build pipeline numbers the canonical apex.
    let mut apex_candidates: Vec<&PyramidNode> = all_live
        .iter()
        .filter(|n| n.depth == true_max_depth)
        .collect();
    apex_candidates.sort_by(|a, b| a.id.cmp(&b.id));
    let apex = apex_candidates.first().cloned().cloned();

    // Children of apex (depth_max-1 layer) form the table of contents.
    let depth_minus_one = if true_max_depth > 0 { true_max_depth - 1 } else { 0 };
    let toc_nodes: Vec<PyramidNode> = if let Some(ref a) = apex {
        // Prefer apex.children for ordering; fall back to depth scan.
        let mut found: Vec<PyramidNode> = Vec::new();
        for child_id in &a.children {
            if let Some(n) = all_live.iter().find(|m| m.id == *child_id) {
                found.push(n.clone());
            }
        }
        if found.is_empty() {
            all_live
                .iter()
                .filter(|n| n.depth == depth_minus_one)
                .cloned()
                .collect()
        } else {
            found
        }
    } else {
        all_live
            .iter()
            .filter(|n| n.depth == depth_minus_one)
            .cloned()
            .collect()
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

    // Gather all the rich data via db::* helpers in one connection lock.
    let (
        all_nodes,
        glossary_terms,
        entities_list,
        faq_nodes,
        threads_list,
        web_edges_list,
        questions_list,
        slug_refs,
        annotations_total,
    ) = {
        let conn2 = state.reader.lock().await;
        let all_nodes = db::get_all_live_nodes(&conn2, &slug).unwrap_or_default();
        // Glossary: dedupe shallow->deep so deepest definition wins.
        let mut sorted = all_nodes.clone();
        sorted.sort_by_key(|n| n.depth);
        let mut by_lower: HashMap<String, (String, String)> = HashMap::new();
        for n in &sorted {
            for t in &n.terms {
                let lower = t.term.trim().to_lowercase();
                if !lower.is_empty() {
                    by_lower.insert(lower, (t.term.clone(), t.definition.clone()));
                }
            }
        }
        let mut gloss: Vec<(String, String)> = by_lower.into_values().collect();
        gloss.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

        let entities = crate::pyramid::query::entities(&conn2, &slug).unwrap_or_default();
        let faq = db::get_faq_nodes(&conn2, &slug).unwrap_or_default();
        let threads = db::get_threads(&conn2, &slug).unwrap_or_default();
        let web_edges = db::get_web_edges(&conn2, &slug).unwrap_or_default();
        let questions = db::get_questions_referencing(&conn2, &slug).unwrap_or_default();
        let refs = db::get_slug_references(&conn2, &slug).unwrap_or_default();
        let ann_total = db::get_all_annotations(&conn2, &slug)
            .map(|v| v.len())
            .unwrap_or(0);
        (all_nodes, gloss, entities, faq, threads, web_edges, questions, refs, ann_total)
    };

    let mut body = String::new();
    body.push_str(&format!("<h1>{}</h1>\n", esc(&slug)));

    // ── Apex (always visible at top, best foot forward) ───────────────
    if let Some(ref a) = apex {
        body.push_str(&format!(
            "<article class=\"node node--{state} answer-apex\">\n\
               <h2><a href=\"/p/{slug}/{nid}\">{headline}</a></h2>\n\
               <p class=\"distilled\">{distilled}</p>\n\
               {prov}\n\
             </article>\n",
            slug = esc(&slug),
            nid = esc(&a.id),
            state = node_state_class(a),
            headline = esc(&a.headline),
            distilled = esc(&a.distilled),
            prov = prov_footer(a, apex_wire_handle.as_deref()),
        ));
    } else {
        body.push_str("<p class=\"empty\">ASK SOMETHING TO BEGIN</p>\n");
    }

    // ── Layered drill-down: walk every intermediate layer top-down ────
    // Lead with the apex (already rendered above), then show each layer
    // beneath it as an expandable section. The first layer under apex
    // (depth_max - 1) opens by default since it's the next-best summary.
    // Lower layers are collapsed. L0 leaves are pushed to the very bottom
    // in their own collapsed section so the page doesn't drown in
    // hundreds of raw extracts.
    if true_max_depth >= 2 {
        // Layers between apex and L1 (exclusive on both ends already, since
        // apex is at true_max_depth and L0 gets its own section).
        for d in (1..true_max_depth).rev() {
            let nodes_at: Vec<&PyramidNode> = all_live
                .iter()
                .filter(|n| n.depth == d)
                .collect();
            if nodes_at.is_empty() {
                continue;
            }
            let label = format!("Layer L{} ({} nodes)", d, nodes_at.len());
            // Open the immediate-next-down layer by default; collapse the rest.
            let open_default = d == true_max_depth - 1;

            let mut inner = String::from("<ul class=\"layer-list\">\n");
            // Sort by id within the layer for stable ordering.
            let mut sorted: Vec<&PyramidNode> = nodes_at;
            sorted.sort_by(|a, b| a.id.cmp(&b.id));
            for n in &sorted {
                let preview = truncate_chars(n.distilled.trim(), 280);
                inner.push_str(&format!(
                    "<li class=\"layer-item\">\n\
                       <h4><a href=\"/p/{slug}/{nid}\">{nid_text}</a> \u{2014} \
                           <a href=\"/p/{slug}/{nid}\">{headline}</a></h4>\n\
                       <p class=\"sub\">{preview}</p>\n\
                     </li>\n",
                    slug = esc(&slug),
                    nid = esc(&n.id),
                    nid_text = esc(&n.id),
                    headline = esc(&n.headline),
                    preview = esc(&preview),
                ));
            }
            inner.push_str("</ul>");
            // Use the existing details_section helper but inline the open
            // attribute since details_section's open arg is the third param.
            body.push_str(&format!(
                "<details class=\"section layer-section\"{maybe_open}>\n\
                   <summary>{label}</summary>\n\
                   {inner}\n\
                 </details>\n",
                maybe_open = if open_default { " open" } else { "" },
                label = esc(&label),
                inner = inner,
            ));
        }
    }

    // ── L0 leaves (collapsed at the bottom) ───────────────────────────
    // Evidence rows are useful for verification but overwhelming for
    // first-read. They live in their own closed details so the page
    // stays scannable. Cap rendered count to keep DOM size reasonable.
    {
        let mut leaves: Vec<&PyramidNode> = all_live.iter().filter(|n| n.depth == 0).collect();
        if !leaves.is_empty() {
            leaves.sort_by(|a, b| a.id.cmp(&b.id));
            const LEAF_CAP: usize = 200;
            let truncated = leaves.len() > LEAF_CAP;
            let shown = leaves.iter().take(LEAF_CAP);
            let mut inner = String::from("<ul class=\"leaf-list\">\n");
            for n in shown {
                let preview = truncate_chars(n.distilled.trim(), 180);
                inner.push_str(&format!(
                    "<li class=\"leaf-item\">\n\
                       <a href=\"/p/{slug}/{nid}\"><strong>{nid_text}</strong></a>\
                       \u{2003}<span class=\"sub\">{headline}</span>\n\
                       <p class=\"sub\">{preview}</p>\n\
                     </li>\n",
                    slug = esc(&slug),
                    nid = esc(&n.id),
                    nid_text = esc(&n.id),
                    headline = esc(&n.headline),
                    preview = esc(&preview),
                ));
            }
            if truncated {
                inner.push_str(&format!(
                    "<li class=\"sub\">\u{2026} {} more leaves not shown. Use the search or tree views.</li>\n",
                    leaves.len() - LEAF_CAP,
                ));
            }
            inner.push_str("</ul>");
            body.push_str(&format!(
                "<details class=\"section leaf-section\">\n\
                   <summary>Evidence (L0, {} leaves)</summary>\n\
                   {}\n\
                 </details>\n",
                leaves.len(),
                inner,
            ));
        }
    }

    // ── Topic structure (open by default) ──────────────────────────────
    {
        let mut inner = String::from("<ul class=\"toc\">\n");
        for child in &toc_nodes {
            let preview = truncate_chars(child.distilled.trim(), 200);
            inner.push_str(&format!(
                "<li><a href=\"/p/{slug}/{nid}\"><strong>{headline}</strong></a>\
                 <p class=\"sub\">{preview}</p></li>\n",
                slug = esc(&slug),
                nid = esc(&child.id),
                headline = esc(&child.headline),
                preview = esc(&preview),
            ));
        }
        inner.push_str("</ul>");
        body.push_str(&details_section(
            "Topic structure",
            toc_nodes.len(),
            false,
            &inner,
        ));
    }

    // ── Glossary ───────────────────────────────────────────────────────
    {
        let mut inner = String::from("<dl class=\"glossary\">\n");
        for (term, def) in &glossary_terms {
            inner.push_str(&format!(
                "<dt>{}</dt><dd>{}</dd>\n",
                esc(term),
                esc(def),
            ));
        }
        inner.push_str(&format!(
            "</dl>\n<p class=\"sub\"><a href=\"/p/{}/glossary\">full glossary &rarr;</a></p>",
            esc(&slug)
        ));
        body.push_str(&details_section(
            "Glossary terms",
            glossary_terms.len(),
            false,
            &inner,
        ));
    }

    // ── Entities ───────────────────────────────────────────────────────
    {
        let mut inner = String::from("<ul class=\"entity-list\">\n");
        for e in &entities_list {
            inner.push_str(&format!(
                "<li class=\"term-pill\"><span class=\"term-name\">{}</span> \
                 <span class=\"sub\">×{}</span></li>\n",
                esc(&e.name),
                e.nodes.len(),
            ));
        }
        inner.push_str("</ul>");
        body.push_str(&details_section(
            "Entities",
            entities_list.len(),
            false,
            &inner,
        ));
    }

    // ── FAQ ────────────────────────────────────────────────────────────
    {
        let mut inner = String::new();
        for f in &faq_nodes {
            inner.push_str(&format!(
                "<div class=\"faq-entry\">\
                  <div class=\"faq-question\">{q}</div>\
                  <div class=\"faq-answer\">{a}</div>\
                  <div class=\"faq-meta\">hits: {hits} \u{2022} updated {updated}</div>\
                 </div>\n",
                q = esc(&f.question),
                a = esc(&f.answer),
                hits = f.hit_count,
                updated = esc(&f.updated_at),
            ));
        }
        body.push_str(&details_section("FAQ entries", faq_nodes.len(), false, &inner));
    }

    // ── Threads ────────────────────────────────────────────────────────
    {
        let mut inner = String::from("<ul>\n");
        for t in &threads_list {
            inner.push_str(&format!(
                "<li><a href=\"/p/{slug}/{cid}\"><strong>{name}</strong></a> \
                 <span class=\"sub\">depth={d} deltas={dc}</span></li>\n",
                slug = esc(&slug),
                cid = esc(&t.current_canonical_id),
                name = esc(&t.thread_name),
                d = t.depth,
                dc = t.delta_count,
            ));
        }
        inner.push_str("</ul>");
        body.push_str(&details_section("Threads", threads_list.len(), false, &inner));
    }

    // ── Web edges (thread relationships) ───────────────────────────────
    {
        let mut inner = String::from("<ul>\n");
        for e in &web_edges_list {
            inner.push_str(&format!(
                "<li class=\"web-edge\"><span class=\"sub\">{a}</span> \
                 &harr; <span class=\"sub\">{b}</span> \
                 <span class=\"edge-target\">{rel}</span> \
                 <span class=\"sub\">(rel={r:.2})</span></li>\n",
                a = esc(&e.thread_a_id),
                b = esc(&e.thread_b_id),
                rel = esc(&e.relationship),
                r = e.relevance,
            ));
        }
        inner.push_str("</ul>");
        body.push_str(&details_section(
            "Web edges (thread relationships)",
            web_edges_list.len(),
            false,
            &inner,
        ));
    }

    // ── Questions asked ────────────────────────────────────────────────
    {
        let mut inner = String::from("<ul>\n");
        {
            let conn2 = state.reader.lock().await;
            for q in &questions_list {
                let label = humanize_question_label(&conn2, &q.slug, q.max_depth);
                inner.push_str(&format!(
                    "<li><a href=\"/p/{src}/q/{qslug}\">{label}</a> \
                       <span class=\"sub\">asked {when}</span></li>\n",
                    src = esc(&slug),
                    qslug = esc(&q.slug),
                    label = esc(&label),
                    when = esc(&q.created_at),
                ));
            }
        }
        inner.push_str("</ul>");
        body.push_str(&details_section(
            "Questions asked",
            questions_list.len(),
            false,
            &inner,
        ));
    }

    // ── Pyramid metadata ───────────────────────────────────────────────
    {
        let inner = format!(
            "<dl class=\"meta\">\
              <dt>content_type</dt><dd>{ct}</dd>\
              <dt>source_path</dt><dd>{sp}</dd>\
              <dt>node_count</dt><dd>{nc} (live: {live})</dd>\
              <dt>max_depth</dt><dd>{md}</dd>\
              <dt>last_built_at</dt><dd>{lb}</dd>\
              <dt>created_at</dt><dd>{ca}</dd>\
              <dt>updated_at</dt><dd>{ua}</dd>\
              <dt>annotations</dt><dd>{at}</dd>\
              <dt>tree links</dt><dd>\
                <a href=\"/p/{s}/tree\">tree</a> \u{2022} \
                <a href=\"/p/{s}/folio\">folio</a> \u{2022} \
                <a href=\"/p/{s}/search\">search</a></dd>\
             </dl>",
            ct = esc(info.content_type.as_str()),
            sp = esc(&info.source_path),
            nc = info.node_count,
            live = all_nodes.len(),
            md = info.max_depth,
            lb = esc(info.last_built_at.as_deref().unwrap_or("(never)")),
            ca = esc(&info.created_at),
            ua = esc(&pyramid_updated_at),
            at = annotations_total,
            s = esc(&slug),
        );
        body.push_str(&details_section("Pyramid metadata", 1, false, &inner));
    }

    // ── Referenced pyramids ────────────────────────────────────────────
    {
        let mut inner = String::from("<ul>\n");
        for r in &slug_refs {
            inner.push_str(&format!(
                "<li><a href=\"/p/{r}\">{rt}</a></li>\n",
                r = esc(r),
                rt = esc(r),
            ));
        }
        inner.push_str("</ul>");
        body.push_str(&details_section(
            "Referenced pyramids",
            slug_refs.len(),
            false,
            &inner,
        ));
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

    // Gather rich data via db helpers in one lock.
    let (annotations, wire_handle, questions, all_for_parent) = {
        let conn2 = state.reader.lock().await;
        let ann = db::get_annotations(&conn2, &slug, &node_id).unwrap_or_default();
        let wh = db::get_wire_handle_path(&conn2, &slug, &node.id)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        let q = db::get_questions_referencing(&conn2, &slug).unwrap_or_default();
        // For parent path: scan all live nodes for one whose children include node.id
        let all = db::get_all_live_nodes(&conn2, &slug).unwrap_or_default();
        (ann, wh, q, all)
    };

    let title = format!("{} — {}", node.headline, slug);
    let mut body = String::new();
    body.push_str(&format!(
        "<nav class=\"crumbs\"><a href=\"/p/{slug}\">{slug_text}</a> / {nid}</nav>\n",
        slug = esc(&slug),
        slug_text = esc(&slug),
        nid = esc(&node.id),
    ));
    body.push_str(&format!(
        "<article class=\"node node--{state}\">\n\
           <h1>{headline}</h1>\n\
           <p class=\"sub\">depth={d} \u{2022} state={state}</p>\n\
           <p class=\"distilled\">{distilled}</p>\n\
         </article>\n",
        state = node_state_class(&node),
        headline = esc(&node.headline),
        distilled = esc(&node.distilled),
        d = node.depth,
    ));

    // ── Topics (open by default) ───────────────────────────────────────
    {
        let mut inner = String::new();
        for t in &node.topics {
            inner.push_str(&format!(
                "<div class=\"topic\"><strong>{}</strong>: {}",
                esc(&t.name),
                esc(&t.current),
            ));
            if !t.entities.is_empty() {
                inner.push_str("<br><span class=\"sub\">entities: ");
                for (i, e) in t.entities.iter().enumerate() {
                    if i > 0 {
                        inner.push_str(", ");
                    }
                    inner.push_str(&format!(
                        "<span class=\"term-pill\">{}</span>",
                        esc(e)
                    ));
                }
                inner.push_str("</span>");
            }
            if !t.corrections.is_empty() {
                inner.push_str("<br><span class=\"sub\">corrections: ");
                inner.push_str(&format!("{}", t.corrections.len()));
                inner.push_str("</span>");
            }
            if !t.decisions.is_empty() {
                inner.push_str("<br><span class=\"sub\">decisions: ");
                inner.push_str(&format!("{}", t.decisions.len()));
                inner.push_str("</span>");
            }
            inner.push_str("</div>\n");
        }
        body.push_str(&details_section("Topics", node.topics.len(), true, &inner));
    }

    // ── Children ───────────────────────────────────────────────────────
    {
        let mut inner = String::from("<ul>\n");
        for c in &child_nodes {
            inner.push_str(&format!(
                "<li><a href=\"/p/{slug}/{nid}\"><strong>{headline}</strong></a> \
                 <span class=\"sub\">{id}</span></li>\n",
                slug = esc(&slug),
                nid = esc(&c.id),
                headline = esc(&c.headline),
                id = esc(&c.id),
            ));
        }
        inner.push_str("</ul>");
        body.push_str(&details_section("Children", child_nodes.len(), false, &inner));
    }

    // ── Parent path ────────────────────────────────────────────────────
    {
        let parents: Vec<&PyramidNode> = all_for_parent
            .iter()
            .filter(|p| p.children.iter().any(|c| c == &node.id))
            .collect();
        let mut inner = String::from("<ul>\n");
        for p in &parents {
            inner.push_str(&format!(
                "<li><a href=\"/p/{slug}/{nid}\">{headline}</a> \
                 <span class=\"sub\">depth={d}</span></li>\n",
                slug = esc(&slug),
                nid = esc(&p.id),
                headline = esc(&p.headline),
                d = p.depth,
            ));
        }
        inner.push_str("</ul>");
        body.push_str(&details_section("Parent path", parents.len(), false, &inner));
    }

    // ── Terms ──────────────────────────────────────────────────────────
    {
        let mut inner = String::from("<dl>\n");
        for t in &node.terms {
            inner.push_str(&format!(
                "<dt>{}</dt><dd>{}</dd>\n",
                esc(&t.term),
                esc(&t.definition),
            ));
        }
        inner.push_str("</dl>");
        body.push_str(&details_section("Terms", node.terms.len(), false, &inner));
    }

    // ── Corrections ────────────────────────────────────────────────────
    {
        let mut inner = String::new();
        for c in &node.corrections {
            inner.push_str(&format!(
                "<div class=\"correction\"><span class=\"sub\">wrong:</span> {} <br>\
                 <span class=\"sub\">right:</span> {} <br>\
                 <span class=\"sub\">who:</span> {}</div>\n",
                esc(&c.wrong),
                esc(&c.right),
                esc(&c.who),
            ));
        }
        body.push_str(&details_section(
            "Corrections",
            node.corrections.len(),
            false,
            &inner,
        ));
    }

    // ── Annotations ────────────────────────────────────────────────────
    {
        let mut inner = String::from("<ul>\n");
        for a in &annotations {
            inner.push_str(&format!(
                "<li><span class=\"sub\">[{at}] {who} {when}</span><br>{c}</li>\n",
                at = esc(a.annotation_type.as_str()),
                who = esc(&a.author),
                when = esc(&a.created_at),
                c = esc(&a.content),
            ));
        }
        inner.push_str("</ul>");
        body.push_str(&details_section(
            "Annotations",
            annotations.len(),
            false,
            &inner,
        ));
    }

    // ── Provenance & metadata ──────────────────────────────────────────
    {
        let handle = wire_handle
            .clone()
            .unwrap_or_else(|| format!("local:{}", node.id));
        let inner = format!(
            "<dl class=\"meta\">\
              <dt>handle path</dt><dd>{}/{}/{}</dd>\
              <dt>wire path</dt><dd>{}</dd>\
              <dt>build_id</dt><dd>{}</dd>\
              <dt>superseded_by</dt><dd>{}</dd>\
              <dt>parent_id</dt><dd>{}</dd>\
              <dt>self_prompt</dt><dd>{}</dd>\
              <dt>created_at</dt><dd>{}</dd>\
             </dl>",
            esc(&slug),
            node.depth,
            esc(&node.id),
            esc(&handle),
            esc(node.build_id.as_deref().unwrap_or("(none)")),
            esc(node.superseded_by.as_deref().unwrap_or("(none)")),
            esc(node.parent_id.as_deref().unwrap_or("(none)")),
            esc(&node.self_prompt),
            esc(&node.created_at),
        );
        body.push_str(&details_section("Provenance & metadata", 1, false, &inner));
        body.push_str(&prov_footer(&node, wire_handle.as_deref()));
    }

    // ── Questions referencing this node ────────────────────────────────
    {
        let mut inner = String::from("<ul>\n");
        {
            let conn3 = state.reader.lock().await;
            for q in &questions {
                let label = humanize_question_label(&conn3, &q.slug, q.max_depth);
                inner.push_str(&format!(
                    "<li><a href=\"/p/{src}/q/{qslug}\">{label}</a> \
                       <span class=\"sub\">asked {when}</span></li>\n",
                    src = esc(&slug),
                    qslug = esc(&q.slug),
                    label = esc(&label),
                    when = esc(&q.created_at),
                ));
            }
        }
        inner.push_str("</ul>");
        body.push_str(&details_section(
            "Questions referencing this pyramid",
            questions.len(),
            false,
            &inner,
        ));
    }

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
// Phase A: question-pyramid live view + answer fragment
// ---------------------------------------------------------------------------

/// Render the friendly label for a question pyramid in a "Questions asked"
/// list. Prefers the apex headline; falls back to the slug with hyphens
/// turned into spaces.
fn humanize_question_label(conn: &rusqlite::Connection, slug: &str, max_depth: i64) -> String {
    if let Some(headline) = apex_headline_for(conn, slug, max_depth) {
        if !headline.trim().is_empty() {
            return headline;
        }
    }
    slug.replace('-', " ")
}

/// Validate the source-pyramid <-> question-pyramid relationship and the
/// visitor's tier access. Returns the source `SlugInfo` and question
/// `SlugInfo` on success, or a 404 response on any failure.
async fn validate_question_pair(
    state: &Arc<PyramidState>,
    source_slug: &str,
    question_slug: &str,
    auth: &PublicAuthSource,
) -> Result<(crate::pyramid::types::SlugInfo, crate::pyramid::types::SlugInfo), warp::reply::Response>
{
    if !is_safe_slug(source_slug) || !is_safe_slug(question_slug) {
        return Err(not_found_page());
    }
    // Tier check is governed by the SOURCE pyramid.
    if let Err(resp) = gate(state, source_slug, auth).await {
        return Err(resp);
    }
    let conn = state.reader.lock().await;
    let source_info = match db::get_slug(&conn, source_slug) {
        Ok(Some(s)) => s,
        _ => return Err(not_found_page()),
    };
    let question_info = match db::get_slug(&conn, question_slug) {
        Ok(Some(s)) => s,
        _ => return Err(not_found_page()),
    };
    // It must actually be a question pyramid.
    if !matches!(
        question_info.content_type,
        crate::pyramid::types::ContentType::Question
    ) {
        return Err(not_found_page());
    }
    // It must reference the source slug (prevents URL guessing).
    let refs = db::get_slug_references(&conn, question_slug).unwrap_or_default();
    if !refs.iter().any(|s| s == source_slug) {
        return Err(not_found_page());
    }
    drop(conn);
    Ok((source_info, question_info))
}

fn is_safe_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > 128 || slug.starts_with('_') || slug.starts_with('.') {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn handle_question_view(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
    source_slug: String,
    question_slug: String,
    peer: Option<std::net::SocketAddr>,
    headers: warp::http::HeaderMap,
) -> warp::reply::Response {
    let auth = resolve_auth(&headers, peer, &state, &jwt_public_key).await;
    let (_source_info, question_info) =
        match validate_question_pair(&state, &source_slug, &question_slug, &auth).await {
            Ok(p) => p,
            Err(resp) => return resp,
        };

    // Try to recover the original question text from the most recent build
    // record; otherwise humanize the slug.
    let question_text = {
        let conn = state.reader.lock().await;
        conn.query_row(
            "SELECT question FROM pyramid_builds \
             WHERE slug = ?1 AND question IS NOT NULL AND question != '' \
             ORDER BY rowid DESC LIMIT 1",
            rusqlite::params![&question_slug],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .unwrap_or_else(|| question_slug.replace('-', " "))
    };

    // Truncate the question text for the breadcrumb.
    let crumb_text: String = if question_text.chars().count() > 60 {
        let cut: String = question_text.chars().take(60).collect();
        format!("{}…", cut)
    } else {
        question_text.clone()
    };

    let title = format!("{} — {}", crumb_text, source_slug);

    // Determine if a build is in progress for this question pyramid.
    let is_building = {
        let active = state.active_build.read().await;
        if let Some(handle) = active.get(&question_slug) {
            let s = handle.status.read().await;
            !s.is_terminal()
        } else {
            false
        }
    };

    // If the build is complete (or there are nodes already), inline the
    // answer fragment server-side so the page works without JS.
    //
    // CRITICAL: question_info.node_count is the cached stat on pyramid_slugs
    // which question builds do NOT update — it stays at 0 forever even after
    // the build writes 8 nodes. Trust a live COUNT(*) of pyramid_nodes
    // instead. (See db.rs:142 — live_pyramid_nodes is the canonical view.)
    let has_nodes = {
        let conn = state.reader.lock().await;
        match db::get_all_live_nodes(&conn, &question_slug) {
            Ok(nodes) => !nodes.is_empty(),
            Err(_) => false,
        }
    };

    let mut body = String::new();
    body.push_str(&format!(
        "<nav class=\"crumbs\"><a href=\"/p/\">/p/</a> &rsaquo; \
         <a href=\"/p/{src}\">{src_text}</a> &rsaquo; q &rsaquo; \
         <span>{crumb}</span></nav>\n",
        src = esc(&source_slug),
        src_text = esc(&source_slug),
        crumb = esc(&crumb_text),
    ));
    body.push_str(&format!(
        "<h1>Question on <code>{src}</code></h1>\n\
         <blockquote class=\"question\">{q}</blockquote>\n",
        src = esc(&source_slug),
        q = esc(&question_text),
    ));

    body.push_str(&format!(
        "<div id=\"build-status\" class=\"build-status\" \
            data-source=\"{src}\" data-qslug=\"{qslug}\" \
            data-building=\"{building}\">\n\
           <p class=\"sub\">{label}</p>\n\
         </div>\n",
        src = esc(&source_slug),
        qslug = esc(&question_slug),
        building = if is_building { "1" } else { "0" },
        label = if is_building {
            "building answer..."
        } else if has_nodes {
            "answer ready"
        } else {
            "build pending"
        },
    ));

    // ── Question metadata + decomposition tree ─────────────────────────
    {
        let (tree_json, build_id) = {
            let conn = state.reader.lock().await;
            let tj = db::get_question_tree(&conn, &question_slug).ok().flatten();
            let bid = db::get_current_build_id(&conn, &question_slug).ok().flatten();
            (tj, bid)
        };
        if let Some(json) = tree_json.as_ref() {
            let pretty = serde_json::to_string_pretty(json)
                .unwrap_or_else(|_| "(invalid tree)".to_string());
            let inner = format!("<pre class=\"tree-json\">{}</pre>", esc(&pretty));
            body.push_str(&details_section(
                "Question decomposition tree",
                1,
                false,
                &inner,
            ));
        } else {
            body.push_str(&details_section(
                "Question decomposition tree",
                0,
                false,
                "",
            ));
        }
        let meta = format!(
            "<dl class=\"meta\">\
              <dt>slug</dt><dd>{}</dd>\
              <dt>source</dt><dd><a href=\"/p/{}\">{}</a></dd>\
              <dt>max_depth</dt><dd>{}</dd>\
              <dt>node_count</dt><dd>{}</dd>\
              <dt>build_id</dt><dd>{}</dd>\
              <dt>created_at</dt><dd>{}</dd>\
              <dt>question</dt><dd>{}</dd>\
             </dl>",
            esc(&question_slug),
            esc(&source_slug),
            esc(&source_slug),
            question_info.max_depth,
            question_info.node_count,
            esc(build_id.as_deref().unwrap_or("(none)")),
            esc(&question_info.created_at),
            esc(&question_text),
        );
        body.push_str(&details_section("This question's metadata", 1, false, &meta));
    }

    // Inline the answer fragment if the apex is actually synthesized.
    // If render returns None (no nodes yet, or apex placeholder still
    // empty), keep the empty placeholder so the JS poll-loop fills it
    // in once the build catches up.
    if has_nodes {
        match render_question_answer_fragment(&state, &source_slug, &question_slug).await {
            Some(frag) => body.push_str(&format!("<div id=\"answer\">{}</div>\n", frag)),
            None => body.push_str("<div id=\"answer\" class=\"empty\"></div>\n"),
        }
    } else {
        body.push_str("<div id=\"answer\" class=\"empty\"></div>\n");
    }

    // Non-JS fallback: meta refresh every 3s while the build is running.
    if is_building {
        body.push_str(
            "<noscript><meta http-equiv=\"refresh\" content=\"3\"></noscript>\n",
        );
    }

    body.push_str(&format!(
        "<p><a href=\"/p/{src}\">&larr; Back to {src_text}</a></p>\n",
        src = esc(&source_slug),
        src_text = esc(&source_slug),
    ));

    page(&title, &body, "no-cache, must-revalidate")
}

/// Render the answer fragment HTML for a question pyramid (no doctype, no
/// head). Caller decides whether to wrap it in a full page or return it as
/// the response body.
/// Render the answer fragment for a question pyramid. Returns None when
/// no usable apex node exists yet (build still in progress, even if the
/// slug row was created). Caller maps None to a 202 "still building"
/// response so the client retries instead of typewritering empty HTML.
async fn render_question_answer_fragment(
    state: &Arc<PyramidState>,
    source_slug: &str,
    question_slug: &str,
) -> Option<String> {
    let conn = state.reader.lock().await;

    // Pull every live node for this question pyramid. The slug-stats
    // max_depth column lags writes, so trusting it produces empty
    // fragments during the window between "nodes inserted" and "stats
    // updated". Use a real query.
    let all = db::get_all_live_nodes(&conn, question_slug).unwrap_or_default();
    if all.is_empty() {
        drop(conn);
        return None;
    }

    // Apex = the highest-depth node. The decomposition tree is rooted
    // here and fans out downward to the leaf sub-answers.
    let max_depth = all.iter().map(|n| n.depth).max().unwrap_or(0);
    let apex = all.iter().find(|n| n.depth == max_depth)?.clone();

    // If the apex hasn't been synthesized yet (placeholder row with empty
    // headline AND empty distilled), treat as still-building. The leaf
    // sub-answers may be present but the layered synthesis isn't done.
    let apex_headline_trim = apex.headline.trim();
    let apex_distilled_trim = apex.distilled.trim();
    if apex_headline_trim.is_empty() && apex_distilled_trim.is_empty() {
        drop(conn);
        return None;
    }

    // Sub-answers: every node BELOW the apex, sorted by (depth desc, id).
    // We render them after the apex so the visitor can drill into the
    // decomposition that produced the synthesized answer at the top.
    let mut sub_answers: Vec<_> = all
        .iter()
        .filter(|n| n.id != apex.id)
        .filter(|n| {
            // Only show nodes that have actual content. Skip empty
            // placeholders left by mid-build crashes.
            !n.headline.trim().is_empty() || !n.distilled.trim().is_empty()
        })
        .cloned()
        .collect();
    sub_answers.sort_by(|a, b| b.depth.cmp(&a.depth).then(a.id.cmp(&b.id)));

    // Cited source nodes: scan the apex distilled text for any node IDs
    // from the source pyramid that appear verbatim.
    let source_nodes =
        db::get_all_live_nodes(&conn, source_slug).unwrap_or_default();
    let mut cited: Vec<(String, String)> = Vec::new();
    for sn in &source_nodes {
        if !sn.id.is_empty() && apex.distilled.contains(&sn.id) {
            cited.push((sn.id.clone(), sn.headline.clone()));
        }
    }
    drop(conn);

    let mut out = String::new();

    // ── Apex (synthesized answer) ──────────────────────────────────────
    out.push_str(&format!(
        "<article class=\"node node--{state} answer-apex\">\n\
           <h2>{headline}</h2>\n\
           <p class=\"distilled\">{distilled}</p>\n",
        state = node_state_class(&apex),
        headline = if apex_headline_trim.is_empty() {
            esc("(no headline)")
        } else {
            esc(apex_headline_trim)
        },
        distilled = if apex_distilled_trim.is_empty() {
            esc("(synthesizing…)")
        } else {
            esc(apex_distilled_trim)
        },
    ));
    if !cited.is_empty() {
        out.push_str("<section class=\"cites\"><h3>Cited from</h3><ul>\n");
        for (nid, hl) in &cited {
            out.push_str(&format!(
                "<li><a href=\"/p/{src}/{nid}\">{hl}</a></li>\n",
                src = esc(source_slug),
                nid = esc(nid),
                hl = esc(hl),
            ));
        }
        out.push_str("</ul></section>\n");
    }
    out.push_str(&format!(
        "<footer class=\"prov\">{qslug}/{depth}/{nid}</footer>\n",
        qslug = esc(question_slug),
        depth = apex.depth,
        nid = esc(&apex.id),
    ));
    out.push_str("</article>\n");

    // ── Decomposition (sub-question answers) ──────────────────────────
    // Render every sub-answer node beneath the apex so the visitor can
    // see what the build actually decomposed and synthesized. Each is a
    // collapsible <details> so the page stays scannable for big trees.
    if !sub_answers.is_empty() {
        out.push_str(&format!(
            "<section class=\"decomposition\">\n\
               <h3>Decomposition ({n} sub-answers)</h3>\n",
            n = sub_answers.len(),
        ));
        // Group by depth so the layered structure is visible at a glance.
        let mut last_depth: Option<i64> = None;
        for n in &sub_answers {
            if last_depth != Some(n.depth) {
                if last_depth.is_some() {
                    out.push_str("</div>\n");
                }
                let layer_desc = if n.depth == 0 {
                    "L0: Direct evidence from source nodes"
                } else if n.depth == 1 {
                    "L1: Layered synthesis answers"
                } else {
                    "Synthesis layer"
                };
                out.push_str(&format!(
                    "<div class=\"decomp-layer\" data-depth=\"{d}\">\n\
                       <h4 class=\"decomp-layer-label\">L{d}</h4>\n\
                       <p class=\"sub\">{desc}</p>\n",
                    d = n.depth,
                    desc = esc(layer_desc),
                ));
                last_depth = Some(n.depth);
            }
            let hl = n.headline.trim();
            let ds = n.distilled.trim();
            // Open the first layer by default, leave the rest collapsed.
            let open_attr = if last_depth == Some(max_depth - 1) {
                " open"
            } else {
                ""
            };
            let mut extras = String::new();
            if !n.topics.is_empty() {
                extras.push_str(&format!(
                    "<p class=\"sub\">topics ({}):</p><ul>",
                    n.topics.len()
                ));
                for t in &n.topics {
                    extras.push_str(&format!(
                        "<li><strong>{}</strong>: {}</li>",
                        esc(&t.name),
                        esc(&t.current),
                    ));
                }
                extras.push_str("</ul>");
            }
            if !n.terms.is_empty() {
                extras.push_str(&format!(
                    "<p class=\"sub\">terms ({}):</p><dl>",
                    n.terms.len()
                ));
                for t in &n.terms {
                    extras.push_str(&format!(
                        "<dt>{}</dt><dd>{}</dd>",
                        esc(&t.term),
                        esc(&t.definition),
                    ));
                }
                extras.push_str("</dl>");
            }
            if !n.corrections.is_empty() {
                extras.push_str(&format!(
                    "<p class=\"sub\">corrections: {}</p>",
                    n.corrections.len()
                ));
            }
            out.push_str(&format!(
                "<details class=\"sub-answer\"{open}>\n\
                   <summary>\n\
                     <span class=\"sub-id\">{id}</span>\n\
                     <span class=\"sub-headline\">{headline}</span>\n\
                   </summary>\n\
                   <div class=\"sub-body\">\n\
                     <p class=\"distilled\">{distilled}</p>\n\
                     {extras}\n\
                     <footer class=\"prov\">{qslug}/{depth}/{id}</footer>\n\
                   </div>\n\
                 </details>\n",
                open = open_attr,
                id = esc(&n.id),
                headline = if hl.is_empty() { esc("(no headline)") } else { esc(hl) },
                distilled = if ds.is_empty() { esc("(no content)") } else { esc(ds) },
                extras = extras,
                qslug = esc(question_slug),
                depth = n.depth,
            ));
        }
        if last_depth.is_some() {
            out.push_str("</div>\n");
        }
        out.push_str("</section>\n");
    }

    Some(out)
}

async fn handle_question_fragment(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
    source_slug: String,
    question_slug: String,
    peer: Option<std::net::SocketAddr>,
    headers: warp::http::HeaderMap,
) -> warp::reply::Response {
    let auth = resolve_auth(&headers, peer, &state, &jwt_public_key).await;
    let (_source_info, _question_info) =
        match validate_question_pair(&state, &source_slug, &question_slug, &auth).await {
            Ok(p) => p,
            Err(resp) => return resp,
        };

    // Still building? 202 + a tiny placeholder.
    //
    // CRITICAL: question_info.node_count is the cached stat on pyramid_slugs
    // and never gets updated by question builds (it stays at 0 forever).
    // Use a real COUNT(*) of pyramid_nodes via get_all_live_nodes instead.
    let is_building = {
        let active = state.active_build.read().await;
        if let Some(handle) = active.get(&question_slug) {
            let s = handle.status.read().await;
            !s.is_terminal()
        } else {
            false
        }
    };
    let live_node_count = {
        let conn = state.reader.lock().await;
        db::get_all_live_nodes(&conn, &question_slug)
            .map(|v| v.len())
            .unwrap_or(0)
    };
    if is_building && live_node_count == 0 {
        let body = "<p class=\"sub\">still building, retry in 2s</p>";
        let resp = warp::http::Response::builder()
            .status(202)
            .header(warp::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(warp::http::header::CACHE_CONTROL, "no-store")
            .body(body.to_string())
            .unwrap();
        return resp.into_response();
    }

    // If the apex isn't synthesized yet (no nodes, or placeholder row
    // with empty headline+distilled), return a 202 so the client retries
    // instead of locking in empty content.
    match render_question_answer_fragment(&state, &source_slug, &question_slug).await {
        Some(frag) => warp::http::Response::builder()
            .status(200)
            .header(warp::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(warp::http::header::CACHE_CONTROL, "no-store")
            .body(frag)
            .unwrap()
            .into_response(),
        None => {
            let body = "<p class=\"sub\">apex not synthesized yet, retry in 2s</p>";
            warp::http::Response::builder()
                .status(202)
                .header(warp::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
                .header(warp::http::header::CACHE_CONTROL, "no-store")
                .body(body.to_string())
                .unwrap()
                .into_response()
        }
    }
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
    let shown: Vec<_> = hits.iter().take(SEARCH_RESULT_CAP).cloned().collect();

    body.push_str(&format!(
        "<h1>results for <q>{qe}</q></h1>\n",
        qe = esc(&q),
    ));

    // Tokenize the query the same way the search does (very rough — for
    // display only) so the visitor sees what was actually matched.
    let tokens: Vec<String> = q
        .split_whitespace()
        .filter(|t| t.len() > 2)
        .map(|t| t.to_lowercase())
        .collect();
    {
        let inner = format!(
            "<dl class=\"meta\">\
              <dt>query</dt><dd>{}</dd>\
              <dt>slug</dt><dd>{}</dd>\
              <dt>tokenized</dt><dd>{}</dd>\
              <dt>matching</dt><dd>OR-match across headline, distilled, terms (stop-words removed)</dd>\
             </dl>",
            esc(&q),
            esc(&slug),
            esc(&tokens.join(" ")),
        );
        body.push_str(&details_section("Search settings", 1, false, &inner));
    }

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
        // Pre-load full distilled bodies for the "more" expandables.
        let full_nodes: HashMap<String, PyramidNode> = {
            let conn2 = state.reader.lock().await;
            db::get_all_live_nodes(&conn2, &slug)
                .unwrap_or_default()
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect()
        };
        body.push_str("<ul class=\"search-results\">\n");
        for hit in &shown {
            // Highlight the snippet by wrapping each token (case-insensitive) in <mark>.
            let snippet_marked = mark_tokens(&hit.snippet, &tokens);
            let depth_label = format!("L{}", hit.depth);
            let full = full_nodes
                .get(&hit.node_id)
                .map(|n| n.distilled.as_str())
                .unwrap_or("");
            body.push_str(&format!(
                "<li><article class=\"search-result\">\
                   <a href=\"/p/{slug}/{nid}\"><strong>{headline}</strong></a> \
                   <span class=\"sub\">[{dl}] score={score:.2}</span>\
                   <p class=\"snippet\">{snippet}</p>\
                   <details><summary>more</summary><p class=\"distilled\">{full}</p></details>\
                 </article></li>\n",
                slug = esc(&slug),
                nid = esc(&hit.node_id),
                headline = esc(&hit.headline),
                dl = depth_label,
                score = hit.score,
                snippet = snippet_marked,
                full = esc(full),
            ));
        }
        body.push_str("</ul>\n");
    }

    // ── How search works ──────────────────────────────────────────────
    body.push_str(&details_section(
        "How search works",
        1,
        false,
        "<p>This is a simple OR-match keyword search across node headlines, \
         distilled text, and terms. Stop-words are removed and tokens shorter \
         than 3 characters are ignored. For richer answers that synthesize \
         across the pyramid, use the question form on the pyramid home page.</p>",
    ));

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
            true,
        );
    }

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
    is_root: bool,
) {
    if *rendered >= TREE_NODE_CAP {
        *truncated = true;
        return;
    }
    *rendered += 1;

    let preview = truncate_chars(node.distilled.trim(), 80);
    let full = truncate_chars(node.distilled.trim(), 400);
    let open_attr = if is_root { " open" } else { "" };
    body.push_str(&format!(
        "<details class=\"tree-node node--{state}\"{open}>\n\
           <summary><span class=\"sub\">{nid}</span> \
                    <a href=\"/p/{slug}/{nid_link}\"><strong>{headline}</strong></a> \
                    <span class=\"sub\">{preview}</span></summary>\n\
           <div class=\"tree-body\"><p class=\"distilled\">{full}</p></div>\n",
        state = node_state_class(node),
        open = open_attr,
        nid = esc(&node.id),
        slug = esc(slug),
        nid_link = esc(&node.id),
        headline = esc(&node.headline),
        preview = esc(&preview),
        full = esc(&full),
    ));

    let child_nodes: Vec<&PyramidNode> = node
        .children
        .iter()
        .filter_map(|cid| by_id.get(cid))
        .filter(|c| c.depth >= min_depth)
        .collect();

    if !child_nodes.is_empty() && *rendered < TREE_NODE_CAP {
        for c in child_nodes {
            if *rendered >= TREE_NODE_CAP {
                *truncated = true;
                break;
            }
            render_tree_node(c, by_id, slug, min_depth, body, rendered, truncated, false);
        }
    }
    body.push_str("</details>\n");
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

    // Collect terms. Track which node_ids each term came from so we can
    // show provenance under each definition.
    let mut sorted_nodes = all;
    sorted_nodes.sort_by_key(|n| n.depth);
    // lower -> (term, def, sources)
    let mut by_lower: HashMap<String, (String, String, Vec<String>)> = HashMap::new();
    for n in &sorted_nodes {
        for t in &n.terms {
            let lower = t.term.trim().to_lowercase();
            if lower.is_empty() {
                continue;
            }
            let entry = by_lower.entry(lower).or_insert_with(|| {
                (t.term.clone(), t.definition.clone(), Vec::new())
            });
            entry.0 = t.term.clone();
            entry.1 = t.definition.clone();
            if !entry.2.contains(&n.id) {
                entry.2.push(n.id.clone());
            }
        }
    }

    let mut entries: Vec<(String, String, Vec<String>)> = by_lower.into_values().collect();
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
        // Build letter index: which letters actually have entries
        let mut letters: HashSet<char> = HashSet::new();
        for (term, _, _) in &entries {
            if let Some(c) = term.chars().next() {
                letters.insert(c.to_ascii_uppercase());
            }
        }
        let mut sorted_letters: Vec<char> = letters.into_iter().collect();
        sorted_letters.sort();
        body.push_str("<nav class=\"glossary-letter-nav\">");
        for l in &sorted_letters {
            body.push_str(&format!("<a href=\"#g-{l}\">{l}</a>", l = l));
        }
        body.push_str("</nav>\n");

        // Group by first letter
        let mut current_letter: Option<char> = None;
        body.push_str("<dl class=\"glossary\">\n");
        for (term, def, sources) in &entries {
            let first = term
                .chars()
                .next()
                .map(|c| c.to_ascii_uppercase())
                .unwrap_or('?');
            if Some(first) != current_letter {
                body.push_str(&format!(
                    "</dl>\n<h2 id=\"g-{l}\">{l}</h2>\n<dl class=\"glossary\">\n",
                    l = first,
                ));
                current_letter = Some(first);
            }
            body.push_str(&format!(
                "<dt>{}</dt><dd>{}<br><small>used in: ",
                esc(term),
                esc(def),
            ));
            for (i, sid) in sources.iter().take(5).enumerate() {
                if i > 0 {
                    body.push_str(", ");
                }
                body.push_str(&format!(
                    "<a href=\"/p/{slug}/{sid}\">{sid}</a>",
                    slug = esc(&slug),
                    sid = esc(sid),
                ));
            }
            if sources.len() > 5 {
                body.push_str(&format!(" +{} more", sources.len() - 5));
            }
            body.push_str("</small></dd>\n");
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

    // Pre-walk: collect every node we'll render so we can build a TOC.
    let mut toc_ids: Vec<(String, String, i64)> = Vec::new();
    {
        let mut seen_pre: HashSet<String> = HashSet::new();
        let mut stack: Vec<&PyramidNode> = vec![&apex];
        while let Some(n) = stack.pop() {
            if !seen_pre.insert(n.id.clone()) {
                continue;
            }
            toc_ids.push((n.id.clone(), n.headline.clone(), n.depth));
            if toc_ids.len() >= FOLIO_NODE_CAP {
                break;
            }
            if n.depth > min_allowed_depth {
                for cid in &n.children {
                    if let Some(c) = by_id.get(cid) {
                        stack.push(c);
                    }
                }
            }
        }
    }

    // Folio metadata + TOC at top.
    {
        let meta = format!(
            "<dl class=\"meta\">\
              <dt>depth</dt><dd>{d}</dd>\
              <dt>nodes rendered</dt><dd>{n}</dd>\
              <dt>content_type</dt><dd>{ct}</dd>\
              <dt>last_built_at</dt><dd>{lb}</dd>\
             </dl>",
            d = depth,
            n = toc_ids.len(),
            ct = esc(info.content_type.as_str()),
            lb = esc(info.last_built_at.as_deref().unwrap_or("(never)")),
        );
        body.push_str(&details_section("Folio metadata", 1, true, &meta));
    }
    {
        let mut toc_inner = String::from("<ul class=\"folio-toc\">\n");
        for (id, hl, d) in &toc_ids {
            toc_inner.push_str(&format!(
                "<li><a href=\"#{id}\">{hl}</a> <span class=\"sub\">L{d}</span></li>\n",
                id = esc(id),
                hl = esc(hl),
                d = d,
            ));
        }
        toc_inner.push_str("</ul>");
        body.push_str(&details_section(
            "Folio table of contents",
            toc_ids.len(),
            false,
            &toc_inner,
        ));
    }

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

/// HTML-escape `text` and wrap any case-insensitive occurrence of any
/// `tokens` element in `<mark>...</mark>`. Used by search to highlight
/// matched query words in result snippets. Both the text and tokens are
/// escaped before insertion so this is XSS-safe.
fn mark_tokens(text: &str, tokens: &[String]) -> String {
    if tokens.is_empty() {
        return esc(text);
    }
    let lower = text.to_lowercase();
    // Build a list of (start, end) byte ranges to highlight, scanning each token.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for tok in tokens {
        if tok.is_empty() {
            continue;
        }
        let mut start = 0;
        while let Some(pos) = lower[start..].find(tok.as_str()) {
            let s = start + pos;
            let e = s + tok.len();
            ranges.push((s, e));
            start = e;
        }
    }
    if ranges.is_empty() {
        return esc(text);
    }
    // Sort + merge overlaps.
    ranges.sort_by_key(|r| r.0);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                if e > last.1 {
                    last.1 = e;
                }
                continue;
            }
        }
        merged.push((s, e));
    }
    let mut out = String::new();
    let mut cursor = 0;
    for (s, e) in merged {
        if s > cursor {
            out.push_str(&esc(&text[cursor..s]));
        }
        out.push_str("<mark>");
        out.push_str(&esc(&text[s..e]));
        out.push_str("</mark>");
        cursor = e;
    }
    if cursor < text.len() {
        out.push_str(&esc(&text[cursor..]));
    }
    out
}

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
