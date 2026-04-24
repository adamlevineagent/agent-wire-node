//! Public HTML auth, CSRF, and anonymous-session machinery (post-agents-retro WS-A).
//!
//! This module is the auth contract layer for the public `/p/` web surface.
//! It is intentionally self-contained: it does NOT modify `pyramid/routes.rs`
//! and does NOT depend on any other Phase 1 workstream's files. WS-E will
//! land `web_sessions::lookup` for the Supabase-backed session path; until
//! then a `wire_session` cookie falls through to Anonymous (see resolution
//! order in `with_public_or_session_auth`).
//!
//! Pillar 13 (identity discipline): `PublicAuthSource::WebSession.user_id`
//! is a Supabase user id. It is NEVER a Wire `operator_id`. Do not pass it
//! into anything that expects an operator_id (billing, JWT mint, contribution
//! authorship, etc.). Per v3.3 contract C5.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use warp::Filter;

use crate::http_utils::ct_eq;
use crate::pyramid::PyramidState;

// ── Cookie names (B12) ──────────────────────────────────────────────────

pub const ANON_SESSION_COOKIE: &str = "anon_session";
pub const WIRE_SESSION_COOKIE: &str = "wire_session";

const ANON_SESSION_MAX_AGE: u64 = 3_600; // 1 hour
const WIRE_SESSION_MAX_AGE: u64 = 604_800; // 7 days

/// Synthetic supabase_user_id prefix used by owner-mode sessions.
/// `web_sessions` rows whose `supabase_user_id` starts with this prefix
/// are recognized by `with_public_or_session_auth` as the local operator
/// (full LocalOperator privileges, billing-exempt). Created via the
/// `pyramid_open_web_as_owner` Tauri IPC command.
pub const LOCAL_OPERATOR_SENTINEL_PREFIX: &str = "__local_operator__:";

// ── PublicAuthSource (A4 + B9 + C5) ─────────────────────────────────────

/// Identity that resolved a request to the public `/p/` surface.
///
/// Pillar 13 hard rule: `WebSession.user_id` is a **Supabase** id, NEVER
/// a Wire `operator_id`. The two id spaces are disjoint and must never be
/// crossed. If a handler needs to bill, mint a Wire JWT, write a
/// contribution, or otherwise act as a Wire identity, it MUST use a
/// `WireOperator`/`LocalOperator` arm — not `WebSession`.
#[derive(Debug, Clone)]
pub enum PublicAuthSource {
    /// No cookie or first-touch visitor; tracked only by an opaque
    /// `client_key` derived from peer addr / forwarded headers (B5).
    Anonymous { client_key: String },
    /// Logged-in via Supabase auth on the public surface. The
    /// `anon_session_token` is the original anonymous cookie value, kept
    /// so handlers can stitch pre-login activity to the post-login user.
    WebSession {
        /// Supabase user id. NEVER a Wire operator_id (Pillar 13).
        user_id: String,
        email: String,
        anon_session_token: String,
    },
    /// Authenticated via the local desktop auth_token. Free, billing-exempt.
    LocalOperator,
    /// Authenticated via a Wire JWT (Bearer). Subject to billing /
    /// rate-limit / circle scoping in handlers.
    WireOperator {
        operator_id: String,
        circle_id: Option<String>,
    },
}

// ── client_key (B5: loopback trust gate) ────────────────────────────────

