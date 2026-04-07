//! OTP login bridge for the public `/p/` web surface (post-agents-retro WS-E).
//!
//! Routes:
//!   GET  /p/{slug}/_login   → email entry form
//!   POST /p/{slug}/_login   → request OTP via Supabase, render OTP entry form
//!   POST /p/{slug}/_verify  → verify OTP, mint web_session, set cookie, redirect
//!   POST /p/{slug}/_logout  → delete web_session, clear cookie, redirect
//!
//! Reuses `crate::auth::send_magic_link` and `crate::auth::verify_otp`
//! UNCHANGED. Source for `supabase_url` / `supabase_anon_key` is the
//! `PyramidState` fields populated at startup from `WireNodeConfig`.
//!
//! Pillar 13: the resulting `web_sessions.supabase_user_id` is a Supabase
//! id, NEVER a Wire operator_id.

use std::collections::HashMap;
use std::sync::Arc;

use warp::Filter;
use warp::filters::BoxedFilter;
use warp::http::{Response, StatusCode, header};

use crate::pyramid::PyramidState;
use crate::pyramid::public_html::auth::{
    ANON_SESSION_COOKIE, WIRE_SESSION_COOKIE, PublicAuthSource, clear_wire_session_cookie,
    client_key, csrf_nonce, issue_anon_session_cookie, issue_wire_session_cookie, read_cookie,
    verify_csrf,
};
use crate::pyramid::public_html::rate_limit;
use crate::pyramid::public_html::web_sessions;

/// Validate that a slug only contains characters safe for both DB lookup and
/// for redirect Location headers / HTML attribute interpolation. Reserved
/// `_*` slugs are intentionally rejected too — those are infra paths, not
/// pyramid slugs (A9 / B11). Length cap mirrors slug constraints elsewhere.
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

fn bad_slug_page() -> Response<String> {
    let body = page(
        "Not found",
        "<h1>Not found</h1><p>Unknown pyramid.</p>",
    );
    html_response(StatusCode::NOT_FOUND, body)
}

fn rate_limited_page(retry_after: u64) -> Response<String> {
    let body = page(
        "Slow down",
        &format!(
            "<h1>Slow down</h1><p class=\"err\">Too many login attempts. \
             Try again in {}s.</p>",
            retry_after
        ),
    );
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header("Retry-After", retry_after.to_string())
        .body(body)
        .unwrap()
}

// ── small HTML helpers ──────────────────────────────────────────────────

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn html_response(status: StatusCode, body: String) -> Response<String> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(body)
        .unwrap()
}

