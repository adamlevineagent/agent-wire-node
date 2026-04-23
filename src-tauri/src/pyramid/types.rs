// pyramid/types.rs — Data model structs for the Knowledge Pyramid engine
// DADBEAR: Detect, Accumulate, Debounce, Batch, Evaluate, Act, Recurse
// v0.2.0 — Live stale detection, FAQ generalization, cost observatory
//
// Types: SlugInfo, ContentType, PyramidNode, Topic, Correction, Decision, Term,
//        TreeNode, DrillResult, SearchHit, EntityEntry, BuildStatus, BuildProgress,
//        PyramidBatch, PendingMutation, AutoUpdateConfig, StaleCheckResult, etc.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlugInfo {
    pub slug: String,
    pub content_type: ContentType,
    pub source_path: String,
    pub node_count: i64,
    pub max_depth: i64,
    pub last_built_at: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub referenced_slugs: Vec<String>,
    #[serde(default)]
    pub referencing_slugs: Vec<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Code,
    Conversation,
    Document,
    Vine,
    Question,
}

impl ContentType {
    /// Convert to the lowercase string stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            ContentType::Code => "code",
            ContentType::Conversation => "conversation",
            ContentType::Document => "document",
            ContentType::Vine => "vine",
            ContentType::Question => "question",
        }
    }

    /// Parse from the lowercase string stored in SQLite.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "code" => Some(ContentType::Code),
            "conversation" => Some(ContentType::Conversation),
            "document" => Some(ContentType::Document),
            "vine" => Some(ContentType::Vine),
            "question" => Some(ContentType::Question),
            other => {
                tracing::warn!("Unknown content type: '{other}', returning None");
                None
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PyramidNode {
    pub id: String,
    pub slug: String,
    pub depth: i64,
    pub chunk_index: Option<i64>,
    pub headline: String,
    pub distilled: String,
    pub topics: Vec<Topic>,
    pub corrections: Vec<Correction>,
    pub decisions: Vec<Decision>,
    pub terms: Vec<Term>,
    pub dead_ends: Vec<String>,
    pub self_prompt: String,
    pub children: Vec<String>,
    pub parent_id: Option<String>,
    pub superseded_by: Option<String>,
    #[serde(default)]
    pub build_id: Option<String>,
    pub created_at: String,

    // ── WS-SCHEMA-V2 (§15.2): new canonical fields for episodic memory ──
    #[serde(default)]
    pub time_range: Option<TimeRange>,
    #[serde(default)]
    pub weight: f64,
    #[serde(default)]
    pub provisional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_from: Option<String>,
    #[serde(default)]
    pub narrative: NarrativeMultiZoom,
    #[serde(default)]
    pub entities: Vec<Entity>,
    #[serde(default)]
    pub key_quotes: Vec<KeyQuote>,
    #[serde(default)]
    pub transitions: Transitions,

    // ── WS-SCHEMA-V2 (§15.7): per-contribution supersession chain ──
    /// Current version number in `pyramid_node_versions`. Starts at 1 on
    /// first write; `apply_supersession` increments on every subsequent
    /// canonical write. Distinct from legacy `build_version` (build sweeps).
    #[serde(default = "default_current_version")]
    pub current_version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version_chain_phase: Option<String>,
}

fn default_current_version() -> i64 {
    1
}

/// WS-SCHEMA-V2 (§15.2): time anchor for a node (ISO timestamps).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimeRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
}

/// WS-SCHEMA-V2 (§15.2): multi-zoom narrative. Each level is a complete
/// narrative at a given zoom-out from the inputs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NarrativeMultiZoom {
    #[serde(default)]
    pub levels: Vec<NarrativeLevel>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NarrativeLevel {
    #[serde(default)]
    pub zoom: i64,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entity {
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub importance: f64,
    #[serde(default)]
    pub liveness: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyQuote {
    pub text: String,
    #[serde(default)]
    pub speaker: String,
    #[serde(default)]
    pub speaker_role: String,
    #[serde(default)]
    pub importance: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_ref: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Transitions {
    #[serde(default)]
    pub prior: String,
    #[serde(default)]
    pub next: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    // ── Load-bearing fields: Rust business logic reads these ──
    pub name: String,
    #[serde(default)]
    pub current: String,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub corrections: Vec<Correction>,
    #[serde(default)]
    pub decisions: Vec<Decision>,

    // ── Pass-through: everything else the LLM produces. ──
    // Prompts can add any fields (summary, current_dense, current_core,
    // future fields) without Rust changes. Stored in DB, served in API,
    // available to dehydration and cluster_item_fields — never read by Rust.
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Correction {
    pub wrong: String,
    pub right: String,
    pub who: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Decision {
    pub decided: String,
    pub why: String,
    #[serde(default)]
    pub rejected: String,
    /// WS-SCHEMA-V2 (§15.2): stance enum. One of:
    /// "committed" | "ruled_out" | "open" | "done" | "deferred" |
    /// "superseded" | "conditional" | "other".
    #[serde(default = "default_stance")]
    pub stance: String,
    /// WS-SCHEMA-V2 (§15.2): importance, 0..1.
    #[serde(default)]
    pub importance: f64,
    /// WS-SCHEMA-V2 (§15.2): related decisions/topics by canonical name.
    /// Renamed from `ties_to` to avoid collision with the plan-wide
    /// `ties_to → cross_refs` rename for cross-pyramid edges.
    #[serde(default)]
    pub related: Vec<String>,
}

fn default_stance() -> String {
    "open".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Term {
    pub term: String,
    pub definition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    pub id: String,
    pub depth: i64,
    pub headline: String,
    pub distilled: String,
    pub self_prompt: Option<String>,
    pub thread_id: Option<String>,
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_slug: Option<String>,
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrillResult {
    pub node: PyramidNode,
    pub children: Vec<PyramidNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_edges: Vec<ConnectedWebEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_web_edges: Vec<ConnectedRemoteWebEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<GapReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_context: Option<QuestionContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionContext {
    pub parent_question: Option<String>,
    pub sibling_questions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedWebEdge {
    pub connected_to: String,
    pub connected_headline: String,
    pub relationship: String,
    pub strength: f64,
}

/// A remote web edge for display in the drill view (WS-ONLINE-F).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedRemoteWebEdge {
    pub remote_handle_path: String,
    pub remote_slug: String,
    pub relationship: String,
    pub relevance: f64,
    pub build_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeWithWebEdges {
    #[serde(flatten)]
    pub node: PyramidNode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_edges: Vec<ConnectedWebEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub node_id: String,
    pub depth: i64,
    pub headline: String,
    pub snippet: String,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_slug: Option<String>,
    #[serde(default)]
    pub child_count: i64,
    #[serde(default)]
    pub annotation_count: i64,
    #[serde(default)]
    pub has_web_edges: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityEntry {
    pub name: String,
    pub nodes: Vec<String>,
    pub depths: Vec<i64>,
    pub topic_names: Vec<String>,
}

/// Per-step activity report for bounded builds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepActivity {
    pub name: String,
    /// One of: "ran", "reused", "skipped", "stopped"
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<i64>,
}

/// Canonical Audience contract (WS-AUDIENCE-CONTRACT, episodic-memory-vine
/// plan §15.1 / §16.1). Parsed from a top-level `audience:` block in chain
/// YAML and propagated into the chain resolution context as a structured
/// JSON object so step inputs and prompt templates can reference
/// `audience.role`, `audience.voice_hints`, etc. Field names are pinned by
/// the plan author and MUST NOT be renamed or reshaped. All fields are
/// optional at the YAML level — chains without an `audience:` block fall
/// back to `Audience::default()`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Audience {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub goals: Vec<String>,
    #[serde(default)]
    pub expertise: String,
    #[serde(default)]
    pub voice_hints: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildStatus {
    pub slug: String,
    /// One of: "idle", "running", "complete", "complete_with_errors", "failed", "cancelled"
    pub status: String,
    pub progress: BuildProgress,
    pub elapsed_seconds: f64,
    /// Number of nodes that failed during the build (LLM errors, timeouts, etc.)
    #[serde(default)]
    pub failures: i32,
    /// Per-step activity breakdown (populated after build completes)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepActivity>,
}

impl BuildStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "complete" | "complete_with_errors" | "failed" | "cancelled"
        )
    }

    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildProgress {
    pub done: i64,
    pub total: i64,
}

// ── Build Visualization V2 ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildProgressV2 {
    pub done: i64,
    pub total: i64,
    pub layers: Vec<LayerProgress>,
    pub current_step: Option<String>,
    pub log: Vec<LogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerProgress {
    pub depth: i64,
    pub step_name: String,
    pub estimated_nodes: i64,
    pub completed_nodes: i64,
    pub failed_nodes: i64,
    /// "pending" | "active" | "complete"
    pub status: String,
    /// Per-node detail for small layers (<=50 nodes). None for large layers.
    pub nodes: Option<Vec<NodeStatus>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatus {
    pub node_id: String,
    /// "complete" | "failed"
    pub status: String,
    /// Headline from PyramidNode, shown on hover.
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub elapsed_secs: f64,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct BuildLayerState {
    pub layers: Vec<LayerProgress>,
    pub current_step: Option<String>,
    pub log: std::collections::VecDeque<LogEntry>,
}

#[derive(Debug, Clone)]
pub enum LayerEvent {
    Discovered {
        depth: i64,
        step_name: String,
        estimated_nodes: i64,
    },
    /// Emitted when an individual node starts LLM processing (before the call).
    NodeStarted {
        depth: i64,
        step_name: String,
        node_id: String,
        /// Row id in pyramid_llm_audit (for in-flight prompt viewing).
        audit_id: Option<i64>,
    },
    NodeCompleted {
        depth: i64,
        step_name: String,
        node_id: String,
        label: Option<String>,
    },
    NodeFailed {
        depth: i64,
        step_name: String,
        node_id: String,
    },
    LayerCompleted {
        depth: i64,
        step_name: String,
    },
    StepStarted {
        step_name: String,
    },
    Log {
        elapsed_secs: f64,
        message: String,
    },
}

// ── Live Pyramid Theatre Types ──────────────────────────────────────────────

/// Full LLM audit record for the Inspector modal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmAuditRecord {
    pub id: i64,
    pub slug: String,
    pub build_id: String,
    pub node_id: Option<String>,
    pub step_name: String,
    pub call_purpose: String,
    pub depth: Option<i64>,
    pub model: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub raw_response: Option<String>,
    pub parsed_ok: bool,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub latency_ms: Option<i64>,
    pub generation_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    /// Phase 18b: distinguishes "served from cache" (`true`) from
    /// "served by HTTP call to provider" (`false`). Cache hits still
    /// write an audit row so the audit trail is contiguous and the
    /// DADBEAR Oversight page / cost reconciliation can show cache
    /// savings without losing audit-completeness.
    #[serde(default)]
    pub cache_hit: bool,
    /// Walker Re-Plan Wire 2.1 Wave 1 task 11: carries the WINNING entry's
    /// provider_id on success or the LAST-ATTEMPTED entry's provider_id on
    /// CallTerminal. Values include `"fleet"` / `"market"` (routing
    /// sentinels) or any registered provider row's id. NULL on legacy /
    /// pre-walker rows. `model` is preserved separately as the canonical
    /// model name and is never overwritten by a routing sentinel.
    #[serde(default)]
    pub provider_id: Option<String>,
}

/// Lightweight node info for the Theatre's live spatial view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveNodeInfo {
    pub node_id: String,
    pub depth: i64,
    pub headline: String,
    pub parent_id: Option<String>,
    pub children: Vec<String>,
    /// "complete" | "pending" | "superseded"
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PyramidBatch {
    pub id: i64,
    pub slug: String,
    pub batch_type: String,
    pub source_path: String,
    pub chunk_offset: i64,
    pub chunk_count: i64,
    pub created_at: String,
}

// ── Delta Chain Types ────────────────────────────────────────────────────────

/// Stable thread identity — maps a thread to its current canonical node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PyramidThread {
    pub slug: String,
    pub thread_id: String,
    pub thread_name: String,
    pub current_canonical_id: String,
    pub depth: i64,
    pub delta_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// A delta — incremental diff against the current understanding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delta {
    pub id: i64,
    pub slug: String,
    pub thread_id: String,
    pub sequence: i64,
    pub content: String,
    pub relevance: DeltaRelevance,
    pub source_node_id: Option<String>,
    pub flag: Option<String>,
    pub created_at: String,
}

/// Delta relevance level — self-assessed by the model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DeltaRelevance {
    Low,
    Medium,
    High,
    Critical,
}

impl Default for DeltaRelevance {
    fn default() -> Self {
        DeltaRelevance::Medium
    }
}

impl DeltaRelevance {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeltaRelevance::Low => "low",
            DeltaRelevance::Medium => "medium",
            DeltaRelevance::High => "high",
            DeltaRelevance::Critical => "critical",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "low" => DeltaRelevance::Low,
            "medium" => DeltaRelevance::Medium,
            "high" => DeltaRelevance::High,
            "critical" => DeltaRelevance::Critical,
            _ => DeltaRelevance::Medium,
        }
    }
}

/// Cumulative distillation — rolling understanding since last collapse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CumulativeDistillation {
    pub slug: String,
    pub thread_id: String,
    pub content: String,
    pub delta_count: i64,
    pub updated_at: String,
}

/// Result of a delta chain collapse operation (WS-COLLAPSE-EXTEND).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollapseResult {
    pub node_id: String,
    pub versions_before: i32,
    pub versions_after: i32,
    pub preserved: bool,
}

/// Record of a collapse event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollapseEvent {
    pub id: i64,
    pub slug: String,
    pub thread_id: String,
    pub old_canonical_id: String,
    pub new_canonical_id: String,
    pub deltas_absorbed: i64,
    pub model_used: String,
    pub elapsed_seconds: f64,
    pub created_at: String,
}

/// A web edge connecting two threads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebEdge {
    pub id: i64,
    pub slug: String,
    pub thread_a_id: String,
    pub thread_b_id: String,
    pub relationship: String,
    pub relevance: f64,
    pub delta_count: i64,
    pub build_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A delta on a web edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebEdgeDelta {
    pub id: i64,
    pub edge_id: i64,
    pub content: String,
    pub created_at: String,
}

/// Structured web edge note from distillation rewrite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebEdgeNote {
    pub thread_id: String,
    pub relationship: String,
}

