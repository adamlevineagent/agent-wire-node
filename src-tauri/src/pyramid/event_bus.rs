use crate::pyramid::types::{BuildProgress, BuildProgressV2};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};

#[derive(Debug, Clone, Serialize)]
pub struct TaggedBuildEvent {
    pub slug: String,
    pub kind: TaggedKind,
}

/// Event kinds broadcast on the build event bus.
///
/// ## SlopeChanged trigger discipline (WS-EVENTS / v4 §15.21)
///
/// `SlopeChanged` is the cache-invalidation signal that WS-PRIMER and the
/// episodic-memory navigation page subscribe to. It MUST fire whenever the
/// leftmost slope of a pyramid changes. Required trigger points:
///
/// 1. A new L0 node lands (chain step at depth 0 saves a node, DADBEAR
///    ingest writes a new L0 chunk-node, vine bunch build completes).
/// 2. A node at depth 0 or 1 is mutated in place (edit via `save_node`'s
///    second-write path or `apply_supersession` on a depth-0-or-1 row).
/// 3. A provisional node is promoted to canonical at depth 0 or 1
///    (WS-PROVISIONAL: emit `ProvisionalPromoted` AND `SlopeChanged`).
/// 4. A chain-driven rebuild completes (chain_executor emits a final
///    `SlopeChanged` on normal termination as a catch-all).
/// 5. Supersession cascade touches a depth-0 or depth-1 row (delta landing
///    in `delta.rs`, apex rebuild in `build.rs`).
///
/// `affected_layers` lists the depth levels mutated (empty = "unknown
/// extent, revalidate everything").
///
/// IMPORTANT: Keep the outer `TaggedBuildEvent { slug, kind }` shape.
/// Add new variants HERE, not on a parallel enum.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaggedKind {
    // ── Existing ──────────────────────────────────────────────────────
    Progress { done: i64, total: i64 },
    V2Snapshot(BuildProgressV2),
    Resync,

    // ── Episodic memory v4 §15.21 ─────────────────────────────────────
    SlopeChanged { affected_layers: Vec<i64> },
    DeltaLanded { depth: i64, node_id: String },
    ApexHeadlineChanged { new_headline: String },
    CostUpdate { cost_so_far_usd: f64, estimate_usd: f64 },
    DeadLetterEnqueued { dead_letter_id: i64 },
    VocabularyPromoted { vocabulary_pyramid_slug: String },
    ProvisionalNodeAdded { node_id: String },
    ProvisionalPromoted { provisional_id: String, canonical_id: String },
    DemandGenStarted { sub_question: String, job_id: String },
    DemandGenCompleted { job_id: String, new_node_ids: Vec<String> },
    ChainProposalReceived { chain_id: String, proposal_id: i64 },

    // ── WS-INGEST-PRIMITIVE (Phase 1.5): ingest lifecycle events ──
    IngestScanComplete {
        new_count: usize,
        modified_count: usize,
        deleted_count: usize,
    },
    IngestStarted {
        source_path: String,
    },
    IngestComplete {
        source_path: String,
        build_id: String,
    },
    IngestFailed {
        source_path: String,
        error: String,
    },

    // ── WS-EVENTS step-level introspection (load-bearing for nav page) ──
    ChainStepStarted {
        step_name: String,
        step_idx: usize,
        primitive: String,
        depth: i64,
    },
    ChainStepFinished {
        step_name: String,
        step_idx: usize,
        status: String,
        elapsed_seconds: f64,
    },

    // ── Phase 4: Config Contribution Foundation ──────────────────────────
    /// Emitted by `config_contributions::sync_config_to_operational()`
    /// after a contribution successfully lands in its operational table.
    /// Consumed by Phase 13's build viz expansion. Phase 4 just emits it;
    /// no consumer exists yet.
    ///
    /// Outer `slug` on the `TaggedBuildEvent` envelope is the
    /// contribution's slug (empty string for global configs — the outer
    /// envelope requires a String, so global configs use "").
    ConfigSynced {
        /// Pyramid slug the contribution scopes to, or `None` for
        /// global configs.
        slug: Option<String>,
        /// `schema_type` discriminator — one of the 14 Phase 4 types.
        schema_type: String,
        /// Contribution UUID that was activated.
        contribution_id: String,
        /// Prior active contribution_id for this (slug, schema_type),
        /// if any. `None` on v1.
        prior_contribution_id: Option<String>,
    },

    // ── Phase 6: LLM Output Cache ─────────────────────────────────────
    /// Emitted when `call_model_unified_with_options_and_ctx` finds a
    /// valid cache entry for the current call and returns it without
    /// hitting the wire. Phase 13's build viz consumes this to render
    /// the step as "cached" instantly.
    CacheHit {
        slug: String,
        step_name: String,
        cache_key: String,
        chunk_index: Option<i64>,
        depth: i64,
    },
    /// Emitted when the cache lookup found no row for the current cache
    /// key — the call continues through the normal HTTP path. Optional
    /// telemetry; Phase 13's build viz may render it as a grey dot.
    CacheMiss {
        slug: String,
        step_name: String,
        cache_key: String,
        chunk_index: Option<i64>,
        depth: i64,
    },
    /// Emitted when `verify_cache_hit` returned anything other than
    /// `Valid` — the stale row has been deleted and the call falls
    /// through to HTTP. The `reason` field carries the mismatch tag
    /// (`mismatch_inputs` / `mismatch_prompt` / `mismatch_model` /
    /// `corrupted_output`) so the oversight page can surface the
    /// failure mode.
    CacheHitVerificationFailed {
        slug: String,
        step_name: String,
        cache_key: String,
        reason: String,
    },

    // ── Phase 11: OpenRouter Broadcast + Cost Reconciliation ─────────
    /// Emitted when a broadcast trace arrives with a cost that differs
    /// from the synchronous ledger by more than the policy ratio. The
    /// `pyramid_cost_log` row has been flipped to
    /// `reconciliation_status = 'discrepancy'` and the fail-loud
    /// oversight banner should surface this to the user. `actual_cost`
    /// is NOT silently rewritten — both values live on the row so the
    /// audit trail preserves the disagreement.
    CostReconciliationDiscrepancy {
        cost_log_id: i64,
        step_name: Option<String>,
        provider_id: Option<String>,
        synchronous_cost_usd: Option<f64>,
        broadcast_cost_usd: Option<f64>,
        discrepancy_ratio: Option<f64>,
    },
    /// Emitted by `sweep_broadcast_missing` when a synchronous row
    /// ages past the grace period without broadcast confirmation.
    /// Indicates either a tunnel outage or a provider-side dropped
    /// trace — the user needs to investigate.
    BroadcastMissing {
        rows_flipped: usize,
        grace_period_secs: i64,
    },
    /// Emitted when the webhook receives a broadcast whose metadata
    /// does not match any local `pyramid_cost_log` row. Primary
    /// credential-exfiltration indicator — the user's OpenRouter API
    /// key may be in use elsewhere.
    OrphanBroadcastDetected {
        orphan_id: i64,
        provider_id: Option<String>,
        generation_id: Option<String>,
        session_id: Option<String>,
        pyramid_slug: Option<String>,
        step_name: Option<String>,
        model: Option<String>,
        cost_usd: Option<f64>,
    },
    /// Emitted when `record_provider_error` transitions a provider to
    /// `degraded` or `down`, or when an admin acknowledges back to
    /// `healthy`. Carries the old and new states for the oversight UI.
    ProviderHealthChanged {
        provider_id: String,
        old_health: String,
        new_health: String,
        reason: String,
    },
}