fn page(title: &str, inner: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title}</title>\
         <style>body{{font-family:system-ui,sans-serif;max-width:480px;margin:4em auto;padding:0 1em;color:#222}}\
         h1{{font-size:1.4em}}input{{font-size:1em;padding:.5em;width:100%;box-sizing:border-box;margin:.25em 0 1em}}\
         button{{font-size:1em;padding:.6em 1.2em;cursor:pointer}}\
         .err{{color:#a00;margin:1em 0}}.muted{{color:#666;font-size:.9em}}</style>\
         </head><body>{inner}</body></html>",
        title = html_escape(title),
        inner = inner,
    )
}

fn not_configured_page() -> Response<String> {
    let body = page(
        "Login unavailable",
        "<h1>Login unavailable</h1>\
         <p>OTP login is not configured on this node — contact the operator.</p>",
    );
    html_response(StatusCode::SERVICE_UNAVAILABLE, body)
}

fn error_page(slug: &str, message: &str) -> Response<String> {
    let body = page(
        "Login error",
        &format!(
            "<h1>Login error</h1><p class=\"err\">{}</p>\
             <p><a href=\"/p/{}/_login\">Try again</a></p>",
            html_escape(message),
            html_escape(slug),
        ),
    );
    html_response(StatusCode::BAD_REQUEST, body)
}

fn redirect_to(slug: &str, set_cookie: Option<String>) -> Response<String> {
    let mut b = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, format!("/p/{}", slug));
    if let Some(c) = set_cookie {
        b = b.header(header::SET_COOKIE, c);
    }
    b.body(String::new()).unwrap()
}

// ── Identity / cookie helpers ────────────────────────────────────────────

/// Read the current anon_session cookie or mint one. Returns
/// `(token, Option<set_cookie_header>)`. The Set-Cookie should be attached
/// to the outgoing response if Some.
fn ensure_anon_session(headers: &warp::http::HeaderMap) -> (String, Option<String>) {
    if let Some(t) = read_cookie(headers, ANON_SESSION_COOKIE) {
        return (t, None);
    }
    let (t, set) = issue_anon_session_cookie();
    (t, Some(set))
}

fn supabase_creds(state: &PyramidState) -> Option<(String, String)> {
    match (state.supabase_url.as_ref(), state.supabase_anon_key.as_ref()) {
        (Some(u), Some(k)) if !u.is_empty() && !k.is_empty() => Some((u.clone(), k.clone())),
        _ => None,
    }
}

// ── HTML form bodies ────────────────────────────────────────────────────

fn email_form_html(slug: &str, csrf: &str, error: Option<&str>) -> String {
    let err_block = error
        .map(|e| format!("<p class=\"err\">{}</p>", html_escape(e)))
        .unwrap_or_default();
    let inner = format!(
        "<h1>Sign in</h1>\
         {err_block}\
         <form method=\"post\" action=\"/p/{slug}/_login\">\
           <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\
           <label>Email<input type=\"email\" name=\"email\" required autofocus></label>\
           <button type=\"submit\">Send code</button>\
         </form>\
         <p class=\"muted\">We will email you a 6-digit code.</p>",
        slug = html_escape(slug),
        csrf = html_escape(csrf),
    );
    page("Sign in", &inner)
}

fn otp_form_html(slug: &str, email: &str, csrf: &str, error: Option<&str>) -> String {
    let err_block = error
        .map(|e| format!("<p class=\"err\">{}</p>", html_escape(e)))
        .unwrap_or_default();
    let inner = format!(
        "<h1>Enter code</h1>\
         <p class=\"muted\">Code sent to {email}.</p>\
         {err_block}\
         <form method=\"post\" action=\"/p/{slug}/_verify\">\
           <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\
           <input type=\"hidden\" name=\"email\" value=\"{email}\">\
           <label>6-digit code<input type=\"text\" name=\"otp\" inputmode=\"numeric\" \
             pattern=\"[0-9]*\" maxlength=\"6\" required autofocus></label>\
           <button type=\"submit\">Verify</button>\
         </form>\
         <p><a href=\"/p/{slug}/_login\">Use a different email</a></p>",
        slug = html_escape(slug),
        email = html_escape(email),
        csrf = html_escape(csrf),
    );
    page("Enter code", &inner)
}

// ── Handlers ────────────────────────────────────────────────────────────

async fn handle_login_get(
    slug: String,
    headers: warp::http::HeaderMap,
    state: Arc<PyramidState>,
) -> Result<Response<String>, warp::Rejection> {
    if !slug_is_safe(&slug) {
        return Ok(bad_slug_page());
    }
    if supabase_creds(&state).is_none() {
        return Ok(not_configured_page());
    }
    let (anon_tok, set_cookie) = ensure_anon_session(&headers);
    let nonce = csrf_nonce(&state.csrf_secret, &anon_tok, &slug);
    let body = email_form_html(&slug, &nonce, None);
    let mut resp = html_response(StatusCode::OK, body);
    if let Some(c) = set_cookie {
        resp.headers_mut()
            .insert(header::SET_COOKIE, c.parse().unwrap());
    }
    Ok(resp)
}

async fn handle_login_post(
    slug: String,
    peer: Option<std::net::SocketAddr>,
    headers: warp::http::HeaderMap,
    form: HashMap<String, String>,
    state: Arc<PyramidState>,
) -> Result<Response<String>, warp::Rejection> {
    if !slug_is_safe(&slug) {
        return Ok(bad_slug_page());
    }
    let (su_url, su_key) = match supabase_creds(&state) {
        Some(v) => v,
        None => return Ok(not_configured_page()),
    };
    let (anon_tok, set_cookie) = ensure_anon_session(&headers);

    // Per-client login bucket (B6: 3/min). Always applies — pre-auth.
    let rl = rate_limit::global();
    let ck = client_key(&headers, peer);
    let anon_principal = PublicAuthSource::Anonymous { client_key: ck };
    if let Err(e) = rate_limit::check_for_login(&rl, &anon_principal).await {
        return Ok(rate_limited_page(e.retry_after));
    }

    let csrf = form.get("csrf").map(String::as_str).unwrap_or("");
    if !verify_csrf(&state.csrf_secret, csrf, &anon_tok, &slug) {
        return Ok(error_page(&slug, "Session expired — please try again."));
    }
    let email = form.get("email").map(String::as_str).unwrap_or("").trim();
    if email.is_empty() || !email.contains('@') {
        let nonce = csrf_nonce(&state.csrf_secret, &anon_tok, &slug);
        let body = email_form_html(&slug, &nonce, Some("Please enter a valid email."));
        let mut resp = html_response(StatusCode::OK, body);
        if let Some(c) = set_cookie {
            resp.headers_mut()
                .insert(header::SET_COOKIE, c.parse().unwrap());
        }
        return Ok(resp);
    }

    // Per-target-email bucket (B6: 10/hour). Mailbomb defense.
    if let Err(e) = rate_limit::check_email_bucket(&rl, email).await {
        return Ok(rate_limited_page(e.retry_after));
    }

    match crate::auth::send_magic_link(&su_url, &su_key, email, 0).await {
        Ok(()) => {
            let nonce = csrf_nonce(&state.csrf_secret, &anon_tok, &slug);
            let body = otp_form_html(&slug, email, &nonce, None);
            let mut resp = html_response(StatusCode::OK, body);
            if let Some(c) = set_cookie {
                resp.headers_mut()
                    .insert(header::SET_COOKIE, c.parse().unwrap());
            }
            Ok(resp)
        }
        Err(e) => {
            tracing::warn!("send_magic_link failed: {}", e);
            Ok(error_page(
                &slug,
                "Could not send code — please try again later.",
            ))
        }
    }
}

async fn handle_verify_post(
    slug: String,
    headers: warp::http::HeaderMap,
    form: HashMap<String, String>,
    state: Arc<PyramidState>,
) -> Result<Response<String>, warp::Rejection> {
    if !slug_is_safe(&slug) {
        return Ok(bad_slug_page());
    }
    let (su_url, su_key) = match supabase_creds(&state) {
        Some(v) => v,
        None => return Ok(not_configured_page()),
    };
    let anon_tok = read_cookie(&headers, ANON_SESSION_COOKIE).unwrap_or_default();
    let csrf = form.get("csrf").map(String::as_str).unwrap_or("");
    if anon_tok.is_empty() || !verify_csrf(&state.csrf_secret, csrf, &anon_tok, &slug) {
        return Ok(error_page(&slug, "Session expired — please try again."));
    }

    let email = form.get("email").map(String::as_str).unwrap_or("").trim();
    let otp = form.get("otp").map(String::as_str).unwrap_or("").trim();
    if email.is_empty() || otp.is_empty() {
        return Ok(error_page(&slug, "Missing email or code."));
    }

    match crate::auth::verify_otp(&su_url, &su_key, email, otp).await {
        Ok(auth_state) => {
            let user_id = auth_state.user_id.unwrap_or_default();
            let resolved_email = auth_state.email.unwrap_or_else(|| email.to_string());
            if user_id.is_empty() {
                return Ok(error_page(&slug, "Verification returned no user."));
            }
            // Insert web_session row.
            let token = {
                let conn = state.writer.lock().await;
                match web_sessions::create(&conn, &user_id, &resolved_email, 604_800) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!("web_sessions::create failed: {}", e);
                        return Ok(error_page(&slug, "Could not save session."));
                    }
                }
            };
            let cookie = issue_wire_session_cookie(&token);
            Ok(redirect_to(&slug, Some(cookie)))
        }
        Err(e) => {
            tracing::warn!("verify_otp failed: {}", e);
            Ok(error_page(
                &slug,
                "Code did not verify — please try again.",
            ))
        }
    }
}

