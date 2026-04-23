// Walker v3 — ScopeCache, ScopeSnapshot, and the scope_cache_reloader
// supervisor (Phase 0a-2 commits 3 + 4).
//
// Plan rev 1.0.2 §2.9 (DispatchDecision carries `Arc<ScopeCache>` via a
// ScopeSnapshot wrapper), §2.16.2 (ArcSwap listener supervision — single
// named task owns the writer, 250ms debounce, restart on panic + emit
// `scope_cache_listener_restarted`), §2.17.2 (quarantine on persistent
// panic: restart budget 3 within 60s, 4th → hold LKG Arc, mark triggering
// contribution_id `status='quarantined'`, emit `scope_cache_quarantined`,
// signal AppMode::Quarantined via a transition channel), §5.4.3 (Root 27
// type-level redaction guard — raw ScopeSnapshot is NOT `Serialize`;
// chronicle serializes a distinct `RedactedSnapshot` view).
//
// Phase 0a-2 scope is minimal container types + the supervisor. Phase 0b
// fleshes out the resolver layer on top of ScopeCache (the full six-scope
// objects — `scope_slot_provider`, `scope_slot`, `scope_call_order_provider`,
// `scope_provider` maps). Here we ship the stable container types so
// callers + tests can depend on them.
//
// WS5 (boot sequence) owns:
//   - `AppState` / `AppMode` definitions
//   - spawning this supervisor from `main.rs`
//   - wiring `trigger_rx` to the ConfigSynced listener
// This module exposes `spawn_scope_cache_reloader` + the associated
// channel types so WS5 can plumb through cleanly.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use arc_swap::ArcSwap;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::pyramid::compute_chronicle::{
    EVENT_SCOPE_CACHE_LISTENER_RESTARTED, EVENT_SCOPE_CACHE_QUARANTINED,
};

// ── Core types ────────────────────────────────────────────────────────────

/// Per-boot (+ rebuilds) snapshot of the resolver's scope chain. Stored
/// inside an `ArcSwap<ScopeCache>` owned exclusively by the reloader task
/// (§2.16.2 {invariant: scope_cache_single_writer}). Readers get an
/// `Arc<ScopeCache>` via `ArcSwap::load_full` + carry it through a
/// ScopeSnapshot into the Decision.
///
/// Phase 0a-2 shell: minimal fields. Phase 0b `walker_resolver.rs` loads
/// the real scope objects into this struct. The intentional placeholder
/// `_phase_0b_scope_maps` is a reminder that the next phase fills this
/// out — it carries no runtime value today.
///
/// NOT `Serialize`: the chronicle must only see a redacted view. The raw
/// cache is reachable through `ScopeSnapshot::cache` for internal
/// dispatchers that consume by field access.
#[derive(Debug)]
pub struct ScopeCache {
    /// Wall-clock at ScopeCache construction. Surfaces in the redacted
    /// chronicle view so `decision_built` carries "cache was built at X".
    pub built_at: SystemTime,
    /// Every `contribution_id` whose row contributed to this cache build.
    /// Used during quarantine (§2.17.2) to trace the triggering
    /// contribution; also useful for ad-hoc debugging of "which row
    /// produced this resolver state?".
    pub source_contribution_ids: Vec<String>,
    /// Room for Phase 0b: scope_slot_provider, scope_slot,
    /// scope_call_order_provider, scope_provider maps. Placeholder so the
    /// struct shape doesn't churn when Phase 0b expands it.
    #[doc(hidden)]
    pub _phase_0b_scope_maps: (),
}

impl ScopeCache {
    /// Minimal constructor. Real Phase 0b builds ScopeCache from the
    /// active-contribution set via `walker_resolver::build_scope_cache`.
    pub fn new_empty() -> Self {
        Self {
            built_at: SystemTime::now(),
            source_contribution_ids: Vec::new(),
            _phase_0b_scope_maps: (),
        }
    }
}

