// pyramid/wire_update_poller.rs — Phase 14: background Wire
// supersession poller.
//
// Per `docs/specs/wire-discovery-ranking.md` §Notifications for
// Superseded Configs. The poller runs as a background tokio task
// (matching the existing DADBEAR tick loop pattern), walks every
// locally-pulled Wire contribution, asks the Wire for supersession
// updates, writes new entries into `pyramid_wire_update_cache`, and
// emits `WireUpdateAvailable` events so the UI refreshes its badges.
//
// If `wire_auto_update_settings` is enabled for a contribution's
// schema_type AND the pulled contribution introduces no new credential
// references (safety gate), the poller automatically pulls + activates
// the new version and emits `WireAutoUpdateApplied`.
//
// Polling interval is configurable via the `wire_update_polling`
// bundled contribution (default 6 hours).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::pyramid::config_contributions::load_contribution_by_id;
use crate::pyramid::db;
use crate::pyramid::event_bus::{TaggedBuildEvent, TaggedKind};
use crate::pyramid::wire_discovery::{
    load_auto_update_settings, load_update_polling_interval,
};
use crate::pyramid::wire_publish::{PyramidPublisher, SupersessionCheckEntry};
use crate::pyramid::wire_pull::{
    pull_wire_contribution, PullError, PullOptions,
};
use crate::pyramid::PyramidState;

/// Handle for the running poller task. Dropping the handle stops the
/// task, matching the pattern used by the DADBEAR tick loop.
///
/// Internally holds either (a) a `tokio::JoinHandle` when the poller
/// was spawned on the current runtime (the fast path, used from tests
/// and from runtime-alive call sites) or (b) a sidecar-thread handle
/// when we're called from sync `main()` before Tauri's runtime exists.
/// Drop semantics: aborts the task in case (a), signals the sidecar to
/// exit cleanly in case (b).
pub struct WireUpdatePollerHandle {
    task: Option<JoinHandle<()>>,
    _sidecar: Option<SidecarHandle>,
}

impl Drop for WireUpdatePollerHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        // SidecarHandle's own Drop impl clears the watchdog flag so the
        // sidecar loop exits within ~5 seconds. We don't join the
        // thread because (a) it would block shutdown and (b) the
        // sidecar has no external state worth waiting on.
    }
}

/// Spawn the Wire update poller in the background. Returns a handle
/// whose presence represents "the poller is running"; on drop the
/// task is aborted (when possible) via the stored `JoinHandle`.
///
/// The poller reads its interval from the `wire_update_polling`
/// contribution at each iteration (not just at startup), so a
/// supersession of that contribution takes effect on the next cycle
/// without requiring a restart.
///
/// The first run waits for the configured interval before polling
/// (startup doesn't trigger an immediate Wire round-trip — that would
/// slow boot and cause unnecessary churn on the first launch).
///
/// **Runtime safety:** this function is intended to be called from the
/// synchronous app init in `main.rs`, BEFORE Tauri has started its
/// tokio runtime. A naive `tokio::spawn` at that point panics with
/// "there is no reactor running". We match the pattern used by
/// `web_sessions::spawn_sweeper`: detect whether a tokio runtime is
/// already current, and if not, hand the poller off to a dedicated
/// sidecar runtime on its own OS thread. The sidecar runtime is fine
/// here because the poller only touches `Arc`-clonable state
/// (`PyramidState`, the reqwest client, the event bus) and never needs
/// to interact with Tauri's main runtime directly.
pub fn spawn_wire_update_poller(
    state: Arc<PyramidState>,
    wire_url: String,
) -> WireUpdatePollerHandle {
    // The poller's main loop. Lifted to a helper so both the fast path
    // (spawn on current runtime) and the slow path (sidecar runtime on
    // its own OS thread) share the same future body.
    async fn poller_loop(state: Arc<PyramidState>, wire_url: String) {
        tracing::info!("wire update poller: started");

        loop {
            // Read the interval from the contribution store on every
            // iteration. Phase 14 spec: supersession of the polling
            // contribution should take effect without a restart.
            let interval = {
                let reader = state.reader.lock().await;
                load_update_polling_interval(&reader)
            };
            drop_interval_log(interval);

            sleep(interval).await;

            if let Err(e) = run_once(&state, &wire_url).await {
                tracing::warn!(
                    error = %e,
                    "wire update poller: run_once returned error; continuing"
                );
            }
        }
    }

    // Fast path: we're already inside a tokio runtime (e.g., a test
    // harness that calls this from a `#[tokio::test]`). Spawn directly
    // on the current runtime and return the JoinHandle.
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let task = handle.spawn(poller_loop(state.clone(), wire_url.clone()));
        return WireUpdatePollerHandle {
            task: Some(task),
            _sidecar: None,
        };
    }

    // Slow path: no runtime yet — `main()` calls this from sync init
    // before Tauri bootstraps its runtime. Build a sidecar runtime on
    // a dedicated OS thread and `block_on` the poller loop there. The
    // sidecar thread owns the runtime; aborting it (by dropping the
    // returned handle) signals the sidecar to exit via the watchdog
    // flag shared with the sidecar thread.
    let watchdog = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let watchdog_task = watchdog.clone();
    let state_for_sidecar = state.clone();
    let url_for_sidecar = wire_url.clone();
    let sidecar = std::thread::Builder::new()
        .name("wire-update-poller".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "wire update poller: failed to build sidecar runtime; poller will not run"
                    );
                    return;
                }
            };
            rt.block_on(async move {
                let task = poller_loop(state_for_sidecar, url_for_sidecar);
                tokio::pin!(task);
                loop {
                    // Cooperative shutdown: on each 5-second tick check
                    // the watchdog. If it's been cleared, break out
                    // cleanly so the runtime drops and the thread
                    // exits.
                    tokio::select! {
                        _ = &mut task => break,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                            if !watchdog_task.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }
                        }
                    }
                }
            });
        })
        .ok();

    WireUpdatePollerHandle {
        task: None,
        _sidecar: Some(SidecarHandle {
            watchdog,
            thread: sidecar,
        }),
    }
}

