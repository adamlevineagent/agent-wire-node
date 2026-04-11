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

    // ── Phase 13: Build Viz Expansion ────────────────────────────────
    //
    // Per-call / per-step introspection events consumed by
    // `PyramidBuildViz.tsx` (step timeline) and
    // `CrossPyramidTimeline.tsx` (compact per-slug rows). Every variant
    // here is discrete — each event carries information that matters on
    // its own, so they naturally bypass the 60ms coalesce buffer. See
    // `docs/specs/build-viz-expansion.md` for the authoritative
    // contract.
    //
    // `slug` is duplicated inside each variant (and also sits on the
    // outer `TaggedBuildEvent` envelope) so downstream consumers that
    // only see the inner `TaggedKind` still have the full correlation
    // keys at hand. Keeping the field on each struct matches the
    // convention established by Phase 6's CacheHit/CacheMiss variants.

    /// Emitted just before `call_model_unified_with_options_and_ctx`
    /// dispatches the HTTP request for an LLM call. Carries the
    /// cache_key (so the UI can correlate with a prior CacheMiss),
    /// the resolved model id, and the tier the step routed through.
    LlmCallStarted {
        slug: String,
        build_id: String,
        step_name: String,
        primitive: String,
        model_tier: String,
        model_id: String,
        cache_key: String,
        depth: i64,
        chunk_index: Option<i64>,
    },

    /// Emitted after a successful LLM call returns. Carries tokens,
    /// estimated cost, and latency for the per-call sub-row in the
    /// step timeline. `cost_usd` is the estimated cost — the actual
    /// cost arrives later via `CostUpdate` or the reconciliation
    /// path in Phase 11.
    LlmCallCompleted {
        slug: String,
        build_id: String,
        step_name: String,
        cache_key: String,
        tokens_prompt: i64,
        tokens_completion: i64,
        cost_usd: f64,
        latency_ms: i64,
        model_id: String,
    },

    /// Emitted on every retry attempt inside the LLM retry loop.
    /// `attempt` is 1-indexed for the UI (attempt 1 is the first retry
    /// after the initial call, not the initial call itself).
    StepRetry {
        slug: String,
        build_id: String,
        step_name: String,
        attempt: i64,
        max_attempts: i64,
        error: String,
        backoff_ms: i64,
    },

    /// Emitted when an LLM call fails after all retries are
    /// exhausted, or when a step-level error occurs outside the
    /// retry loop.
    StepError {
        slug: String,
        build_id: String,
        step_name: String,
        error: String,
        depth: i64,
        chunk_index: Option<i64>,
    },

    /// Emitted at the start of a web-edge generation step (webbing).
    WebEdgeStarted {
        slug: String,
        build_id: String,
        step_name: String,
        source_node_count: i64,
    },

    /// Emitted after web-edge generation completes. `edges_created`
    /// is the number of edges that landed in `pyramid_edges`.
    WebEdgeCompleted {
        slug: String,
        build_id: String,
        step_name: String,
        edges_created: i64,
        latency_ms: i64,
    },

    /// Emitted at the start of an evidence-answering or triage
    /// batch. `action` is one of `"triage"`, `"answer"`, `"defer"`,
    /// `"skip"`.
    EvidenceProcessing {
        slug: String,
        build_id: String,
        step_name: String,
        question_count: i64,
        action: String,
        model_tier: String,
    },

    /// Emitted once per triage decision. `decision` is the tag from
    /// `TriageDecision::as_action_tag()` and `reason` is the matched
    /// rule or the default fallback description.
    TriageDecision {
        slug: String,
        build_id: String,
        step_name: String,
        item_id: String,
        decision: String,
        reason: String,
    },

    /// Emitted when gap processing is running. `action` is one of
    /// `"identify"`, `"fill"`, or `"defer"`.
    GapProcessing {
        slug: String,
        build_id: String,
        step_name: String,
        depth: i64,
        gap_count: i64,
        action: String,
    },

    /// Emitted when a recursive-cluster iteration produces a new
    /// cluster partition at a given depth.
    ClusterAssignment {
        slug: String,
        build_id: String,
        step_name: String,
        depth: i64,
        node_count: i64,
        cluster_count: i64,
    },

    /// Emitted by the reroll IPC after a new cache entry is written
    /// (and, for node reroll, after a change manifest is inserted).
    /// `new_cache_entry_id` is the id of the replacement row;
    /// `manifest_id` is `None` for intermediate-output (cache_key)
    /// reroll where no manifest is produced.
    NodeRerolled {
        slug: String,
        build_id: String,
        node_id: Option<String>,
        step_name: String,
        note: String,
        new_cache_entry_id: i64,
        manifest_id: Option<i64>,
    },

    /// Emitted for each cache entry marked stale by the downstream
    /// invalidation walker. `reason` is a short tag like `"reroll"`
    /// or `"upstream_reroll"`.
    CacheInvalidated {
        slug: String,
        build_id: String,
        cache_key: String,
        reason: String,
    },

    /// Emitted after a change manifest row lands in
    /// `pyramid_change_manifests`. `node_id` is the target node;
    /// `depth` is the node's depth.
    ManifestGenerated {
        slug: String,
        build_id: String,
        manifest_id: i64,
        depth: i64,
        node_id: String,
    },

    // ── Phase 14: Wire discovery + update notifications ──────────────
    /// Emitted by `WireUpdatePoller::run_once` when a newer Wire
    /// contribution is detected for a local contribution the user
    /// has already pulled. The frontend's My Tools tab listens for
    /// this event to refresh its update badges.
    WireUpdateAvailable {
        local_contribution_id: String,
        schema_type: String,
        latest_wire_contribution_id: String,
        chain_length_delta: i64,
    },
    /// Emitted by `WireUpdatePoller::run_once` when auto-update is
    /// enabled for a schema_type AND the poller successfully pulled +
    /// activated the new version. The frontend uses this to surface a
    /// non-modal toast / log entry so the user can see what changed.
    WireAutoUpdateApplied {
        local_contribution_id: String,
        schema_type: String,
        new_local_contribution_id: String,
        chain_length_delta: i64,
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

#[cfg(test)]
mod phase13_tests {
    use super::*;

    #[test]
    fn test_llm_call_started_serde_shape() {
        let kind = TaggedKind::LlmCallStarted {
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "extract".into(),
            primitive: "for_each".into(),
            model_tier: "fast_extract".into(),
            model_id: "inception/mercury-2".into(),
            cache_key: "abcd".into(),
            depth: 0,
            chunk_index: Some(3),
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["type"], "llm_call_started");
        assert_eq!(json["step_name"], "extract");
    }

    #[test]
    fn test_llm_call_completed_serde_shape() {
        let kind = TaggedKind::LlmCallCompleted {
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "extract".into(),
            cache_key: "abcd".into(),
            tokens_prompt: 100,
            tokens_completion: 50,
            cost_usd: 0.012,
            latency_ms: 3800,
            model_id: "inception/mercury-2".into(),
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["type"], "llm_call_completed");
        assert_eq!(json["tokens_prompt"], 100);
        assert_eq!(json["cost_usd"], 0.012);
    }

    #[test]
    fn test_step_retry_serde_shape() {
        let kind = TaggedKind::StepRetry {
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "n".into(),
            attempt: 2,
            max_attempts: 5,
            error: "HTTP 503".into(),
            backoff_ms: 2000,
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["type"], "step_retry");
        assert_eq!(json["attempt"], 2);
    }

    #[test]
    fn test_step_error_serde_shape() {
        let kind = TaggedKind::StepError {
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "n".into(),
            error: "permanent".into(),
            depth: 1,
            chunk_index: None,
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["type"], "step_error");
        assert!(json["chunk_index"].is_null());
    }

    #[test]
    fn test_web_edge_serde_shapes() {
        let started = TaggedKind::WebEdgeStarted {
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "l0_webbing".into(),
            source_node_count: 112,
        };
        let completed = TaggedKind::WebEdgeCompleted {
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "l0_webbing".into(),
            edges_created: 340,
            latency_ms: 2800,
        };
        assert_eq!(serde_json::to_value(&started).unwrap()["type"], "web_edge_started");
        assert_eq!(serde_json::to_value(&completed).unwrap()["type"], "web_edge_completed");
    }

    #[test]
    fn test_evidence_and_triage_serde() {
        let ev = TaggedKind::EvidenceProcessing {
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "answer".into(),
            question_count: 14,
            action: "triage".into(),
            model_tier: "fast_extract".into(),
        };
        let td = TaggedKind::TriageDecision {
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "triage".into(),
            item_id: "q-abc".into(),
            decision: "defer".into(),
            reason: "low_value".into(),
        };
        assert_eq!(serde_json::to_value(&ev).unwrap()["type"], "evidence_processing");
        assert_eq!(serde_json::to_value(&td).unwrap()["decision"], "defer");
    }

    #[test]
    fn test_gap_and_cluster_serde() {
        let gap = TaggedKind::GapProcessing {
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "process_gaps".into(),
            depth: 0,
            gap_count: 3,
            action: "fill".into(),
        };
        let cluster = TaggedKind::ClusterAssignment {
            slug: "s".into(),
            build_id: "b".into(),
            step_name: "recursive_cluster".into(),
            depth: 2,
            node_count: 24,
            cluster_count: 5,
        };
        assert_eq!(serde_json::to_value(&gap).unwrap()["type"], "gap_processing");
        assert_eq!(serde_json::to_value(&cluster).unwrap()["cluster_count"], 5);
    }

    #[test]
    fn test_reroll_and_invalidation_serde() {
        let reroll = TaggedKind::NodeRerolled {
            slug: "s".into(),
            build_id: "b".into(),
            node_id: Some("L0-001".into()),
            step_name: "synth".into(),
            note: "needs more detail".into(),
            new_cache_entry_id: 42,
            manifest_id: Some(7),
        };
        let inv = TaggedKind::CacheInvalidated {
            slug: "s".into(),
            build_id: "b".into(),
            cache_key: "abcdef".into(),
            reason: "upstream_reroll".into(),
        };
        let man = TaggedKind::ManifestGenerated {
            slug: "s".into(),
            build_id: "b".into(),
            manifest_id: 7,
            depth: 1,
            node_id: "L1-003".into(),
        };
        assert_eq!(serde_json::to_value(&reroll).unwrap()["type"], "node_rerolled");
        assert_eq!(serde_json::to_value(&reroll).unwrap()["new_cache_entry_id"], 42);
        assert_eq!(serde_json::to_value(&inv).unwrap()["type"], "cache_invalidated");
        assert_eq!(serde_json::to_value(&man).unwrap()["type"], "manifest_generated");
    }

    #[test]
    fn test_all_phase13_variants_are_discrete() {
        // Phase 13 variants are all discrete — each event matters
        // on its own, so they bypass the 60ms coalesce buffer.
        // Only Progress + V2Snapshot should be coalesced.
        let variants = vec![
            TaggedKind::LlmCallStarted {
                slug: "s".into(), build_id: "b".into(), step_name: "n".into(),
                primitive: "p".into(), model_tier: "t".into(), model_id: "m".into(),
                cache_key: "k".into(), depth: 0, chunk_index: None,
            },
            TaggedKind::WebEdgeStarted {
                slug: "s".into(), build_id: "b".into(), step_name: "n".into(),
                source_node_count: 1,
            },
            TaggedKind::ClusterAssignment {
                slug: "s".into(), build_id: "b".into(), step_name: "n".into(),
                depth: 0, node_count: 1, cluster_count: 1,
            },
            TaggedKind::NodeRerolled {
                slug: "s".into(), build_id: "b".into(), node_id: None,
                step_name: "n".into(), note: "".into(),
                new_cache_entry_id: 1, manifest_id: None,
            },
        ];
        for v in variants {
            assert!(v.is_discrete(), "variant should be discrete: {:?}", v);
        }
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
