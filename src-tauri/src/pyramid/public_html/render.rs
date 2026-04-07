//! HTML rendering primitives for the post-agents-retro public web surface.
//!
//! Owns: `esc()`, `safe_href()`, `page()` layout wrapper, the staleness
//! border-class helper, and the per-node provenance footer (Pillar 14).
//!
//! WS-D ships the actual CSS at `/p/_assets/app.css` (content-hashed at build
//! time). Until that lands, this module references the literal asset URL —
//! the page still renders unstyled HTML, which is the V1 fallback target
//! ("works without JavaScript and without CSS").

use crate::pyramid::types::PyramidNode;
use warp::http::header;
use warp::http::Response;
use warp::Reply;

/// HTML-escape a string for safe interpolation into element bodies and
/// double-quoted attribute values. Escapes `& < > " '` per OWASP rule #1/#2.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Validate a URL for use in `href`/`src`. Only `http://` and `https://`
/// (and protocol-relative `//`, and same-document fragments / absolute
/// paths) are allowed. Returns `None` for `javascript:`, `data:`, `vbscript:`
/// and other hostile schemes. The returned string is HTML-escaped.
pub fn safe_href(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Same-document and same-origin paths are always safe.
    if trimmed.starts_with('/') || trimmed.starts_with('#') || trimmed.starts_with('?') {
        return Some(esc(trimmed));
    }
    // Scheme allowlist.
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(esc(trimmed));
    }
    None
}

/// Path served by WS-D for the main stylesheet. WS-D will replace this with
/// a content-hashed URL via the build manifest; until then we hit the literal
/// path. The constant lives here so WS-C can be edited independently.
pub const APP_CSS_URL: &str = "/p/_assets/app.css";

/// Wrap a body fragment in the full retro layout (HTML5 doctype, head, the
/// stylesheet link, viewport, charset). Sets the security headers required
/// by A12 (CSP) and the cache-control header passed by the caller.
///
/// `cache_control` examples:
/// - `"no-store"` for cookie-issuing pages (login, verify)
/// - `"no-cache, must-revalidate"` for ordinary read pages (default)
pub fn page(title: &str, body: &str, cache_control: &str) -> warp::reply::Response {
    page_with_etag(title, body, cache_control, None, None)
}

/// Like `page()`, but also emits an `ETag` header when `etag` is `Some`,
/// and embeds an ASCII-art banner into `<body data-banner>` when `banner` is
/// `Some`. Used by WS-I for conditional GETs on the `/p/...` HTML routes;
/// the `banner` arg is the WS-L hook (pyramid pages only).
pub fn page_with_etag(
    title: &str,
    body: &str,
    cache_control: &str,
    etag: Option<&str>,
    banner: Option<&str>,
) -> warp::reply::Response {
    // WS-J: progressive-enhancement canvas overlay client. Loaded with
    // `defer`; the page is fully functional without JS. The CSP already
    // permits `script-src 'self'` so no inline script is needed.
    // WS-L will populate `data-banner` once the ascii_art module is wired
    // in; for now we emit it empty so the client doesn't draw a banner.
    let client_js_url = crate::pyramid::public_html::routes_assets::hashed_path("client.js")
        .unwrap_or("/p/_assets/client.js");
    // WS-K: WebSocket client + live build animation. Loaded AFTER client.js
    // so it can override `window.__wireCanvasUpdate`.
    let client_ws_js_url = crate::pyramid::public_html::routes_assets::hashed_path("client_ws.js")
        .unwrap_or("/p/_assets/client_ws.js");
    let css_url =
        crate::pyramid::public_html::routes_assets::hashed_path("app.css").unwrap_or(APP_CSS_URL);
    let html = format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n\
         <link rel=\"stylesheet\" href=\"{css}\">\n\
         <link rel=\"icon\" href=\"/favicon.ico\">\n\
         </head>\n\
         <body data-banner=\"{banner}\" aria-live=\"polite\">\n\
         <main class=\"page\">\n\
         {body}\n\
         </main>\n\
         <script src=\"{client_js}\" defer></script>\n\
         <script src=\"{client_ws_js}\" defer></script>\n\
         </body>\n\
         </html>\n",
        title = esc(title),
        css = css_url,
        body = body,
        client_js = client_js_url,
        client_ws_js = client_ws_js_url,
        banner = banner.map(esc).unwrap_or_default(),
    );

    let mut builder = Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, cache_control);
    if let Some(tag) = etag {
        builder = builder.header("etag", tag);
    }
    builder
        // CSP per A12. WS-D may relax img-src for the favicon if needed.
        .header(
            "content-security-policy",
            "default-src 'self'; \
             script-src 'self'; \
             style-src 'self'; \
             img-src 'self' data:; \
             connect-src 'self'; \
             frame-ancestors 'none'",
        )
        .header("x-content-type-options", "nosniff")
        .header("referrer-policy", "same-origin")
        .body(html)
        .unwrap_or_else(|_| {
            Response::builder()
                .status(500)
                .body("layout build failed".to_string())
                .unwrap()
        })
        .into_response()
}

/// Render an arbitrary HTTP status as a tiny retro-styled HTML page. Used by
/// the read handlers for 404s and the soft "tier denied" page.
pub fn status_page(status: u16, title: &str, body_html: &str) -> warp::reply::Response {
    let mut resp = page(title, body_html, "no-store");
    *resp.status_mut() = warp::http::StatusCode::from_u16(status)
        .unwrap_or(warp::http::StatusCode::INTERNAL_SERVER_ERROR);
    resp
}

/// Map a node's freshness state to one of the four CSS classes used by
/// the staleness border encoding (see Aesthetic spec, §Staleness Border
/// Encoding). For the V1 cut we don't yet have a precomputed `state` column
/// on `pyramid_nodes`, so we use a coarse heuristic:
///
/// - `superseded_by` set                                    → `"stale"`
/// - `distilled` empty / `"<gap>"`                          → `"gap"`
/// - everything else                                        → `"verified"`
///
/// WS-I (ETag + revision sourcing) will replace this with the real
/// staleness state once `pyramid_slugs.updated_at` is wired through.
pub fn node_state_class(node: &PyramidNode) -> &'static str {
    if node.superseded_by.is_some() {
        return "stale";
    }
    let trimmed = node.distilled.trim();
    if trimmed.is_empty() || trimmed == "<gap>" {
        return "gap";
    }
    "verified"
}

/// Render the per-node provenance footer (Pillar 14).
///
/// `wire_handle`: when `Some` and non-empty, render as the published Wire
/// handle path. Otherwise fall back to `local:<id>` so unpublished nodes
/// still display a stable identifier. Per Pillar 14, handle-paths ARE the
/// human interface — when present, prefer them.
pub fn prov_footer(node: &PyramidNode, wire_handle: Option<&str>) -> String {
    let depth = node.depth;
    let path = match wire_handle {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => format!("local:{}", node.id),
    };
    format!(
        "<footer class=\"prov\">depth={depth} \u{2022} path={path}</footer>",
        depth = depth,
        path = esc(&path),
    )
}
