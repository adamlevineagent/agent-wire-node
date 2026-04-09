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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationType {
    Observation,
    Correction,
    Question,
    Friction,
    Idea,
    Era,
    Transition,
    #[serde(rename = "health_check")]
    HealthCheck,
    #[serde(rename = "directory")]
    Directory,
}

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
    pub fn as_str(&self) -> &'static str {
        match self {
            AnnotationType::Observation => "observation",
            AnnotationType::Correction => "correction",
            AnnotationType::Question => "question",
            AnnotationType::Friction => "friction",
            AnnotationType::Idea => "idea",
            AnnotationType::Era => "era",
            AnnotationType::Transition => "transition",
            AnnotationType::HealthCheck => "health_check",
            AnnotationType::Directory => "directory",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "observation" => AnnotationType::Observation,
            "correction" => AnnotationType::Correction,
            "question" => AnnotationType::Question,
            "friction" => AnnotationType::Friction,
            "idea" => AnnotationType::Idea,
            "era" => AnnotationType::Era,
            "transition" => AnnotationType::Transition,
            "health_check" => AnnotationType::HealthCheck,
            "directory" => AnnotationType::Directory,
            other => {
                tracing::warn!("Unknown annotation type: '{other}', defaulting to Observation");
                AnnotationType::Observation
            }
        }
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

// ── WS-VINE-UNIFY (Phase 2b): Vine composition tracking ──────────────────────

/// A row from `pyramid_vine_compositions` tracking the relationship between
/// a vine pyramid and one of its bedrock pyramids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VineComposition {
    pub id: i64,
    pub vine_slug: String,
    pub bedrock_slug: String,
    /// Ordering of bedrocks in the vine (0 = leftmost / most recent).
    pub position: i32,
    /// Current apex node ID of the bedrock pyramid, updated after each build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bedrock_apex_node_id: Option<String>,
    /// active, stale, or removed.
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
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