/// Derive an opaque client key for an anonymous visitor.
///
/// The trust gate is the peer socket: forwarded headers (`cf-connecting-ip`,
/// `x-forwarded-for`) are ONLY honored when the immediate peer is on a
/// loopback interface — i.e. behind a known local reverse proxy / tunnel.
/// Otherwise we use the raw peer ip and ignore any forged forwarded headers.
pub fn client_key(headers: &warp::http::HeaderMap, peer: Option<std::net::SocketAddr>) -> String {
    let peer_is_loopback = peer.map(|p| p.ip().is_loopback()).unwrap_or(false);
    if peer_is_loopback {
        if let Some(v) = headers
            .get("cf-connecting-ip")
            .and_then(|h| h.to_str().ok())
        {
            return v.to_string();
        }
        if let Some(v) = headers.get("x-forwarded-for").and_then(|h| h.to_str().ok()) {
            if let Some(first) = v.split(',').next() {
                return first.trim().to_string();
            }
        }
    }
    peer.map(|p| p.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

// ── HMAC-SHA256 (manual; sha2 is in Cargo.toml, hmac is not) ────────────

const HMAC_BLOCK_SIZE: usize = 64; // SHA-256 block size

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    // Normalize key to block size.
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

// ── CSRF nonce (A7) ─────────────────────────────────────────────────────

fn epoch_minute_div5() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 60 / 5)
        .unwrap_or(0)
}

fn csrf_nonce_at(secret: &[u8; 32], session_token: &str, slug: &str, window: u64) -> String {
    let msg = format!("{}:{}:{}", session_token, slug, window);
    let mac = hmac_sha256(secret, msg.as_bytes());
    hex::encode(mac)
}

/// Generate a CSRF nonce bound to (session_token, slug, current 5-minute window).
///
/// `session_token` is the `wire_session` cookie value for an authenticated
/// web session, or the `anon_session` cookie value for an anonymous visitor.
pub fn csrf_nonce(secret: &[u8; 32], session_token: &str, slug: &str) -> String {
    csrf_nonce_at(secret, session_token, slug, epoch_minute_div5())
}

/// Constant-time CSRF verification accepting the current OR previous
/// 5-minute window (so a nonce minted near a boundary still passes).
pub fn verify_csrf(secret: &[u8; 32], nonce: &str, session_token: &str, slug: &str) -> bool {
    let window = epoch_minute_div5();
    let cur = csrf_nonce_at(secret, session_token, slug, window);
    if ct_eq(nonce, &cur) {
        return true;
    }
    let prev = csrf_nonce_at(secret, session_token, slug, window.saturating_sub(1));
    ct_eq(nonce, &prev)
}

// ── Cookie helpers (B12) ────────────────────────────────────────────────

/// Read a single cookie value by name from a `Cookie` header.
pub fn read_cookie(headers: &warp::http::HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get("cookie").and_then(|h| h.to_str().ok())?;
    for part in raw.split(';') {
        let trimmed = part.trim();
        if let Some(eq) = trimmed.find('=') {
            let (k, v) = trimmed.split_at(eq);
            if k == name {
                return Some(v[1..].to_string());
            }
        }
    }
    None
}

fn random_token_hex() -> String {
    let a = *uuid::Uuid::new_v4().as_bytes();
    let b = *uuid::Uuid::new_v4().as_bytes();
    let mut buf = [0u8; 32];
    buf[..16].copy_from_slice(&a);
    buf[16..].copy_from_slice(&b);
    hex::encode(buf)
}

/// Mint a fresh anon-session token + the matching `Set-Cookie` header value.
pub fn issue_anon_session_cookie() -> (String, String) {
    let token = random_token_hex();
    let header = format!(
        "{name}={value}; HttpOnly; Secure; SameSite=Lax; Path=/p/; Max-Age={max_age}",
        name = ANON_SESSION_COOKIE,
        value = token,
        max_age = ANON_SESSION_MAX_AGE,
    );
    (token, header)
}

/// Build a `Set-Cookie` header value for an authenticated wire session.
pub fn issue_wire_session_cookie(token: &str) -> String {
    format!(
        "{name}={value}; HttpOnly; Secure; SameSite=Lax; Path=/p/; Max-Age={max_age}",
        name = WIRE_SESSION_COOKIE,
        value = token,
        max_age = WIRE_SESSION_MAX_AGE,
    )
}

/// Build a `Set-Cookie` header that clears the wire-session cookie (logout).
pub fn clear_wire_session_cookie() -> String {
    format!(
        "{name}=; HttpOnly; Secure; SameSite=Lax; Path=/p/; Max-Age=0",
        name = WIRE_SESSION_COOKIE,
    )
}