/// A remote web edge referencing a node on another pyramid (WS-ONLINE-F).
///
/// Unlike local `WebEdge` which uses FK-constrained thread IDs, remote edges
/// store a Wire handle-path (`slug/depth/node-id`) pointing to a node on
/// another node's pyramid. The tunnel URL is stored so the build runner can
/// resolve the reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteWebEdge {
    pub id: i64,
    pub slug: String,
    pub local_thread_id: String,
    pub remote_handle_path: String,
    pub remote_tunnel_url: String,
    pub relationship: String,
    pub relevance: f64,
    pub delta_count: i64,
    pub build_id: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Parsed components of a three-segment Wire handle-path (`slug/depth/node-id`).
#[derive(Debug, Clone, PartialEq)]
pub struct HandlePath {
    pub slug: String,
    pub depth: String,
    pub node_id: String,
}

impl HandlePath {
    /// Parse a three-segment handle-path. Returns None if not exactly three segments.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.splitn(3, '/').collect();
        if parts.len() == 3 && !parts[0].is_empty() && !parts[1].is_empty() && !parts[2].is_empty()
        {
            Some(HandlePath {
                slug: parts[0].to_string(),
                depth: parts[1].to_string(),
                node_id: parts[2].to_string(),
            })
        } else {
            None
        }
    }

    /// Returns true if this handle-path references a remote pyramid (different slug).
    pub fn is_remote(&self, local_slug: &str) -> bool {
        self.slug != local_slug
    }
}

/// A usage log entry tracking pyramid read queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageLogEntry {
    pub id: i64,
    pub slug: String,
    pub query_type: String, // "search", "drill", "apex", "node", "entities", "corrections", "terms", "resolved", "tree"
    pub query_params: String, // JSON string of the query details
    pub result_node_ids: String, // JSON array of node IDs returned
    pub agent_id: Option<String>, // From X-Agent-Id header
    pub created_at: String,
}

/// An annotation on a pyramid node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PyramidAnnotation {
    pub id: i64,
    pub slug: String,
    pub node_id: String,
    pub annotation_type: AnnotationType,
    pub content: String,
    pub question_context: Option<String>,
    pub author: String,
    pub created_at: String,
}

/// Annotation type as a vocab-validated newtype wrapper.
///
/// Phase 6c-B: this was an 11-variant Rust enum until v5; the enum was the
/// source of truth AND parsers/dispatch arms encoded type-specific behavior
/// in match-legs. That made the registry (Phase 6c-A) dead plumbing —
/// publishing a new vocab entry had zero runtime effect.
///
/// Now the newtype wraps the canonical string and the
/// `pyramid_config_contributions` vocabulary registry is the authoritative
/// list. `from_str_strict(conn, s)` validates against the registry so an
/// agent who publishes a new vocab entry for `my_new_type` can POST an
/// annotation with that type the moment the entry is active — no code
/// deploy.
///
/// Serde transparent keeps wire compat: it serializes to / deserializes
/// from the plain string. The DB column stores the same string via
/// `as_str()`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnnotationType(String);

// ── Canonical string constants for the 11 genesis annotation types ──
//
// These are NOT an authoritative list (the vocabulary registry is) — they
// exist so internal Rust call sites that already know a specific genesis
// type (vine.rs writing an ERA annotation, `emit_annotation_observation_events`
// detecting "correction") don't have to hit the vocab cache just to spell
// the canonical string. Adding a new vocab type via contribution does NOT
// require adding a constant here; these are convenience for the baked-in
// names that predate the registry.
pub const ANNOTATION_TYPE_OBSERVATION: &str = "observation";
pub const ANNOTATION_TYPE_CORRECTION: &str = "correction";
pub const ANNOTATION_TYPE_QUESTION: &str = "question";
pub const ANNOTATION_TYPE_FRICTION: &str = "friction";
pub const ANNOTATION_TYPE_IDEA: &str = "idea";
pub const ANNOTATION_TYPE_ERA: &str = "era";
pub const ANNOTATION_TYPE_TRANSITION: &str = "transition";
pub const ANNOTATION_TYPE_HEALTH_CHECK: &str = "health_check";
pub const ANNOTATION_TYPE_DIRECTORY: &str = "directory";
pub const ANNOTATION_TYPE_STEEL_MAN: &str = "steel_man";
pub const ANNOTATION_TYPE_RED_TEAM: &str = "red_team";

/// Raised when a string does not match a known annotation type.
/// Used by `AnnotationType::from_str_strict`. Production write paths must
/// use the strict form so unknown types raise (Pillar 38 absorbed bug:
/// the old lossy `from_str` silently mapped unknown → Observation). Legacy
/// read paths use `AnnotationType::from_db_string` which wraps whatever
/// the DB had, knowing the DB column can only contain strings that were
/// accepted at write time.
#[derive(Debug, thiserror::Error)]
#[error("Unknown annotation type: '{0}'")]
pub struct UnknownAnnotationType(pub String);

// ── FAQ Types ────────────────────────────────────────────────────────────────

