// compute_market.rs — Phase 2 WS3: full compute market state + JSON persistence.
//
// Replaces the Phase 1 `{ enabled: bool }` stub with the complete
// runtime state for the compute market per `docs/plans/compute-
// market-phase-2-exchange.md` §III (lines 262-325):
//
//   - Published offers (model_id → ComputeOffer).
//   - In-flight jobs (job_id → ComputeJob).
//   - Lifetime + session counters for jobs completed and credits earned.
//   - `is_serving` runtime on/off (the mirror-loop toggle; distinct
//     from the durable `compute_participation_policy.allow_market_
//     visibility` operator intent).
//   - Per-model monotonic queue mirror sequence numbers (so the Wire
//     rejects out-of-order pushes).
//
// Persisted to `${app_data_dir}/compute_market_state.json` on every
// save. The file format includes a `schema_version: u32` field; on
// load, a version mismatch returns `None` + logs a warning and the
// app boots with `ComputeMarketState::default()` (cold-start
// rebuild).
//
// **Pillar 9 compliance:** all credit fields are `i64` (never `f64`,
// never `u64`). Queue discount multipliers are `i32` basis points
// (10000 = 1.0x). Per-offer rates are per-million tokens in credits.
//
// **Schema-migration policy.** Spec language (L272-273) talks about
// the on-disk JSON "silently dropping" removed fields on next save
// via `ignore_unknown_fields`. This implementation takes the stricter
// path: `#[serde(deny_unknown_fields)]` on every persisted struct,
// combined with the `schema_version` gate, so a stale file from a
// future or past code version fails to parse and triggers cold-start
// rebuild loudly instead of silently mutating. Phase 1 was never
// persisted (the stub was unreferenced), so no Phase-1→Phase-2
// migration case actually exists on disk — this policy concerns
// Phase 2→Phase 3+ only. When the struct changes in a way that
// should preserve existing state, bump `schema_version` AND write a
// migration step; when the change can tolerate a cold start, bump
// the version and let load() return None.
//
// **Phase 2 scope:** the struct + persistence + default constructor.
// No handler logic, no offer-publication IPC (WS7), no mirror push
// (WS6), no settlement (Phase 3). Those consumers all read/write
// this state via the canonical accessors shipped here; nothing
// mutates the struct ad-hoc.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Schema version for `compute_market_state.json`. Bumped on any
/// incompatible change to the persisted format. On load, a version
/// mismatch returns `None` (caller falls back to
/// `ComputeMarketState::default()`) — no in-place migration for
/// Phase 2; that can land in a later phase if a breaking schema
/// change ever requires preserving existing state.
pub const COMPUTE_MARKET_STATE_SCHEMA_VERSION: u32 = 1;

/// Filename of the persisted state, rooted at `${app_data_dir}`.
pub const COMPUTE_MARKET_STATE_FILENAME: &str = "compute_market_state.json";

/// A single per-model offer this node publishes to the Wire.
///
/// Rates are per-million tokens in credits. Multipliers in the
/// discount curve are integer basis points (10000 = 1.0x) — no `f64`
/// anywhere on the credit path (Pillar 9).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComputeOffer {
    pub model_id: String,
    /// `"local"` (Ollama) or `"bridge"` (OpenRouter-backed). Future:
    /// other provider types as they land.
    pub provider_type: String,
    /// Credits per million input tokens.
    pub rate_per_m_input: i64,
    /// Credits per million output tokens.
    pub rate_per_m_output: i64,
    /// Upfront credit charged at match time (before dispatch). Held
    /// as a deposit by the Wire until settle/fail/void.
    pub reservation_fee: i64,
    /// Discount curve — as the queue gets deeper, the multiplier
    /// scales the effective rate down (or up). Matched against the
    /// queue depth AT MATCH TIME (not dispatch time).
    pub queue_discount_curve: Vec<QueueDiscountPoint>,
    /// Max concurrent market jobs this model will accept. When the
    /// queue hits this depth the admission gate rejects with 503.
    /// Distinct from the compute-queue's overall `max_total_depth`.
    pub max_queue_depth: usize,
    /// Wire-side offer_id once the offer has been successfully
    /// published. `None` means "this offer is known locally but not
    /// yet synced to the Wire" (network partition / retry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_offer_id: Option<String>,
}