impl TaggedKind {
    /// True for discrete, low-frequency events that should bypass the
    /// WebSocket coalesce buffer. Progress / V2Snapshot are high-frequency
    /// and still coalesced.
    pub fn is_discrete(&self) -> bool {
        !matches!(
            self,
            TaggedKind::Progress { .. } | TaggedKind::V2Snapshot(_)
        )
    }
}

pub struct BuildEventBus {
    pub tx: broadcast::Sender<TaggedBuildEvent>,
}

impl BuildEventBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(4096);
        Self { tx }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<TaggedBuildEvent> {
        self.tx.subscribe()
    }
}

impl Default for BuildEventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Creates an mpsc channel for BuildProgress AND spawns a relay task that
/// forwards every event onto the broadcast bus tagged with the given slug.
/// Per v3.3 B3 — call this at every build-launch site that previously did
/// `mpsc::channel::<BuildProgress>(64)` directly.
///
/// NOTE: this helper consumes the receiver internally, so it is only suitable
/// for sites that do not need a downstream consumer of the BuildProgress
/// stream. Sites that already have a desktop UI consumer reading from the
/// receiver should use [`tee_build_progress_to_bus`] instead.
pub fn spawn_build_progress_channel(
    bus: &BuildEventBus,
    slug: String,
) -> mpsc::Sender<BuildProgress> {
    let (tx, mut rx) = mpsc::channel::<BuildProgress>(256);
    let bus_tx = bus.tx.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let _ = bus_tx.send(TaggedBuildEvent {
                slug: slug.clone(),
                kind: TaggedKind::Progress {
                    done: p.done,
                    total: p.total,
                },
            });
        }
    });
    tx
}

/// Tee variant: takes ownership of an existing upstream `Receiver<BuildProgress>`,
/// spawns a relay task that forwards every event onto the broadcast bus tagged
/// with `slug`, AND returns a downstream receiver that yields the same events.
///
/// This is the minimum-friction substitution for build-launch sites that
/// previously did `let (tx, rx) = mpsc::channel(64)` and then read from `rx`
/// to drive the desktop UI / build status. Replace with:
///
/// ```ignore
/// let (progress_tx, raw_rx) = tokio::sync::mpsc::channel::<BuildProgress>(64);
/// let mut progress_rx = crate::pyramid::event_bus::tee_build_progress_to_bus(
///     &state.build_event_bus,
///     slug.clone(),
///     raw_rx,
/// );
/// ```
///
/// The desktop UI consumer continues reading from `progress_rx` exactly as
/// before; the bus tee is purely additive.
pub fn tee_build_progress_to_bus(
    bus: &BuildEventBus,
    slug: String,
    upstream: mpsc::Receiver<BuildProgress>,
) -> mpsc::Receiver<BuildProgress> {
    let (down_tx, down_rx) = mpsc::channel::<BuildProgress>(256);
    let bus_tx = bus.tx.clone();
    tokio::spawn(async move {
        let mut up = upstream;
        while let Some(p) = up.recv().await {
            // Mirror onto the broadcast bus first (lossy/best-effort).
            let _ = bus_tx.send(TaggedBuildEvent {
                slug: slug.clone(),
                kind: TaggedKind::Progress {
                    done: p.done,
                    total: p.total,
                },
            });
            // Then forward to the downstream consumer. If the downstream
            // consumer has dropped its receiver, stop relaying.
            if down_tx.send(p).await.is_err() {
                break;
            }
        }
    });
    down_rx
}
