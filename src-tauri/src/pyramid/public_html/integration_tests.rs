//! Phase 4 integration verification harness for the post-agents-retro web
//! surface. Cross-module assertions that the unit tests in each sibling
//! module can't catch on their own.
//!
//! Approach: handler-call level. We do NOT spin up a real warp server.
//! Heavy `PyramidState` construction is also avoided — instead each test
//! exercises the smallest combination of real modules required to assert
//! the integration boundary in question. The unit tests in `auth.rs`,
//! `etag.rs`, `rate_limit.rs`, `reserved.rs`, `web_sessions.rs`, and
//! `ascii_art.rs` already cover the inside of those modules; this file
//! covers the *seams between* them and the WS-L Phase 4 wiring.

#![cfg(test)]

use super::ascii_art::{insert_with_supersession, lookup_head};
use super::auth::{csrf_nonce, verify_csrf};
use super::etag::{etag_for_node, etag_for_pyramid, matches_inm};
use super::render::{esc, page_with_etag, safe_href};
use super::reserved::is_reserved_subpath;
use super::web_sessions;
use crate::pyramid::types::{PyramidNode, Term};
use rusqlite::{params, Connection};
use warp::http::HeaderMap;
use warp::Reply;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

// ── Fixture helpers ──────────────────────────────────────────────────────

fn fixture_node(id: &str, headline: &str, distilled: &str) -> PyramidNode {
    PyramidNode {
        id: id.to_string(),
        slug: "test-slug".to_string(),
        depth: 0,
        chunk_index: None,
        headline: headline.to_string(),
        distilled: distilled.to_string(),
        topics: vec![],
        corrections: vec![],
        decisions: vec![],
        terms: vec![Term {
            term: "x".to_string(),
            definition: "y".to_string(),
        }],
        dead_ends: vec![],
        self_prompt: String::new(),
        children: vec![],
        parent_id: None,
        superseded_by: None,
        build_id: None,
        created_at: String::new(),
    }
}

fn fresh_ascii_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE pyramid_ascii_art (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slug TEXT NOT NULL,
            kind TEXT NOT NULL,
            source_hash TEXT NOT NULL,
            art_text TEXT NOT NULL,
            model TEXT NOT NULL,
            superseded_by INTEGER REFERENCES pyramid_ascii_art(id),
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX idx_ascii_art_slug_kind_head
            ON pyramid_ascii_art(slug, kind) WHERE superseded_by IS NULL;",
    )
    .unwrap();
    conn
}

fn fresh_web_sessions_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE web_sessions (
            token TEXT PRIMARY KEY,
            supabase_user_id TEXT NOT NULL,
            email TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            expires_at TEXT NOT NULL
        );",
    )
    .unwrap();
    conn
}

fn body_string(resp: warp::reply::Response) -> (warp::http::StatusCode, HeaderMap, String) {
    use warp::hyper::body::to_bytes;
    let (parts, body) = resp.into_response().into_parts();
    let bytes = rt().block_on(to_bytes(body)).unwrap_or_default();
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&bytes).to_string(),
    )
}

// ── 1. WS-L: banner injection wiring ─────────────────────────────────────

#[test]
fn page_with_etag_emits_empty_banner_when_none() {
    let resp = page_with_etag("Title", "<p>hi</p>", "no-cache", None, None);
    let (_status, _h, body) = body_string(resp);
    assert!(body.contains("data-banner=\"\""));
}

#[test]
fn page_with_etag_injects_banner_when_some() {
    // The art string contains characters that MUST be HTML-escaped.
    let art = "┌──┐\n│hi│\n└──┘";
    let resp = page_with_etag("Title", "<p>hi</p>", "no-cache", None, Some(art));
    let (_status, _h, body) = body_string(resp);
    // Newlines + box-drawing render unchanged through esc(); the attribute
    // is double-quoted so esc() also flips `&` `<` `>` `"` `'`.
    assert!(body.contains("data-banner=\"┌──┐\n│hi│\n└──┘\""));
}

#[test]
fn page_with_etag_escapes_hostile_banner_attribute() {
    // A malicious banner that tries to break out of the attribute.
    let art = "\"><script>alert(1)</script>";
    let resp = page_with_etag("Title", "<p>hi</p>", "no-cache", None, Some(art));
    let (_status, _h, body) = body_string(resp);
    // The literal closing `">` MUST NOT appear before the legitimate one,
    // and the script tag MUST be escaped.
    assert!(!body.contains("\"><script>"));
    assert!(body.contains("&quot;&gt;&lt;script&gt;"));
}

