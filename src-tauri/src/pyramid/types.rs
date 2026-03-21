// pyramid/types.rs — Data model structs for the Knowledge Pyramid engine
//
// Types: SlugInfo, ContentType, PyramidNode, Topic, Correction, Decision, Term,
//        TreeNode, DrillResult, SearchHit, EntityEntry, BuildStatus, BuildProgress,
//        PyramidBatch

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
    pub distilled: String,
    pub topics: Vec<Topic>,
    pub corrections: Vec<Correction>,
    pub decisions: Vec<Decision>,
    pub terms: Vec<Term>,
    pub dead_ends: Vec<String>,
    pub self_prompt: String,
    pub children: Vec<String>,
    pub parent_id: Option<String>,
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
    pub distilled: String,
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
    /// One of: "idle", "running", "complete", "failed"
    pub status: String,
    pub progress: BuildProgress,
    pub elapsed_seconds: f64,
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
