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

use warp::filters::BoxedFilter;
use warp::http::{header, StatusCode};
use warp::reply::Response as WarpResponse;
use warp::Filter;

use crate::pyramid::public_html::auth::{
    clear_wire_session_cookie, client_key, csrf_nonce, issue_anon_session_cookie,
    issue_wire_session_cookie, read_cookie, verify_csrf, PublicAuthSource, ANON_SESSION_COOKIE,
    WIRE_SESSION_COOKIE,
};
use crate::pyramid::public_html::rate_limit;
use crate::pyramid::public_html::render::{esc, page_with_etag};
use crate::pyramid::public_html::web_sessions;
use crate::pyramid::PyramidState;

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

/// Build a login-page response using `render::page_with_etag` so the global
/// CSP, nosniff, and referrer-policy headers are applied uniformly. All
/// cookie-issuing pages use `no-store`.
fn login_page(title: &str, inner: &str, status: StatusCode) -> WarpResponse {
    let mut resp = page_with_etag(title, inner, "no-store", None, None);
    *resp.status_mut() = status;
    resp
}

fn bad_slug_page() -> WarpResponse {
    login_page(
        "Not found",
        "<h1>Not found</h1><p>Unknown pyramid.</p>",
        StatusCode::NOT_FOUND,
    )
}

fn rate_limited_page(retry_after: u64) -> WarpResponse {
    let body = format!(
        "<h1>Slow down</h1><p class=\"login-error\">Too many login attempts. \
         Try again in {}s.</p>",
        retry_after
    );
    let mut resp = login_page("Slow down", &body, StatusCode::TOO_MANY_REQUESTS);
    if let Ok(hv) = warp::http::HeaderValue::from_str(&retry_after.to_string()) {
        resp.headers_mut().insert("retry-after", hv);
    }
    resp
}

fn not_configured_page() -> WarpResponse {
    login_page(
        "Login unavailable",
        "<h1>Login unavailable</h1>\
         <p>OTP login is not configured on this node — contact the operator.</p>",
        StatusCode::SERVICE_UNAVAILABLE,
    )
}

fn error_page(slug: &str, message: &str) -> WarpResponse {
    let body = format!(
        "<h1>Login error</h1><p class=\"login-error\">{}</p>\
         <p><a href=\"/p/{}/_login\">Try again</a></p>",
        esc(message),
        esc(slug),
    );
    login_page("Login error", &body, StatusCode::BAD_REQUEST)
}

fn redirect_to(slug: &str, set_cookie: Option<String>) -> WarpResponse {
    let mut b = warp::http::Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, format!("/p/{}", slug));
    if let Some(c) = set_cookie {
        b = b.header(header::SET_COOKIE, c);
    }
    b.body(warp::hyper::Body::empty()).unwrap()
}

fn attach_set_cookie(resp: &mut WarpResponse, cookie: &str) {
    if let Ok(hv) = warp::http::HeaderValue::from_str(cookie) {
        resp.headers_mut().append(header::SET_COOKIE, hv);
    }
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
    match (
        state.supabase_url.as_ref(),
        state.supabase_anon_key.as_ref(),
    ) {
        (Some(u), Some(k)) if !u.is_empty() && !k.is_empty() => Some((u.clone(), k.clone())),
        _ => None,
    }
}

// ── HTML form bodies ────────────────────────────────────────────────────

fn email_form_html(slug: &str, csrf: &str, error: Option<&str>) -> String {
    let err_block = error
        .map(|e| format!("<p class=\"login-error\">{}</p>", esc(e)))
        .unwrap_or_default();
    format!(
        "<h1>Sign in</h1>\
         {err_block}\
         <form class=\"login-form\" method=\"post\" action=\"/p/{slug}/_login\">\
           <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\
           <label>Email<input type=\"email\" name=\"email\" required autofocus></label>\
           <button type=\"submit\">Send code</button>\
         </form>\
         <p class=\"login-muted\">We will email you a 6-digit code.</p>",
        slug = esc(slug),
        csrf = esc(csrf),
    )
}

fn otp_form_html(slug: &str, email: &str, csrf: &str, error: Option<&str>) -> String {
    let err_block = error
        .map(|e| format!("<p class=\"login-error\">{}</p>", esc(e)))
        .unwrap_or_default();
    format!(
        "<h1>Enter code</h1>\
         <p class=\"login-muted\">Code sent to {email}.</p>\
         {err_block}\
         <form class=\"login-form\" method=\"post\" action=\"/p/{slug}/_verify\">\
           <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\
           <input type=\"hidden\" name=\"email\" value=\"{email}\">\
           <label>6-digit code<input type=\"text\" name=\"otp\" inputmode=\"numeric\" \
             pattern=\"[0-9]*\" maxlength=\"6\" required autofocus></label>\
           <button type=\"submit\">Verify</button>\
         </form>\
         <p><a href=\"/p/{slug}/_login\">Use a different email</a></p>",
        slug = esc(slug),
        email = esc(email),
        csrf = esc(csrf),
    )
}

// ── Handlers ────────────────────────────────────────────────────────────

async fn handle_login_get(
    slug: String,
    headers: warp::http::HeaderMap,
    state: Arc<PyramidState>,
) -> Result<WarpResponse, warp::Rejection> {
    if !slug_is_safe(&slug) {
        return Ok(bad_slug_page());
    }
    if supabase_creds(&state).is_none() {
        return Ok(not_configured_page());
    }
    let (anon_tok, set_cookie) = ensure_anon_session(&headers);
    let nonce = csrf_nonce(&state.csrf_secret, &anon_tok, &slug);
    let body = email_form_html(&slug, &nonce, None);
    let mut resp = login_page("Sign in", &body, StatusCode::OK);
    if let Some(c) = set_cookie {
        attach_set_cookie(&mut resp, &c);
    }
    Ok(resp)
}

