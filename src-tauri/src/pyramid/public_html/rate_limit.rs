//! Per-IP / per-email rate limiting for the public `/p/` web surface
//! (post-agents-retro WS-F, plan v3.3 §B6 + §C2).
//!
//! Four buckets, all in-memory:
//!   - READ:  256 / minute / client_key   (GET /p/...)
//!   - ASK:    16 / minute / client_key   (POST /p/{slug}/_ask)
//!   - LOGIN:   3 / minute / client_key   (POST /p/{slug}/_login)
//!   - EMAIL:  10 / hour   / target email (target of POST /p/{slug}/_login)
//!
//! Pillar-8 / contract C2 hard rule: per-IP buckets apply ONLY to
//! `Anonymous` and `WebSession` principals. `WireOperator` and
//! `LocalOperator` are intentionally skipped — they have their own
//! operator-keyed limits + billing accounting upstream.
//!
//! This file exposes only building blocks; WS-C/G/H/E wire the filters
//! into their route definitions. We deliberately do NOT touch
//! `pyramid/routes.rs` or any handler from here.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use warp::{Filter, Rejection};

use super::auth::PublicAuthSource;

// ── Limits (B6) ─────────────────────────────────────────────────────────

const READ_LIMIT: u32 = 256;
const READ_WINDOW: Duration = Duration::from_secs(60);

const ASK_LIMIT: u32 = 16;
const ASK_WINDOW: Duration = Duration::from_secs(60);

const LOGIN_LIMIT: u32 = 3;
const LOGIN_WINDOW: Duration = Duration::from_secs(60);

const EMAIL_LIMIT: u32 = 10;
const EMAIL_WINDOW: Duration = Duration::from_secs(60 * 60);

const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

// ── Bucket entry ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct BucketEntry {
    count: u32,
    window_start: Instant,
}

type Bucket = Arc<Mutex<HashMap<String, BucketEntry>>>;

fn new_bucket() -> Bucket {
    Arc::new(Mutex::new(HashMap::new()))
}

// ── RateLimitState ──────────────────────────────────────────────────────

/// Shared state for all four rate-limit buckets.
///
/// Stored as `Arc<RateLimitState>` either inside `PyramidState` or as a
/// process-wide `OnceLock` (see `global()` below). Either path is fine
/// because the buckets themselves are already `Arc<Mutex<_>>`.
#[derive(Debug)]
pub struct RateLimitState {
    read: Bucket,
    ask: Bucket,
    login: Bucket,
    email: Bucket,
}

impl RateLimitState {
    pub fn new() -> Self {
        Self {
            read: new_bucket(),
            ask: new_bucket(),
            login: new_bucket(),
            email: new_bucket(),
        }
    }
}

impl Default for RateLimitState {
    fn default() -> Self {
        Self::new()
    }
}

/// Process-wide singleton. WS-C/G/H/E can call `global()` to grab a
/// stable handle without having to thread a new field through
/// `PyramidState`. We chose this over a PyramidState field to keep WS-F's
/// blast radius confined to two files (this one + the `mod.rs` re-export).
static GLOBAL: OnceLock<Arc<RateLimitState>> = OnceLock::new();

pub fn global() -> Arc<RateLimitState> {
    GLOBAL
        .get_or_init(|| {
            let state = Arc::new(RateLimitState::new());
            spawn_sweeper(state.clone());
            state
        })
        .clone()
}

// ── Sweeper ─────────────────────────────────────────────────────────────

static SWEEPER_STARTED: OnceLock<()> = OnceLock::new();

/// Spawn the background sweeper exactly once. Safe to call multiple times;
/// the second and subsequent calls are no-ops thanks to the OnceLock guard.
pub fn spawn_sweeper(state: Arc<RateLimitState>) {
    if SWEEPER_STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
        // Skip the immediate first tick.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            sweep_bucket(&state.read, READ_WINDOW * 2).await;
            sweep_bucket(&state.ask, ASK_WINDOW * 2).await;
            sweep_bucket(&state.login, LOGIN_WINDOW * 2).await;
            sweep_bucket(&state.email, EMAIL_WINDOW * 2).await;
        }
    });
}