/// A FAQ node — aggregated question/answer derived from annotations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaqNode {
    pub id: String, // "FAQ-{uuid}" format
    pub slug: String,
    pub question: String,              // The canonical question
    pub answer: String,                // Accumulated answer from annotations
    pub related_node_ids: Vec<String>, // Pyramid nodes that help answer this
    pub annotation_ids: Vec<i64>,      // Annotation IDs that contributed to this FAQ
    pub hit_count: i64,                // Times this FAQ was matched by a query
    #[serde(default)]
    pub match_triggers: Vec<String>, // Trigger patterns for auto-matching
    pub created_at: String,
    pub updated_at: String,
}

impl AnnotationType {
    /// Raw constructor — wraps an arbitrary string. Use for internal call
    /// sites that already know the string is a canonical genesis name
    /// (e.g. `vine.rs` building an `era` annotation, DB read path
    /// re-wrapping a value that was validated at write time). Write paths
    /// that accept external input (HTTP, MCP CLI) MUST use
    /// `from_str_strict` instead so unknown values raise.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The canonical string form. Stored in `pyramid_annotations.annotation_type`
    /// and used as the wire-protocol string in JSON.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the inner string.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Wrap a value read from the DB. Does NOT validate against the
    /// vocabulary — the DB column can only contain strings that were
    /// accepted at write time (write paths use `from_str_strict`). Kept
    /// as a named helper so the intent is clear at read call sites vs
    /// the generic `new` constructor.
    pub fn from_db_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Strict parse — validates `s` against the vocabulary registry
    /// (`pyramid_config_contributions` rows with
    /// `schema_type LIKE 'vocabulary_entry:annotation_type:%'`). Unknown
    /// values raise `UnknownAnnotationType`. Use in every write path
    /// (CLI, MCP, HTTP) so agents publishing new vocab entries can
    /// IMMEDIATELY post annotations of that type without a code deploy —
    /// the whole point of Phase 6c-B.
    ///
    /// Pillar 38 absorbed bug: the pre-v5 lossy `from_str` silently
    /// defaulted unknowns to Observation; the strict form refuses.
    pub fn from_str_strict(
        conn: &rusqlite::Connection,
        s: &str,
    ) -> std::result::Result<Self, UnknownAnnotationType> {
        match super::vocab_entries::get_vocabulary_entry(
            conn,
            super::vocab_entries::VOCAB_KIND_ANNOTATION_TYPE,
            s,
        ) {
            Ok(Some(_)) => Ok(Self(s.to_string())),
            Ok(None) => Err(UnknownAnnotationType(s.to_string())),
            Err(e) => {
                // Vocabulary read failure should not silently-accept
                // unknown strings. Log the DB error and refuse — the
                // caller sees UnknownAnnotationType which surfaces in
                // HTTP as 400, and the DB error is in the trace.
                tracing::error!(
                    "vocabulary lookup failed while validating annotation_type '{s}': {e}"
                );
                Err(UnknownAnnotationType(s.to_string()))
            }
        }
    }

    /// Active set of annotation types from the vocabulary registry,
    /// sorted by name. Replaces the pre-v5 `AnnotationType::ALL`
    /// static slice. If vocab lookup fails, returns an empty Vec plus
    /// an error — callers that want the bare list (e.g. error-message
    /// rendering in HTTP 400 response) should fall back to the
    /// genesis constants on error.
    pub fn all(conn: &rusqlite::Connection) -> anyhow::Result<Vec<Self>> {
        let entries = super::vocab_entries::list_vocabulary(
            conn,
            super::vocab_entries::VOCAB_KIND_ANNOTATION_TYPE,
        )?;
        Ok(entries.into_iter().map(|e| Self(e.name)).collect())
    }
}

impl std::fmt::Display for AnnotationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── FAQ Category / Directory Types ────────────────────────────────────────────

/// A FAQ category — grouping of related FAQ nodes with a distilled summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaqCategory {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub distilled_summary: String,
    pub faq_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The FAQ directory — flat or hierarchical depending on FAQ count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaqDirectory {
    pub slug: String,
    pub mode: String, // "flat" or "hierarchical"
    pub total_faqs: i64,
    pub categories: Vec<FaqCategoryEntry>,
    pub uncategorized: Vec<FaqNode>, // FAQs not in any category, or ALL faqs in flat mode
}

/// A single category entry in the directory (with optional children on drill).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaqCategoryEntry {
    pub category: FaqCategory,
    pub faq_count: i64,
    pub children: Option<Vec<FaqNode>>, // populated on drill
}

// ── v4.2 Auto-Update & Stale-Check Types ─────────────────────────────────────

/// WAL entry for crash recovery of pending mutations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingMutation {
    pub id: i64,
    pub slug: String,
    pub layer: i32,
    pub mutation_type: String,
    pub target_ref: String,
    pub detail: Option<String>,
    pub cascade_depth: i32,
    pub detected_at: String,
    pub processed: bool,
    pub batch_id: Option<String>,
}

/// Per-pyramid auto-update configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoUpdateConfig {
    pub slug: String,
    pub auto_update: bool,
    pub debounce_minutes: i32,
    pub min_changed_files: i32,
    pub runaway_threshold: f64,
    pub breaker_tripped: bool,
    pub breaker_tripped_at: Option<String>,
    pub frozen: bool,
    pub frozen_at: Option<String>,
}

/// Result of a stale-check on a node or file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleCheckResult {
    pub id: i64,
    pub slug: String,
    pub batch_id: String,
    pub layer: i32,
    pub target_id: String,
    /// Integer stale value: 0=no, 1=yes, 2=new, 3=deleted, 4=renamed, 5=skipped
    pub stale: i32,
    pub reason: String,
    pub checker_index: i32,
    pub checker_batch_size: i32,
    pub checked_at: String,
    pub cost_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    /// Cascade depth from the source mutation, used for propagation tracking.
    /// Populated from the PendingMutation that triggered this check.
    pub cascade_depth: i32,
}

/// Result of a connection carryforward check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionCheckResult {
    pub id: i64,
    pub slug: String,
    pub supersession_node_id: String,
    pub new_node_id: String,
    pub connection_type: String,
    pub connection_id: String,
    pub still_valid: bool,
    pub reason: String,
    pub checked_at: String,
}

/// File hash tracking entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHash {
    pub slug: String,
    pub file_path: String,
    pub hash: String,
    pub chunk_count: i32,
    pub node_ids: Vec<String>,
    pub last_ingested_at: String,
}

/// Token usage from an LLM API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

// ── Stale-check helper response types (parsed from LLM JSON output) ──────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStaleResult {
    pub file_path: String,
    pub stale: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStaleResult {
    pub node_id: String,
    pub stale: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionResult {
    pub connection_id: String,
    pub still_valid: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameResult {
    pub rename: bool,
    pub reason: String,
}

// ── Vine Types ──────────────────────────────────────────────────────────────

/// A bunch in the vine — one complete conversation pyramid from a single session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineBunch {
    pub id: i64,
    pub vine_slug: String,
    pub bunch_slug: String,
    pub session_id: String,
    pub jsonl_path: String,
    pub bunch_index: i64,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    pub message_count: Option<i64>,
    pub chunk_count: Option<i64>,
    pub apex_node_id: Option<String>,
    pub penultimate_node_ids: Vec<String>,
    pub status: String,
    pub metadata: Option<VineBunchMetadata>,
}

/// Metadata extracted from a bunch's apex + penultimate layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineBunchMetadata {
    pub topics: Vec<String>,
    pub entities: Vec<String>,
    pub decisions: Vec<VineDecision>,
    pub corrections: Vec<VineCorrection>,
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub penultimate_summaries: Vec<String>,
}

/// Decision with temporal context for cross-bunch evolution chains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineDecision {
    pub decision: Decision,
    pub bunch_index: i64,
    pub bunch_ts: String,
}

/// Correction with temporal context for cross-bunch correction chains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineCorrection {
    pub correction: Correction,
    pub bunch_index: i64,
    pub bunch_ts: String,
}

/// Discovery result for a JSONL conversation file.
#[derive(Debug, Clone)]
pub struct BunchDiscovery {
    pub session_id: String,
    pub jsonl_path: std::path::PathBuf,
    pub first_ts: String,
    pub last_ts: String,
    pub message_count: i64,
}

// ── Evidence System Types (Phase 1 — Question Pyramid) ────────────────────────

/// Verdict for an evidence link between two pyramid nodes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum EvidenceVerdict {
    Keep,
    Disconnect,
    Missing,
}

impl EvidenceVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            EvidenceVerdict::Keep => "KEEP",
            EvidenceVerdict::Disconnect => "DISCONNECT",
            EvidenceVerdict::Missing => "MISSING",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "KEEP" => EvidenceVerdict::Keep,
            "DISCONNECT" => EvidenceVerdict::Disconnect,
            "MISSING" => EvidenceVerdict::Missing,
            other => {
                tracing::warn!("Unknown evidence verdict: '{other}', defaulting to Keep");
                EvidenceVerdict::Keep
            }
        }
    }
}

/// A weighted evidence link between a source node (evidence provider) and a
/// target node (question answerer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceLink {
    pub slug: String,
    pub source_node_id: String, // child node (evidence provider)
    pub target_node_id: String, // parent node (question answerer)
    pub verdict: EvidenceVerdict,
    pub weight: Option<f64>, // 0.0-1.0, None for DISCONNECT/MISSING
    pub reason: Option<String>,
    #[serde(default)]
    pub build_id: Option<String>,
    #[serde(default)]
    pub live: Option<bool>,
}

// ── Evidence Answering Types (Phase 1, Steps 3.1–3.2) ─────────────────────────

/// A question for a specific layer of the pyramid, produced by the question compiler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerQuestion {
    pub question_id: String,
    pub question_text: String,
    pub layer: i64,
    /// What this question is about (e.g., "trust boundaries", "error handling").
    pub about: String,
    /// What answering this question creates (e.g., "a synthesized view of auth flow").
    pub creates: String,
}