// ── with_public_or_session_auth (A4) ────────────────────────────────────

/// Resolve the public-surface identity of a request.
///
/// Resolution order (first match wins):
///   1. `Authorization: Bearer <token>` →
///        - matches local `auth_token` → `LocalOperator`
///        - looks like an Ed25519 JWT (two dots) and verifies → `WireOperator`
///   2. `Cookie: wire_session=<opaque>` → WS-E will resolve via Supabase.
///      Until WS-E lands, this falls through to Anonymous (the cookie is
///      treated as if it weren't there). The `wire_session` cookie value is
///      still echoed into the anonymous client_key path, so CSRF nonces
///      remain stable across the rollout.
///   3. Otherwise: read or implicitly issue an `anon_session` cookie →
///      `Anonymous { client_key }`. (Cookie issuance happens in the handler
///      that builds the response; this filter only resolves identity.)
pub fn with_public_or_session_auth(
    state: Arc<PyramidState>,
    jwt_public_key: Arc<tokio::sync::RwLock<String>>,
) -> impl Filter<Extract = (PublicAuthSource,), Error = warp::Rejection> + Clone {
    warp::header::headers_cloned()
        .and(warp::filters::addr::remote())
        .and(warp::any().map(move || state.clone()))
        .and(warp::any().map(move || jwt_public_key.clone()))
        .and_then(
            |headers: warp::http::HeaderMap,
             peer: Option<std::net::SocketAddr>,
             state: Arc<PyramidState>,
             jwt_pk: Arc<tokio::sync::RwLock<String>>| async move {
                // 1. Authorization: Bearer ...
                if let Some(auth_header) =
                    headers.get("authorization").and_then(|h| h.to_str().ok())
                {
                    if let Some(token) = auth_header.strip_prefix("Bearer ") {
                        // Local auth_token first.
                        let local_token = {
                            let cfg = state.config.read().await;
                            cfg.auth_token.clone()
                        };
                        if !local_token.is_empty() && ct_eq(token, &local_token) {
                            return Ok::<_, warp::Rejection>(PublicAuthSource::LocalOperator);
                        }

                        // Wire JWT: header.payload.signature → two dots.
                        if token.matches('.').count() == 2 {
                            let pk_str = jwt_pk.read().await;
                            if !pk_str.is_empty() {
                                if let Ok(claims) =
                                    crate::server::verify_pyramid_query_jwt(token, &pk_str)
                                {
                                    let operator_id = claims.operator_id.unwrap_or_default();
                                    let circle_id = claims.circle_id;
                                    return Ok(PublicAuthSource::WireOperator {
                                        operator_id,
                                        circle_id,
                                    });
                                }
                            }
                        }
                        // Bearer present but unparseable → fall through to
                        // anonymous rather than 401, since /p/ is a public
                        // surface and a bad header should not break browsing.
                    }
                }

                // 2. wire_session cookie → WS-E lookup (Supabase-backed).
                if let Some(wire_token) = read_cookie(&headers, WIRE_SESSION_COOKIE) {
                    if !wire_token.is_empty() {
                        let session_opt = {
                            let conn = state.reader.lock().await;
                            crate::pyramid::public_html::web_sessions::lookup(&conn, &wire_token)
                                .ok()
                                .flatten()
                        };
                        if let Some(sess) = session_opt {
                            // Owner-mode sentinel: web_sessions rows minted by
                            // the desktop app's "Open as owner" Tauri command
                            // carry a synthetic supabase_user_id prefixed with
                            // `__local_operator__:`. They map to LocalOperator
                            // (full operator privileges, billing-exempt).
                            // See pyramid_open_web_as_owner in main.rs.
                            if sess
                                .supabase_user_id
                                .starts_with(LOCAL_OPERATOR_SENTINEL_PREFIX)
                            {
                                return Ok(PublicAuthSource::LocalOperator);
                            }
                            let anon_tok =
                                read_cookie(&headers, ANON_SESSION_COOKIE).unwrap_or_default();
                            return Ok(PublicAuthSource::WebSession {
                                user_id: sess.supabase_user_id,
                                email: sess.email,
                                anon_session_token: anon_tok,
                            });
                        }
                        // Cookie present but session expired/unknown → fall
                        // through to Anonymous (do not 401: /p/ is public).
                    }
                }

                // 3. anon_session cookie (read existing or implicitly empty;
                //    issuance is the handler's job so it can attach a
                //    Set-Cookie header on the response).
                let _ = read_cookie(&headers, ANON_SESSION_COOKIE);
                let key = client_key(&headers, peer);
                Ok(PublicAuthSource::Anonymous { client_key: key })
            },
        )
}