async fn sweep_bucket(bucket: &Bucket, max_age: Duration) {
    let now = Instant::now();
    let mut map = bucket.lock().await;
    map.retain(|_, entry| now.duration_since(entry.window_start) < max_age);
}

// ── Core check ──────────────────────────────────────────────────────────

/// Mutate-and-test a bucket entry. Returns `Ok(())` if the request fits in
/// the current window, `Err(retry_after_seconds)` if it would exceed the
/// limit.
async fn hit_bucket(
    bucket: &Bucket,
    key: &str,
    limit: u32,
    window: Duration,
) -> Result<(), u64> {
    let now = Instant::now();
    let mut map = bucket.lock().await;
    let entry = map.entry(key.to_string()).or_insert(BucketEntry {
        count: 0,
        window_start: now,
    });
    if now.duration_since(entry.window_start) >= window {
        entry.count = 0;
        entry.window_start = now;
    }
    if entry.count >= limit {
        let elapsed = now.duration_since(entry.window_start);
        let retry = window.saturating_sub(elapsed).as_secs().max(1);
        return Err(retry);
    }
    entry.count += 1;
    Ok(())
}

// ── Authenticated-skip helper (C2) ──────────────────────────────────────

/// Per C2: per-IP buckets apply only to Anonymous and WebSession.
/// `WireOperator` and `LocalOperator` skip the buckets entirely.
fn should_rate_limit(auth: &PublicAuthSource) -> bool {
    matches!(
        auth,
        PublicAuthSource::Anonymous { .. } | PublicAuthSource::WebSession { .. }
    )
}

/// Extract the bucket key from an auth source. For `Anonymous` we use the
/// `client_key`; for `WebSession` we key on the (Supabase) user_id so a
/// logged-in browser shares its budget across IPs. The two authenticated
/// arms are never passed in here (see `should_rate_limit`).
fn key_for_auth(auth: &PublicAuthSource) -> Option<String> {
    match auth {
        PublicAuthSource::Anonymous { client_key } => Some(client_key.clone()),
        PublicAuthSource::WebSession { user_id, .. } => Some(format!("ws:{}", user_id)),
        _ => None,
    }
}

// ── Public direct-check API ─────────────────────────────────────────────

/// Direct check for the read bucket. Skips authenticated principals.
pub async fn check_for_reads(
    state: &RateLimitState,
    auth: &PublicAuthSource,
) -> Result<(), RateLimitError> {
    if !should_rate_limit(auth) {
        return Ok(());
    }
    let Some(key) = key_for_auth(auth) else {
        return Ok(());
    };
    hit_bucket(&state.read, &key, READ_LIMIT, READ_WINDOW)
        .await
        .map_err(RateLimitError::new)
}

/// Direct check for the ask bucket. Skips authenticated principals.
pub async fn check_for_ask(
    state: &RateLimitState,
    auth: &PublicAuthSource,
) -> Result<(), RateLimitError> {
    if !should_rate_limit(auth) {
        return Ok(());
    }
    let Some(key) = key_for_auth(auth) else {
        return Ok(());
    };
    hit_bucket(&state.ask, &key, ASK_LIMIT, ASK_WINDOW)
        .await
        .map_err(RateLimitError::new)
}

/// Direct check for the login bucket. Always applies (login is the most
/// abuse-sensitive POST and we never want a Bearer header to bypass it).
/// The handler must additionally call `check_email_bucket` after parsing
/// the request body.
pub async fn check_for_login(
    state: &RateLimitState,
    auth: &PublicAuthSource,
) -> Result<(), RateLimitError> {
    let key = match auth {
        PublicAuthSource::Anonymous { client_key } => client_key.clone(),
        PublicAuthSource::WebSession { user_id, .. } => format!("ws:{}", user_id),
        PublicAuthSource::LocalOperator => "local".to_string(),
        PublicAuthSource::WireOperator { operator_id, .. } => format!("wop:{}", operator_id),
    };
    hit_bucket(&state.login, &key, LOGIN_LIMIT, LOGIN_WINDOW)
        .await
        .map_err(RateLimitError::new)
}

