// partner/routes.rs — Warp HTTP route handlers for the Partner API
//
// 5 routes:
//   POST /partner/message          — send message, get response + brain state
//   GET  /partner/session/:id      — get session state
//   POST /partner/session/new      — create a new session
//   GET  /partner/brain/:session_id — current brain map for Space tab
//   GET  /partner/sessions         — list all sessions
//
// All routes require bearer token authentication (same as pyramid routes).

use std::sync::Arc;
use warp::Filter;

use super::{list_sessions, load_session, BrainState, PartnerState, BUFFER_HARD_LIMIT};
use crate::http_utils::{ct_eq, json_error, json_ok, Unauthorized};

// ── Auth middleware ──────────────────────────────────────────────────

/// Validate bearer token and pass PartnerState through.
fn with_auth_state(
    state: Arc<PartnerState>,
) -> impl Filter<Extract = (Arc<PartnerState>,), Error = warp::Rejection> + Clone {
    warp::header::optional::<String>("authorization")
        .and(warp::any().map(move || state.clone()))
        .and_then(
            |auth_header: Option<String>, state: Arc<PartnerState>| async move {
                let token = match auth_header {
                    Some(h) => match h.strip_prefix("Bearer ") {
                        Some(t) => t.to_string(),
                        None => return Err(warp::reject::custom(Unauthorized)),
                    },
                    None => return Err(warp::reject::custom(Unauthorized)),
                };

                // Read auth token from the pyramid config (shared auth)
                let auth_token = {
                    let config = state.pyramid.config.read().await;
                    config.auth_token.clone()
                };
                if auth_token.is_empty() || !ct_eq(&token, &auth_token) {
                    return Err(warp::reject::custom(Unauthorized));
                }

                Ok(state)
            },
        )
}

// ── Route definitions ───────────────────────────────────────────────

pub fn partner_routes(
    state: Arc<PartnerState>,
) -> warp::filters::BoxedFilter<(warp::reply::Response,)> {
    let prefix = warp::path("partner");

    macro_rules! route {
        ($filter:expr) => {
            $filter.map(|r: warp::reply::Response| r).boxed()
        };
    }

    // MOVED TO IPC: see main.rs — partner_session_new command
    // POST /partner/session/new (must be before /partner/session/:id)
    // let new_session = route!(prefix
    //     .and(warp::path("session"))
    //     .and(warp::path("new"))
    //     .and(warp::path::end())
    //     .and(warp::post())
    //     .and(with_auth_state(state.clone()))
    //     .and(warp::body::json())
    //     .and_then(handle_new_session));
    let new_session = prefix
        .and(warp::path("session"))
        .and(warp::path("new"))
        .and(warp::path::end())
        .and(warp::post())
        .and_then(|| async {
            Ok::<warp::reply::Response, warp::Rejection>(
                warp::http::Response::builder()
                    .status(410)
                    .header("Content-Type", "application/json")
                    .body(r#"{"error":"moved to IPC","command":"partner_session_new"}"#.into())
                    .unwrap(),
            )
        })
        .map(|r: warp::reply::Response| r)
        .boxed();

    // GET /partner/session/:id
    let get_session = route!(prefix
        .and(warp::path("session"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_get_session));

    // MOVED TO IPC: see main.rs — partner_send_message command
    // POST /partner/message
    // let send_message = route!(prefix
    //     .and(warp::path("message"))
    //     .and(warp::path::end())
    //     .and(warp::post())
    //     .and(with_auth_state(state.clone()))
    //     .and(warp::body::json())
    //     .and_then(handle_send_message));
    let send_message = prefix
        .and(warp::path("message"))
        .and(warp::path::end())
        .and(warp::post())
        .and_then(|| async {
            Ok::<warp::reply::Response, warp::Rejection>(
                warp::http::Response::builder()
                    .status(410)
                    .header("Content-Type", "application/json")
                    .body(r#"{"error":"moved to IPC","command":"partner_send_message"}"#.into())
                    .unwrap(),
            )
        })
        .map(|r: warp::reply::Response| r)
        .boxed();

    // GET /partner/brain/:session_id
    let get_brain = route!(prefix
        .and(warp::path("brain"))
        .and(warp::path::param::<String>())
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_get_brain));

    // GET /partner/sessions
    let list_all = route!(prefix
        .and(warp::path("sessions"))
        .and(warp::path::end())
        .and(warp::get())
        .and(with_auth_state(state.clone()))
        .and_then(handle_list_sessions));

    // Combine routes
    let r1 = new_session.or(get_session).unify().boxed();
    let r2 = send_message.or(get_brain).unify().boxed();

    let g1 = r1.or(r2).unify().boxed();
    g1.or(list_all).unify().boxed()
}

// ── Route handlers ──────────────────────────────────────────────────

async fn handle_get_session(
    session_id: String,
    state: Arc<PartnerState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Check in-memory cache first
    {
        let sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get(&session_id) {
            return Ok(json_ok(session));
        }
    }

    // Try loading from DB
    let db = state.partner_db.lock().await;
    match load_session(&db, &session_id) {
        Ok(Some(session)) => Ok(json_ok(&session)),
        Ok(None) => Ok(json_error(
            warp::http::StatusCode::NOT_FOUND,
            "Session not found",
        )),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}

async fn handle_get_brain(
    session_id: String,
    state: Arc<PartnerState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    // Check in-memory cache first
    let session = {
        let sessions = state.sessions.lock().await;
        sessions.get(&session_id).cloned()
    };

    let session = match session {
        Some(s) => s,
        None => {
            // Try loading from DB
            let db = state.partner_db.lock().await;
            match load_session(&db, &session_id) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    return Ok(json_error(
                        warp::http::StatusCode::NOT_FOUND,
                        "Session not found",
                    ));
                }
                Err(e) => {
                    return Ok(json_error(
                        warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                        &e.to_string(),
                    ));
                }
            }
        }
    };

    let buffer_tokens: usize = session
        .conversation_buffer
        .iter()
        .map(|m| m.token_estimate)
        .sum();

    let brain = BrainState {
        hydrated_node_ids: session.hydrated_node_ids,
        session_topics: session.session_topics,
        lifted_results: session.lifted_results,
        buffer_tokens,
        buffer_capacity: BUFFER_HARD_LIMIT,
    };

    Ok(json_ok(&brain))
}

async fn handle_list_sessions(
    state: Arc<PartnerState>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let db = state.partner_db.lock().await;
    match list_sessions(&db) {
        Ok(sessions) => Ok(json_ok(&sessions)),
        Err(e) => Ok(json_error(
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
        )),
    }
}