// ── enforce_public_tier (A4 + Pillar 25) ────────────────────────────────

/// Reason a public-tier check rejected a request. The route layer turns
/// every variant into a 404 (per Pillar 25: never reveal embargo state to
/// the public surface).
#[derive(Debug, Clone)]
pub enum TierRejection {
    /// Slug is non-public and the caller is anonymous / web-session only.
    NotPublic,
    /// Caller is authenticated to Wire but not in an allowed circle.
    NotInCircle,
    /// Slug doesn't exist (or DB read failed). Surfaces as 404 too.
    Unknown,
}

/// Decide whether `auth` is allowed to view `slug` on the public surface.
///
/// Tier semantics (per v3.3 A4 + Pillar 25):
/// - `public`         → everyone allowed
/// - `priced`         → Anonymous/WebSession → 404; Wire/Local → allow
///                      (paywall UI is rendered server-side, not gated here)
/// - `circle-scoped`  → Anonymous/WebSession → 404; Wire/Local → allow
///                      (strict circle_id matching is deferred to B9 plumbing;
///                      for Phase 1 we are deliberately loose here)
/// - `embargoed`      → Anonymous/WebSession → 404; Wire/Local → allow
///                      (the existing 451 path remains for the JSON API)
pub async fn enforce_public_tier(
    state: &PyramidState,
    slug: &str,
    auth: &PublicAuthSource,
) -> Result<(), TierRejection> {
    let (tier, allowed_circles_json) = {
        let conn = state.reader.lock().await;
        match crate::pyramid::db::get_access_tier(&conn, slug) {
            Ok((t, _price, circles)) => (t, circles),
            Err(_) => return Err(TierRejection::Unknown),
        }
    };

    match tier.as_str() {
        "public" => Ok(()),
        "priced" | "embargoed" => match auth {
            PublicAuthSource::LocalOperator | PublicAuthSource::WireOperator { .. } => Ok(()),
            _ => Err(TierRejection::NotPublic),
        },
        "circle-scoped" => match auth {
            PublicAuthSource::LocalOperator => Ok(()),
            PublicAuthSource::WireOperator { circle_id, .. } => {
                // Membership match: parse the JSON list of allowed circle
                // UUIDs from `pyramid_slugs.allowed_circles`. If the list is
                // unset (NULL/empty/parse-fail), allow any WireOperator —
                // operators reading their own pyramids without an explicit
                // circle restriction shouldn't be locked out. If the list is
                // present and non-empty, the operator's circle_id must match
                // (case-insensitive) one of the entries.
                let entries: Vec<String> = allowed_circles_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                    .unwrap_or_default();
                if entries.is_empty() {
                    return Ok(());
                }
                let op_circle = circle_id.as_deref().unwrap_or("").to_ascii_lowercase();
                if op_circle.is_empty() {
                    return Err(TierRejection::NotInCircle);
                }
                let hit = entries
                    .iter()
                    .any(|c| c.trim().to_ascii_lowercase() == op_circle);
                if hit {
                    Ok(())
                } else {
                    Err(TierRejection::NotInCircle)
                }
            }
            _ => Err(TierRejection::NotInCircle),
        },
        // Unknown tier strings: treat as embargoed (most restrictive).
        _ => match auth {
            PublicAuthSource::LocalOperator | PublicAuthSource::WireOperator { .. } => Ok(()),
            _ => Err(TierRejection::NotPublic),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_sha256_known_vector() {
        // RFC 4231 Test Case 1: key = 0x0b * 20, data = "Hi There"
        let key = [0x0bu8; 20];
        let mac = hmac_sha256(&key, b"Hi There");
        let expected =
            hex::decode("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
                .unwrap();
        assert_eq!(mac.to_vec(), expected);
    }

    #[test]
    fn csrf_nonce_round_trip() {
        let secret = [42u8; 32];
        let n = csrf_nonce(&secret, "tok", "slug");
        assert!(verify_csrf(&secret, &n, "tok", "slug"));
        assert!(!verify_csrf(&secret, &n, "tok", "other-slug"));
        assert!(!verify_csrf(&secret, &n, "other-tok", "slug"));
        assert!(!verify_csrf(&secret, "deadbeef", "tok", "slug"));
    }

    #[test]
    fn read_cookie_parses_multi() {
        let mut h = warp::http::HeaderMap::new();
        h.insert(
            "cookie",
            "anon_session=abc; wire_session=xyz; foo=bar"
                .parse()
                .unwrap(),
        );
        assert_eq!(read_cookie(&h, "anon_session"), Some("abc".to_string()));
        assert_eq!(read_cookie(&h, "wire_session"), Some("xyz".to_string()));
        assert_eq!(read_cookie(&h, "missing"), None);
    }

    #[test]
    fn client_key_loopback_trust_gate() {
        use std::net::SocketAddr;
        let mut h = warp::http::HeaderMap::new();
        h.insert("cf-connecting-ip", "1.2.3.4".parse().unwrap());
        // Non-loopback peer: spoofed header MUST be ignored.
        let non_lb: SocketAddr = "192.168.1.5:50000".parse().unwrap();
        assert_eq!(client_key(&h, Some(non_lb)), "192.168.1.5");
        // Loopback peer: header IS honored.
        let lb: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        assert_eq!(client_key(&h, Some(lb)), "1.2.3.4");
        // x-forwarded-for fallback (loopback only, first entry).
        let mut h2 = warp::http::HeaderMap::new();
        h2.insert("x-forwarded-for", "5.6.7.8, 9.9.9.9".parse().unwrap());
        assert_eq!(client_key(&h2, Some(lb)), "5.6.7.8");
        assert_eq!(client_key(&h2, Some(non_lb)), "192.168.1.5");
        // No peer at all → "unknown".
        assert_eq!(client_key(&warp::http::HeaderMap::new(), None), "unknown");
    }

    #[test]
    fn cookie_helpers_have_required_attrs() {
        let wire = issue_wire_session_cookie("abc");
        assert!(wire.contains("HttpOnly"));
        assert!(wire.contains("Secure"));
        assert!(wire.contains("SameSite=Lax"));
        assert!(wire.contains("Path=/p/"));
        assert!(wire.contains("Max-Age=604800"));
        let cleared = clear_wire_session_cookie();
        assert!(cleared.contains("Max-Age=0"));
        assert!(cleared.contains("Path=/p/"));
    }

    #[test]
    fn read_cookie_handles_malformed() {
        let mut h = warp::http::HeaderMap::new();
        // Cookie part with no '=': must not panic.
        h.insert("cookie", "garbage; anon_session=ok".parse().unwrap());
        assert_eq!(read_cookie(&h, "anon_session"), Some("ok".to_string()));
        assert_eq!(read_cookie(&h, "garbage"), None);
    }

    #[test]
    fn issue_anon_cookie_has_attrs() {
        let (token, header) = issue_anon_session_cookie();
        assert_eq!(token.len(), 64);
        assert!(header.contains("HttpOnly"));
        assert!(header.contains("Secure"));
        assert!(header.contains("SameSite=Lax"));
        assert!(header.contains("Path=/p/"));
        assert!(header.contains("Max-Age=3600"));
    }
}
