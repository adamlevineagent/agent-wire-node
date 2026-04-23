// Walker v3 Phase 0a-2 WS5 — AppMode state machine + boot/runtime guards.
//
// Plan rev 1.0.2 §2.17 (boot sequence), §2.17.1 (AppMode in-memory state
// machine + guard invariants), §2.17.3 (boot-aborts-to-known-states).
//
// AppMode is a process-global, in-memory-only signal of where the boot
// coordinator is in the canonical 11-step sequence. Persistence is
// deliberately NOT a goal — boot always starts at `Booting`. Quarantine
// (set when the scope_cache_reloader exhausts its restart budget) is a
// PROCESS-lifetime fence, not a durable state; an operator restart is
// the recovery path along with shipping a corrected contribution.
//
// {invariant: app_mode_single_writer} — only the boot coordinator (and
// the single quarantine relay task it spawns from `walker_cache::
// spawn_scope_cache_reloader`) call `transition_to`. All other code
// READS via `guard_app_ready` or by inspecting `AppState::app_mode`.

use std::sync::Arc;
use tokio::sync::RwLock;

/// In-memory state machine for the node's boot/run lifecycle.
///
/// Transitions (per §2.17.1):
/// ```text
///   Booting ─► Migrating ─► Ready ─► ShuttingDown
///                           │
///                           └─► Quarantined (terminal until restart)
/// ```
/// `Quarantined` and `ShuttingDown` are both terminal in the sense that
/// no further transition unwinds them inside the same process — quarantine
/// requires a restart + corrected contribution; shutdown is process exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// Process started; boot coordinator has not yet completed steps 1-8.
    /// Build-starters MUST refuse to run while in this state.
    Booting,
    /// DDL/data migration in progress (step 4 of the canonical sequence).
    /// Phase 0a-2 does not yet implement the v3 DDL body, but the state
    /// is reserved so future phases can pin migration windows precisely.
    Migrating,
    /// All boot steps succeeded; HTTP listeners + background loops are up
    /// and build-starters are unblocked. The ONLY mode in which
    /// `guard_app_ready` returns Ok.
    Ready,
    /// `scope_cache_reloader` exhausted its restart budget (§2.17.2). LKG
    /// ScopeCache continues to serve readers but no new builds are
    /// accepted. Operator must ship a corrected contribution + restart.
    Quarantined,
    /// Tauri / process shutdown signal observed; in-flight builds may
    /// still drain but no new ones start.
    ShuttingDown,
}

/// Error returned by `guard_app_ready` when the node is not in `Ready`.
/// Rendered as a string at the IPC/HTTP boundary so callers can surface
/// a stable user-facing message + log the exact mode for triage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppNotReady {
    pub current_mode: AppMode,
}

impl std::fmt::Display for AppNotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "node is not Ready (mode: {:?}); refusing to accept build-starter",
            self.current_mode
        )
    }
}

impl std::error::Error for AppNotReady {}

/// Transition the global AppMode. Caller must be the boot coordinator
/// or the dedicated quarantine relay task spawned next to the
/// scope_cache_reloader (the {invariant: app_mode_single_writer} fence).
///
/// Emits a structured `tracing::info!` on every transition with the old
/// and new mode so a log scrape can reconstruct the boot timeline.
///
/// Idempotent: a transition where `from == to` is logged at debug + the
/// mode is left unchanged (no spurious info log noise during recovery
/// flows that defensively re-set the state).
pub async fn transition_to(app_mode: &Arc<RwLock<AppMode>>, to: AppMode) {
    let mut guard = app_mode.write().await;
    let from = *guard;
    if from == to {
        tracing::debug!(
            event = "app_mode_transition_noop",
            from = ?from,
            to = ?to,
            "app_mode transition is a no-op (already in target state)"
        );
        return;
    }
    *guard = to;
    tracing::info!(
        event = "app_mode_transition",
        from = ?from,
        to = ?to,
        "app_mode transition"
    );
}

/// Read-only guard for build-starter code paths. Returns `Ok(())` iff
/// the global AppMode is `Ready`; otherwise returns an `AppNotReady`
/// carrying the current mode for triage.
///
/// Plan §2.17.1: "every current starter (HTTP build routes, Tauri
/// pyramid_build, question-build spawn, folder-ingestion initial-build
/// spawn, DADBEAR manual trigger, stale-engine startup reconciliation,
/// and any future spawn_*build* helper) must route through the same
/// guard helper so boot ordering and runtime gating cannot drift apart."
pub async fn guard_app_ready(app_mode: &Arc<RwLock<AppMode>>) -> Result<(), AppNotReady> {
    let current = *app_mode.read().await;
    if current == AppMode::Ready {
        Ok(())
    } else {
        tracing::warn!(
            event = "app_mode_guard_rejected",
            current_mode = ?current,
            "build-starter rejected: app_mode is not Ready"
        );
        Err(AppNotReady { current_mode: current })
    }
}

/// Construct the initial `Arc<RwLock<AppMode>>` the boot coordinator
/// owns. Always starts at `Booting` per §2.17.1: "AppMode is NOT
/// persisted. Boot always starts at `Booting`."
pub fn new_app_mode() -> Arc<RwLock<AppMode>> {
    Arc::new(RwLock::new(AppMode::Booting))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_app_mode_starts_in_booting() {
        let mode = new_app_mode();
        assert_eq!(*mode.read().await, AppMode::Booting);
    }

    #[tokio::test]
    async fn guard_rejects_until_ready() {
        let mode = new_app_mode();
        // Booting → reject
        assert!(guard_app_ready(&mode).await.is_err());
        transition_to(&mode, AppMode::Migrating).await;
        // Migrating → reject
        let err = guard_app_ready(&mode).await.unwrap_err();
        assert_eq!(err.current_mode, AppMode::Migrating);
        // Ready → accept
        transition_to(&mode, AppMode::Ready).await;
        assert!(guard_app_ready(&mode).await.is_ok());
        // Quarantined → reject
        transition_to(&mode, AppMode::Quarantined).await;
        let err = guard_app_ready(&mode).await.unwrap_err();
        assert_eq!(err.current_mode, AppMode::Quarantined);
    }

    #[tokio::test]
    async fn transition_is_idempotent_noop() {
        let mode = new_app_mode();
        transition_to(&mode, AppMode::Ready).await;
        transition_to(&mode, AppMode::Ready).await; // no-op log path
        assert_eq!(*mode.read().await, AppMode::Ready);
    }
}