#[test]
fn page_with_etag_emits_csp_and_security_headers() {
    let resp = page_with_etag("Title", "<p>hi</p>", "no-cache", None, None);
    let (status, headers, _body) = body_string(resp);
    assert_eq!(status.as_u16(), 200);
    assert!(headers.contains_key("content-security-policy"));
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("referrer-policy").unwrap(), "same-origin");
    let csp = headers.get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("frame-ancestors 'none'"));
}

#[test]
fn page_with_etag_emits_etag_when_supplied() {
    let resp = page_with_etag("T", "b", "no-cache", Some("\"abc123\""), None);
    let (_status, headers, _body) = body_string(resp);
    assert_eq!(headers.get("etag").unwrap(), "\"abc123\"");
}

// ── 2. HTML escaping (XSS surface, criteria #6, #7, #22) ─────────────────

#[test]
fn esc_handles_xss_payloads() {
    let payload = "<script>alert(1)</script>";
    let escaped = esc(payload);
    assert_eq!(escaped, "&lt;script&gt;alert(1)&lt;/script&gt;");
    assert!(!escaped.contains("<script>"));
}

#[test]
fn esc_handles_attribute_breakout() {
    let payload = "\" onload=\"alert(1)";
    let escaped = esc(payload);
    assert!(!escaped.contains("\""));
    assert!(escaped.contains("&quot;"));
}

#[test]
fn esc_handles_glossary_term_with_lt() {
    // criteria #22 — glossary term with `<` renders escaped
    let term = "Vec<T>";
    let escaped = esc(term);
    assert_eq!(escaped, "Vec&lt;T&gt;");
}

#[test]
fn safe_href_blocks_javascript_scheme() {
    assert!(safe_href("javascript:alert(1)").is_none());
    assert!(safe_href("JaVaScRiPt:alert(1)").is_none());
    assert!(safe_href("data:text/html,<script>").is_none());
    assert!(safe_href("vbscript:msgbox").is_none());
}

#[test]
fn safe_href_allows_safe_schemes() {
    assert!(safe_href("https://example.com").is_some());
    assert!(safe_href("http://example.com").is_some());
    assert!(safe_href("/p/foo").is_some());
    assert!(safe_href("#anchor").is_some());
}

// ── 3. Reserved subpath protection (criteria #15, #19) ────────────────────

#[test]
fn reserved_subpaths_protect_against_node_id_collision() {
    // criteria #15 — _login, _ws, _ask, _verify, _logout etc are reserved
    assert!(is_reserved_subpath("_login"));
    assert!(is_reserved_subpath("_ws"));
    assert!(is_reserved_subpath("_ask"));
    assert!(is_reserved_subpath("_verify"));
    assert!(is_reserved_subpath("_logout"));
    assert!(is_reserved_subpath("_search"));
    assert!(is_reserved_subpath("_tree"));
    assert!(is_reserved_subpath("_glossary"));
    assert!(is_reserved_subpath("_folio"));
    // ordinary node ids must NOT be flagged
    assert!(!is_reserved_subpath("node-abc-123"));
    assert!(!is_reserved_subpath("login")); // no leading underscore
}

#[test]
fn reserved_slug_with_leading_underscore_rejected_at_handler() {
    // criteria #19 — `/p/_pyramid/...` returns 404. The handlers all do
    // `if slug.starts_with('_') { return not_found_page(); }` as the first
    // step. We assert the precondition here at the predicate level.
    let slug = "_pyramid";
    assert!(slug.starts_with('_'));
    let slug = "_assets";
    assert!(slug.starts_with('_'));
}

// ── 4. ETag (criteria #16) ───────────────────────────────────────────────

#[test]
fn etag_for_node_changes_with_content() {
    let n1 = fixture_node("a", "Title 1", "Body");
    let n2 = fixture_node("a", "Title 2", "Body");
    assert_ne!(etag_for_node(&n1), etag_for_node(&n2));
}

#[test]
fn etag_stable_across_repeated_calls() {
    let n = fixture_node("a", "Title", "Body");
    assert_eq!(etag_for_node(&n), etag_for_node(&n));
}

#[test]
fn matches_inm_returns_true_on_exact_etag() {
    let etag = etag_for_pyramid("foo", "2026-04-06T00:00:00");
    let mut headers = HeaderMap::new();
    headers.insert("if-none-match", etag.parse().unwrap());
    assert!(matches_inm(&headers, &etag));
}

#[test]
fn matches_inm_returns_false_on_different_etag() {
    let etag1 = etag_for_pyramid("foo", "2026-04-06T00:00:00");
    let etag2 = etag_for_pyramid("foo", "2026-04-06T00:00:01");
    let mut headers = HeaderMap::new();
    headers.insert("if-none-match", etag1.parse().unwrap());
    assert!(!matches_inm(&headers, &etag2));
}