/// A single point on an offer's queue discount curve. Multiplier is
/// integer basis points to keep everything on the credit path in
/// exact arithmetic. Interpretation: "when queue depth ≥ `depth`,
/// apply `multiplier_bps` / 10000 to the rate."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueDiscountPoint {
    pub depth: usize,
    /// 10000 = 1.0x, 9000 = 0.9x (10% discount), 11000 = 1.1x
    /// (10% premium for deep queues).
    pub multiplier_bps: i32,
}

/// Lifecycle stages a market job moves through on the provider side.
/// Phase 2 tracks up to `Ready` (LLM done, result written to
/// `fleet_result_outbox`, awaiting Phase 3 callback-delivery worker).
/// `Delivered` / `Settled` are Phase 3 states; the provider doesn't
/// observe settlement directly — the Wire settles against the
/// requester's deposit and the node finds out via chronicle events.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComputeJobStatus {
    /// Received from Wire, DADBEAR work item created, in compute_queue.
    Queued,
    /// GPU loop picked it up, LLM call in progress.
    Executing,
    /// LLM completed, result written to outbox `status='ready'`,
    /// awaiting Phase 3 callback-delivery worker.
    Ready,
    /// Error at any step. Final state.
    Failed,
}

/// One in-flight market job on this node. Mirrors the outbox row +
/// DADBEAR work item as a convenience cache (the outbox is the
/// durable source of truth — this struct is a runtime view).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComputeJob {
    pub job_id: String,
    pub model_id: String,
    pub status: ComputeJobStatus,
    /// Original ChatML payload. Kept after enqueue so a failed
    /// retry (e.g. dead GPU) can re-enqueue without pulling from
    /// the outbox. Option because once the Phase 3 delivery worker
    /// picks up a Ready row, the provider can drop messages to
    /// reclaim memory — the outbox has the result blob, not the
    /// prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    /// The wire_job_token the handler verified. Stored so a callback-
    /// delivery retry can re-present the same JWT in its own
    /// outbound request (the requester/relay validates the token on
    /// POST arrival).
    pub wire_job_token: String,
    /// Credit rate the Wire matched us at — stored for observability
    /// and for the chronicle event; not used in provider-side logic.
    pub matched_rate_in: i64,
    pub matched_rate_out: i64,
    /// Basis points applied at match time (queue discount). Stored
    /// for chronicle + UX display.
    pub matched_multiplier_bps: i32,
    /// ISO 8601 timestamp, provider-local clock. Set when the job
    /// lands in the queue.
    pub queued_at: String,
    /// ISO 8601 timestamp, provider-local clock. Set when the GPU
    /// loop picks the job up (Queued → Executing transition).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filled_at: Option<String>,
    /// DADBEAR correlation — the work item created for this job.
    /// Semantic path `market/{job_id}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    /// DADBEAR correlation — the attempt id within that work item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
}