/// Result of the horizontal pre-mapping step: question_id → candidate node IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateMap {
    pub mappings: HashMap<String, Vec<String>>,
}

/// A question that has been answered with evidence verdicts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnsweredNode {
    /// The pyramid node created from answering the question.
    pub node: PyramidNode,
    /// Evidence links (KEEP/DISCONNECT verdicts) from the answering step.
    pub evidence: Vec<EvidenceLink>,
    /// Descriptions of missing evidence the LLM wished it had.
    pub missing: Vec<String>,
}

/// Result of an answer_questions batch, including both successes and failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerBatchResult {
    /// Successfully answered questions.
    pub answered: Vec<AnsweredNode>,
    /// Questions that failed to answer (question_id, error description).
    /// Callers should persist these as gap reports.
    pub failed: Vec<FailedQuestion>,
}

/// A question that failed during the answering step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedQuestion {
    pub question_id: String,
    pub question_text: String,
    pub layer: i64,
    pub error: String,
}

// ── Characterization ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterizationResult {
    pub material_profile: String,
    pub interpreted_question: String,
    pub audience: String,
    pub tone: String,
}

// ── Reconciliation ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationResult {
    pub orphans: Vec<String>,             // node IDs never referenced
    pub gaps: Vec<GapReport>,             // MISSING evidence reports
    pub central_nodes: Vec<String>,       // high-citation nodes
    pub weight_map: HashMap<String, f64>, // node_id → aggregate weight
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapReport {
    pub question_id: String,
    pub description: String,
    pub layer: i64,
    /// Backward compat: kept for serde deserialization of existing data
    #[serde(default)]
    pub resolved: bool,
    /// 0.0 = completely open, 1.0 = definitively answered. Threshold for "resolved" display: 0.8.
    #[serde(default)]
    pub resolution_confidence: f64,
}

/// A group of targeted L0 nodes sharing the same triggering question (self_prompt).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceSet {
    pub self_prompt: String,
    pub member_count: i64,
    pub index_headline: Option<String>,
}

// ── Publication ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationManifest {
    pub slug: String,
    pub layer: i64,
    pub nodes_to_publish: Vec<String>, // non-orphan node IDs
    pub skipped_orphans: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdMapping {
    pub local_id: String,
    pub wire_handle_path: String,
    pub wire_uuid: Option<String>,
    pub published_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedFromEntry {
    pub ref_path: String,    // handle-path or corpus path
    pub source_type: String, // "contribution" or "source_document"
    pub weight: f64,         // 0.0-1.0
    pub justification: Option<String>,
}

// ── Extraction Schema (Phase 1, Step 1.3 — Dynamic Prompt Generation) ─────────

/// Dynamic extraction schema generated from leaf questions BEFORE L0 extraction.
/// This is the critical quality lever — makes extraction question-shaped instead of generic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionSchema {
    /// Question-shaped extraction prompt: tells L0 exactly what to look for in each source file.
    /// NOT "list every function" — specifically what the downstream questions need.
    pub extraction_prompt: String,
    /// Topic fields that each extracted node should contain. Varies by question domain.
    pub topic_schema: Vec<TopicField>,
    /// Orientation guidance: how detailed, what tone, what to emphasize.
    pub orientation_guidance: String,
}

/// A field in the dynamic topic schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicField {
    /// Machine-friendly name (e.g., "trust_boundaries", "user_features").
    pub name: String,
    /// Human-readable description of what this field captures.
    pub description: String,
    /// Whether this field must be present in every extracted node.
    pub required: bool,
}

/// Per-layer synthesis prompts generated AFTER L0 extraction, BEFORE L1 answering.
/// References actual extracted evidence so synthesis is grounded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisPrompts {
    /// Prompt for the pre-mapping step (organizing L0 nodes under questions).
    pub pre_mapping_prompt: String,
    /// Prompt for the answering step (synthesizing L0 evidence into L1 answers).
    pub answering_prompt: String,
    /// Prompt for web edge discovery between answered questions.
    pub web_edge_prompt: String,
}

// ── Composed View Types (Cross-Slug Graph) ────────────────────────────────────

/// A node in the composed cross-slug graph view.
#[derive(Debug, Clone, Serialize)]
pub struct ComposedNode {
    pub id: String,
    pub slug: String,
    pub depth: i64,
    pub headline: String,
    pub distilled: String,
    pub self_prompt: Option<String>,
    pub node_type: String, // "mechanical" or "answer"
}

/// An edge in the composed cross-slug graph view.
#[derive(Debug, Clone, Serialize)]
pub struct ComposedEdge {
    pub source_id: String,
    pub target_id: String,
    pub weight: f64,
    pub edge_type: String, // "evidence", "child", "web"
    pub live: bool,
}

/// Full composed view: all live nodes across a slug and its references, with edges.
#[derive(Debug, Clone, Serialize)]
pub struct ComposedView {
    pub nodes: Vec<ComposedNode>,
    pub edges: Vec<ComposedEdge>,
    pub slugs: Vec<String>,
}

// ── Source Delta (file-level, NOT thread-level) ───────────────────────────────

/// A file-level source delta for crystallization tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDelta {
    pub id: i64,
    pub slug: String,
    pub file_path: String,
    pub change_type: String,
    pub diff_summary: Option<String>,
    pub processed: bool,
    pub created_at: String,
}

// ── Staleness Queue ───────────────────────────────────────────────────────────

/// An item in the staleness re-answer queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StalenessItem {
    pub id: i64,
    pub slug: String,
    pub question_id: String,
    pub reason: String,
    pub channel: String,
    pub priority: f64,
    pub created_at: String,
}

// ── Belief Supersession Types (Channel B) ─────────────────────────────────────

/// A contradiction detected by LLM analysis when source content changes.
/// Represents a specific claim in the pyramid that is now false.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contradiction {
    /// The claim in the existing pyramid extraction that is now false.
    pub superseded_claim: String,
    /// What the source now says instead.
    pub corrected_to: String,
    /// LLM confidence that this is a genuine contradiction (0.0-1.0).
    pub confidence: f64,
    /// The L0 node ID that contained the superseded claim.
    pub source_node_id: String,
}

/// Trace of all pyramid nodes affected by one or more contradictions.
/// Unlike Channel A (staleness), this trace does NOT attenuate through layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupersessionTrace {
    /// The contradictions that triggered this trace.
    pub contradictions: Vec<Contradiction>,
    /// Every node in the pyramid affected by these contradictions.
    pub affected_nodes: Vec<AffectedNode>,
    /// Total count of affected nodes (convenience field).
    pub total_nodes_affected: usize,
    /// The deepest layer reached during the trace.
    pub max_depth_reached: i64,
}

/// A single pyramid node affected by a belief supersession.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffectedNode {
    /// The pyramid node ID that contains the superseded claim.
    pub node_id: String,
    /// How many layers above the source L0 node this is.
    pub depth: i64,
    /// The specific superseded claim text found in this node.
    pub contains_claim: String,
    /// Trace path from the source node to this node: [L0-id, L1-id, ...].
    pub path_from_source: Vec<String>,
}

// ── WS-INGEST-PRIMITIVE (§15 / Phase 1.5) ─────────────────────────────────

/// Configuration parameters that affect how a source is chunked. Used to
/// compute the `ingest_signature` that uniquely identifies a chunking
/// configuration for multi-chain overlay detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestConfig {
    /// Target lines per chunk (from Tier2Config::chunk_target_lines).
    pub chunk_target_lines: usize,
    /// Target tokens per chunk. Currently unused (always 0) but the
    /// ingest_signature formula from the plan includes it for forward compat.
    #[serde(default)]
    pub chunk_target_tokens: usize,
    /// Code file extensions to include (code content type only).
    #[serde(default)]
    pub code_extensions: Vec<String>,
    /// Directories to skip during scanning (code content type only).
    #[serde(default)]
    pub skip_dirs: Vec<String>,
    /// Config file names to include (code content type only).
    #[serde(default)]
    pub config_files: Vec<String>,
    /// Document file extensions (document content type only).
    #[serde(default)]
    pub doc_extensions: Vec<String>,
}

/// A single file discovered during source directory scanning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceFile {
    /// Absolute path to the file.
    pub path: String,
    /// ISO8601 modification time.
    pub mtime: String,
    /// SHA-256 content hash.
    pub file_hash: String,
    /// File size in bytes.
    pub size: u64,
}

/// Persistent record of an ingested source file, tracked in
/// `pyramid_ingest_records`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRecord {
    pub id: i64,
    pub slug: String,
    pub source_path: String,
    pub content_type: String,
    pub ingest_signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_mtime: Option<String>,
    /// One of: pending, processing, complete, failed, stale
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Result of comparing current source files against existing ingest records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSet {
    /// Files not yet recorded in ingest records.
    pub new_files: Vec<SourceFile>,
    /// Files whose hash or mtime changed since last ingest.
    pub modified_files: Vec<SourceFile>,
    /// Source paths in ingest records whose files no longer exist on disk.
    pub deleted_paths: Vec<String>,
    /// Files that are unchanged.
    pub unchanged_count: usize,
}

// ── WS-PRIMER (§15 / Part III): Leftmost-slope primer types ──────────────────

/// The full primer context for a pyramid slug, containing the leftmost-slope
/// nodes and the canonical vocabulary extracted from the apex. This is the
/// artifact that rides in every extraction prompt during bedrock builds and
/// serves as the agent's initial cognitive substrate at session cold-start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimerContext {
    pub slug: String,
    pub slope_nodes: Vec<PrimerNode>,
    pub canonical_vocabulary: CanonicalVocabulary,
    pub total_tokens_estimate: usize,
}