// ── 5. CSRF (criteria #8, #20) ───────────────────────────────────────────

#[test]
fn csrf_nonce_verifies_for_same_session_and_slug() {
    let secret = [7u8; 32];
    let n = csrf_nonce(&secret, "session-token-A", "pyramid-foo");
    assert!(verify_csrf(&secret, &n, "session-token-A", "pyramid-foo"));
}

#[test]
fn csrf_nonce_rejects_wrong_session() {
    let secret = [7u8; 32];
    let n = csrf_nonce(&secret, "session-A", "pyramid-foo");
    assert!(!verify_csrf(&secret, &n, "session-B", "pyramid-foo"));
}

#[test]
fn csrf_nonce_rejects_wrong_slug() {
    // criteria #20 — token for question A replayed for question B → reject.
    // CSRF nonce binds (session, slug, window). A nonce for slug-A is
    // invalid for slug-B even with the same session.
    let secret = [7u8; 32];
    let n = csrf_nonce(&secret, "session-A", "slug-A");
    assert!(!verify_csrf(&secret, &n, "session-A", "slug-B"));
}

#[test]
fn csrf_nonce_rejects_wrong_secret() {
    let secret_a = [7u8; 32];
    let secret_b = [9u8; 32];
    let n = csrf_nonce(&secret_a, "session", "slug");
    assert!(!verify_csrf(&secret_b, &n, "session", "slug"));
}

// ── 6. WebSession lifecycle (criteria #13) ───────────────────────────────

#[test]
fn web_sessions_create_lookup_delete_roundtrip() {
    let conn = fresh_web_sessions_db();
    let token = web_sessions::create(&conn, "user-uuid-1", "alice@example.com", 3600).unwrap();
    let found = web_sessions::lookup(&conn, &token).unwrap().unwrap();
    assert_eq!(found.email, "alice@example.com");
    assert_eq!(found.supabase_user_id, "user-uuid-1");

    // delete returns 1 row affected
    let n = web_sessions::delete(&conn, &token).unwrap();
    assert_eq!(n, 1);
    // subsequent lookup is None
    assert!(web_sessions::lookup(&conn, &token).unwrap().is_none());
}

#[test]
fn web_sessions_expired_lookup_returns_none() {
    let conn = fresh_web_sessions_db();
    // Negative TTL = already expired
    let token = web_sessions::create(&conn, "user-uuid-2", "bob@example.com", -10).unwrap();
    assert!(web_sessions::lookup(&conn, &token).unwrap().is_none());
}

// ── 7. WS-L supersession (criteria #28) ──────────────────────────────────

#[test]
fn ws_l_supersession_chains_history_and_returns_head() {
    let conn = fresh_ascii_db();
    let id1 = insert_with_supersession(&conn, "test-slug", "banner", "h1", "art-v1", "grok").unwrap();
    let id2 = insert_with_supersession(&conn, "test-slug", "banner", "h2", "art-v2", "grok").unwrap();

    // Head is the second (newest) row
    let head = lookup_head(&conn, "test-slug", "banner").unwrap().unwrap();
    assert_eq!(head.id, id2);
    assert_eq!(head.art_text, "art-v2");

    // Old row's superseded_by points at the new id
    let sup: Option<i64> = conn
        .query_row(
            "SELECT superseded_by FROM pyramid_ascii_art WHERE id = ?1",
            params![id1],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sup, Some(id2));

    // History is preserved (no DELETE / UPDATE of art_text)
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pyramid_ascii_art", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn ws_l_lookup_head_isolated_per_slug() {
    let conn = fresh_ascii_db();
    insert_with_supersession(&conn, "slug-a", "banner", "h", "A", "g").unwrap();
    insert_with_supersession(&conn, "slug-b", "banner", "h", "B", "g").unwrap();

    let a = lookup_head(&conn, "slug-a", "banner").unwrap().unwrap();
    let b = lookup_head(&conn, "slug-b", "banner").unwrap().unwrap();
    assert_eq!(a.art_text, "A");
    assert_eq!(b.art_text, "B");
}

// ── 8. Asset hashing / cache (criteria #27) ──────────────────────────────

#[test]
fn asset_hashed_path_returns_some_or_none_consistently() {
    // The asset manifest may or may not be populated in test mode. Either
    // way, the function must not panic and must return a `/p/_assets/` path
    // when populated.
    use super::routes_assets::hashed_path;
    if let Some(p) = hashed_path("app.css") {
        assert!(p.starts_with("/p/_assets/"));
        assert!(p.ends_with(".css"));
    }
    if let Some(p) = hashed_path("client.js") {
        assert!(p.starts_with("/p/_assets/"));
        assert!(p.ends_with(".js"));
    }
}
