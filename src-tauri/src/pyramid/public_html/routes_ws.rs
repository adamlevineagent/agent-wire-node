//! WebSocket upgrade handler for the public pyramid web surface.
//!
//! Per post-agents-retro v3.1 (B3) + v3.3 patches, this module exposes a
//! single WebSocket endpoint at `GET /p/{slug}/_ws` that subscribes to the
//! per-process [`BuildEventBus`] and forwards slug-filtered
//! [`TaggedBuildEvent`]s to the connected client as JSON text frames.
//!
//! This file is owned by WS-B. The route is intentionally NOT mounted by
//! [`crate::pyramid::public_html::routes`] — WS-C owns the mount integration
//! once all WS-A..F filters are landed. WS-B's deliverable is just the
//! [`ws_route`] function and the [`handle_ws`] task body, plus the tee at the
//! 5 build-launch sites (see `event_bus::tee_build_progress_to_bus`).
//!
//! Coalescing: per the plan, each subscriber drops intermediate `Progress`
//! events of the same kind for the same slug at a 60ms cadence and always
//! flushes the most recent one. `V2Snapshot` always sends the latest. The
//! coalesce buffer is per-connection so slow clients cannot back up the
//! global broadcast bus.
//!
//! Lag handling: when the broadcast receiver returns
//! `RecvError::Lagged(n)`, the handler sends a single `{"type":"resync"}`
//! frame (matching [`TaggedKind::Resync`]) and continues forwarding events.

use crate::pyramid::event_bus::{TaggedBuildEvent, TaggedKind};
use crate::pyramid::public_html::auth::{enforce_public_tier, PublicAuthSource};
use crate::pyramid::PyramidState;
use futures_util::sink::SinkExt;
use futures_util::stream::{SplitSink, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use warp::http::StatusCode;
use warp::ws::{Message, WebSocket};
use warp::Filter;

/// Mounts the `GET /p/{slug}/_ws` upgrade handler.
///
/// WS-C will compose this filter into the public_html routes tree once
/// WS-A's auth + tier-enforcement filters are available. For now this route
/// is unauthenticated and lets the browser subscribe to the per-slug build
/// event stream — public-tier pyramids only. Tier checks should be added as
/// `.and(WS-A::enforce_public_tier)` once that filter lands.
pub fn ws_route(
    state: Arc<PyramidState>,
) -> warp::filters::BoxedFilter<(warp::reply::Response,)> {
    warp::get()
        .and(warp::path("p"))
        .and(warp::path::param::<String>())
        .and(warp::path("_ws"))
        .and(warp::path::end())
        .and(warp::ws())
        .and(with_state(state))
        .and_then(
            |slug: String, ws: warp::ws::Ws, state: Arc<PyramidState>| async move {
                // Public-tier gate (Pillar 25, verification criterion 15):
                // priced/embargoed/circle-scoped slugs must NOT leak build
                // progress to anonymous WS clients. Until WS-A's
                // `with_public_or_session_auth` is composed in by WS-C, we
                // hard-gate every WS upgrade as Anonymous — which is the
                // strictest path through `enforce_public_tier` and matches
                // the actual identity of every browser-initiated WS handshake
                // (no Authorization header on the upgrade).
                let anon = PublicAuthSource::Anonymous {
                    client_key: String::new(),
                };
                if enforce_public_tier(&state, &slug, &anon).await.is_err() {
                    let resp = warp::http::Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(warp::hyper::Body::empty())
                        .unwrap();
                    return Ok::<_, warp::Rejection>(resp);
                }
                let response = ws.on_upgrade(move |socket| handle_ws(socket, slug, state));
                Ok(warp::reply::Reply::into_response(response))
            },
        )
        .boxed()
}

fn with_state(
    state: Arc<PyramidState>,
) -> impl Filter<Extract = (Arc<PyramidState>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || state.clone())
}