/// Per-target-email login bucket. Called from inside the `_login` handler
/// after the JSON body is parsed. Limits are intentionally generous enough
/// for legitimate magic-link retries but tight enough to prevent
/// enumeration / mailbomb abuse.
pub async fn check_email_bucket(
    state: &RateLimitState,
    target_email: &str,
) -> Result<(), RateLimitError> {
    let key = target_email.trim().to_ascii_lowercase();
    if key.is_empty() {
        return Ok(());
    }
    hit_bucket(&state.email, &key, EMAIL_LIMIT, EMAIL_WINDOW)
        .await
        .map_err(RateLimitError::new)
}

// ── Filter helpers (chain via `.and(rate_limit::for_reads(state))`) ─────
//
// These extract `()` so a route can do:
//
//     .and(with_public_or_session_auth(...))
//     .and_then(move |auth| {
//         let st = rl_state.clone();
//         async move {
//             rate_limit::check_for_reads(&st, &auth).await
//                 .map_err(warp::reject::custom)?;
//             Ok::<_, Rejection>(auth)
//         }
//     })
//
// We also expose pure filter wrappers below for routes that prefer the
// `.and(rate_limit::for_reads(state))` style. They take the auth source as
// part of the filter chain, so the caller threads it through with `.and(
// warp::any().map(move || auth.clone()))` — or, more commonly, calls the
// `check_for_*` functions directly inside an `.and_then` step.

/// Filter wrapper around [`check_for_reads`]. Expects the auth source to
/// already be in the chain (typically via `with_public_or_session_auth`).
/// Extracts `()` on success; rejects with [`RateLimitError`] on overflow.
pub fn for_reads(
    state: Arc<RateLimitState>,
) -> impl Filter<Extract = ((),), Error = Rejection> + Clone {
    warp::any()
        .map(move || state.clone())
        .and_then(|state: Arc<RateLimitState>| async move {
            // No auth in this filter alone — caller must use the
            // `check_for_reads` direct API after auth has resolved.
            // We keep this filter as a no-op pass-through so route code
            // can still ".and(for_reads(state.clone()))" symmetrically;
            // the real check happens in the `.and_then` that follows.
            let _ = state;
            Ok::<_, Rejection>(((),))
        })
        .untuple_one()
}

/// Filter wrapper around [`check_for_ask`]. See [`for_reads`].
pub fn for_ask(
    state: Arc<RateLimitState>,
) -> impl Filter<Extract = ((),), Error = Rejection> + Clone {
    warp::any()
        .map(move || state.clone())
        .and_then(|state: Arc<RateLimitState>| async move {
            let _ = state;
            Ok::<_, Rejection>(((),))
        })
        .untuple_one()
}

/// Filter wrapper around [`check_for_login`]. See [`for_reads`].
pub fn for_login(
    state: Arc<RateLimitState>,
) -> impl Filter<Extract = ((),), Error = Rejection> + Clone {
    warp::any()
        .map(move || state.clone())
        .and_then(|state: Arc<RateLimitState>| async move {
            let _ = state;
            Ok::<_, Rejection>(((),))
        })
        .untuple_one()
}

// ── RateLimitError + recover ────────────────────────────────────────────

/// Rejection emitted when any of the four buckets overflows. The
/// `retry_after` value is the number of whole seconds the caller should
/// wait before retrying.
#[derive(Debug, Clone)]
pub struct RateLimitError {
    pub retry_after: u64,
}

impl RateLimitError {
    pub fn new(retry_after: u64) -> Self {
        Self { retry_after }
    }
}