/// A single node projected from the leftmost slope. Carries the headline,
/// distilled narrative, and key vocabulary dimensions (topics, decisions,
/// entities) at the resolution appropriate for that layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimerNode {
    pub node_id: String,
    pub depth: i64,
    pub headline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distilled: Option<String>,
    #[serde(default)]
    pub topics: Vec<serde_json::Value>,
    #[serde(default)]
    pub decisions: Vec<serde_json::Value>,
    #[serde(default)]
    pub entities: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range: Option<String>,
}

/// The canonical identity catalog extracted from the apex node. This is the
/// running vocabulary that propagates forward into new bedrock builds via
/// the primer — the "index of thinkable thoughts" (plan §1.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalVocabulary {
    #[serde(default)]
    pub topics: Vec<serde_json::Value>,
    #[serde(default)]
    pub entities: Vec<serde_json::Value>,
    #[serde(default)]
    pub decisions: Vec<serde_json::Value>,
    #[serde(default)]
    pub terms: Vec<serde_json::Value>,
}

// ── WS-PROVISIONAL (Phase 2b): Provisional session lifecycle ────────────────

/// Tracks a live-session provisional processing session. As a conversation
/// runs, chunks past the debounce line are processed into provisional pyramid
/// nodes. The session tracks which nodes were created provisionally so they
/// can be batch-promoted when the canonical build completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionalSession {
    pub id: i64,
    pub slug: String,
    pub source_path: String,
    /// UUID identifying this provisional session.
    pub session_id: String,
    /// One of: active, promoting, promoted, failed
    pub status: String,
    /// Node IDs created as provisional during this session.
    #[serde(default)]
    pub provisional_node_ids: Vec<String>,
    /// build_id of the canonical build that replaced these provisional nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_build_id: Option<String>,
    /// Last observed file mtime for session boundary detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_mtime: Option<String>,
    /// Index of the last chunk processed in this session.
    #[serde(default)]
    pub last_chunk_processed: i64,
    pub created_at: String,
    pub updated_at: String,
}

// ── WS-DADBEAR-EXTEND (Phase 2b): DADBEAR watch configuration ───────────────

/// Configuration for DADBEAR's source folder watcher. Each row represents
/// a watched source path for a given pyramid slug. Stored in
/// `pyramid_dadbear_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DadbearWatchConfig {
    pub id: i64,
    pub slug: String,
    /// Absolute path to the source directory to watch.
    pub source_path: String,
    /// Content type of files in this directory (code, conversation, document).
    pub content_type: String,
    /// How often (in seconds) DADBEAR scans this source path. Default: 10.
    pub scan_interval_secs: u64,
    /// Seconds a file must be stable before provisional build fires. Default: 30.
    pub debounce_secs: u64,
    /// Seconds of inactivity on a conversation file before session promotion. Default: 1800 (30 min).
    pub session_timeout_secs: u64,
    /// How many pending ingest records to dispatch per tick. Default: 1.
    pub batch_size: u32,
    /// Whether this watch config is active.
    pub enabled: bool,
    /// Timestamp of the last DADBEAR scan for this config.
    pub last_scan_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Status snapshot for a DADBEAR watch config, returned by the status endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DadbearWatchStatus {
    pub config: DadbearWatchConfig,
    pub pending_ingests: usize,
    pub active_sessions: usize,
    pub last_scan_at: Option<String>,
}

// ── WS-DEMAND-GEN (Phase 3): Demand-driven L0 generation job tracking ────────

/// A demand-generation job that tracks async generation of evidence-grounded
/// L0 nodes in response to questions whose answers don't exist in the pyramid.
/// Stored in `pyramid_demand_gen_jobs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemandGenJob {
    pub id: i64,
    pub job_id: String,
    pub slug: String,
    pub question: String,
    #[serde(default)]
    pub sub_questions: Vec<String>,
    /// One of: "queued", "running", "complete", "failed"
    pub status: String,
    #[serde(default)]
    pub result_node_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub requested_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

// ── WS-QUESTION-RETRIEVE (Phase 3): Question retrieval result types ──────────

/// Result of a question retrieval operation. Contains the decomposed sub-questions,
/// their answers (if found), and any demand-gen job IDs for unanswerable sub-questions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionRetrieveResult {
    /// The original question asked.
    pub question: String,
    /// Results for each sub-question produced by decomposition.
    pub sub_questions: Vec<SubQuestionResult>,
    /// Composed answer synthesized from sub-question evidence. None if no
    /// sub-questions could be answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composed_answer: Option<String>,
    /// Sub-question texts that could not be answered from existing pyramid content
    /// and need demand-gen to produce new evidence.
    #[serde(default)]
    pub demand_gen_needed: Vec<String>,
    /// If allow_demand_gen was true, the job IDs of spawned demand-gen jobs.
    #[serde(default)]
    pub demand_gen_job_ids: Vec<String>,
    /// Node IDs used as evidence sources across all sub-questions.
    #[serde(default)]
    pub sources: Vec<String>,
}

/// Result for a single sub-question within a question retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubQuestionResult {
    /// The sub-question text.
    pub question: String,
    /// Answer text composed from matched evidence. None if no evidence found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Node IDs that provided evidence for this sub-question.
    #[serde(default)]
    pub evidence_nodes: Vec<String>,
    /// Confidence score 0.0-1.0 based on match quality.
    /// 0.0 = no evidence, 1.0 = strong vocabulary + FTS match with detail.
    pub confidence: f64,
}

// ── WS-VINE-UNIFY (Phase 2b): Vine composition tracking ──────────────────────

/// A row from `pyramid_vine_compositions` tracking the relationship between
/// a vine pyramid and one of its children (bedrocks or sub-vines).
///
/// Phase 16 added `child_type` to support vine-of-vines composition. The
/// `bedrock_slug` column name is retained for backwards compatibility; it
/// holds a bedrock slug when `child_type == "bedrock"` and a child vine slug
/// when `child_type == "vine"`. New code should read `child_slug()` /
/// `child_type` rather than referencing `bedrock_slug` directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineComposition {
    pub id: i64,
    pub vine_slug: String,
    /// Child slug — either a bedrock or a child vine depending on `child_type`.
    /// Column retained as `bedrock_slug` in the database for backwards compat.
    pub bedrock_slug: String,
    /// Ordering of children in the vine (0 = leftmost / most recent).
    pub position: i32,
    /// Current apex node ID of the child pyramid, updated after each build.
    /// For vine children this is the child vine's own apex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bedrock_apex_node_id: Option<String>,
    /// active, stale, or removed.
    pub status: String,
    /// Phase 16: `'bedrock'` or `'vine'`. Defaults to `'bedrock'` for
    /// rows that existed before the migration.
    #[serde(default = "default_child_type_bedrock")]
    pub child_type: String,
    pub created_at: String,
    pub updated_at: String,
}

fn default_child_type_bedrock() -> String {
    "bedrock".to_string()
}

impl VineComposition {
    /// Alias for `bedrock_slug`. Use this in new code so the intent — "the
    /// slug of whatever child the vine composes here" — is clear regardless
    /// of whether the child is a bedrock or another vine.
    pub fn child_slug(&self) -> &str {
        &self.bedrock_slug
    }

    /// True when this composition row references a child vine rather than a
    /// bedrock pyramid.
    pub fn is_vine_child(&self) -> bool {
        self.child_type == "vine"
    }
}

// ── WS-CHAIN-PUBLISH (Phase 3): Chain publication tracking ─────────────────────

/// A row from `pyramid_chain_publications` tracking the publication state of
/// a chain configuration to the Wire contribution graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainPublication {
    pub id: i64,
    pub chain_id: String,
    pub version: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_handle_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
    /// local, published, or deprecated.
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

// ── WS-CHAIN-PROPOSAL (Phase 3): Agent-proposed chain updates ────────────────

/// A row from `pyramid_chain_proposals` tracking an agent-proposed update to a
/// chain configuration. Proposals accumulate as contributions and surface to the
/// operator for review — closing the learning loop so the substrate improves how
/// content gets processed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainProposal {
    pub id: i64,
    pub proposal_id: String,
    pub chain_id: String,
    pub proposer: String,
    pub proposal_type: String,
    pub description: String,
    pub reasoning: String,
    pub patch: serde_json::Value,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_notes: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<String>,
}

// ── WS-VOCAB (Phase 3): Vocabulary catalog types ─────────────────────────────

/// A vocabulary catalog extracted from a pyramid's apex node. Contains all
/// canonical identities (topics, entities, decisions, terms, practices) with
/// their importance, liveness, and category information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VocabularyCatalog {
    pub slug: String,
    pub topics: Vec<VocabEntry>,
    pub entities: Vec<VocabEntry>,
    pub decisions: Vec<VocabEntry>,
    pub terms: Vec<VocabEntry>,
    pub practices: Vec<VocabEntry>,
    pub total_entries: usize,
    pub extracted_at: String,
}

/// A single entry in the vocabulary catalog — a canonical identity extracted
/// from the pyramid apex's structured fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VocabEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importance: Option<f64>,
    /// "live" or "mooted" — tracks whether this identity is still active
    /// or has been superseded by a different canonical form.
    pub liveness: String,
    /// Full original entry data from the apex node, preserved for drill queries.
    #[serde(default)]
    pub detail: serde_json::Value,
}

/// Result of a reverse vocabulary query: the matched identity plus its
/// category context and neighboring identities in the same category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VocabReverseResult {
    pub entry: VocabEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub neighbors: Vec<VocabEntry>,
}

// ── WS-MANIFEST-API (Phase 3): Context manifest for agent cognition steering ──

