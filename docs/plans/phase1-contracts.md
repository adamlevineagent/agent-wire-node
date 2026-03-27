# Phase 1 Shared Contracts

> All Phase 1 workstreams MUST conform to these contracts. Do NOT create local type definitions — import from canonical locations.

## Module Organization

All new code goes in `src-tauri/src/pyramid/`. The module structure:
```
pyramid/
  mod.rs          — re-exports, PyramidState, PyramidConfig
  types.rs        — PyramidNode, Topic, Correction, Decision, Term, etc.
  db.rs           — all DB operations (CREATE TABLE, queries, inserts)
  build_runner.rs — orchestrates builds
  build.rs        — legacy build pipelines
  chain_executor.rs — IR + legacy executor
  wire_publish.rs — Wire publication
  crystallization.rs — staleness/supersession
  question_decomposition.rs — question tree generation
  question_compiler.rs — QuestionSet → ExecutionPlan
  question_yaml.rs — QuestionSet, Question types
  question_loader.rs — YAML loading + validation
  execution_plan.rs — ExecutionPlan, Step, etc.
  converge_expand.rs — converge block expansion
  event_chain.rs — event bus, subscriptions
  routes.rs — HTTP API endpoints
  llm.rs — LLM dispatch
```

## Naming Convention: New Tables

IMPORTANT: `pyramid_deltas` and `pyramid_threads` ALREADY EXIST with different schemas (thread-level deltas, not file-level). New tables must use distinct names:

| Plan Name | Actual Table Name | Reason |
|-----------|-------------------|--------|
| pyramid_evidence | `pyramid_evidence` | New, no conflict |
| pyramid_question_tree | `pyramid_question_tree` | New, no conflict |
| pyramid_gaps | `pyramid_gaps` | New, no conflict |
| pyramid_id_map | `pyramid_id_map` | New, no conflict |
| pyramid_deltas (file-level) | `pyramid_source_deltas` | Avoids collision with existing `pyramid_deltas` (thread-level) |
| pyramid_supersessions | `pyramid_supersessions` | New, no conflict |
| pyramid_staleness_queue | `pyramid_staleness_queue` | New, no conflict |

## Shared Type Definitions (NEW — add to types.rs)

```rust
// === Evidence System ===

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum EvidenceVerdict {
    Keep,
    Disconnect,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceLink {
    pub slug: String,
    pub source_node_id: String,   // child node (evidence provider)
    pub target_node_id: String,   // parent node (question answerer)
    pub verdict: EvidenceVerdict,
    pub weight: Option<f64>,      // 0.0-1.0, None for DISCONNECT/MISSING
    pub reason: Option<String>,
}

// === Characterization ===

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterizationResult {
    pub material_profile: String,
    pub interpreted_question: String,
    pub audience: String,
    pub tone: String,
}

// === Reconciliation ===

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationResult {
    pub orphans: Vec<String>,          // node IDs never referenced
    pub gaps: Vec<GapReport>,          // MISSING evidence reports
    pub central_nodes: Vec<String>,    // high-citation nodes
    pub weight_map: HashMap<String, f64>, // node_id → aggregate weight
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapReport {
    pub question_id: String,
    pub description: String,
    pub layer: i64,
}

// === Publication ===

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationManifest {
    pub slug: String,
    pub layer: i64,
    pub nodes_to_publish: Vec<String>,  // non-orphan node IDs
    pub skipped_orphans: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdMapping {
    pub local_id: String,
    pub wire_handle_path: String,
    pub wire_uuid: Option<String>,
    pub published_at: String,
}
```

## DB Function Contracts (add to db.rs)

WS1-A must implement these functions. WS1-B and WS1-C will call them.