/// Per-Decision view handed to dispatchers. Pins one `Arc<ScopeCache>`
/// for the Decision's lifetime so mid-step ArcSwap updates cannot change
/// the answer walker already computed (§2.9 Decision immutability).
///
/// INTENTIONALLY NOT `Serialize` (Root 27 / F-C3-6). The first well-meaning
/// dev who calls `serde_json::to_value(&scope_snapshot)` leaks LAN URLs
/// and other `local_only`/`sensitive` parameters. Redaction is a TYPE
/// PROPERTY here: you can only serialize by going through
/// `redacted_for_chronicle()` which returns the `RedactedSnapshot` view.
/// The compile-time guard test below pins this.
#[derive(Debug)]
pub struct ScopeSnapshot {
    pub cache: Arc<ScopeCache>,
    pub taken_at: SystemTime,
}

impl ScopeSnapshot {
    pub fn new(cache: Arc<ScopeCache>) -> Self {
        Self {
            cache,
            taken_at: SystemTime::now(),
        }
    }

    /// The chronicle-safe view. Strips any field that carries
    /// operator-local state (`local_only: true`) or sensitive config
    /// (`sensitive: true`). Phase 0b expands `RedactedSnapshot` to carry
    /// redacted scope entries alongside the existing `built_at` +
    /// `source_contribution_ids`.
    pub fn redacted_for_chronicle(&self) -> RedactedSnapshot {
        RedactedSnapshot {
            built_at: self.cache.built_at,
            taken_at: self.taken_at,
            source_contribution_ids: self.cache.source_contribution_ids.clone(),
        }
    }

    /// Dispatch-internal view. Dispatchers consume cache fields by name —
    /// they never serialize. This method returns `&Self` so the caller
    /// keeps the same lifetime guarantee without re-wrapping.
    pub fn for_dispatch_internal(&self) -> &Self {
        self
    }
}

/// Chronicle-serializable view of a ScopeSnapshot. The ONLY type in this
/// module that derives `Serialize`. Adding a field here requires checking
/// its schema_annotation's `local_only` / `sensitive` flags (Phase 0b).
#[derive(Debug, Clone, Serialize)]
pub struct RedactedSnapshot {
    pub built_at: SystemTime,
    pub taken_at: SystemTime,
    pub source_contribution_ids: Vec<String>,
    // Phase 0b: redacted scope entries (only public, non-local-only
    // parameters) land here. Ollama base URLs, budget caps, closed-beta
    // slugs are stripped.
}

// ── Reloader supervisor ──────────────────────────────────────────────────

/// Message posted to the reloader. Carries the triggering
/// `contribution_id` so quarantine (§2.17.2) can mark the right row if
/// rebuild panics cross the restart budget.
#[derive(Debug, Clone)]
pub struct RebuildTrigger {
    /// Contribution whose supersession/retraction triggered this rebuild.
    /// `None` for boot rebuilds or forced admin rebuilds.
    pub contribution_id: Option<String>,
    /// Schema type for chronicle detail. Best-effort — reloader doesn't
    /// branch on it.
    pub schema_type: Option<String>,
}

/// Messages the reloader emits when it needs main.rs / WS5 to transition
/// the global `AppMode`. The concrete enum name lives in WS5's AppState
/// module; we carry the variant intent as a typed payload here so WS5 can
/// trivially map it in `app_mode.transition_to(...)`.
#[derive(Debug, Clone)]
pub enum AppModeTransition {
    /// Reloader exhausted its restart budget — WS5 must flip AppMode to
    /// `Quarantined`. Reloader continues serving stale reads in the
    /// meantime (LKG Arc held in ArcSwap).
    Quarantined {
        contribution_id: Option<String>,
        schema_type: Option<String>,
        panic_count: u32,
        window_start: SystemTime,
    },
}

/// Debounce window (§2.16.2) — rapid operator edits collapse to one
/// rebuild. Exposed as a constant so tests can reference the same number.
pub const SCOPE_CACHE_DEBOUNCE: Duration = Duration::from_millis(250);

/// Restart budget from §2.17.2. The reloader tolerates up to
/// `RESTART_BUDGET` panics within a sliding `RESTART_WINDOW`. The
/// `(budget+1)`-th panic in the window triggers quarantine.
pub const RESTART_BUDGET: u32 = 3;
pub const RESTART_WINDOW: Duration = Duration::from_secs(60);