/// A single manifest operation that an agent emits between turns to steer
/// its Brain Map. The runtime harness executes these against the pyramid
/// graph before the next turn.
///
/// See plan §9.2: "Between turns, the agent emits a structured context
/// manifest as part of its response — invisible to the human user,
/// machine-readable, consumed by the runtime harness."
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum ManifestOperation {
    /// Pull a specific node into the Brain Map at a given abstraction level.
    Hydrate {
        node_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        abstraction_level: Option<String>,
    },
    /// Drop a Brain Map node's richer content, retaining vocabulary floor only
    /// (headline + topics + entity names, no detail).
    Dehydrate { node_id: String },
    /// Replace a stretch of dialogue turns with a synthesis node (placeholder —
    /// actual buffer compression is runtime-side).
    Compress { buffer_range: (usize, usize) },
    /// Request async helper to produce a missing mid-level synthesis node.
    Densify { missing_node_id: String },
    /// Pull in nodes related to a seed via ties_to / web edges.
    Colocate { seed_node_id: String },
    /// Speculatively pre-stage nodes the agent anticipates needing next turn.
    Lookahead { node_ids: Vec<String> },
    /// Flag a node as possibly stale and request async verification.
    Investigation { node_id: String },
    /// Fire a question against a pyramid; answer flows into Brain Map or
    /// triggers demand-driven generation.
    Ask {
        pyramid_slug: String,
        question: String,
    },
    /// Propose an update to a chain configuration based on session learning.
    ProposeChainUpdate {
        chain_id: String,
        patch: serde_json::Value,
    },
}

/// Result of executing a batch of manifest operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestResult {
    /// Number of operations that were executed (including failures).
    pub operations_executed: usize,
    /// Per-operation results in the same order as the input operations.
    pub results: Vec<ManifestOpResult>,
    /// UUID for the audit trail — correlates with pyramid_manifest_log.
    pub provenance_id: String,
}

/// Result of a single manifest operation execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestOpResult {
    /// Operation type name (e.g. "Hydrate", "Dehydrate").
    pub op: String,
    /// Whether the operation completed successfully.
    pub success: bool,
    /// Nodes returned by this operation (hydrated content, colocated neighbors, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes_returned: Vec<serde_json::Value>,
    /// Error message if the operation failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Payload returned by the cold-start endpoint for a new agent session.
/// Contains the primer (leftmost slope + canonical vocabulary) plus
/// the initial Brain Map nodes for immediate cognition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColdStartPayload {
    /// The primer context (slope nodes + canonical vocabulary).
    pub primer: PrimerContext,
    /// Initial Brain Map nodes — the slope nodes as full JSON for direct
    /// inclusion in the agent's context window.
    pub brain_map_initial: Vec<serde_json::Value>,
    /// Session ID for correlating subsequent manifest calls.
    pub session_id: String,
}

// ── WS-MULTI-CHAIN-OVERLAY: Multi-chain overlay tracking ─────────────────

/// A chain overlay relationship: the same source content (identified by
/// `source_slug`) has been built into an additional pyramid (`overlay_slug`)
/// using a different chain configuration (`chain_id`). The `ingest_signature`
/// must match between source and overlay to guarantee shared chunking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainOverlay {
    pub id: i64,
    pub source_slug: String,
    pub overlay_slug: String,
    pub chain_id: String,
    pub ingest_signature: String,
    pub status: String,
    pub created_at: String,
}

// ── WS-PREVIEW (Phase 3): Build preview types ─────────────────────────────────

/// Preview of what a pyramid build will produce, shown to the operator before
/// they commit. Generated by scanning source directory + loading chain definition
/// + consulting the cost model. See episodic-memory-vine-canonical-v4.md §8.2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPreview {
    /// Absolute path to the source directory being scanned.
    pub source_path: String,
    /// Content type of the source material (code, conversation, document).
    pub content_type: String,
    /// Chain definition ID that will be used for the build.
    pub chain_id: String,
    /// Number of ingestible source files discovered.
    pub file_count: usize,
    /// Estimated total tokens across all source files (heuristic: ~4 chars/token).
    pub estimated_total_tokens: usize,
    /// Estimated number of bedrock pyramids (typically 1 per source file for
    /// conversation, 1 total for code/document).
    pub estimated_pyramids: usize,
    /// Estimated number of layers in the resulting pyramid.
    pub estimated_layers: usize,
    /// Estimated total nodes across all layers.
    pub estimated_nodes: usize,
    /// Estimated cost in USD based on the cost model.
    pub estimated_cost_dollars: f64,
    /// Estimated wall-clock time in seconds.
    pub estimated_time_seconds: u64,
    /// Estimated on-disk size in bytes for the resulting pyramid data.
    pub estimated_disk_bytes: u64,
    /// Warnings about the source material (large files, empty files, etc.).
    pub warnings: Vec<PreviewWarning>,
    /// ISO8601 timestamp when this preview was generated.
    pub generated_at: String,
}

/// A single warning surfaced during preview generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewWarning {
    /// Severity: "info", "warning", or "error".
    pub level: String,
    /// File path this warning relates to, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// Human-readable description of the warning.
    pub message: String,
}

// ── WS-READING-MODES (Phase 4): Six reading mode view types ───────────────

/// Memoir: apex top-to-bottom, dense prose at whole-arc scale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoirView {
    pub slug: String,
    pub headline: String,
    pub distilled: String,
    pub narrative: NarrativeMultiZoom,
    pub topics: Vec<Topic>,
    pub decisions: Vec<Decision>,
    pub terms: Vec<Term>,
}

/// Walk: paginated nodes at a specified layer, chronological order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkView {
    pub slug: String,
    pub layer: i64,
    pub nodes: Vec<WalkNode>,
    pub total_count: i64,
    pub offset: usize,
}

/// A single node in a Walk view (lightweight projection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkNode {
    pub id: String,
    pub chunk_index: Option<i64>,
    pub headline: String,
    pub distilled: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range: Option<TimeRange>,
    pub weight: f64,
    pub topic_names: Vec<String>,
}

/// Thread: follow a canonical identity across non-adjacent nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadView {
    pub slug: String,
    pub identity: String,
    pub mentions: Vec<ThreadMention>,
}

/// A single mention of an identity across the pyramid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadMention {
    pub node_id: String,
    pub depth: i64,
    pub headline: String,
    /// "topic", "entity", or "decision"
    pub mention_type: String,
    /// The matched name/decided text for context.
    pub matched_text: String,
    #[serde(default)]
    pub importance: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range: Option<TimeRange>,
}

/// Decisions Ledger: aggregated decisions across the corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionsView {
    pub slug: String,
    pub decisions: Vec<DecisionEntry>,
    pub total_count: usize,
}

/// A single decision entry with source provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionEntry {
    pub decided: String,
    pub why: String,
    pub stance: String,
    pub importance: f64,
    #[serde(default)]
    pub related: Vec<String>,
    /// Node this decision was extracted from.
    pub source_node_id: String,
    pub source_headline: String,
    pub source_depth: i64,
}

/// Speaker: filter to one speaker role's contributions via key_quotes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerView {
    pub slug: String,
    pub role: String,
    pub quotes: Vec<SpeakerQuote>,
    pub total_count: usize,
}

/// A single quote attributed to a speaker role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerQuote {
    pub text: String,
    pub speaker_role: String,
    pub importance: f64,
    /// Node this quote was extracted from.
    pub source_node_id: String,
    pub source_headline: String,
    pub source_depth: i64,
}

/// Search: FTS/LIKE search with ancestor node chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchReadingView {
    pub slug: String,
    pub query: String,
    pub results: Vec<SearchReadingHit>,
    pub total_count: usize,
}

/// A single search hit with ancestry trail (L0 -> L1 -> ... -> apex).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchReadingHit {
    pub node_id: String,
    pub depth: i64,
    pub headline: String,
    pub snippet: String,
    pub score: f64,
    /// Ancestor chain from this node up to apex: [(node_id, depth, headline), ...]
    pub ancestors: Vec<AncestorNode>,
}

/// Lightweight ancestor node reference for search ancestry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AncestorNode {
    pub node_id: String,
    pub depth: i64,
    pub headline: String,
}

// ── Phase 2: Change-Manifest Supersession ────────────────────────────────────
//
// Types backing the change-manifest flow defined in
// `docs/specs/change-manifest-supersession.md`. The change manifest is the
// LLM-produced targeted delta that stale-checks apply in place on an existing
// upper-layer node (same ID, bumped version), rather than inserting a brand
// new node with a fresh ID. This is the structural fix for the viz-orphaning
// bug.

/// A single topic-level operation in a change manifest: add a new topic,
/// update an existing topic's content, or remove an obsolete topic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TopicOp {
    /// One of: "add" | "update" | "remove".
    pub action: String,
    /// Topic name — required for all operations.
    pub name: String,
    /// New topic text — required for "add" and "update", ignored for "remove".
    #[serde(default)]
    pub current: String,
}

/// A single term-level operation in a change manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TermOp {
    /// One of: "add" | "update" | "remove".
    pub action: String,
    /// Term identifier — required for all operations.
    pub term: String,
    /// New definition text — required for "add" and "update".
    #[serde(default)]
    pub definition: String,
}

/// A single decision-level operation in a change manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionOp {
    /// One of: "add" | "update" | "remove".
    pub action: String,
    /// The decision text (identity key for add/update/remove).
    pub decided: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub stance: String,
}

/// A single dead-end operation in a change manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeadEndOp {
    /// One of: "add" | "remove". (Dead ends are opaque strings with no
    /// "update" semantic — remove and re-add.)
    pub action: String,
    pub value: String,
}

/// Field-level updates that a change manifest applies to the target node.
/// Every field is optional — `None` means "leave unchanged".
///
/// `topics`, `terms`, `decisions`, and `dead_ends` use per-entry action ops
/// (add/update/remove); `distilled` and `headline` are wholesale replacements
/// when present.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContentUpdates {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distilled: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topics: Option<Vec<TopicOp>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms: Option<Vec<TermOp>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decisions: Option<Vec<DecisionOp>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_ends: Option<Vec<DeadEndOp>>,
}