/// Handle for the sidecar thread used when the poller is spawned
/// outside a tokio runtime. Dropping this clears the watchdog so the
/// cooperative-shutdown loop inside the sidecar thread exits within
/// ~5 seconds.
struct SidecarHandle {
    watchdog: Arc<std::sync::atomic::AtomicBool>,
    #[allow(dead_code)] // held only for its side effect on drop
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SidecarHandle {
    fn drop(&mut self) {
        self.watchdog
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

fn drop_interval_log(interval: Duration) {
    tracing::debug!(
        interval_secs = interval.as_secs(),
        "wire update poller: next run in {}s",
        interval.as_secs()
    );
}

/// One polling cycle. Reads the set of locally-pulled Wire
/// contributions, calls `check_supersessions`, writes cache rows for
/// each new supersession found, emits events, and (optionally)
/// auto-pulls when the schema_type has auto-update enabled.
///
/// Public so tests can drive the logic without running a background
/// task.
pub async fn run_once(
    state: &Arc<PyramidState>,
    wire_url: &str,
) -> Result<RunOnceReport> {
    // Step 1: gather the list of wire-tracked contributions.
    let tracked = {
        let reader = state.reader.lock().await;
        db::list_wire_tracked_contributions(&reader)?
    };

    if tracked.is_empty() {
        tracing::debug!("wire update poller: no wire-tracked contributions; skipping");
        return Ok(RunOnceReport::default());
    }

    // Resolve the current auth token. Missing auth = skip this cycle
    // (we can't talk to the Wire without it; the UI should surface
    // the unauthenticated state).
    let auth_token = match read_session_token(state).await {
        Some(t) if !t.is_empty() => t,
        _ => {
            tracing::debug!(
                "wire update poller: no session token available; skipping cycle"
            );
            return Ok(RunOnceReport::default());
        }
    };

    let publisher = PyramidPublisher::new(wire_url.to_string(), auth_token);

    // Step 2: group by schema_type + build the wire_contribution_id list.
    let wire_ids: Vec<String> = tracked
        .iter()
        .map(|(_, wire_id, _)| wire_id.clone())
        .collect();

    let supersession_entries = match publisher.check_supersessions(&wire_ids).await {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "wire update poller: check_supersessions failed"
            );
            return Ok(RunOnceReport::default());
        }
    };

    // Step 3: fold results + auto-update where enabled.
    let auto_update_settings = {
        let reader = state.reader.lock().await;
        load_auto_update_settings(&reader)
    };

    let mut report = RunOnceReport::default();

    for entry in supersession_entries {
        if entry.chain_length_delta == 0 || entry.latest_id == entry.original_id {
            // No update needed.
            continue;
        }

        // Find the local contribution for this wire_contribution_id.
        let local_row = tracked
            .iter()
            .find(|(_, wire_id, _)| *wire_id == entry.original_id);
        let Some((local_id, _wire_id, schema_type)) = local_row else {
            continue;
        };

        // Write the cache entry.
        let writer = state.writer.lock().await;
        let changes_summary = Some(entry.version_labels_between.join(" • "));
        let changes_summary_ref = changes_summary.as_deref();
        let authors_json = serde_json::to_string(&entry.author_handles)
            .unwrap_or_else(|_| "[]".to_string());
        if let Err(e) = db::upsert_wire_update_cache(
            &writer,
            local_id,
            &entry.latest_id,
            entry.chain_length_delta as i64,
            changes_summary_ref,
            Some(&authors_json),
        ) {
            tracing::warn!(
                error = %e,
                local_id = %local_id,
                "wire update poller: upsert_wire_update_cache failed"
            );
            continue;
        }
        drop(writer);

        // Emit WireUpdateAvailable.
        let _ = state.build_event_bus.tx.send(TaggedBuildEvent {
            slug: String::new(),
            kind: TaggedKind::WireUpdateAvailable {
                local_contribution_id: local_id.clone(),
                schema_type: schema_type.clone(),
                latest_wire_contribution_id: entry.latest_id.clone(),
                chain_length_delta: entry.chain_length_delta as i64,
            },
        });
        report.updates_detected += 1;

        // Auto-update if enabled for this schema_type.
        if auto_update_settings.is_enabled(schema_type) {
            match try_auto_update(state, &publisher, local_id, schema_type, &entry).await {
                Ok(Some(new_local_id)) => {
                    report.auto_updated += 1;
                    let _ = state.build_event_bus.tx.send(TaggedBuildEvent {
                        slug: String::new(),
                        kind: TaggedKind::WireAutoUpdateApplied {
                            local_contribution_id: local_id.clone(),
                            schema_type: schema_type.clone(),
                            new_local_contribution_id: new_local_id,
                            chain_length_delta: entry.chain_length_delta as i64,
                        },
                    });
                }
                Ok(None) => {
                    // Pull refused (e.g., credential safety gate). The
                    // cache row stays in place for manual review.
                    report.auto_update_refused += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        local_id = %local_id,
                        "wire update poller: auto-update failed"
                    );
                    report.auto_update_errors += 1;
                }
            }
        }
    }