async fn handle_logout_post(
    slug: String,
    headers: warp::http::HeaderMap,
    form: HashMap<String, String>,
    state: Arc<PyramidState>,
) -> Result<Response<String>, warp::Rejection> {
    if !slug_is_safe(&slug) {
        return Ok(bad_slug_page());
    }
    let wire_tok = read_cookie(&headers, WIRE_SESSION_COOKIE).unwrap_or_default();
    let csrf = form.get("csrf").map(String::as_str).unwrap_or("");
    // CSRF on logout is bound to the wire_session cookie.
    if !wire_tok.is_empty() && verify_csrf(&state.csrf_secret, csrf, &wire_tok, &slug) {
        let conn = state.writer.lock().await;
        let _ = web_sessions::delete(&conn, &wire_tok);
    }
    Ok(redirect_to(&slug, Some(clear_wire_session_cookie())))
}

// ── Filter assembly ─────────────────────────────────────────────────────

const FORM_BODY_LIMIT: u64 = 8 * 1024;

fn with_state(
    state: Arc<PyramidState>,
) -> impl Filter<Extract = (Arc<PyramidState>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || state.clone())
}

/// All four routes for WS-C to mount under /p/{slug}/.
pub fn login_routes(state: Arc<PyramidState>) -> BoxedFilter<(warp::reply::Response,)> {
    let login_get = warp::path!("p" / String / "_login")
        .and(warp::get())
        .and(warp::header::headers_cloned())
        .and(with_state(state.clone()))
        .and_then(handle_login_get)
        .map(|r: Response<String>| r.map(|b| b.into()));

    let login_post = warp::path!("p" / String / "_login")
        .and(warp::post())
        .and(warp::filters::addr::remote())
        .and(warp::header::headers_cloned())
        .and(warp::body::content_length_limit(FORM_BODY_LIMIT))
        .and(warp::body::form::<HashMap<String, String>>())
        .and(with_state(state.clone()))
        .and_then(handle_login_post)
        .map(|r: Response<String>| r.map(|b| b.into()));

    let verify_post = warp::path!("p" / String / "_verify")
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::content_length_limit(FORM_BODY_LIMIT))
        .and(warp::body::form::<HashMap<String, String>>())
        .and(with_state(state.clone()))
        .and_then(handle_verify_post)
        .map(|r: Response<String>| r.map(|b| b.into()));

    let logout_post = warp::path!("p" / String / "_logout")
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::content_length_limit(FORM_BODY_LIMIT))
        .and(warp::body::form::<HashMap<String, String>>())
        .and(with_state(state))
        .and_then(handle_logout_post)
        .map(|r: Response<String>| r.map(|b| b.into()));

    login_get
        .or(login_post)
        .unify()
        .or(verify_post)
        .unify()
        .or(logout_post)
        .unify()
        .boxed()
}