/// A child-id pair used by the `children_swapped` list in a change manifest.
/// For pyramid-local manifests the ids are bare node ids (e.g. `L2-004`).
/// For vine-level manifests they are slug-prefixed (`bedrock-x:L3-001`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChildSwap {
    pub old: String,
    pub new: String,
}

/// The full change manifest produced by the LLM for a single stale-check
/// (or user-initiated reroll) against a single upper-layer node. Serializes
/// to / from the JSON shape documented in
/// `docs/specs/change-manifest-supersession.md` → "Change Manifest Format".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChangeManifest {
    /// Node this manifest targets.
    pub node_id: String,
    /// `true` iff the node's fundamental identity changed (rare). When
    /// `true`, `execute_supersession` falls back to the legacy new-id path.
    #[serde(default)]
    pub identity_changed: bool,
    /// Wholesale + per-field updates to apply to the target node.
    #[serde(default)]
    pub content_updates: ContentUpdates,
    /// Which child references were replaced, for evidence-link rewriting.
    #[serde(default)]
    pub children_swapped: Vec<ChildSwap>,
    /// LLM-authored reason: one sentence explaining what changed and why.
    pub reason: String,
    /// Expected new `build_version` after this manifest applies. Validated
    /// against the live node's current `build_version + 1` — non-contiguous
    /// bumps are rejected.
    pub build_version: i64,
}

impl ChangeManifest {
    /// Helper used by tests and by the update path to convert the manifest's
    /// typed `children_swapped` into the `&[(String, String)]` slice shape
    /// expected by the db helper.
    pub fn children_swapped_pairs(&self) -> Vec<(String, String)> {
        self.children_swapped
            .iter()
            .map(|swap| (swap.old.clone(), swap.new.clone()))
            .collect()
    }
}

/// A row from `pyramid_change_manifests`. Carries both the raw manifest JSON
/// (for round-tripping and audit display) and the parsed form (for app code).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeManifestRecord {
    pub id: i64,
    pub slug: String,
    pub node_id: String,
    pub build_version: i64,
    pub manifest_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_manifest_id: Option<i64>,
    pub applied_at: String,
}

/// Validation errors raised by `validate_change_manifest`. Every variant is
/// surfaced in WARN-level logs with the full manifest JSON and, in Phase 15,
/// in the DADBEAR oversight page as an unapplied-manifest entry.
#[derive(Debug, Clone, PartialEq)]
pub enum ManifestValidationError {
    /// The target node_id does not exist as a live row (no pyramid_nodes row
    /// or all rows superseded) in this slug.
    TargetNotFound,
    /// An `old` id in `children_swapped` does not appear as a source_node_id
    /// in `pyramid_evidence` with verdict = 'KEEP' targeting this node.
    MissingOldChild(String),
    /// A `new` id in `children_swapped` does not exist in `pyramid_nodes`.
    MissingNewChild(String),
    /// `identity_changed: true` but neither `distilled` nor `headline` is
    /// provided — invalid per spec.
    IdentityChangedWithoutRewrite,
    /// A topic/term/decision op is missing a required field.
    InvalidContentOp {
        field: String,
        detail: String,
    },
    /// A topic/term/decision op uses an unknown action string.
    InvalidContentOpAction {
        field: String,
        action: String,
    },
    /// A "remove" op targets an entry that does not exist on the current node.
    RemovingNonexistentEntry {
        field: String,
        name: String,
    },
    /// The LLM-authored `reason` field is empty or whitespace only.
    EmptyReason,
    /// The manifest's `build_version` is not exactly `current + 1`.
    NonContiguousVersion {
        expected: i64,
        got: i64,
    },
}

impl std::fmt::Display for ManifestValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TargetNotFound => write!(f, "target node not found or not live"),
            Self::MissingOldChild(id) => {
                write!(f, "children_swapped.old='{id}' has no KEEP evidence link")
            }
            Self::MissingNewChild(id) => {
                write!(f, "children_swapped.new='{id}' does not exist as a node")
            }
            Self::IdentityChangedWithoutRewrite => write!(
                f,
                "identity_changed=true requires distilled or headline to be set"
            ),
            Self::InvalidContentOp { field, detail } => {
                write!(f, "invalid {field} op: {detail}")
            }
            Self::InvalidContentOpAction { field, action } => {
                write!(f, "unknown {field} op action: '{action}'")
            }
            Self::RemovingNonexistentEntry { field, name } => {
                write!(f, "remove {field} '{name}' — not present on current node")
            }
            Self::EmptyReason => write!(f, "reason field is empty"),
            Self::NonContiguousVersion { expected, got } => {
                write!(
                    f,
                    "build_version bump is non-contiguous: expected {expected}, got {got}"
                )
            }
        }
    }
}

impl std::error::Error for ManifestValidationError {}

// ── Post-build accretion v5 types ────────────────────────────────────────────
// See .lab/architecture/agent-wire-node-post-build-plan-v5.md
// StepOperation intentionally NOT modified — role_bound dispatch routes via
// string primitive "role_bound" + dadbear_work_items.resolved_chain_id column.

/// Error raised when a DB row contains a `node_shape` string value that
/// the vocabulary registry does not know about. Emitted by
/// `NodeShape::from_db` so readers fail loud on typos / registry drift
/// rather than silently rendering a shaped node as plain scaffolding.
///
/// Phase 6c-D: the enum-variant form of this error (4 hardcoded shapes)
/// was replaced with a newtype wrapper around a vocabulary-validated
/// string — an agent can publish a new `NodeShape` vocab entry without
/// a code deploy. Unknown shapes at read time still raise loud because
/// the registry is the authoritative list; a DB row whose `node_shape`
/// string has no active vocab entry is either a forward-drift migration
/// in progress or a data bug.
#[derive(Debug, thiserror::Error)]
#[error("Unknown node_shape value in DB: '{0}'")]
pub struct UnknownNodeShape(pub String);

// ── Canonical string constants for the 4 genesis node shapes ──
//
// Mirror of the AnnotationType convention above. Not an authoritative
// list — the vocabulary registry is — but internal callers that already
// know a specific genesis shape string (tests, compiler arms) can refer
// to these constants instead of repeating the literal.
pub const NODE_SHAPE_SCAFFOLDING: &str = "scaffolding";
pub const NODE_SHAPE_DEBATE: &str = "debate";
pub const NODE_SHAPE_META_LAYER: &str = "meta_layer";
pub const NODE_SHAPE_GAP: &str = "gap";

/// Error raised when `parse_shape_payload` encounters a shape string that
/// is known to the vocabulary registry but has no typed payload struct
/// wired into the parser. Phase 6c-D limitation: the payload-handler
/// registry is NOT contribution-driven yet (Phase 10+ will add that) —
/// so an agent CAN publish a new node-shape vocab entry (`annotation_cluster`,
/// say), and `NodeShape::from_db` will accept it as a valid discriminator,
/// but until the payload struct + match arm here are added, the shape
/// reader raises. FIXME(phase-10+): replace this static match with a
/// contribution-driven shape-handler registry.
#[derive(Debug, thiserror::Error)]
#[error(
    "No payload handler registered for node_shape '{0}' — the vocab entry \
exists but no typed payload struct has been wired into parse_shape_payload. \
Remediation: either (1) add a `NODE_SHAPE_{{name}}` const + payload struct \
+ match arm in `parse_shape_payload` (src-tauri/src/pyramid/types.rs), or \
(2) wait for the Phase 10+ contribution-driven shape-handler registry that \
will make payload types first-class contributions. Context (slug + node_id) \
is attached by the caller via with_context."
)]
pub struct UnknownShapePayload(pub String);

/// Node shape discriminator stored in `pyramid_nodes.node_shape`.
/// NULL / empty / "scaffolding" all normalize to the scaffolding shape.
///
/// Phase 6c-D: flipped from a 4-variant enum to a newtype wrapper around
/// the canonical string — per `feedback_generalize_not_enumerate`.
/// The vocabulary registry (`pyramid_config_contributions` rows with
/// `schema_type LIKE 'vocabulary_entry:node_shape:%'`) is the
/// authoritative catalog. `from_db` validates against it so an agent
/// who publishes a new node-shape vocab entry can point rows at that
/// shape the moment the entry is active.
///
/// Serde transparent keeps wire compat: serializes to / deserializes from
/// the plain string. The `node_shape` DB column stores the same string
/// via `to_db()` (NULL for scaffolding).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeShape(String);

impl NodeShape {
    /// Raw constructor — wraps an arbitrary string. Use for internal call
    /// sites that already know the string is a canonical genesis shape
    /// (tests, shape-writer handlers). Write paths that accept external
    /// input (HTTP, MCP CLI) MUST use `from_db` so unknown strings raise.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The canonical string form. Stored in `pyramid_nodes.node_shape`
    /// (NULL for scaffolding via `to_db`) and used as the wire-protocol
    /// discriminator in JSON.
    pub fn as_str(&self) -> &str {
        if self.is_scaffolding() {
            NODE_SHAPE_SCAFFOLDING
        } else {
            &self.0
        }
    }

    /// True for the default scaffolding shape (empty inner string OR
    /// literal "scaffolding"). Empty normalizes to scaffolding so
    /// `NodeShape::new("")` and `NodeShape::from_db(None)` round-trip
    /// to the same semantic shape.
    pub fn is_scaffolding(&self) -> bool {
        self.0.is_empty() || self.0 == NODE_SHAPE_SCAFFOLDING
    }

    /// Convenience: the canonical scaffolding sentinel. Equivalent to
    /// `NodeShape::new("scaffolding")` / `NodeShape::from_db(None)`.
    pub fn scaffolding() -> Self {
        Self(String::new())
    }