/// Per-connection task: subscribe to the bus, filter to `slug`, coalesce at
/// 60ms, send JSON frames over the websocket. Runs until either side hangs
/// up.
async fn handle_ws(socket: WebSocket, slug: String, state: Arc<PyramidState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut bus_rx = state.build_event_bus.subscribe();

    // Per-subscriber 60ms coalesce buffer.
    //
    // Latest pending Progress event for the slug (overwrites previous), and
    // latest V2Snapshot. We collect events for up to `COALESCE_WINDOW` ms then
    // flush whatever's pending. A Resync frame is sent immediately and then
    // also clears the pending buffer (the client is going to refetch anyway).
    const COALESCE_WINDOW: Duration = Duration::from_millis(60);

    let mut pending_progress: Option<TaggedKind> = None;
    let mut pending_v2: Option<TaggedKind> = None;
    let mut flush_deadline: Option<tokio::time::Instant> = None;

    loop {
        // Compute the next flush time. If nothing pending, wait indefinitely
        // for a bus event or a client message.
        let sleep_until = match flush_deadline {
            Some(d) => d,
            None => tokio::time::Instant::now() + Duration::from_secs(3600),
        };

        tokio::select! {
            biased;

            // 1) Client → server: detect close / drain pings.
            client_msg = ws_rx.next() => {
                match client_msg {
                    Some(Ok(msg)) if msg.is_close() => break,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => break,
                    None => break,
                }
            }

            // 2) Bus event arrives.
            bus_msg = bus_rx.recv() => {
                match bus_msg {
                    Ok(TaggedBuildEvent { slug: ev_slug, kind }) => {
                        if ev_slug != slug {
                            continue;
                        }
                        match kind {
                            TaggedKind::Progress { .. } => {
                                pending_progress = Some(kind);
                            }
                            TaggedKind::V2Snapshot(_) => {
                                pending_v2 = Some(kind);
                            }
                            TaggedKind::Resync => {
                                // Forward immediately; clear pending since the
                                // client will resync from REST.
                                let payload = TaggedBuildEvent { slug: slug.clone(), kind: TaggedKind::Resync };
                                if !send_event(&mut ws_tx, &payload).await {
                                    break;
                                }
                                pending_progress = None;
                                pending_v2 = None;
                                flush_deadline = None;
                                continue;
                            }
                        }
                        if flush_deadline.is_none() {
                            flush_deadline = Some(tokio::time::Instant::now() + COALESCE_WINDOW);
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        let payload = TaggedBuildEvent { slug: slug.clone(), kind: TaggedKind::Resync };
                        if !send_event(&mut ws_tx, &payload).await {
                            break;
                        }
                        pending_progress = None;
                        pending_v2 = None;
                        flush_deadline = None;
                    }
                    Err(RecvError::Closed) => break,
                }
            }

            // 3) Coalesce window elapsed — flush whatever's pending.
            _ = tokio::time::sleep_until(sleep_until), if flush_deadline.is_some() => {
                if let Some(kind) = pending_progress.take() {
                    let payload = TaggedBuildEvent { slug: slug.clone(), kind };
                    if !send_event(&mut ws_tx, &payload).await {
                        break;
                    }
                }
                if let Some(kind) = pending_v2.take() {
                    let payload = TaggedBuildEvent { slug: slug.clone(), kind };
                    if !send_event(&mut ws_tx, &payload).await {
                        break;
                    }
                }
                flush_deadline = None;
            }
        }
    }

    let _ = ws_tx.close().await;
}

/// Serialize a TaggedBuildEvent and send as a text frame. Returns false on
/// send failure (caller should hang up the connection).
async fn send_event(
    ws_tx: &mut SplitSink<WebSocket, Message>,
    event: &TaggedBuildEvent,
) -> bool {
    let json = match serde_json::to_string(event) {
        Ok(j) => j,
        Err(_) => return true, // skip malformed; keep the socket alive
    };
    ws_tx.send(Message::text(json)).await.is_ok()
}
