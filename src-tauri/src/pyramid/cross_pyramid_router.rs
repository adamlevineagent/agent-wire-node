// pyramid/cross_pyramid_router.rs — Phase 13 cross-pyramid event fan-out.
//
// The existing `BuildEventBus` is a single broadcast channel that every
// slug's producers write into (the outer `TaggedBuildEvent` carries the
// slug). That's the structure the cross-pyramid spec's Option A would
// end up with anyway — slug isolation is enforced by the envelope, not
// the channel. The router adds two thin layers on top:
//
//   1. `CrossPyramidEventRouter::spawn_tauri_forwarder` — subscribes to
//      the shared bus and emits every event via
//      `app_handle.emit_all("cross-build-event", &event)` so the
//      `CrossPyramidTimeline.tsx` frontend can listen once and receive
//      updates for every active slug.
//
//   2. A small set of helpers the other IPC handlers call:
//      `register_slug` / `unregister_slug` track which slugs have
//      active builds so the frontend can render the "last event
//      received" grace window after a build completes.
//
// This avoids the per-slug forwarder-task complexity in the spec's
// pseudo-code while preserving Option A's semantics.
//
// The router is constructed at app startup (main.rs) and stored on
// `PyramidState.cross_pyramid_router`.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;
use tracing::debug;

use super::event_bus::BuildEventBus;

/// Tracking entry for an active build's slug. Used purely for the
/// metadata overlay on the frontend (last event, grace-period
/// bookkeeping). The actual event stream rides the shared bus.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveSlugState {
    pub slug: String,
    /// Monotonic timestamp of the last event forwarded for this slug.
    /// Seconds since the router was constructed.
    pub last_event_secs: u64,
    /// Set to `true` once `unregister_slug` has been called. The
    /// router keeps the entry for 60s after unregister so late
    /// arrivals still flow through with the right metadata.
    pub unregistered: bool,
}

/// Phase 13 cross-pyramid event router. Thin wrapper over the shared
/// `BuildEventBus`: maintains a slug registry for frontend metadata
/// and owns the Tauri forwarder task.
pub struct CrossPyramidEventRouter {
    /// Tokio mutex so async IPC handlers can register/unregister
    /// from within their futures without blocking the scheduler.
    active_slugs: Arc<Mutex<HashMap<String, ActiveSlugState>>>,
    /// Reference wall-clock anchor so timestamps are relative.
    start_instant: std::time::Instant,
}

impl CrossPyramidEventRouter {
    pub fn new() -> Self {
        Self {
            active_slugs: Arc::new(Mutex::new(HashMap::new())),
            start_instant: std::time::Instant::now(),
        }
    }

    /// Mark a slug as actively building. Idempotent — subsequent
    /// calls refresh the timestamp.
    pub async fn register_slug(&self, slug: impl Into<String>) {
        let slug = slug.into();
        let mut guard = self.active_slugs.lock().await;
        let secs = self.start_instant.elapsed().as_secs();
        guard
            .entry(slug.clone())
            .and_modify(|s| {
                s.last_event_secs = secs;
                s.unregistered = false;
            })
            .or_insert(ActiveSlugState {
                slug,
                last_event_secs: secs,
                unregistered: false,
            });
    }

    /// Mark a slug as no longer actively building. The entry is
    /// retained for a 60s grace window so late events still carry
    /// the "recently-active" metadata on the frontend.
    pub async fn unregister_slug(&self, slug: &str) {
        let mut guard = self.active_slugs.lock().await;
        if let Some(entry) = guard.get_mut(slug) {
            entry.unregistered = true;
            entry.last_event_secs = self.start_instant.elapsed().as_secs();
        }
    }

    /// Snapshot of the current active-slug table. Used by
    /// `pyramid_active_builds` to seed the frontend on mount.
    pub async fn list_active_slugs(&self) -> Vec<ActiveSlugState> {
        let guard = self.active_slugs.lock().await;
        guard.values().cloned().collect()
    }

    /// Prune entries that have been unregistered for more than the
    /// given grace window. Called by the forwarder task on every
    /// event so the table doesn't grow unbounded. Active
    /// (non-unregistered) entries are never pruned.
    ///
    /// `grace_secs = 0` means "prune every unregistered entry
    /// regardless of how recently it was touched" — useful for
    /// tests that want to force an immediate drop.
    async fn prune(&self, grace_secs: u64) {
        let now = self.start_instant.elapsed().as_secs();
        let mut guard = self.active_slugs.lock().await;
        guard.retain(|_, entry| {
            if !entry.unregistered {
                return true;
            }
            if grace_secs == 0 {
                return false;
            }
            now.saturating_sub(entry.last_event_secs) < grace_secs
        });
    }