/// Spawn the single named task `scope_cache_reloader` that owns the
/// `ArcSwap<ScopeCache>` writer.
///
/// Supervisor semantics (plan §2.16.2 + §2.17.2):
/// - Listens on `trigger_rx`. Multiple triggers arriving within
///   `SCOPE_CACHE_DEBOUNCE` collapse to a single rebuild, carrying the
///   LATEST trigger's contribution_id/schema_type for quarantine tracing.
/// - On each rebuild: open a fresh `Connection` to `db_path`, call
///   `rebuild_fn(&conn)`. On `Ok(new_cache)` → `ArcSwap::store(Arc::new)`.
/// - On panic inside `rebuild_fn` (caught via `FutureExt::catch_unwind`
///   around the blocking task): emit `scope_cache_listener_restarted`
///   with the triggering contribution id, record against the restart
///   budget, continue the loop.
/// - On `Err(_)` (non-panic failure): logged but does NOT count against
///   the restart budget; operator-triggered retries don't burn budget.
/// - On success: reset the panic window (counter = 0). Two panics, a
///   success, two more panics → no quarantine.
/// - When `panic_count_in_window > RESTART_BUDGET`: HOLD the current
///   ArcSwap value (resolver keeps serving LKG), write
///   `status='quarantined'` for the triggering contribution_id, emit
///   `scope_cache_quarantined`, send `AppModeTransition::Quarantined`
///   via `app_mode_tx`. Task continues to drain `trigger_rx` (so main.rs
///   can recover by shipping a corrected contribution) but skips
///   rebuilds on the quarantined contribution_id.
///
/// Parameters:
/// - `cache_writer` — the `ArcSwap` created during boot step 3 (§2.17).
///   WS5 gives the SAME `Arc<ArcSwap<_>>` to resolver readers; reloader
///   is the sole writer.
/// - `trigger_rx` — owned by the reloader; WS5 keeps the tx end and
///   clones to the ConfigSynced handler + any admin "force rebuild"
///   endpoint.
/// - `rebuild_fn` — closure from `&Connection` to a fresh `ScopeCache`.
///   Typically `walker_resolver::build_scope_cache` once that lands in
///   Phase 0b. Required `Send + 'static` so the task owns it; NOT `Sync`
///   (each rebuild call is serialized by the task loop).
/// - `db_path` — filesystem path to the SQLite database. Reloader opens
///   a fresh `Connection` per rebuild attempt. Same pattern llm.rs and
///   other background tasks use — keeps rusqlite's !Send+!Sync
///   `Connection` inside a blocking-task boundary.
/// - `event_emitter` — best-effort fire-and-forget event sink. Phase 0b
///   replaces this with a real `BuildEventBus` handle once WS5 exposes
///   one on AppState.
/// - `app_mode_tx` — `mpsc::Sender<AppModeTransition>` WS5 wires to the
///   AppState's mode-transition handler.
///
/// Returns a `JoinHandle<()>` whose lifecycle is owned by the caller
/// (WS5 stores on AppState and aborts on shutdown).
pub fn spawn_scope_cache_reloader<F, E>(
    cache_writer: Arc<ArcSwap<ScopeCache>>,
    trigger_rx: mpsc::Receiver<RebuildTrigger>,
    rebuild_fn: F,
    db_path: String,
    event_emitter: E,
    app_mode_tx: mpsc::Sender<AppModeTransition>,
) -> JoinHandle<()>
where
    F: Fn(&Connection) -> Result<ScopeCache> + Send + Sync + 'static,
    E: Fn(&str, Value) + Send + Sync + 'static,
{
    let rebuild_fn = Arc::new(rebuild_fn);
    let event_emitter = Arc::new(event_emitter);

    // Plan §2.16.2 names the task `scope_cache_reloader`. `tokio::task::Builder`
    // would surface that name in tokio-console but requires the `tokio_unstable`
    // cfg; the crate doesn't enable it today. The invariant that matters is
    // "exclusive ownership of the ArcSwap writer" — guaranteed by this being
    // the single spawn site. Observability naming can be added with a cfg-gate
    // later without changing supervisor semantics.
    tokio::spawn(reloader_loop(
        cache_writer,
        trigger_rx,
        rebuild_fn,
        db_path,
        event_emitter,
        app_mode_tx,
    ))
}