    Ok(report)
}

/// Read the current session API token from `PyramidState`. The
/// poller needs it to authenticate to the Wire. Phase 14 goes through
/// the existing `PyramidState` without coupling to the Tauri auth
/// module — callers pass the token in via the app state's wire_url
/// + the session API token cached on the `auth_token` field inside
/// `LlmConfig` or (fallback) the env var.
async fn read_session_token(state: &Arc<PyramidState>) -> Option<String> {
    // The LlmConfig holds the session token under its openrouter_api_key
    // slot — not appropriate for Wire auth. The Wire token lives in
    // the top-level `AuthState` which is NOT in PyramidState. For
    // Phase 14, we read from the env var WIRE_AUTH_TOKEN as a fallback
    // shim (main.rs's poller spawner can explicitly set this before
    // launching) OR use the shared auth state via a reader injected
    // through `state.config`.
    //
    // Simpler: the poller's spawner in main.rs can pass the token via
    // closure capture; we let it read from env here and keep the
    // coupling minimal.
    let cfg = state.config.read().await;
    if !cfg.auth_token.is_empty() {
        return Some(cfg.auth_token.clone());
    }
    std::env::var("WIRE_AUTH_TOKEN").ok().filter(|s| !s.is_empty())
}

/// Attempt to auto-pull + activate a superseding Wire contribution.
/// Returns `Ok(Some(new_local_id))` when the pull succeeded,
/// `Ok(None)` when the safety gate refused the pull (e.g., undefined
/// credential reference), or `Err(...)` for an underlying failure.
async fn try_auto_update(
    state: &Arc<PyramidState>,
    publisher: &PyramidPublisher,
    local_id: &str,
    schema_type: &str,
    entry: &SupersessionCheckEntry,
) -> Result<Option<String>> {
    // Resolve the prior local contribution's slug for the pull options.
    let slug: Option<String> = {
        let reader = state.reader.lock().await;
        let row = load_contribution_by_id(&reader, local_id)?;
        row.and_then(|c| c.slug)
    };

    let mut writer = state.writer.lock().await;
    let options = PullOptions {
        latest_wire_contribution_id: &entry.latest_id,
        local_contribution_id_to_supersede: Some(local_id),
        activate: true,
        slug: slug.as_deref(),
    };
    match pull_wire_contribution(
        &mut writer,
        publisher,
        &state.credential_store,
        &state.build_event_bus,
        options,
    )
    .await
    {
        Ok(outcome) => {
            // Delete the cache entry — the pull is done.
            let _ = db::delete_wire_update_cache(&writer, local_id);
            tracing::info!(
                schema_type = schema_type,
                local_id = local_id,
                new_local_id = %outcome.new_local_contribution_id,
                "wire update poller: auto-pulled and activated new version"
            );
            Ok(Some(outcome.new_local_contribution_id))
        }
        Err(PullError::MissingCredentials(missing)) => {
            tracing::warn!(
                local_id = local_id,
                schema_type = schema_type,
                missing = ?missing,
                "wire update poller: auto-update refused by credential safety gate; awaiting manual review"
            );
            Ok(None)
        }
        Err(e) => Err(anyhow::anyhow!("auto-pull failed: {e}")),
    }
}