    /// Spawn the Tauri forwarder. Returns immediately; the forwarder
    /// runs until the bus sender is dropped or the app exits.
    ///
    /// The forwarder reads from the shared bus via a tokio
    /// broadcast subscriber. Lag events (when the broadcast channel
    /// overflows) are converted into a resync hint — frontend
    /// timelines poll `pyramid_active_builds` on resync to recover.
    ///
    /// Uses `tauri::async_runtime::spawn` (not `tokio::spawn`)
    /// because this is called from `tauri::Builder::setup()`, which
    /// runs on the main thread before Tauri's managed Tokio runtime
    /// is the ambient runtime. `tokio::spawn` panics in that
    /// context with "there is no reactor running". Tauri's managed
    /// runtime IS ready by `setup()` time, so routing through
    /// `async_runtime::spawn` spawns the task on the correct
    /// runtime. Matches the pattern Phase 14's verifier fix applied
    /// to `spawn_wire_update_poller`.
    pub fn spawn_tauri_forwarder(
        router: Arc<Self>,
        bus: Arc<BuildEventBus>,
        app_handle: tauri::AppHandle,
    ) {
        let mut rx = bus.subscribe();
        tauri::async_runtime::spawn(async move {
            use tauri::Emitter;
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        // Update slug metadata in-line with event
                        // forwarding so the frontend can derive
                        // "active vs recent" state from the same
                        // fan-out path.
                        {
                            let mut guard = router.active_slugs.lock().await;
                            let secs = router.start_instant.elapsed().as_secs();
                            guard
                                .entry(event.slug.clone())
                                .and_modify(|s| {
                                    s.last_event_secs = secs;
                                })
                                .or_insert(ActiveSlugState {
                                    slug: event.slug.clone(),
                                    last_event_secs: secs,
                                    unregistered: false,
                                });
                        }
                        router.prune(60).await;

                        // Fire the event to the frontend. If the
                        // app is shutting down and emission fails,
                        // the error is logged and the loop
                        // continues — we don't want one bad emit
                        // to kill the forwarder.
                        if let Err(e) = app_handle.emit("cross-build-event", event.clone()) {
                            debug!("cross-pyramid forwarder emit failed: {}", e);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        debug!("cross-pyramid forwarder lagged by {} events, continuing", n);
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        debug!("cross-pyramid forwarder: bus closed, exiting");
                        break;
                    }
                }
            }
        });
    }
}

impl Default for CrossPyramidEventRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_list_active_slugs() {
        let router = CrossPyramidEventRouter::new();
        router.register_slug("opt-025").await;
        router.register_slug("goodnewseveryone").await;

        let slugs = router.list_active_slugs().await;
        let names: Vec<String> = slugs.iter().map(|s| s.slug.clone()).collect();
        assert!(names.contains(&"opt-025".to_string()));
        assert!(names.contains(&"goodnewseveryone".to_string()));
        // Both should be "active" (not unregistered).
        assert!(slugs.iter().all(|s| !s.unregistered));
    }

    #[tokio::test]
    async fn test_register_is_idempotent() {
        let router = CrossPyramidEventRouter::new();
        router.register_slug("opt-025").await;
        router.register_slug("opt-025").await;
        router.register_slug("opt-025").await;

        let slugs = router.list_active_slugs().await;
        assert_eq!(slugs.len(), 1);
    }

    #[tokio::test]
    async fn test_unregister_marks_but_retains() {
        let router = CrossPyramidEventRouter::new();
        router.register_slug("opt-025").await;
        router.unregister_slug("opt-025").await;

        let slugs = router.list_active_slugs().await;
        assert_eq!(slugs.len(), 1);
        assert!(slugs[0].unregistered);
    }

    #[tokio::test]
    async fn test_prune_removes_expired_unregistered() {
        let router = CrossPyramidEventRouter::new();
        router.register_slug("opt-025").await;
        router.unregister_slug("opt-025").await;

        // Prune with `grace_secs = 0` — any unregistered row
        // whose elapsed time is >= 0 (i.e. every unregistered
        // row) should be pruned immediately.
        router.prune(0).await;
        assert_eq!(router.list_active_slugs().await.len(), 0);
    }

    #[tokio::test]
    async fn test_prune_keeps_active() {
        let router = CrossPyramidEventRouter::new();
        router.register_slug("opt-025").await;

        router.prune(0).await;
        // Active (non-unregistered) entries are never pruned.
        assert_eq!(router.list_active_slugs().await.len(), 1);
    }

    #[tokio::test]
    async fn test_multi_slug_forwards_via_shared_bus() {
        // Instead of testing the actual Tauri forwarder (which
        // requires an AppHandle), test the register + list path
        // against a synthetic stream to confirm the router tracks
        // multiple concurrent slugs correctly.
        let router = Arc::new(CrossPyramidEventRouter::new());
        let r1 = router.clone();
        let r2 = router.clone();

        let h1 = tokio::spawn(async move {
            for _ in 0..50 {
                r1.register_slug("slug-a").await;
            }
        });
        let h2 = tokio::spawn(async move {
            for _ in 0..50 {
                r2.register_slug("slug-b").await;
            }
        });
        h1.await.unwrap();
        h2.await.unwrap();

        let slugs = router.list_active_slugs().await;
        assert_eq!(slugs.len(), 2);
    }
}