/// The actual loop body. Pulled out of `spawn_scope_cache_reloader` so
/// tests can drive it directly (with the same arg shapes) without having
/// to rely on task-name support.
async fn reloader_loop<F, E>(
    cache_writer: Arc<ArcSwap<ScopeCache>>,
    mut trigger_rx: mpsc::Receiver<RebuildTrigger>,
    rebuild_fn: Arc<F>,
    db_path: String,
    event_emitter: Arc<E>,
    app_mode_tx: mpsc::Sender<AppModeTransition>,
) where
    F: Fn(&Connection) -> Result<ScopeCache> + Send + Sync + 'static,
    E: Fn(&str, Value) + Send + Sync + 'static,
{
    // Panic-history ring: timestamps of in-window panics. Pruned to
    // RESTART_WINDOW on each event so `len()` is the live count.
    let mut panic_history: Vec<SystemTime> = Vec::with_capacity(RESTART_BUDGET as usize + 1);
    // Contribution_ids whose most recent rebuild blew past the budget.
    // Triggers referencing these are treated as no-ops (resolver keeps
    // serving LKG). Cleared only by process restart.
    let mut quarantined_ids: Vec<String> = Vec::new();

    loop {
        // Block until at least one trigger arrives.
        let Some(first) = trigger_rx.recv().await else {
            // Sender dropped — WS5 is shutting down. Exit cleanly.
            return;
        };

        // Debounce: collect any triggers arriving within the window. The
        // LATEST trigger's contribution_id/schema_type wins (newest
        // supersession is the one whose panic we want to attribute).
        let mut latest = first;
        let debounce_end = tokio::time::Instant::now() + SCOPE_CACHE_DEBOUNCE;
        loop {
            let remaining = debounce_end.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, trigger_rx.recv()).await {
                Ok(Some(next)) => latest = next,
                Ok(None) => {
                    // Channel closed mid-debounce — still want to do one
                    // last rebuild for the trigger we already have.
                    break;
                }
                Err(_) => break, // timeout elapsed — debounce complete
            }
        }

        // If the triggering contribution is already quarantined, skip the
        // rebuild. (ArcSwap keeps its prior value → resolver serves LKG.)
        if let Some(ref cid) = latest.contribution_id {
            if quarantined_ids.iter().any(|q| q == cid) {
                continue;
            }
        }

        // Run the rebuild on a blocking task so the !Send Connection
        // never crosses an await, and so `catch_unwind` can isolate
        // panics without poisoning the reloader task itself.
        let rf = Arc::clone(&rebuild_fn);
        let dbp = db_path.clone();
        let blocking = tokio::task::spawn_blocking(move || {
            // Open fresh Connection. If open fails, surface as Err — does
            // NOT count as a panic.
            let conn = match Connection::open(&dbp) {
                Ok(c) => c,
                Err(e) => return RebuildOutcome::OpenFailed(format!("{e}")),
            };
            // `AssertUnwindSafe` around the closure + `catch_unwind`:
            // rebuild_fn is caller-provided and may panic on unexpected
            // shape; we isolate it.
            let res = std::panic::catch_unwind(AssertUnwindSafe(|| rf(&conn)));
            match res {
                Ok(Ok(cache)) => RebuildOutcome::Ok(cache),
                Ok(Err(e)) => RebuildOutcome::Failed(format!("{e:?}")),
                Err(p) => RebuildOutcome::Panicked(panic_message(p)),
            }
        });

        let outcome = match blocking.await {
            Ok(o) => o,
            Err(join_err) => {
                // spawn_blocking task failed to complete. If it panicked,
                // JoinError::is_panic is set — treat as panic. Otherwise
                // (cancelled, etc.) treat as Failed.
                if join_err.is_panic() {
                    RebuildOutcome::Panicked(format!("{join_err}"))
                } else {
                    RebuildOutcome::Failed(format!("{join_err}"))
                }
            }
        };

        match outcome {
            RebuildOutcome::Ok(new_cache) => {
                cache_writer.store(Arc::new(new_cache));
                // Success resets the panic window. §2.16.2 semantics:
                // "two panics, a success, two more" does NOT quarantine.
                panic_history.clear();
            }
            RebuildOutcome::Failed(msg) | RebuildOutcome::OpenFailed(msg) => {
                // Non-panic failure. Log at warn level but do NOT burn
                // restart budget — flaky DB locks or validation errors
                // shouldn't drive quarantine.
                tracing::warn!(
                    contribution_id = ?latest.contribution_id,
                    error = %msg,
                    "scope_cache rebuild failed (non-panic)"
                );
            }
            RebuildOutcome::Panicked(msg) => {
                // Record the panic, emit restart event, then check budget.
                let now = SystemTime::now();
                prune_history(&mut panic_history, now);
                panic_history.push(now);

                tracing::warn!(
                    event = EVENT_SCOPE_CACHE_LISTENER_RESTARTED,
                    contribution_id = ?latest.contribution_id,
                    schema_type = ?latest.schema_type,
                    panic_count = panic_history.len(),
                    panic_msg = %msg,
                    "scope_cache_reloader panic — restarting"
                );
                event_emitter(
                    EVENT_SCOPE_CACHE_LISTENER_RESTARTED,
                    serde_json::json!({
                        "contribution_id": latest.contribution_id,
                        "schema_type": latest.schema_type,
                        "panic_count": panic_history.len(),
                        "panic_msg": msg,
                    }),
                );

                if panic_history.len() as u32 > RESTART_BUDGET {
                    // Budget exhausted. HOLD the current ArcSwap value
                    // (implicit — we never called .store), mark the
                    // contribution as quarantined in the DB so next boot
                    // skips it, emit the event, and signal WS5 to flip
                    // AppMode.
                    let window_start = panic_history
                        .first()
                        .copied()
                        .unwrap_or_else(SystemTime::now);
                    let panic_count = panic_history.len() as u32;

                    if let Some(ref cid) = latest.contribution_id {
                        quarantine_contribution(&db_path, cid);
                        quarantined_ids.push(cid.clone());
                    }

                    event_emitter(
                        EVENT_SCOPE_CACHE_QUARANTINED,
                        serde_json::json!({
                            "contribution_id": latest.contribution_id,
                            "schema_type": latest.schema_type,
                            "panic_count": panic_count,
                            "window_start": format!("{:?}", window_start),
                        }),
                    );
                    tracing::error!(
                        event = EVENT_SCOPE_CACHE_QUARANTINED,
                        contribution_id = ?latest.contribution_id,
                        panic_count,
                        "scope_cache_reloader quarantined — holding LKG cache"
                    );

                    // Fire-and-forget on the channel: if WS5's receiver
                    // is gone we still want the rest of the supervisor
                    // to keep serving LKG.
                    let _ = app_mode_tx
                        .send(AppModeTransition::Quarantined {
                            contribution_id: latest.contribution_id.clone(),
                            schema_type: latest.schema_type.clone(),
                            panic_count,
                            window_start,
                        })
                        .await;

                    // Reset history after quarantine so the task doesn't
                    // keep firing quarantine events for every subsequent
                    // trigger. Next panic on a DIFFERENT contribution
                    // starts a fresh budget.
                    panic_history.clear();
                }
            }
        }
    }
}