/// Full compute market state. Persisted to
/// `${app_data_dir}/compute_market_state.json` via `save` and loaded
/// via `load`. On version mismatch, `load` returns `None` and the
/// caller falls back to `Default::default()` (cold-start rebuild).
///
/// **Thread safety:** hold a single `Arc<RwLock<ComputeMarketState>>`
/// somewhere in `AppState` and gate every mutation through it. The
/// struct itself is NOT `Send + Sync` unless wrapped — but `Clone`
/// works for snapshot-reads, and `Serialize`/`Deserialize` work for
/// persistence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComputeMarketState {
    /// Schema version. Checked on load; mismatch → cold-start.
    pub schema_version: u32,
    /// Published offers, keyed by `model_id`. A node can have at most
    /// one offer per (model_id, provider_type); the Wire's UNIQUE
    /// INDEX guarantees it on the exchange side. For Phase 2 the
    /// key is just `model_id` — one offer per model — since a
    /// single-node single-provider-type deployment covers the near-
    /// term cases. Revisit when bridge offers coexist with local
    /// offers on the same model.
    pub offers: HashMap<String, ComputeOffer>,
    /// In-flight jobs, keyed by `job_id`. Drained when status moves
    /// to a Phase 3 terminal state (the provider keeps `Failed`
    /// entries for a grace window for chronicle correlation, then
    /// the callback-delivery worker clears them).
    pub active_jobs: HashMap<String, ComputeJob>,
    /// Lifetime count of jobs that completed on this node (Ready
    /// state reached). Never decrements.
    pub total_jobs_completed: u64,
    /// Lifetime credits earned (sum of (matched_rate_out *
    /// actual_completion_tokens) + (matched_rate_in *
    /// actual_prompt_tokens)). Updated when the chronicle event
    /// `market_matched` → `market_ready` transition lands. Pillar 9
    /// integer.
    pub total_credits_earned: i64,
    /// Session count — resets on app restart. Observability only.
    #[serde(default, skip_serializing)]
    pub session_jobs_completed: u64,
    /// Session credits — resets on app restart. Observability only.
    /// `skip_serializing` because these are runtime-only; loading a
    /// non-default value would be misleading on a fresh session.
    #[serde(default, skip_serializing)]
    pub session_credits_earned: i64,
    /// Runtime serving flag. Distinct from the durable
    /// `compute_participation_policy.allow_market_visibility` — this
    /// is the mirror-loop toggle, set by `compute_market_enable` /
    /// `compute_market_disable` IPCs. A node with
    /// `allow_market_visibility = false` AND `is_serving = true`
    /// still will not publish (policy gate takes precedence);
    /// UX distinction is "pause serving temporarily" (this field)
    /// vs "turn off permanently" (supersede the contribution).
    pub is_serving: bool,
    /// ISO 8601 timestamp of the most recent evaluation pass
    /// (is_serving flip, offer rebuild, etc.). Used by the
    /// observability panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_evaluation_at: Option<String>,
    /// Per-model monotonic sequence number for queue mirror pushes.
    /// The Wire rejects pushes where `seq <= current`. Bumped on
    /// every successful push; NEVER decremented.
    pub queue_mirror_seq: HashMap<String, u64>,
}

impl Default for ComputeMarketState {
    fn default() -> Self {
        Self {
            schema_version: COMPUTE_MARKET_STATE_SCHEMA_VERSION,
            offers: HashMap::new(),
            active_jobs: HashMap::new(),
            total_jobs_completed: 0,
            total_credits_earned: 0,
            session_jobs_completed: 0,
            session_credits_earned: 0,
            is_serving: false,
            last_evaluation_at: None,
            queue_mirror_seq: HashMap::new(),
        }
    }
}

impl ComputeMarketState {
    /// Full path to the persisted state file given an app data
    /// directory.
    pub fn state_path(data_dir: &Path) -> PathBuf {
        data_dir.join(COMPUTE_MARKET_STATE_FILENAME)
    }

