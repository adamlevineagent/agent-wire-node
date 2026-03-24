// pyramid/types.rs — Data model structs for the Knowledge Pyramid engine
// DADBEAR: Detect, Accumulate, Debounce, Batch, Evaluate, Act, Recurse
// v0.2.0 — Live stale detection, FAQ generalization, cost observatory
//
// Types: SlugInfo, ContentType, PyramidNode, Topic, Correction, Decision, Term,
//        TreeNode, DrillResult, SearchHit, EntityEntry, BuildStatus, BuildProgress,
//        PyramidBatch, PendingMutation, AutoUpdateConfig, StaleCheckResult, etc.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlugInfo {
    pub slug: String,
    pub content_type: ContentType,
    pub source_path: String,
    pub node_count: i64,
    pub max_depth: i64,
    pub last_built_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Code,
    Conversation,
    Document,
}

impl ContentType {
    /// Convert to the lowercase string stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            ContentType::Code => "code",
            ContentType::Conversation => "conversation",
            ContentType::Document => "document",
        }
    }

    /// Parse from the lowercase string stored in SQLite.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "code" => Some(ContentType::Code),
            "conversation" => Some(ContentType::Conversation),
            "document" => Some(ContentType::Document),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub name: String,
    pub current: String,
    pub entities: Vec<String>,
    pub corrections: Vec<Correction>,
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Correction {
    pub wrong: String,
    pub right: String,
    pub who: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub decided: String,
    pub why: String,
    #[serde(default)]
    pub rejected: String,
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
    pub thread_id: Option<String>,
    pub source_path: Option<String>,
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrillResult {
    pub node: PyramidNode,
    pub children: Vec<PyramidNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub node_id: String,
    pub depth: i64,
    pub snippet: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityEntry {
    pub name: String,
    pub nodes: Vec<String>,
    pub depths: Vec<i64>,
    pub topic_names: Vec<String>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildProgress {
    pub done: i64,
    pub total: i64,
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

/// A usage log entry tracking pyramid read queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageLogEntry {
    pub id: i64,
    pub slug: String,
    pub query_type: String,       // "search", "drill", "apex", "node", "entities", "corrections", "terms", "resolved", "tree"
    pub query_params: String,     // JSON string of the query details
    pub result_node_ids: String,  // JSON array of node IDs returned
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
}

// ── FAQ Types ────────────────────────────────────────────────────────────────

/// A FAQ node — aggregated question/answer derived from annotations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaqNode {
    pub id: String,               // "FAQ-{uuid}" format
    pub slug: String,
    pub question: String,         // The canonical question
    pub answer: String,           // Accumulated answer from annotations
    pub related_node_ids: Vec<String>, // Pyramid nodes that help answer this
    pub annotation_ids: Vec<i64>, // Annotation IDs that contributed to this FAQ
    pub hit_count: i64,           // Times this FAQ was matched by a query
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
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "observation" => AnnotationType::Observation,
            "correction" => AnnotationType::Correction,
            "question" => AnnotationType::Question,
            "friction" => AnnotationType::Friction,
            "idea" => AnnotationType::Idea,
            _ => AnnotationType::Observation,
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
    pub stale: bool,
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