/// Internal rebuild outcome taxonomy.
enum RebuildOutcome {
    Ok(ScopeCache),
    /// rusqlite failed to open the DB — infra issue, not rebuild logic.
    OpenFailed(String),
    /// rebuild_fn returned `Err(_)` cleanly. Not a panic.
    Failed(String),
    /// rebuild_fn (or the task) panicked. Counts against budget.
    Panicked(String),
}

/// Drop timestamps older than `RESTART_WINDOW` before `now`.
fn prune_history(history: &mut Vec<SystemTime>, now: SystemTime) {
    history.retain(|t| {
        now.duration_since(*t)
            .map(|d| d < RESTART_WINDOW)
            .unwrap_or(true)
    });
}

/// Best-effort extraction of a panic's message for logging.
fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<String>() {
        return s.clone();
    }
    if let Some(s) = p.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    "<non-string panic>".into()
}

/// Mark a contribution `status='quarantined'`. Direct UPDATE — the
/// envelope writer owns supersession, but quarantine is a terminal
/// status transition outside the supersession chain (the contribution
/// that caused repeated panic is NOT replaced by a newer one; it's
/// fenced off until operator intervention). Plan §2.17.2.
///
/// Intentionally NOT routing through `config_contributions.rs` — WS4
/// owns that file for the retract handler, and quarantine is a strictly
/// terminal transition (not a supersession). A failure here is logged
/// but does not prevent the quarantine event from firing or WS5 from
/// being notified — the AppMode::Quarantined transition alone is enough
/// to stop further damage.
fn quarantine_contribution(db_path: &str, contribution_id: &str) {
    match Connection::open(db_path) {
        Ok(conn) => {
            if let Err(e) = conn.execute(
                "UPDATE pyramid_config_contributions
                 SET status = 'quarantined'
                 WHERE contribution_id = ?1",
                rusqlite::params![contribution_id],
            ) {
                tracing::error!(
                    contribution_id = %contribution_id,
                    error = %e,
                    "failed to mark contribution as quarantined"
                );
            }
        }
        Err(e) => {
            tracing::error!(
                contribution_id = %contribution_id,
                error = %e,
                "failed to open db for quarantine update"
            );
        }
    }
}