```rust
// Evidence
pub fn save_evidence_link(conn: &Connection, link: &EvidenceLink) -> Result<()>;
pub fn get_evidence_for_target(conn: &Connection, slug: &str, target_node_id: &str) -> Result<Vec<EvidenceLink>>;
pub fn get_evidence_for_source(conn: &Connection, slug: &str, source_node_id: &str) -> Result<Vec<EvidenceLink>>;
pub fn get_keep_evidence_for_target(conn: &Connection, slug: &str, target_node_id: &str) -> Result<Vec<EvidenceLink>>;
pub fn clear_evidence_for_slug(conn: &Connection, slug: &str) -> Result<()>;

// Question tree
pub fn save_question_tree(conn: &Connection, slug: &str, tree: &serde_json::Value) -> Result<()>;
pub fn get_question_tree(conn: &Connection, slug: &str) -> Result<Option<serde_json::Value>>;

// Gaps
pub fn save_gap(conn: &Connection, slug: &str, gap: &GapReport) -> Result<()>;
pub fn get_gaps_for_slug(conn: &Connection, slug: &str) -> Result<Vec<GapReport>>;

// ID map (local → Wire handle-path)
pub fn save_id_mapping(conn: &Connection, slug: &str, mapping: &IdMapping) -> Result<()>;
pub fn get_wire_handle_path(conn: &Connection, slug: &str, local_id: &str) -> Result<Option<String>>;
pub fn get_all_id_mappings(conn: &Connection, slug: &str) -> Result<Vec<IdMapping>>;
pub fn is_already_published(conn: &Connection, slug: &str, local_id: &str) -> Result<bool>;

// Source deltas (NOT thread deltas)
pub fn save_source_delta(conn: &Connection, slug: &str, file_path: &str, change_type: &str, diff_summary: Option<&str>) -> Result<()>;
pub fn get_unprocessed_source_deltas(conn: &Connection, slug: &str) -> Result<Vec<SourceDelta>>;
pub fn mark_source_delta_processed(conn: &Connection, id: i64) -> Result<()>;

// Supersessions
pub fn save_supersession(conn: &Connection, slug: &str, node_id: &str, superseded_claim: &str, corrected_to: &str, source_node: Option<&str>, channel: &str) -> Result<()>;

// Staleness queue
pub fn enqueue_staleness(conn: &Connection, slug: &str, question_id: &str, reason: &str, channel: &str, priority: f64) -> Result<()>;
pub fn dequeue_staleness(conn: &Connection, slug: &str, limit: u32) -> Result<Vec<StalenessItem>>;
```

## Wire Publish Contract Updates (WS1-C)

Current `publish_pyramid_node` signature:
```rust
pub async fn publish_pyramid_node(
    &self,
    node: &PyramidNode,
    derived_from_wire_uuids: &[(String, String)],  // (child_wire_uuid, justification)
) -> Result<String>
```

Must change to:
```rust
pub async fn publish_pyramid_node(
    &self,
    node: &PyramidNode,
    derived_from: &[DerivedFromEntry],
) -> Result<String>

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedFromEntry {
    pub ref_path: String,           // handle-path or corpus path
    pub source_type: String,        // "contribution" or "source_document"
    pub weight: f64,                // 0.0-1.0
    pub justification: Option<String>,
}
```

## Characterize Step Contract (WS1-B)

New file: `src-tauri/src/pyramid/characterize.rs`

Must integrate with existing build flow in `build_runner.rs`:
```rust
pub async fn characterize(
    source_path: &str,
    apex_question: &str,
    llm_config: &LlmConfig,
) -> Result<CharacterizationResult>;
```

The build_runner's `run_decomposed_build` gains an optional `characterization: Option<CharacterizationResult>` parameter. If None, characterize is called automatically. If Some, the provided characterization is used (user confirmed/overrode).

New HTTP endpoints in routes.rs:
```
POST /pyramid/:slug/characterize
  Body: { "question": "...", "source_path": "..." }
  Response: CharacterizationResult

POST /pyramid/:slug/build/question
  Body: { "question": "...", "characterization": <optional CharacterizationResult>, ... }
```

## Existing File Backfill (WS1-A)

When migrating, backfill `pyramid_evidence` from existing `pyramid_nodes.children` JSON arrays:
- For each node with children: create evidence links with verdict=KEEP, weight=1.0, reason="legacy backfill"
- This preserves existing pyramid queryability through the new evidence system