    /// Convert to the string value stored in `pyramid_nodes.node_shape`.
    /// Scaffolding maps to None (NULL in DB) — no row write needed for the
    /// default case; every other shape stores its canonical string.
    pub fn to_db(&self) -> Option<&str> {
        if self.is_scaffolding() {
            None
        } else {
            Some(&self.0)
        }
    }

    /// Parse from the optional string stored in the DB column, validating
    /// against the vocabulary registry.
    ///
    /// None / empty / "scaffolding" normalize to the scaffolding shape
    /// WITHOUT hitting the registry (scaffolding is the default, and a
    /// NULL column existed before the `node_shape` column was added).
    ///
    /// Anything else is validated against the `node_shape` vocab kind —
    /// unknown strings raise `UnknownNodeShape`. Silent-default-to-
    /// scaffolding would let a typo'd or forward-drift shape silently
    /// render as plain scaffolding while still holding a payload in
    /// `shape_payload_json`, masking a real data bug. Per
    /// `feedback_loud_deferrals`, fail loud.
    pub fn from_db(
        conn: &rusqlite::Connection,
        s: Option<&str>,
    ) -> Result<Self, UnknownNodeShape> {
        match s {
            None | Some("") | Some(NODE_SHAPE_SCAFFOLDING) => Ok(Self::scaffolding()),
            Some(other) => {
                match super::vocab_entries::get_vocabulary_entry(
                    conn,
                    super::vocab_entries::VOCAB_KIND_NODE_SHAPE,
                    other,
                ) {
                    Ok(Some(_)) => Ok(Self(other.to_string())),
                    Ok(None) => Err(UnknownNodeShape(other.to_string())),
                    Err(e) => {
                        // Registry read failure must NOT silently-accept
                        // unknown strings — log + refuse.
                        tracing::error!(
                            "vocabulary lookup failed while validating node_shape '{other}': {e}"
                        );
                        Err(UnknownNodeShape(other.to_string()))
                    }
                }
            }
        }
    }

    /// Unchecked wrapper — wraps whatever the DB had without vocab
    /// validation. Callers must only use this in read paths where the
    /// string was validated at write time (matches the AnnotationType
    /// `from_db_string` pattern).
    pub fn from_db_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl std::fmt::Display for NodeShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Purpose row — per-slug declaration of what the pyramid is for.
/// Supersession chain via `superseded_by`. One active row per slug
/// (enforced by partial UNIQUE index).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Purpose {
    pub id: i64,
    pub slug: String,
    pub purpose_text: String,
    pub stock_purpose_key: Option<String>,
    pub decomposition_chain_ref: Option<String>,
    pub created_at: String,
    pub superseded_by: Option<i64>,
    pub supersede_reason: Option<String>,
}

/// Role binding row — per-pyramid mapping of role name to handler chain id.
/// Supersession chain. One active row per (slug, role_name, scope).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleBinding {
    pub id: i64,
    pub slug: String,
    pub role_name: String,
    pub handler_chain_id: String,
    pub scope: String,
    pub created_at: String,
    pub superseded_by: Option<i64>,
}

// ── Node-shape-specific payload structs ────────────────────────────────
// Stored serialized in `pyramid_nodes.shape_payload_json`. The legacy
// `topics: Vec<Topic>` column is untouched for shape nodes so non-shape-aware
// readers see an empty Vec rather than silently-corrupted JSON.

/// Debate node payload — contested-claim structure with positions,
/// steel-mannings, red-teams, cross-references, and vote lean.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateTopic {
    pub concern: String,
    pub positions: Vec<DebatePosition>,
    #[serde(default)]
    pub cross_refs: Vec<String>,
    #[serde(default)]
    pub vote_lean: Option<VoteLean>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebatePosition {
    pub label: String,
    pub steel_manning: String,
    #[serde(default)]
    pub red_teams: Vec<RedTeamEntry>,
    #[serde(default)]
    pub evidence_anchors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedTeamEntry {
    pub from_position: String,
    pub argument: String,
    #[serde(default)]
    pub evidence_anchors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteLean {
    pub up_count: i64,
    pub down_count: i64,
    #[serde(default)]
    pub per_position: Option<HashMap<String, (i64, i64)>>,
}

/// Meta-layer node payload — purpose-aligned synthesis referencing substrate.
///
/// `topics` is the audit trail: each topic names a theme the synthesizer
/// surfaced across the substrate and lists the specific substrate node ids
/// that anchor it. Phase 7b verifier added this field — the LLM step's
/// `response_schema` already requires it, but the original writer dropped
/// the array on the floor (silent data loss). See
/// `chain_dispatch::create_meta_layer_node`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaLayerTopic {
    pub purpose_question: String,
    #[serde(default)]
    pub parent_meta_layer_id: Option<String>,
    #[serde(default)]
    pub covered_substrate_nodes: Vec<String>,
    #[serde(default)]
    pub topics: Vec<MetaLayerTopicEntry>,
}

/// One theme surfaced by the meta-layer synthesizer, with anchor ids.
///
/// Shape mirrors the starter-synthesizer response_schema's `topics[]`
/// entries so the LLM output deserializes directly into this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaLayerTopicEntry {
    pub topic: String,
    #[serde(default)]
    pub anchor_nodes: Vec<String>,
}

/// Gap node payload — explicit absence with demand state and candidate
/// resolutions. Demand state lifecycle: open -> dispatched -> closed |
/// tombstoned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapTopic {
    pub concern: String,
    pub description: String,
    pub demand_state: String,
    #[serde(default)]
    pub candidate_resolutions: Vec<GapCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapCandidate {
    pub resolution_type: String,
    #[serde(default)]
    pub cost_estimate: Option<String>,
    #[serde(default)]
    pub authorization_required: bool,
}

/// Tagged enum over shape-specific payloads. Serialized to
/// `pyramid_nodes.shape_payload_json` via serde untagged with a sibling
/// node_shape column as the discriminator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ShapePayload {
    Debate(DebateTopic),
    MetaLayer(MetaLayerTopic),
    Gap(GapTopic),
}

/// Typed view of a node's shape + payload as resolved from the two
/// parallel columns `pyramid_nodes.node_shape` and
/// `pyramid_nodes.shape_payload_json`. Produced by `db::get_node_shape`.
#[derive(Debug, Clone)]
pub struct NodeShapeView {
    pub shape: NodeShape,
    /// `None` iff `shape == Scaffolding`. For Debate / MetaLayer / Gap this
    /// is always Some — a node claiming a non-scaffolding shape with NULL
    /// payload is a data bug and raises at read time.
    pub payload: Option<ShapePayload>,
}

/// Resolve `shape_payload_json` against a declared `NodeShape` discriminator.
///
/// Rather than rely on serde's `#[serde(untagged)]` required-field-set
/// guessing (brittle when DebateTopic and MetaLayerTopic overlap), this
/// function deserializes into the concrete inner struct named by `shape`,
/// then wraps it. Errors are loud and contextual.
///
/// Returns:
/// - `Ok(None)` iff `shape == Scaffolding` and `json` is None or empty.
/// - `Ok(Some(payload))` for Debate/MetaLayer/Gap with a matching JSON body.
/// - `Err` when the payload is missing for a shape that requires one, or
///   when the JSON doesn't match the shape's expected struct (misaligned).
pub fn parse_shape_payload(
    shape: &NodeShape,
    json: Option<&str>,
) -> anyhow::Result<Option<ShapePayload>> {
    let json_str = json.and_then(|s| if s.trim().is_empty() { None } else { Some(s) });
    let shape_str = shape.as_str();

    // Scaffolding handled first — scaffolding nodes must not carry a payload.
    if shape.is_scaffolding() {
        return match json_str {
            None => Ok(None),
            Some(_) => Err(anyhow::anyhow!(
                "pyramid_nodes.shape_payload_json is non-empty but node_shape is Scaffolding — \
                 data bug: shape and payload columns are misaligned"
            )),
        };
    }

    // Non-scaffolding shapes require a payload.
    let payload = json_str.ok_or_else(|| {
        anyhow::anyhow!(
            "node_shape is '{}' but shape_payload_json is NULL — \
             non-scaffolding shapes require a payload",
            shape_str
        )
    })?;

    // Phase 6c-D: match on the canonical string because NodeShape is now
    // a newtype not an enum. Anything not in the genesis-known set raises
    // `UnknownShapePayload` loud — the agent can publish a new node-shape
    // vocab entry (accepted by `NodeShape::from_db`), but until someone
    // writes the payload struct + extends this match, the parser refuses.
    //
    // FIXME(phase-10+): replace this static match with a contribution-driven
    // shape-handler registry so payload types are first-class contributions
    // too. Today this is the one spot where NodeShape is NOT fully generalized.
    match shape_str {
        NODE_SHAPE_DEBATE => serde_json::from_str::<DebateTopic>(payload)
            .map(|t| Some(ShapePayload::Debate(t)))
            .map_err(|e| {
                anyhow::anyhow!(
                    "shape_payload_json failed to parse as DebateTopic (node_shape='debate'): {e}"
                )
            }),
        NODE_SHAPE_META_LAYER => serde_json::from_str::<MetaLayerTopic>(payload)
            .map(|t| Some(ShapePayload::MetaLayer(t)))
            .map_err(|e| {
                anyhow::anyhow!(
                    "shape_payload_json failed to parse as MetaLayerTopic (node_shape='meta_layer'): {e}"
                )
            }),
        NODE_SHAPE_GAP => serde_json::from_str::<GapTopic>(payload)
            .map(|t| Some(ShapePayload::Gap(t)))
            .map_err(|e| {
                anyhow::anyhow!(
                    "shape_payload_json failed to parse as GapTopic (node_shape='gap'): {e}"
                )
            }),
        other => Err(anyhow::Error::new(UnknownShapePayload(other.to_string()))),
    }
}