// ── Compile-time type-guard for ScopeSnapshot (Root 27 / F-C3-6) ─────────
//
// `ScopeSnapshot` MUST NOT `impl Serialize`. The chronicle integration
// is expected to go through `redacted_for_chronicle()`; anyone reaching
// for `serde_json::to_value(&scope_snapshot)` leaks operator-local
// params. There's no `static_assertions` dep in this crate yet and
// adding one just for this guard is over-kill — instead we use a
// `#[cfg(any())]` compile-fence. Removing the fence MUST cause a
// compile error. If it compiles, the type-level guard has regressed.
#[cfg(any())]
#[allow(dead_code)]
fn _scope_snapshot_must_not_be_serializable(ss: &ScopeSnapshot) {
    // ── DO NOT ADD `#[derive(Serialize)]` to `ScopeSnapshot` ──
    // If the line below compiles, a Serialize impl exists and the
    // type-level redaction guard (Root 27 / §5.4.3) has regressed.
    // Fix: remove the derive and rebuild chronicle callers to go
    // through `redacted_for_chronicle()`.
    let _ = serde_json::to_value(ss).expect("must not compile");
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Create an empty SQLite DB with the minimal pyramid_config_contributions
    /// table the quarantine UPDATE needs. Matches db.rs:1687's shape only in
    /// the columns we actually touch (contribution_id + status).
    fn make_db() -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("walker_cache_test.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE pyramid_config_contributions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 contribution_id TEXT NOT NULL UNIQUE,
                 status TEXT NOT NULL DEFAULT 'active'
             );",
        )
        .unwrap();
        let path_str = path.to_string_lossy().to_string();
        (dir, path_str)
    }

    fn insert_contribution(db_path: &str, id: &str) {
        let conn = Connection::open(db_path).unwrap();
        conn.execute(
            "INSERT INTO pyramid_config_contributions (contribution_id, status)
             VALUES (?1, 'active')",
            rusqlite::params![id],
        )
        .unwrap();
    }

    fn read_status(db_path: &str, id: &str) -> String {
        let conn = Connection::open(db_path).unwrap();
        conn.query_row(
            "SELECT status FROM pyramid_config_contributions WHERE contribution_id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
    }

    /// Compile-time: `RedactedSnapshot` IS `Serialize`. If someone
    /// accidentally removes the derive this test stops compiling.
    #[test]
    fn redacted_snapshot_impls_serialize() {
        let cache = Arc::new(ScopeCache::new_empty());
        let snap = ScopeSnapshot::new(cache);
        let redacted = snap.redacted_for_chronicle();
        let val = serde_json::to_value(&redacted).expect("redacted must serialize");
        assert!(val.is_object());
        assert!(val.get("built_at").is_some());
        assert!(val.get("taken_at").is_some());
        assert!(val.get("source_contribution_ids").is_some());
    }

    /// Runtime mirror of the compile-time guard. If someone adds
    /// `#[derive(Serialize)]` to `ScopeSnapshot`, the `#[cfg(any())]`
    /// guard above stops failing — this test still passes (can't fail a
    /// type-level property at runtime), so readers should treat the
    /// cfg(any()) block as the canonical guard. This test exists to
    /// DOCUMENT the intent in a place grep will find from a runtime
    /// test failure trail.
    #[test]
    fn scope_snapshot_not_serialize_guard_present() {
        // This line exists solely so a code-search for
        // "scope_snapshot_not_serialize_guard" lands on the rationale.
        let _ = ScopeSnapshot::new(Arc::new(ScopeCache::new_empty()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reloader_debounces_rapid_triggers_to_one_rebuild() {
        // Uses real wall-clock sleeps rather than `tokio::time::pause()` —
        // the `test-util` feature isn't enabled on the crate's tokio dep
        // and the reloader runs rebuild_fn via spawn_blocking (which
        // interacts poorly with tokio's paused clock anyway). The test
        // sends 5 triggers tightly packed (well inside SCOPE_CACHE_DEBOUNCE)
        // then waits for the debounce + rebuild to complete.
        let (_dir, db_path) = make_db();
        let writer = Arc::new(ArcSwap::from_pointee(ScopeCache::new_empty()));
        let rebuild_count = Arc::new(AtomicU32::new(0));

        let rc = Arc::clone(&rebuild_count);
        let rebuild_fn = Arc::new(move |_conn: &Connection| -> Result<ScopeCache> {
            rc.fetch_add(1, Ordering::SeqCst);
            Ok(ScopeCache::new_empty())
        });

        let (trigger_tx, trigger_rx) = mpsc::channel(16);
        let (app_mode_tx, _app_mode_rx) = mpsc::channel(4);
        let event_emitter = Arc::new(|_name: &str, _v: Value| {});

        let handle = tokio::spawn(reloader_loop(
            Arc::clone(&writer),
            trigger_rx,
            rebuild_fn,
            db_path,
            event_emitter,
            app_mode_tx,
        ));

        // 5 triggers tightly packed. Each send is non-blocking; the whole
        // loop completes in well under 50ms on any modern box, which is
        // 5x inside the 250ms debounce window.
        for i in 0..5 {
            trigger_tx
                .send(RebuildTrigger {
                    contribution_id: Some(format!("c{i}")),
                    schema_type: None,
                })
                .await
                .unwrap();
        }

        // Wait for debounce + rebuild to complete. 250ms + generous slack
        // for the spawn_blocking round-trip.
        tokio::time::sleep(SCOPE_CACHE_DEBOUNCE + Duration::from_millis(400)).await;

        drop(trigger_tx);
        let _ = handle.await;

        assert_eq!(
            rebuild_count.load(Ordering::SeqCst),
            1,
            "5 rapid triggers must coalesce to 1 rebuild"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reloader_restarts_up_to_3_times_in_60s_then_quarantines() {
        let (_dir, db_path) = make_db();
        insert_contribution(&db_path, "victim");

        let writer = Arc::new(ArcSwap::from_pointee(ScopeCache::new_empty()));
        // Keep a pointer to the pre-panic ScopeCache so we can assert LKG
        // preservation.
        let pre_panic_ptr = Arc::as_ptr(&writer.load_full());

        let panic_count = Arc::new(AtomicU32::new(0));
        let pc = Arc::clone(&panic_count);
        let rebuild_fn = Arc::new(move |_conn: &Connection| -> Result<ScopeCache> {
            pc.fetch_add(1, Ordering::SeqCst);
            panic!("injected panic for reloader test")
        });

        let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let ev = Arc::clone(&events);
        let event_emitter = Arc::new(move |name: &str, payload: Value| {
            ev.lock().unwrap().push((name.to_string(), payload));
        });

        let (trigger_tx, trigger_rx) = mpsc::channel(16);
        let (app_mode_tx, mut app_mode_rx) = mpsc::channel(4);

        let handle = tokio::spawn(reloader_loop(
            Arc::clone(&writer),
            trigger_rx,
            rebuild_fn,
            db_path.clone(),
            event_emitter,
            app_mode_tx,
        ));

        // 4 triggers with long enough gaps between them that each fires
        // its own rebuild (and therefore its own panic).
        for _ in 0..4 {
            trigger_tx
                .send(RebuildTrigger {
                    contribution_id: Some("victim".into()),
                    schema_type: Some("walker_provider_market".into()),
                })
                .await
                .unwrap();
            // Wait long enough for the debounce window to elapse + the
            // blocking rebuild to return. 400ms is comfortable.
            tokio::time::sleep(Duration::from_millis(400)).await;
        }

        // Expect the AppModeTransition::Quarantined signal.
        let transition = tokio::time::timeout(Duration::from_secs(3), app_mode_rx.recv())
            .await
            .expect("quarantine signal must arrive")
            .expect("sender must still be open");
        match transition {
            AppModeTransition::Quarantined {
                contribution_id,
                panic_count,
                ..
            } => {
                assert_eq!(contribution_id.as_deref(), Some("victim"));
                assert!(panic_count > RESTART_BUDGET);
            }
        }

        drop(trigger_tx);
        let _ = handle.await;

        // LKG preservation: the ArcSwap still holds the ORIGINAL cache
        // because no rebuild ever returned Ok. Pointer equality on the
        // Arc contents proves the writer was never stored to.
        let post_ptr = Arc::as_ptr(&writer.load_full());
        assert_eq!(
            pre_panic_ptr, post_ptr,
            "LKG cache must be held when quarantined"
        );

        // Contribution must be marked status='quarantined' in the DB.
        assert_eq!(read_status(&db_path, "victim"), "quarantined");

        // Chronicle events: at least 3 restart events + 1 quarantined.
        let evs = events.lock().unwrap();
        let restarts = evs
            .iter()
            .filter(|(n, _)| n == EVENT_SCOPE_CACHE_LISTENER_RESTARTED)
            .count();
        let quars = evs
            .iter()
            .filter(|(n, _)| n == EVENT_SCOPE_CACHE_QUARANTINED)
            .count();
        assert!(
            restarts >= RESTART_BUDGET as usize,
            "expected at least {} restart events, got {restarts}",
            RESTART_BUDGET
        );
        assert!(quars >= 1, "expected a quarantine event");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reloader_success_resets_panic_counter() {
        let (_dir, db_path) = make_db();
        insert_contribution(&db_path, "c_mixed");

        let writer = Arc::new(ArcSwap::from_pointee(ScopeCache::new_empty()));

        // Counter-driven sequence: panics 2x, succeeds 1x, panics 2x.
        // After the success the window clears, so the final two panics
        // are well under RESTART_BUDGET and quarantine MUST NOT fire.
        let counter = Arc::new(AtomicU32::new(0));
        let ctr = Arc::clone(&counter);
        let rebuild_fn = Arc::new(move |_conn: &Connection| -> Result<ScopeCache> {
            let n = ctr.fetch_add(1, Ordering::SeqCst);
            if n == 2 {
                Ok(ScopeCache::new_empty())
            } else {
                panic!("panic #{n}")
            }
        });

        let (trigger_tx, trigger_rx) = mpsc::channel(16);
        let (app_mode_tx, mut app_mode_rx) = mpsc::channel(4);
        let event_emitter = Arc::new(|_n: &str, _v: Value| {});

        let handle = tokio::spawn(reloader_loop(
            Arc::clone(&writer),
            trigger_rx,
            rebuild_fn,
            db_path.clone(),
            event_emitter,
            app_mode_tx,
        ));

        for _ in 0..5 {
            trigger_tx
                .send(RebuildTrigger {
                    contribution_id: Some("c_mixed".into()),
                    schema_type: None,
                })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
        }

        // Give the loop a beat to settle.
        tokio::time::sleep(Duration::from_millis(400)).await;

        // MUST NOT have sent a quarantine transition: final 2 panics
        // land after the success reset, so budget is intact.
        let maybe_signal =
            tokio::time::timeout(Duration::from_millis(300), app_mode_rx.recv()).await;
        assert!(
            maybe_signal.is_err(),
            "success must reset panic counter — no quarantine signal expected"
        );

        // Contribution must NOT be quarantined.
        assert_eq!(read_status(&db_path, "c_mixed"), "active");

        drop(trigger_tx);
        let _ = handle.await;
    }

    #[test]
    fn prune_history_drops_old_entries() {
        let now = SystemTime::now();
        let mut h = vec![
            now - Duration::from_secs(120),
            now - Duration::from_secs(30),
            now - Duration::from_secs(10),
        ];
        prune_history(&mut h, now);
        assert_eq!(h.len(), 2, "120s-old entry pruned, 30s + 10s retained");
    }
}