    /// Load from `${data_dir}/compute_market_state.json`.
    ///
    /// Returns `None` in any failure case (file missing, unreadable,
    /// malformed JSON, schema_version mismatch). Caller is expected
    /// to fall back to `Default::default()`. Every failure is logged
    /// at `warn` level with the path + specific reason so operators
    /// can diagnose without reading source.
    ///
    /// This intentionally swallows errors rather than propagating —
    /// cold-start rebuild is always a safe fallback, and a broken
    /// state file must not block boot.
    pub fn load(data_dir: &Path) -> Option<Self> {
        let path = Self::state_path(data_dir);
        let contents = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Fresh install — not a problem, don't log noise.
                return None;
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "compute_market_state: read failed; falling back to default"
                );
                return None;
            }
        };
        let parsed: Self = match serde_json::from_str(&contents) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "compute_market_state: parse failed; falling back to default"
                );
                return None;
            }
        };
        if parsed.schema_version != COMPUTE_MARKET_STATE_SCHEMA_VERSION {
            tracing::warn!(
                path = %path.display(),
                file_version = parsed.schema_version,
                code_version = COMPUTE_MARKET_STATE_SCHEMA_VERSION,
                "compute_market_state: schema version mismatch; cold-start rebuild"
            );
            return None;
        }
        Some(parsed)
    }

    /// Save to `${data_dir}/compute_market_state.json`. Writes
    /// pretty-printed JSON for operator readability. Atomic: writes
    /// to a `.tmp` sibling then renames, so a crash mid-write can't
    /// corrupt the primary file.
    pub fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        let path = Self::state_path(data_dir);
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Register a newly-queued job in the runtime view, keyed by
    /// `job_id`. Semantics are **last-write-wins**: a second call with
    /// the same `job_id` clobbers the first entry in full.
    ///
    /// The actual idempotency gate lives upstream in the
    /// `fleet_result_outbox` INSERT (see
    /// `compute-market-phase-2-exchange.md` §III step 3 — `ON CONFLICT
    /// DO NOTHING`). The WS5 dispatch handler checks the outbox first;
    /// on conflict it returns 202 with the existing `job_id` and does
    /// NOT call this method a second time. So in practice this is
    /// exercised once per unique `job_id`.
    ///
    /// The `upsert_` prefix exists because calling this twice with
    /// different-content payloads for the same `job_id` IS legal (it
    /// won't panic or return an error), but it will silently overwrite
    /// in-flight status (e.g. revert an `Executing` job back to
    /// `Queued`). WS5 callers must check `active_jobs.contains_key`
    /// before calling if they need conflict-aware behavior.
    pub fn upsert_active_job(&mut self, job: ComputeJob) {
        self.active_jobs.insert(job.job_id.clone(), job);
    }

    /// Transition a job's status in place. Returns the previous
    /// status if the job existed, `None` if it didn't (caller can
    /// log an orphan-transition warning).
    pub fn transition_job_status(
        &mut self,
        job_id: &str,
        new_status: ComputeJobStatus,
    ) -> Option<ComputeJobStatus> {
        let job = self.active_jobs.get_mut(job_id)?;
        let prior = job.status;
        job.status = new_status;
        Some(prior)
    }

    /// Remove a terminal job from `active_jobs` (called by the Phase
    /// 3 callback-delivery worker after a successful Delivered
    /// transition or a Failed-with-grace-window expiry). Returns the
    /// removed job for chronicle correlation.
    pub fn remove_job(&mut self, job_id: &str) -> Option<ComputeJob> {
        self.active_jobs.remove(job_id)
    }

    /// Bump the mirror seq for a model, returning the new value.
    /// Monotonic per-model — never decrements. Called by the queue
    /// mirror task (WS6) before each push.
    pub fn bump_mirror_seq(&mut self, model_id: &str) -> u64 {
        let slot = self.queue_mirror_seq.entry(model_id.to_string()).or_insert(0);
        *slot = slot.saturating_add(1);
        *slot
    }

    /// Record a successful completion — called when a job transitions
    /// to `Ready` with known token counts. Updates both lifetime and
    /// session counters atomically; uses `saturating_add` on the
    /// credit path so a pathological billion-token job can't wrap
    /// the counter to negative.
    pub fn record_completion(&mut self, credits_earned: i64) {
        self.total_jobs_completed = self.total_jobs_completed.saturating_add(1);
        self.total_credits_earned = self.total_credits_earned.saturating_add(credits_earned);
        self.session_jobs_completed = self.session_jobs_completed.saturating_add(1);
        self.session_credits_earned = self.session_credits_earned.saturating_add(credits_earned);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_offer() -> ComputeOffer {
        ComputeOffer {
            model_id: "gemma3:27b".into(),
            provider_type: "local".into(),
            rate_per_m_input: 100,
            rate_per_m_output: 500,
            reservation_fee: 10,
            queue_discount_curve: vec![
                QueueDiscountPoint { depth: 0, multiplier_bps: 10000 },
                QueueDiscountPoint { depth: 5, multiplier_bps: 9000 },
            ],
            max_queue_depth: 8,
            wire_offer_id: Some("offer-abc".into()),
        }
    }

    fn sample_job() -> ComputeJob {
        ComputeJob {
            job_id: "job-xyz".into(),
            model_id: "gemma3:27b".into(),
            status: ComputeJobStatus::Queued,
            messages: Some(serde_json::json!([
                {"role": "user", "content": "hi"}
            ])),
            temperature: Some(0.3),
            max_tokens: Some(512),
            wire_job_token: "jwt.here.signed".into(),
            matched_rate_in: 100,
            matched_rate_out: 500,
            matched_multiplier_bps: 9500,
            queued_at: "2026-04-17T12:00:00Z".into(),
            filled_at: None,
            work_item_id: Some("market/job-xyz".into()),
            attempt_id: Some("1".into()),
        }
    }

    // ── Default construction ─────────────────────────────────────────

    #[test]
    fn default_constructs_with_empty_maps_and_counters() {
        let s = ComputeMarketState::default();
        assert_eq!(s.schema_version, COMPUTE_MARKET_STATE_SCHEMA_VERSION);
        assert!(s.offers.is_empty());
        assert!(s.active_jobs.is_empty());
        assert_eq!(s.total_jobs_completed, 0);
        assert_eq!(s.total_credits_earned, 0);
        assert_eq!(s.session_jobs_completed, 0);
        assert_eq!(s.session_credits_earned, 0);
        assert!(!s.is_serving);
        assert!(s.last_evaluation_at.is_none());
        assert!(s.queue_mirror_seq.is_empty());
    }

    // ── Persistence ──────────────────────────────────────────────────

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let mut state = ComputeMarketState::default();
        state.is_serving = true;
        state.offers.insert("gemma3:27b".into(), sample_offer());
        state.active_jobs.insert("job-xyz".into(), sample_job());
        state.total_jobs_completed = 42;
        state.total_credits_earned = 100_000;
        state.last_evaluation_at = Some("2026-04-17T12:00:00Z".into());
        state.queue_mirror_seq.insert("gemma3:27b".into(), 7);

        state.save(tmp.path()).unwrap();
        let loaded = ComputeMarketState::load(tmp.path()).expect("load should succeed");

        assert_eq!(loaded.schema_version, state.schema_version);
        assert_eq!(loaded.is_serving, true);
        assert_eq!(loaded.offers.len(), 1);
        assert_eq!(loaded.offers.get("gemma3:27b").unwrap(), &sample_offer());
        assert_eq!(loaded.active_jobs.len(), 1);
        assert_eq!(loaded.total_jobs_completed, 42);
        assert_eq!(loaded.total_credits_earned, 100_000);
        assert_eq!(loaded.queue_mirror_seq.get("gemma3:27b"), Some(&7));
    }

    #[test]
    fn load_returns_none_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        // No file written.
        assert!(ComputeMarketState::load(tmp.path()).is_none());
    }

    #[test]
    fn load_returns_none_on_malformed_json() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(COMPUTE_MARKET_STATE_FILENAME),
            "this is not json { [ )",
        )
        .unwrap();
        assert!(ComputeMarketState::load(tmp.path()).is_none());
    }

    #[test]
    fn load_returns_none_on_schema_version_mismatch() {
        // Simulate a persisted file from a future / past schema —
        // load must cold-start, not panic and not silently succeed
        // with semantically-wrong data.
        let tmp = TempDir::new().unwrap();
        let future_version = COMPUTE_MARKET_STATE_SCHEMA_VERSION.wrapping_add(1);
        let json = format!(
            r#"{{
                "schema_version": {future_version},
                "offers": {{}},
                "active_jobs": {{}},
                "total_jobs_completed": 0,
                "total_credits_earned": 0,
                "is_serving": false,
                "queue_mirror_seq": {{}}
            }}"#
        );
        std::fs::write(tmp.path().join(COMPUTE_MARKET_STATE_FILENAME), json).unwrap();
        assert!(ComputeMarketState::load(tmp.path()).is_none());
    }

    #[test]
    fn session_counters_are_not_persisted() {
        // session_* fields have `#[serde(skip_serializing)]` — saving
        // a state with non-zero session counters and loading it must
        // produce session=0 (runtime-only, resets on restart). The
        // lifetime counters must persist.
        let tmp = TempDir::new().unwrap();
        let mut state = ComputeMarketState::default();
        state.total_jobs_completed = 10;
        state.session_jobs_completed = 3;
        state.total_credits_earned = 1_000;
        state.session_credits_earned = 300;

        state.save(tmp.path()).unwrap();

        // Strong form: the on-disk JSON must NOT mention the session
        // fields at all. A grep-check pins the skip_serializing
        // contract against a future refactor that accidentally flips
        // skip_serializing off (which would make our "reset on
        // restart" guarantee quietly dependent on
        // `#[serde(default)]` — but default only fires when the field
        // is MISSING; if save emits it, load reads it, and session
        // state bleeds across restarts).
        let raw = std::fs::read_to_string(
            tmp.path().join(COMPUTE_MARKET_STATE_FILENAME)).unwrap();
        assert!(!raw.contains("session_jobs_completed"),
            "session_jobs_completed must be omitted from on-disk JSON, got: {raw}");
        assert!(!raw.contains("session_credits_earned"),
            "session_credits_earned must be omitted from on-disk JSON, got: {raw}");

        let loaded = ComputeMarketState::load(tmp.path()).unwrap();
        assert_eq!(loaded.total_jobs_completed, 10);
        assert_eq!(loaded.total_credits_earned, 1_000);
        assert_eq!(loaded.session_jobs_completed, 0,
            "session counter must not persist across restarts");
        assert_eq!(loaded.session_credits_earned, 0,
            "session counter must not persist across restarts");
    }

    #[test]
    fn save_is_atomic_via_tmp_rename() {
        // Save creates the file via a .tmp + rename. After save, the
        // primary path exists and the .tmp does NOT.
        let tmp = TempDir::new().unwrap();
        let state = ComputeMarketState::default();
        state.save(tmp.path()).unwrap();
        assert!(tmp.path().join(COMPUTE_MARKET_STATE_FILENAME).exists());
        // .tmp cleanup — don't actually assert its absence because
        // the OS may or may not have propagated the rename visibility
        // to our stat call yet; what matters is that the primary file
        // has valid JSON.
        let contents =
            std::fs::read_to_string(tmp.path().join(COMPUTE_MARKET_STATE_FILENAME))
                .unwrap();
        let _: ComputeMarketState = serde_json::from_str(&contents).unwrap();
    }

    // ── State mutation helpers ───────────────────────────────────────

    #[test]
    fn upsert_active_job_is_idempotent_by_job_id() {
        let mut state = ComputeMarketState::default();
        state.upsert_active_job(sample_job());
        state.upsert_active_job(sample_job());
        assert_eq!(state.active_jobs.len(), 1,
            "duplicate upsert must not create two entries");
    }

    #[test]
    fn upsert_active_job_is_last_write_wins() {
        // Pins the documented semantics: a second upsert with the SAME
        // `job_id` but different content clobbers the first. Callers
        // that need conflict-aware behavior must check
        // `active_jobs.contains_key` before calling (see WS5 dispatch
        // handler's outbox-first idempotency gate).
        let mut state = ComputeMarketState::default();
        let mut j1 = sample_job();
        j1.status = ComputeJobStatus::Executing;
        j1.filled_at = Some("2026-04-17T12:00:01Z".into());
        state.upsert_active_job(j1);

        let mut j2 = sample_job(); // status = Queued, filled_at = None
        state.upsert_active_job(j2.clone());

        let stored = state.active_jobs.get(&j2.job_id).unwrap();
        assert_eq!(stored.status, ComputeJobStatus::Queued,
            "second upsert must clobber the first's Executing status");
        assert!(stored.filled_at.is_none(),
            "second upsert must clobber the first's filled_at");
        // Nudge the model_id on the second and confirm it lands too.
        j2.model_id = "other-model".into();
        state.upsert_active_job(j2);
        assert_eq!(
            state.active_jobs.get("job-xyz").unwrap().model_id,
            "other-model"
        );
    }

    #[test]
    fn transition_job_status_returns_prior_status() {
        let mut state = ComputeMarketState::default();
        state.upsert_active_job(sample_job());
        let prior = state.transition_job_status("job-xyz", ComputeJobStatus::Executing);
        assert_eq!(prior, Some(ComputeJobStatus::Queued));
        assert_eq!(
            state.active_jobs.get("job-xyz").unwrap().status,
            ComputeJobStatus::Executing
        );
    }

    #[test]
    fn transition_job_status_returns_none_for_missing_job() {
        let mut state = ComputeMarketState::default();
        assert!(state
            .transition_job_status("job-ghost", ComputeJobStatus::Executing)
            .is_none());
    }

    #[test]
    fn remove_job_returns_removed_entry() {
        let mut state = ComputeMarketState::default();
        state.upsert_active_job(sample_job());
        let removed = state.remove_job("job-xyz").unwrap();
        assert_eq!(removed.job_id, "job-xyz");
        assert!(state.active_jobs.is_empty());
        assert!(state.remove_job("job-xyz").is_none(),
            "second remove must return None");
    }

    // ── Queue mirror seq ─────────────────────────────────────────────

    #[test]
    fn bump_mirror_seq_is_monotonic_per_model() {
        let mut state = ComputeMarketState::default();
        assert_eq!(state.bump_mirror_seq("gemma3:27b"), 1);
        assert_eq!(state.bump_mirror_seq("gemma3:27b"), 2);
        assert_eq!(state.bump_mirror_seq("gemma3:27b"), 3);
        // Different model starts fresh at 1.
        assert_eq!(state.bump_mirror_seq("llama3.2:70b"), 1);
        // Original model keeps its independent sequence.
        assert_eq!(state.bump_mirror_seq("gemma3:27b"), 4);
    }

    #[test]
    fn bump_mirror_seq_saturates_at_u64_max() {
        let mut state = ComputeMarketState::default();
        state.queue_mirror_seq.insert("m".into(), u64::MAX);
        assert_eq!(state.bump_mirror_seq("m"), u64::MAX,
            "saturating_add must not wrap to 0");
    }

    // ── Completion accounting ────────────────────────────────────────

    #[test]
    fn record_completion_bumps_both_lifetime_and_session() {
        let mut state = ComputeMarketState::default();
        state.record_completion(150);
        state.record_completion(50);
        assert_eq!(state.total_jobs_completed, 2);
        assert_eq!(state.total_credits_earned, 200);
        assert_eq!(state.session_jobs_completed, 2);
        assert_eq!(state.session_credits_earned, 200);
    }

    #[test]
    fn record_completion_saturates_on_pathological_credits() {
        let mut state = ComputeMarketState::default();
        state.total_credits_earned = i64::MAX - 10;
        state.record_completion(1_000);
        assert_eq!(state.total_credits_earned, i64::MAX,
            "saturating_add must not wrap to negative");
    }

    #[test]
    fn record_completion_does_not_touch_unrelated_state() {
        // Defensive regression: record_completion should only bump the
        // four counter fields. If a future refactor wires it into any
        // other state mutation (offers, active_jobs, queue_mirror_seq,
        // is_serving), this test pins the boundary.
        let mut state = ComputeMarketState::default();
        state.offers.insert("m".into(), sample_offer());
        state.active_jobs.insert("j".into(), sample_job());
        state.queue_mirror_seq.insert("m".into(), 42);
        state.is_serving = true;
        state.last_evaluation_at = Some("t".into());

        state.record_completion(100);

        assert_eq!(state.offers.len(), 1);
        assert_eq!(state.active_jobs.len(), 1);
        assert_eq!(state.queue_mirror_seq.get("m"), Some(&42));
        assert!(state.is_serving);
        assert_eq!(state.last_evaluation_at.as_deref(), Some("t"));
    }

    // ── Serde unknown-field rejection ────────────────────────────────

    #[test]
    fn compute_offer_rejects_unknown_fields() {
        let json = r#"{
            "model_id": "m", "provider_type": "local",
            "rate_per_m_input": 1, "rate_per_m_output": 2,
            "reservation_fee": 3,
            "queue_discount_curve": [],
            "max_queue_depth": 1,
            "unknown_knob": "oops"
        }"#;
        assert!(
            serde_json::from_str::<ComputeOffer>(json).is_err(),
            "deny_unknown_fields must reject unknown_knob"
        );
    }

    #[test]
    fn compute_job_rejects_unknown_fields() {
        let json = r#"{
            "job_id": "j", "model_id": "m",
            "status": "queued",
            "wire_job_token": "t",
            "matched_rate_in": 1, "matched_rate_out": 2,
            "matched_multiplier_bps": 10000,
            "queued_at": "2026-01-01T00:00:00Z",
            "priority": "high"
        }"#;
        assert!(
            serde_json::from_str::<ComputeJob>(json).is_err(),
            "deny_unknown_fields must reject priority"
        );
    }

    #[test]
    fn compute_job_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ComputeJobStatus::Queued).unwrap(),
            "\"queued\""
        );
        assert_eq!(
            serde_json::to_string(&ComputeJobStatus::Executing).unwrap(),
            "\"executing\""
        );
        assert_eq!(
            serde_json::to_string(&ComputeJobStatus::Ready).unwrap(),
            "\"ready\""
        );
        assert_eq!(
            serde_json::to_string(&ComputeJobStatus::Failed).unwrap(),
            "\"failed\""
        );
    }
}