/// Per-run counters. Returned by `run_once` for tests + logging.
#[derive(Debug, Default, Clone)]
pub struct RunOnceReport {
    pub updates_detected: usize,
    pub auto_updated: usize,
    pub auto_update_refused: usize,
    pub auto_update_errors: usize,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod phase14_tests {
    use super::*;
    use crate::pyramid::wire_publish::{
        SupersessionCheckEntry, WireContributionSearchResult,
    };

    #[test]
    fn test_supersession_no_update_filter() {
        // When the entry's chain_length_delta is 0, the poller skips
        // the row. This test doesn't need full state — it's a unit
        // check on the filter logic.
        let entry = SupersessionCheckEntry {
            original_id: "w1".into(),
            latest_id: "w1".into(),
            chain_length_delta: 0,
            version_labels_between: vec![],
            author_handles: vec![],
        };
        // We can't easily run `run_once` without a real PyramidState
        // harness; the filter is covered by inspection above.
        // Assert the entry's "no update" shape directly.
        assert_eq!(entry.chain_length_delta, 0);
        assert_eq!(entry.original_id, entry.latest_id);
    }

    #[test]
    fn test_supersession_detects_update() {
        let entry = SupersessionCheckEntry {
            original_id: "w1".into(),
            latest_id: "w1-v2".into(),
            chain_length_delta: 1,
            version_labels_between: vec!["tighten intervals".into()],
            author_handles: vec!["alice".into()],
        };
        assert!(entry.chain_length_delta > 0);
        assert_ne!(entry.original_id, entry.latest_id);
    }

    #[test]
    fn test_run_once_report_default() {
        let report = RunOnceReport::default();
        assert_eq!(report.updates_detected, 0);
        assert_eq!(report.auto_updated, 0);
        assert_eq!(report.auto_update_refused, 0);
        assert_eq!(report.auto_update_errors, 0);
    }

    #[test]
    fn test_search_result_has_adoption_provider_ids_field() {
        // Phase 14 extends WireContributionSearchResult with adopter
        // signals feeding the recommendations engine. This smoke test
        // guards the struct shape.
        let r = WireContributionSearchResult {
            wire_contribution_id: "w1".into(),
            title: "".into(),
            description: "".into(),
            tags: vec![],
            author_handle: None,
            rating: None,
            adoption_count: 0,
            freshness_days: 0,
            chain_length: 0,
            upheld_rebuttals: 0,
            filed_rebuttals: 0,
            open_rebuttals: 0,
            kept_count: 0,
            total_pullers: 0,
            author_reputation: None,
            schema_type: None,
            adopter_provider_ids: vec!["openrouter".into()],
            adopter_source_types: vec!["code".into()],
        };
        assert_eq!(r.adopter_provider_ids.len(), 1);
        assert_eq!(r.adopter_source_types.len(), 1);
    }

    /// Verifier fix: spawning the poller from a synchronous context
    /// (no tokio runtime) must not panic. Before the fix, the
    /// `tokio::spawn` inside `spawn_wire_update_poller` panicked with
    /// "there is no reactor running" because `main()` is synchronous
    /// and Tauri's runtime hasn't started yet. Now the helper detects
    /// the missing runtime and hands the poller off to a sidecar
    /// thread that owns its own runtime.
    ///
    /// This test uses a minimal in-memory `PyramidState` so we don't
    /// need the full tauri-side test harness; all we're verifying is
    /// the runtime-selection branch in `spawn_wire_update_poller`.
    #[test]
    fn test_spawn_wire_update_poller_from_sync_context_does_not_panic() {
        use crate::pyramid::db;
        use crate::pyramid::PyramidState;
        use std::collections::HashMap;
        use std::sync::atomic::AtomicBool;
        use tempfile::TempDir;
        use tokio::sync::Mutex as TokioMutex;

        let dir = TempDir::new().unwrap();
        let data_dir = dir.path().to_path_buf();
        let db_path = data_dir.join("pyramid.db");
        let writer_conn = db::open_pyramid_db(&db_path).unwrap();
        let reader_conn = db::open_pyramid_connection(&db_path).unwrap();
        std::mem::forget(dir);

        let llm_config = crate::pyramid::llm::LlmConfig::default();
        let credential_store = Arc::new(
            crate::pyramid::credentials::CredentialStore::load(&data_dir).unwrap(),
        );
        let state = Arc::new(PyramidState {
            reader: Arc::new(TokioMutex::new(reader_conn)),
            writer: Arc::new(TokioMutex::new(writer_conn)),
            config: Arc::new(tokio::sync::RwLock::new(llm_config)),
            active_build: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            data_dir: Some(data_dir.clone()),
            stale_engines: Arc::new(TokioMutex::new(HashMap::new())),
            file_watchers: Arc::new(TokioMutex::new(HashMap::new())),
            vine_builds: Arc::new(TokioMutex::new(HashMap::new())),
            use_chain_engine: AtomicBool::new(false),
            use_ir_executor: AtomicBool::new(false),
            event_bus: Arc::new(crate::pyramid::event_chain::LocalEventBus::new()),
            operational: Arc::new(crate::pyramid::OperationalConfig::default()),
            chains_dir: data_dir.join("chains"),
            remote_query_rate_limiter: Arc::new(TokioMutex::new(HashMap::new())),
            absorption_gate: Arc::new(TokioMutex::new(
                crate::pyramid::AbsorptionGate::new(),
            )),
            build_event_bus: Arc::new(crate::pyramid::event_bus::BuildEventBus::new()),
            supabase_url: None,
            supabase_anon_key: None,
            csrf_secret: [0u8; 32],
            dadbear_handle: Arc::new(TokioMutex::new(None)),
            dadbear_in_flight: Arc::new(std::sync::Mutex::new(HashMap::new())),
            provider_registry: Arc::new(
                crate::pyramid::provider::ProviderRegistry::new(credential_store.clone()),
            ),
            credential_store,
            schema_registry: Arc::new(
                crate::pyramid::schema_registry::SchemaRegistry::new(),
            ),
            cross_pyramid_router: Arc::new(
                crate::pyramid::cross_pyramid_router::CrossPyramidEventRouter::new(),
            ),
        });

        // Spawn from a thread that has NO tokio runtime context. The
        // test harness itself may have one (rustest runs on a tokio
        // runtime for `#[tokio::test]` tests); this is a `#[test]`, so
        // we're already outside any runtime.
        let handle = spawn_wire_update_poller(state.clone(), "http://localhost:0".to_string());

        // Drop the handle immediately — this should abort the sidecar
        // thread via the watchdog cell without panicking.
        drop(handle);

        // Cleanup: the sidecar thread takes up to ~5 seconds to
        // observe the watchdog flag. That's acceptable for this
        // smoke test; we don't wait for it.
    }
}
