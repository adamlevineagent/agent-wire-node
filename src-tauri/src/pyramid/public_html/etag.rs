//! ETag + 304 short-circuit helpers for the post-agents-retro public HTML
//! surface (WS-I). Per plan v3.3 A10/P2-2:
//!
//! - Per-node ETag uses `node.id` + a content version (updated_at if the
//!   schema had it; we fall back to a hash of headline+distilled because
//!   `PyramidNode` does not yet carry `updated_at`, and adding it has a
//!   large blast radius).
//! - Pyramid-level ETag uses `pyramid_slugs.updated_at` (added by the
//!   Phase 0.5 skeleton migration).
//!
//! All ETags are weak (`W/"..."`) because the body is rendered HTML and
//! whitespace/time-of-day interpolation can vary insignificantly between
//! otherwise-equivalent renders.

use crate::pyramid::types::PyramidNode;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Compute a weak ETag for a single node. Because `PyramidNode` does not
/// currently expose an `updated_at` timestamp, we derive a cheap content
/// version by hashing the load-bearing fields that the read view actually
/// renders (headline + distilled + superseded_by + children list). This is
/// enough to flip the ETag whenever the visible content changes, which is
/// all a conditional GET needs.
///
/// Format: `W/"node-{id}-{hex16}"`.
pub fn etag_for_node(node: &PyramidNode) -> String {
    let mut hasher = DefaultHasher::new();
    node.headline.hash(&mut hasher);
    node.distilled.hash(&mut hasher);
    node.superseded_by.hash(&mut hasher);
    for c in &node.children {
        c.hash(&mut hasher);
    }
    // Terms appear in the rendered page too — include them so edits flip
    // the ETag without needing a schema timestamp.
    for t in &node.terms {
        t.term.hash(&mut hasher);
        t.definition.hash(&mut hasher);
    }
    let h = hasher.finish();
    format!("W/\"node-{}-{:016x}\"", node.id, h)
}

/// Compute a weak ETag for a whole pyramid (apex/home, tree, search, folio,
/// glossary). Derived from the slug plus `pyramid_slugs.updated_at`, which
/// is bumped whenever the pyramid is rebuilt.
///
/// Format: `W/"pyramid-{slug}-{updated_at}"`.
pub fn etag_for_pyramid(slug: &str, updated_at: &str) -> String {
    format!("W/\"pyramid-{}-{}\"", slug, updated_at)
}

/// Returns true when the request's `If-None-Match` header matches the
/// supplied ETag, so the caller should short-circuit with 304.
///
/// Per RFC 7232 §2.3, weak and strong comparisons for `If-None-Match` use
/// weak matching: `W/"foo"` and `"foo"` are equivalent. We normalise by
/// stripping the optional `W/` prefix and the surrounding quotes before
/// comparing each opaque-tag, and we also tolerate the raw unquoted form
/// that some clients (and our own `matches_inm_handles_weak_strong` test)
/// emit.
pub fn matches_inm(headers: &warp::http::HeaderMap, etag: &str) -> bool {
    let inm = match headers.get("if-none-match").and_then(|h| h.to_str().ok()) {
        Some(v) => v,
        None => return false,
    };
    let want = strip_weak(etag);
    // `*` matches anything.
    if inm.trim() == "*" {
        return true;
    }
    inm.split(',')
        .map(str::trim)
        .any(|t| strip_weak(t) == want)
}

/// Strip an optional `W/` weakness prefix and surrounding double quotes so
/// two tags can be compared for equivalence per RFC 7232.
fn strip_weak(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("W/").unwrap_or(s);
    s.trim_matches('"')
}

/// Build a bare 304 Not Modified response that carries the ETag header but
/// no body. Used by the read handlers for conditional GETs.
pub fn not_modified(etag: &str) -> warp::reply::Response {
    let mut resp = warp::reply::Response::new(warp::hyper::Body::empty());
    *resp.status_mut() = warp::http::StatusCode::NOT_MODIFIED;
    if let Ok(v) = warp::http::HeaderValue::from_str(etag) {
        resp.headers_mut().insert("etag", v);
    }
    resp
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::types::PyramidNode;
    use warp::http::{HeaderMap, HeaderValue};

    fn mk_node(id: &str, headline: &str, distilled: &str) -> PyramidNode {
        PyramidNode {
            id: id.to_string(),
            slug: "s".to_string(),
            depth: 0,
            chunk_index: None,
            headline: headline.to_string(),
            distilled: distilled.to_string(),
            topics: Vec::new(),
            corrections: Vec::new(),
            decisions: Vec::new(),
            terms: Vec::new(),
            dead_ends: Vec::new(),
            self_prompt: String::new(),
            children: Vec::new(),
            parent_id: None,
            superseded_by: None,
            build_id: None,
            created_at: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn etag_for_node_changes_on_content_change() {
        let a = mk_node("n1", "Alpha", "first body");
        let b = mk_node("n1", "Alpha", "second body");
        assert_ne!(etag_for_node(&a), etag_for_node(&b));
    }

    #[test]
    fn etag_for_node_stable_on_identical_content() {
        let a = mk_node("n1", "Alpha", "same");
        let b = mk_node("n1", "Alpha", "same");
        assert_eq!(etag_for_node(&a), etag_for_node(&b));
    }

    #[test]
    fn etag_for_node_includes_id() {
        let a = mk_node("n1", "Alpha", "body");
        let b = mk_node("n2", "Alpha", "body");
        assert_ne!(etag_for_node(&a), etag_for_node(&b));
    }

    #[test]
    fn etag_for_pyramid_format() {
        let e = etag_for_pyramid("my-slug", "2026-04-01 10:11:12");
        assert_eq!(e, "W/\"pyramid-my-slug-2026-04-01 10:11:12\"");
    }

    #[test]
    fn matches_inm_handles_weak_strong() {
        let mut h = HeaderMap::new();
        h.insert("if-none-match", HeaderValue::from_static("\"foo\""));
        assert!(matches_inm(&h, "W/\"foo\""));
        assert!(matches_inm(&h, "\"foo\""));

        let mut h2 = HeaderMap::new();
        h2.insert("if-none-match", HeaderValue::from_static("W/\"foo\""));
        assert!(matches_inm(&h2, "W/\"foo\""));
        assert!(matches_inm(&h2, "\"foo\""));
    }

    #[test]
    fn matches_inm_multi_list() {
        let mut h = HeaderMap::new();
        h.insert(
            "if-none-match",
            HeaderValue::from_static("\"a\", W/\"b\", \"c\""),
        );
        assert!(matches_inm(&h, "W/\"b\""));
        assert!(matches_inm(&h, "\"c\""));
        assert!(!matches_inm(&h, "\"d\""));
    }

    #[test]
    fn matches_inm_wildcard() {
        let mut h = HeaderMap::new();
        h.insert("if-none-match", HeaderValue::from_static("*"));
        assert!(matches_inm(&h, "W/\"anything\""));
    }

    #[test]
    fn matches_inm_missing_header() {
        let h = HeaderMap::new();
        assert!(!matches_inm(&h, "W/\"foo\""));
    }

    #[test]
    fn not_modified_has_no_body_and_etag_header() {
        let r = not_modified("W/\"foo\"");
        assert_eq!(r.status(), warp::http::StatusCode::NOT_MODIFIED);
        assert_eq!(
            r.headers().get("etag").and_then(|v| v.to_str().ok()),
            Some("W/\"foo\"")
        );
    }
}