async fn handle_login_post(
    slug: String,
    peer: Option<std::net::SocketAddr>,
    headers: warp::http::HeaderMap,
    form: HashMap<String, String>,
    state: Arc<PyramidState>,
) -> Result<WarpResponse, warp::Rejection> {
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
        let mut resp = login_page("Sign in", &body, StatusCode::OK);
        if let Some(c) = set_cookie {
            attach_set_cookie(&mut resp, &c);
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
            let mut resp = login_page("Enter code", &body, StatusCode::OK);
            if let Some(c) = set_cookie {
                attach_set_cookie(&mut resp, &c);
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
) -> Result<WarpResponse, warp::Rejection> {
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
            Ok(error_page(&slug, "Code did not verify — please try again."))
        }
    }
}

async fn handle_logout_post(
    slug: String,
    headers: warp::http::HeaderMap,
    form: HashMap<String, String>,
    state: Arc<PyramidState>,
) -> Result<WarpResponse, warp::Rejection> {
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
        .and_then(handle_login_get);

    let login_post = warp::path!("p" / String / "_login")
        .and(warp::post())
        .and(warp::filters::addr::remote())
        .and(warp::header::headers_cloned())
        .and(warp::body::content_length_limit(FORM_BODY_LIMIT))
        .and(warp::body::form::<HashMap<String, String>>())
        .and(with_state(state.clone()))
        .and_then(handle_login_post);

    let verify_post = warp::path!("p" / String / "_verify")
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::content_length_limit(FORM_BODY_LIMIT))
        .and(warp::body::form::<HashMap<String, String>>())
        .and(with_state(state.clone()))
        .and_then(handle_verify_post);

    let logout_post = warp::path!("p" / String / "_logout")
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::content_length_limit(FORM_BODY_LIMIT))
        .and(warp::body::form::<HashMap<String, String>>())
        .and(with_state(state.clone()))
        .and_then(handle_logout_post);

    // Owner-mode bridge: GET /p/_owner_login?token=<one-time>&return=<slug>
    // Mints the wire_session cookie from a pre-existing web_sessions row
    // (created by the desktop app's pyramid_open_web_as_owner Tauri IPC).
    // Per A9 the /_ namespace is reserved so this never collides with a slug.
    let owner_login = warp::path!("p" / "_owner_login")
        .and(warp::get())
        .and(warp::query::<HashMap<String, String>>())
        .and(with_state(state))
        .and_then(handle_owner_login);

    login_get
        .or(login_post)
        .unify()
        .or(verify_post)
        .unify()
        .or(logout_post)
        .unify()
        .or(owner_login)
        .unify()
        .boxed()
}

/// GET /p/_owner_login?token=<hex>&return=<slug>
///
/// One-time consume of an owner-mode bridge token. The desktop app
/// has already inserted a row into `web_sessions` with this token,
/// supabase_user_id prefixed with `__local_operator__:`. We just need
/// to set the wire_session cookie pointing at that row and 302 the
/// browser to the requested slug. The auth filter then recognizes the
/// sentinel and treats every subsequent request as LocalOperator.
async fn handle_owner_login(
    params: HashMap<String, String>,
    state: Arc<PyramidState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    use crate::pyramid::public_html::auth::{
        issue_wire_session_cookie, LOCAL_OPERATOR_SENTINEL_PREFIX,
    };
    use crate::pyramid::public_html::web_sessions;

    let token = match params.get("token").map(|s| s.as_str()).unwrap_or("") {
        "" => return Ok(error_page("_owner", "missing token")),
        t => t.to_string(),
    };
    let return_slug = params
        .get("return")
        .and_then(|s| {
            if s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && !s.is_empty()
                && !s.starts_with('_')
            {
                Some(s.clone())
            } else {
                None
            }
        })
        .unwrap_or_default();

    // Verify the token exists in web_sessions and carries the owner sentinel.
    let session_opt = {
        let conn = state.reader.lock().await;
        web_sessions::lookup(&conn, &token).ok().flatten()
    };
    let sess = match session_opt {
        Some(s)
            if s.supabase_user_id
                .starts_with(LOCAL_OPERATOR_SENTINEL_PREFIX) =>
        {
            s
        }
        _ => return Ok(error_page("_owner", "invalid or expired owner token")),
    };
    let _ = sess; // sentinel-only check; cookie value is the token itself

    // Set wire_session cookie + 302 to the target page.
    let target = if return_slug.is_empty() {
        "/p/".to_string()
    } else {
        format!("/p/{}", return_slug)
    };
    let cookie = issue_wire_session_cookie(&token);
    let mut resp = warp::reply::Response::new(warp::hyper::Body::empty());
    *resp.status_mut() = warp::http::StatusCode::FOUND;
    resp.headers_mut().insert(
        "location",
        warp::http::HeaderValue::from_str(&target)
            .unwrap_or_else(|_| warp::http::HeaderValue::from_static("/p/")),
    );
    if let Ok(v) = warp::http::HeaderValue::from_str(&cookie) {
        resp.headers_mut().append("set-cookie", v);
    }
    Ok(resp)
}