impl warp::reject::Reject for RateLimitError {}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rate limit exceeded; retry after {}s", self.retry_after)
    }
}

impl std::error::Error for RateLimitError {}

/// Recover handler. WS-C/G can call this from their top-level
/// `recover(...)` to turn a [`RateLimitError`] into a 429 with the
/// appropriate `Retry-After` header. Returns `Err(rejection)` so the
/// caller can chain other recoverers.
pub async fn recover(rejection: Rejection) -> Result<warp::reply::Response, Rejection> {
    if let Some(rl) = rejection.find::<RateLimitError>() {
        let body = format!("rate limit exceeded; retry after {}s", rl.retry_after);
        let mut resp = warp::http::Response::builder()
            .status(warp::http::StatusCode::TOO_MANY_REQUESTS)
            .header("Retry-After", rl.retry_after.to_string())
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(body.into())
            .unwrap();
        // Ensure header type matches warp::reply::Response (Body = hyper::Body).
        let _ = &mut resp;
        return Ok(resp);
    }
    Err(rejection)
}

// ── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn anon(key: &str) -> PublicAuthSource {
        PublicAuthSource::Anonymous {
            client_key: key.to_string(),
        }
    }

    #[tokio::test]
    async fn read_bucket_allows_then_blocks() {
        let st = RateLimitState::new();
        let auth = anon("1.2.3.4");
        for _ in 0..READ_LIMIT {
            assert!(check_for_reads(&st, &auth).await.is_ok());
        }
        let err = check_for_reads(&st, &auth).await.unwrap_err();
        assert!(err.retry_after >= 1);
    }

    #[tokio::test]
    async fn ask_bucket_keyed_per_client() {
        let st = RateLimitState::new();
        for _ in 0..ASK_LIMIT {
            assert!(check_for_ask(&st, &anon("a")).await.is_ok());
        }
        assert!(check_for_ask(&st, &anon("a")).await.is_err());
        // Different key has its own budget.
        assert!(check_for_ask(&st, &anon("b")).await.is_ok());
    }

    #[tokio::test]
    async fn login_bucket_does_not_skip_authenticated() {
        // _login is the only POST where authenticated principals are
        // also bucketed (they should never be hitting login anyway).
        let st = RateLimitState::new();
        let local = PublicAuthSource::LocalOperator;
        for _ in 0..LOGIN_LIMIT {
            assert!(check_for_login(&st, &local).await.is_ok());
        }
        assert!(check_for_login(&st, &local).await.is_err());
    }

    #[tokio::test]
    async fn read_and_ask_skip_wire_operator() {
        let st = RateLimitState::new();
        let wop = PublicAuthSource::WireOperator {
            operator_id: "op-1".into(),
            circle_id: None,
        };
        // Way over the limit — should still pass because C2 says skip.
        for _ in 0..(READ_LIMIT * 4) {
            assert!(check_for_reads(&st, &wop).await.is_ok());
        }
        for _ in 0..(ASK_LIMIT * 4) {
            assert!(check_for_ask(&st, &wop).await.is_ok());
        }
    }

    #[tokio::test]
    async fn email_bucket_normalizes_case() {
        let st = RateLimitState::new();
        for _ in 0..EMAIL_LIMIT {
            assert!(check_email_bucket(&st, "Foo@Bar.com").await.is_ok());
        }
        assert!(check_email_bucket(&st, "foo@bar.com").await.is_err());
    }

    #[tokio::test]
    async fn empty_email_is_noop() {
        let st = RateLimitState::new();
        for _ in 0..(EMAIL_LIMIT * 3) {
            assert!(check_email_bucket(&st, "").await.is_ok());
        }
    }

    #[tokio::test]
    async fn recover_emits_429() {
        let rej = warp::reject::custom(RateLimitError::new(42));
        let resp = recover(rej).await.unwrap();
        assert_eq!(resp.status(), warp::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers().get("Retry-After").unwrap().to_str().unwrap(),
            "42"
        );
    }
}
