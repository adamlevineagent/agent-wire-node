// pyramid/build.rs — LLM-powered pyramid build pipeline with 3 content type variants
//
// Pipelines:
//   build_conversation — forward → reverse → combine → L1 pairing → L2 threads → L3+
//   build_code         — mechanical passes → concurrent L0 → import clustering L1 → L2 threads → L3+
//   build_docs         — concurrent L0 → entity clustering L1 → L2 threads → L3+
//
// All pipelines are resumable (step_exists checks), cancellable (CancellationToken),
// and report progress via mpsc channel.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use regex::Regex;
use rusqlite::Connection;
use serde_json::Value;
use std::sync::LazyLock;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::db;
use super::llm::{call_model, extract_json, LlmConfig};
use super::naming::headline_from_analysis;
use super::types::*;

// ── UTF-8 safe slicing helpers (delegated to shared utils) ──────────────────

use crate::utils::{safe_slice_end, safe_slice_start};

// ── DB read helper (moves Connection access to blocking task) ────────────────

async fn db_read<F, T>(db: &Arc<tokio::sync::Mutex<Connection>>, f: F) -> Result<T>
where
    F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        f(&conn)
    })
    .await?
}

// ── WriteOp ──────────────────────────────────────────────────────────────────

/// Message type for the DB writer channel.  All DB mutations flow through a
/// single writer task so the rusqlite `Connection` is never shared across threads.
#[derive(Debug)]
pub enum WriteOp {
    SaveNode {
        node: PyramidNode,
        topics_json: Option<String>,
    },
    SaveStep {
        slug: String,
        step_type: String,
        chunk_index: i64,
        depth: i64,
        node_id: String,
        output_json: String,
        model: String,
        elapsed: f64,
    },
    UpdateParent {
        slug: String,
        node_id: String,
        parent_id: String,
    },
    UpdateStats {
        slug: String,
    },
    Flush {
        done: oneshot::Sender<()>,
    },
}

// ── PROMPT CONSTANTS ─────────────────────────────────────────────────────────

pub const FORWARD_PROMPT: &str = r#"You are a distillation engine. Compress this conversation chunk into the fewest possible words while preserving ALL information. Zero loss. Maximum density.

RULES:
- Preserve every proper noun, product name, technical term, and number exactly as written
- Corrections are the HIGHEST VALUE signal. "No, it's X not Y" matters more than anything else. Always capture: what was wrong, what replaced it, who corrected whom.
- Preserve every decision: what was chosen, what was rejected, why
- Cut all filler, pleasantries, repetition, elaboration, and hedging
- NEVER use abstract phrases like "active substrate", "self-validating engine", "emergent property". Use the concrete terms from the conversation.
- If someone reads only your output, they should know everything the input said

You are processing FORWARD (earliest to latest). Each chunk continues from prior context.

Output valid JSON only (no markdown fences, no extra text):
{
  "distilled": "The chunk compressed to maximum density. Every decision, name, mechanism, correction preserved. Target: 10-15% of input length.",
  "corrections": [{"wrong": "what was believed", "right": "what replaced it", "who": "who corrected"}],
  "decisions": [{"decided": "what was chosen", "rejected": "what was rejected", "why": "reasoning"}],
  "terms": [{"term": "exact term", "definition": "concrete definition from the conversation"}],
  "running_context": "1-2 sentences: what the conversation now knows that it didn't before"
}

/no_think"#;

pub const REVERSE_PROMPT: &str = r#"You are a distillation engine processing in REVERSE (latest to earliest). You know how the conversation ENDS.

Your job: mark what in this chunk ACTUALLY MATTERED given the final outcome, and what turned out to be noise.

RULES:
- Be brutally specific. Use exact names, terms, and mechanisms from the text.
- NEVER use abstract language. "Context as substrate" is FORBIDDEN. Say what actually happened.
- Flag anything said here that was LATER CORRECTED — these corrections are the most valuable signal
- Flag ideas here that BECAME major architecture components later
- Flag ideas here that went NOWHERE — dead ends that can be dropped

Output valid JSON only (no markdown fences, no extra text):
{
  "distilled": "The chunk compressed to maximum density, annotated with what mattered and what didn't given the conversation's final state.",
  "survived": ["specific ideas/decisions from this chunk that made it to the final architecture"],
  "superseded": [{"original": "what was said here", "replaced_by": "what it became later"}],
  "dead_ends": ["ideas discussed here that were abandoned"],
  "running_context": "1-2 sentences: looking backward from the end, what in this chunk matters?"
}

/no_think"#;

pub const COMBINE_PROMPT: &str = r#"You combine a FORWARD distillation (what was understood at the time) with a REVERSE distillation (what actually mattered in hindsight) into one maximally dense L0 node.

Keep everything that survived. Drop dead ends. Preserve corrections with full context (wrong → right → who).

RULES:
- Maximum information density. Every word must carry meaning.
- Use exact terms, names, numbers from the source. NEVER abstract them.
- "Deck is glass, agent-wire local is engine" is good. "The system separates concerns" is bad.
- Corrections are the most important content. Always preserve them.

Output valid JSON only (no markdown fences, no extra text):
{
  "headline": "2-6 word chunk name that helps a human recognize this chunk later",
  "distilled": "The definitive dense record of this chunk. Everything important, nothing wasted. A reader learns everything the chunk contained.",
  "corrections": [{"wrong": "...", "right": "...", "who": "..."}],
  "decisions": [{"decided": "...", "rejected": "...", "why": "..."}],
  "terms": [{"term": "exact name", "definition": "concrete meaning"}],
  "dead_ends": ["things discussed but abandoned"]
}

/no_think"#;

pub const DISTILL_PROMPT: &str = r#"You read two sibling nodes describing parts of a system. Organize everything they contain into coherent TOPICS.

A topic is a bundle: a named subject that groups together all related entities, decisions, and corrections. Everything we know about that subject belongs in that bundle.

SIBLING B IS LATER. When they contradict, B is current truth.

Your job is to understand both children and decide: what are the 3-6 coherent topics that organize everything here? A reader should scan your topic names and immediately know which thread to pull for what they care about.

Merge topics that cover the same domain. If both children discuss the same subject, that is ONE topic, not two.

For each topic:
- name: a clear, descriptive name
- current: 1-2 sentences explaining what this topic IS right now
- entities: the specific named things in this topic
- corrections: wrong/right/who for things that changed within this topic
- decisions: what was decided and why, within this topic
- headline: a 2-6 word label for the parent node itself. Concrete and human-friendly. No "This node..."

Output valid JSON only:
{
  "headline": "2-6 word node label",
  "orientation": "1-2 sentences: what this node covers. Which children to drill for which topics.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "What this topic IS right now. Current truth only.",
      "entities": ["named thing 1", "named thing 2"],
      "corrections": [{"wrong": "...", "right": "...", "who": "..."}],
      "decisions": [{"decided": "...", "why": "..."}]
    }
  ]
}

/no_think"#;

pub const THREAD_CLUSTER_PROMPT: &str = r#"You are given a flat list of topics extracted from L1 nodes of a knowledge pyramid. Each topic has a name, a summary, and an entity list. Topics come from different L1 nodes (different parts of the conversation).

Your job: identify the 6-12 coherent THREADS that organize ALL these topics. A thread is a narrative strand that weaves through the conversation — "Privacy Architecture" is a thread, "Pipeline Mechanics" is a thread.

Rules:
- Every topic must be assigned to exactly ONE thread
- Topics about the same subject from different L1 nodes belong in the SAME thread
- Use clear, descriptive thread names
- Merge aggressively — if two topic names cover the same domain, that is one thread
- Fuzzy-match entities: "helpers" and "helper agents" and "9B helpers" are the same thing
- 6-12 threads total. Fewer is better if the coverage is complete.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name",
      "description": "1 sentence: what this thread covers",
      "assignments": [
        {"source_node": "L1-000", "topic_index": 0, "topic_name": "Original Topic Name"},
        {"source_node": "L1-003", "topic_index": 2, "topic_name": "Original Topic Name"}
      ]
    }
  ]
}

/no_think"#;

pub const THREAD_NARRATIVE_PROMPT: &str = r#"You are given all the topics from a single THREAD — a coherent narrative strand pulled from across a knowledge pyramid. These topics come from different L1 nodes (different parts of the conversation) but all relate to the same subject.

Your job: synthesize this thread into coherent sub-topics. What is the CURRENT TRUTH? Organize by sub-theme, not by source.

CRITICAL TEMPORAL RULE:
Each topic has an "order" number. Higher order = later in the conversation = MORE AUTHORITATIVE.
When a high-order topic contradicts a low-order topic, the HIGH-ORDER topic IS the current truth and the low-order topic IS the old/superseded state. Record the superseded state as a correction (wrong → right).
DO NOT present early ideas as current when they were later overridden.
Topics marked [LATE — AUTHORITATIVE] represent the final state of the conversation and ALWAYS override earlier topics on the same subject.

For each sub-topic:
- name: a clear aspect of this thread
- current: what this aspect IS RIGHT NOW per the latest/highest-order topics (1-2 sentences)
- entities: specific named things from the CURRENT state
- corrections: wrong/right/who for things that changed, with source node
- decisions: what was decided and why, with source node (prefer late decisions)
- headline: a 2-6 word label for the thread node itself. Concrete and human-friendly. No "This thread..."

Output valid JSON only:
{
  "headline": "2-6 word thread label",
  "orientation": "1-2 sentences: what this thread covers. Which source nodes to drill for which sub-topics.",
  "source_nodes": ["L1-000", "L1-003"],
  "topics": [
    {
      "name": "Sub-topic Name",
      "current": "What this sub-topic IS right now per the LATEST topics.",
      "entities": ["named thing 1", "named thing 2"],
      "corrections": [{"wrong": "...", "right": "...", "who": "...", "source": "L1-XXX"}],
      "decisions": [{"decided": "...", "why": "...", "source": "L1-XXX"}]
    }
  ]
}

/no_think"#;

pub const CODE_EXTRACT_PROMPT: &str = r#"You are analyzing a single source code file. Extract its structure with maximum precision.

RULES:
- List EVERY function, type, struct, interface, and enum. Do not summarize or skip any.
- List EVERY external resource this file touches: every API endpoint, every database table name, every file path, every HTTP URL. Enumerate them ALL individually — do not collapse "7 tables" into "database tables."
- Note ALL defensive/integrity mechanisms: hash verification, retry logic, error recovery, self-healing, validation, sanitization.
- Note ALL platform-specific behavior: OS conditionals, architecture checks, platform-specific file paths.
- For the 3-5 most complex functions, describe the step-by-step LOGIC FLOW: what happens first, what conditions are checked, what branches exist, what side effects occur.
- Do NOT generate corrections. Code has no temporal authority. Describe current state only.

Be concrete. Use the actual names from the code. Do not abstract or generalize. Enumerate, do not summarize.

Output valid JSON only:
{
  "headline": "2-6 word file label",
  "purpose": "1-2 sentences: what this file does in the system",
  "line_count": 0,
  "exports": [{"name": "...", "type": "function|struct|interface|type|const|enum", "signature": "..."}],
  "key_types": [{"name": "...", "fields": ["field1", "field2"]}],
  "key_functions": [{"name": "...", "params": "...", "returns": "...", "does": "1 sentence"}],
  "logic_flows": [{"function": "do_sync", "steps": ["1. Check auth state", "2. Fetch track metadata from Supabase", "3. For each track: check storage cap", "4. Download if not cached", "5. Compute SHA-256 hash"]}],
  "external_resources": ["Supabase table: relay_nodes", "Supabase storage: audio-files bucket", "HTTP: vibesmithing.com/api/relay/tunnel"],
  "state_mutations": ["What state this file reads/writes"],
  "defensive_mechanisms": ["SHA-256 hash verification on downloads", "retry with backoff on API failure"],
  "platform_specific": ["macOS: tgz extraction via tar", "pkill orphan cloudflared processes"],
  "background_tasks": [{"name": "...", "interval": "...", "does": "..."}],
  "discrepancies": ["Frontend removed password login UI but backend still exposes login() command"]
}

/no_think"#;

pub const CONFIG_EXTRACT_PROMPT: &str = r#"You are analyzing a configuration file. Extract the key facts about the application.

Output valid JSON only:
{
  "headline": "2-6 word config label",
  "purpose": "What this config file controls",
  "app_identity": {"name": "...", "version": "...", "description": "..."},
  "dependencies": [{"name": "...", "version": "...", "role": "1-3 words: what it does"}],
  "platform": {"targets": ["..."], "runtime": "...", "build_tool": "..."},
  "security": ["Any security-relevant config: CSP, permissions, keys, etc."],
  "notable": ["Anything unusual or important about this config"]
}

/no_think"#;

pub const CODE_GROUP_PROMPT: &str = r#"You are given a cluster of related source files from the same codebase. They are grouped because they import from each other or share dependencies.

You also receive the IMPORT GRAPH showing which files depend on which, the IPC MAP showing frontend→backend command bindings (if applicable), and MECHANICAL METADATA (spawn counts, string resources, complexity metrics).

Organize everything into coherent topics that describe what this module/feature does.

Do NOT generate corrections. Code has no temporal authority. Describe current state only.

For each topic:
- name: what this aspect of the module does
- current: 1-2 sentences describing the current implementation
- entities: specific types, functions, endpoints
- api_surface: public interface (what other modules call into)
- depends_on: external services or other modules this depends on
- patterns: structural observations about how the code works (error handling style, state access pattern, async patterns)
- discrepancies: any inconsistencies between files (e.g., frontend removed a feature but backend still exposes the endpoint)
- headline: a 2-6 word label for this grouped node. Concrete and recognizable. No "This module..."

Output valid JSON only:
{
  "headline": "2-6 word module label",
  "orientation": "1-2 sentences: what this module does. Which files to read for which aspects.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "What this topic IS right now.",
      "entities": ["AuthState", "send_magic_link"],
      "api_surface": ["send_magic_link(email) -> Result<()>"],
      "depends_on": ["Supabase REST API"],
      "patterns": ["All commands return Result<T, String>", "State via Arc<RwLock<T>>"],
      "discrepancies": []
    }
  ]
}

/no_think"#;

pub const DOC_EXTRACT_PROMPT: &str = r#"You are analyzing a document from a creative fiction project. Extract the key elements.

For each document, identify:
- purpose: What this document IS (chapter draft, character sheet, worldbuilding notes, outline, research, etc.)
- summary: 2-4 sentences describing the content
- characters: Named characters that appear, with brief role descriptions
- locations: Named places or settings
- plot_points: Key events, revelations, or turning points
- themes: Thematic elements or motifs
- timeline: When events occur relative to the story (if applicable)
- connections: References to other characters, events, or documents in the project
- open_threads: Unresolved questions, setups without payoffs, or dangling plot elements

Output valid JSON only:
{
  "headline": "2-6 word document label",
  "purpose": "chapter draft / character sheet / worldbuilding / outline / research",
  "summary": "2-4 sentence description of this document's content",
  "characters": [{"name": "...", "role": "...", "arc": "what happens to them here"}],
  "locations": [{"name": "...", "significance": "..."}],
  "plot_points": ["event 1", "event 2"],
  "themes": ["theme 1", "theme 2"],
  "timeline": "when this occurs in the story",
  "connections": ["references to other parts of the project"],
  "open_threads": ["unresolved element 1"]
}

/no_think"#;

pub const DOC_GROUP_PROMPT: &str = r#"You are grouping related documents from a creative fiction project. These documents have been clustered because they share characters, locations, or plot threads.

Describe what this cluster covers as a unit. What storylines, character arcs, or worldbuilding threads connect these documents?

For each topic:
- name: A clear name for this narrative thread
- current: What the reader knows at this point
- characters: Characters involved
- plot_status: Where this thread stands (setup / developing / climax / resolved / open)
- connections: How this thread connects to other parts of the story
- headline: a 2-6 word label for this grouped node. Concrete and reader-friendly.

Output valid JSON only:
{
  "headline": "2-6 word story arc label",
  "orientation": "1-2 sentences: what this cluster covers and which documents to read for which threads.",
  "topics": [
    {
      "name": "Thread/Arc Name",
      "current": "Where this thread stands",
      "entities": ["character 1", "location 1", "plot element 1"],
      "plot_status": "setup / developing / climax / resolved / open",
      "connections": ["connects to thread X via character Y"]
    }
  ]
}

/no_think"#;

const MERGE_PROMPT: &str = r#"You are given thread clusters from multiple batches. Each batch independently grouped topics into threads. Your job: merge them into a single unified set of 8-15 threads.

Rules:
- Threads with similar names across batches are the SAME thread — merge their assignments
- Use the clearest name from any batch
- Every assignment must appear in exactly one thread
- 8-15 threads total

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name",
      "description": "1 sentence",
      "assignments": [
        {"source_node": "...", "topic_index": 0, "topic_name": "..."}
      ]
    }
  ]
}

/no_think"#;

// ── HELPERS ──────────────────────────────────────────────────────────────────

/// Call the LLM and parse JSON from the response.  On parse failure, retry once
/// at temperature 0.1.  Returns the parsed JSON value.
pub(crate) async fn call_and_parse(
    config: &LlmConfig,
    system: &str,
    user: &str,
    fallback_key: &str,
) -> Result<Value> {
    let resp = call_model(config, system, user, 0.3, 50_000).await?;
    match extract_json(&resp) {
        Ok(v) => Ok(v),
        Err(_) => {
            info!("  JSON parse error on {fallback_key}, retrying at temp 0.1...");
            let resp2 = call_model(config, system, user, 0.1, 50_000).await?;
            extract_json(&resp2)
                .map_err(|e| anyhow!("JSON parse failed twice for {fallback_key}: {e}"))
        }
    }
}

/// Flatten a topics-bearing analysis into the legacy node columns.
fn flatten_analysis(
    analysis: &Value,
) -> (String, Vec<Correction>, Vec<Decision>, Vec<Term>, String) {
    let distilled = analysis
        .get("orientation")
        .or_else(|| analysis.get("distilled"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let self_prompt = analysis
        .get("orientation")
        .or_else(|| analysis.get("self_prompt"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut corrections = Vec::new();
    let mut decisions = Vec::new();
    let mut entities = Vec::new();

    if let Some(topics) = analysis.get("topics").and_then(|t| t.as_array()) {
        for topic in topics {
            if let Some(corrs) = topic.get("corrections").and_then(|c| c.as_array()) {
                for c in corrs {
                    corrections.push(Correction {
                        wrong: c.get("wrong").and_then(|v| v.as_str()).unwrap_or("").into(),
                        right: c.get("right").and_then(|v| v.as_str()).unwrap_or("").into(),
                        who: c.get("who").and_then(|v| v.as_str()).unwrap_or("").into(),
                    });
                }
            }
            if let Some(decs) = topic.get("decisions").and_then(|d| d.as_array()) {
                for d in decs {
                    decisions.push(Decision {
                        decided: d
                            .get("decided")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                        why: d.get("why").and_then(|v| v.as_str()).unwrap_or("").into(),
                        rejected: d
                            .get("rejected")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                    });
                }
            }
            if let Some(ents) = topic.get("entities").and_then(|e| e.as_array()) {
                for e in ents {
                    if let Some(s) = e.as_str() {
                        entities.push(s.to_string());
                    }
                }
            }
        }
    }

    let terms: Vec<Term> = entities
        .into_iter()
        .map(|e| Term {
            term: e,
            definition: String::new(),
        })
        .collect();

    (distilled, corrections, decisions, terms, self_prompt)
}

/// Build a PyramidNode from an LLM analysis JSON value.
pub(crate) fn node_from_analysis(
    analysis: &Value,
    id: &str,
    slug: &str,
    depth: i64,
    chunk_index: Option<i64>,
    children: Vec<String>,
) -> PyramidNode {
    let (distilled, corrections, decisions, terms, self_prompt) = flatten_analysis(analysis);

    let dead_ends: Vec<String> = analysis
        .get("dead_ends")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let topics: Vec<Topic> =
        if let Some(topics_arr) = analysis.get("topics").and_then(|t| t.as_array()) {
            topics_arr
                .iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        } else {
            Vec::new()
        };

    PyramidNode {
        id: id.to_string(),
        slug: slug.to_string(),
        depth,
        chunk_index,
        headline: headline_from_analysis(analysis, id),
        distilled,
        topics,
        corrections,
        decisions,
        terms,
        dead_ends,
        self_prompt,
        children,
        parent_id: None,
        superseded_by: None,
        created_at: String::new(),
    }
}

/// Send a SaveNode WriteOp through the channel.
/// Logs and continues if the writer channel has closed.
pub(crate) async fn send_save_node(
    writer_tx: &mpsc::Sender<WriteOp>,
    node: PyramidNode,
    topics_json: Option<String>,
) {
    if let Err(e) = writer_tx
        .send(WriteOp::SaveNode { node, topics_json })
        .await
    {
        warn!("Writer channel closed, SaveNode dropped: {e}");
    }
}

/// Send a SaveStep WriteOp through the channel.
/// Logs and continues if the writer channel has closed.
pub(crate) async fn send_save_step(
    writer_tx: &mpsc::Sender<WriteOp>,
    slug: &str,
    step_type: &str,
    chunk_index: i64,
    depth: i64,
    node_id: &str,
    output_json: &str,
    model: &str,
    elapsed: f64,
) {
    if let Err(e) = writer_tx
        .send(WriteOp::SaveStep {
            slug: slug.to_string(),
            step_type: step_type.to_string(),
            chunk_index,
            depth,
            node_id: node_id.to_string(),
            output_json: output_json.to_string(),
            model: model.to_string(),
            elapsed,
        })
        .await
    {
        warn!("Writer channel closed, SaveStep dropped: {e}");
    }
}

/// Send an UpdateParent WriteOp through the channel.
/// Logs and continues if the writer channel has closed.
pub(crate) async fn send_update_parent(
    writer_tx: &mpsc::Sender<WriteOp>,
    slug: &str,
    node_id: &str,
    parent_id: &str,
) {
    if let Err(e) = writer_tx
        .send(WriteOp::UpdateParent {
            slug: slug.to_string(),
            node_id: node_id.to_string(),
            parent_id: parent_id.to_string(),
        })
        .await
    {
        warn!("Writer channel closed, UpdateParent dropped: {e}");
    }
}

/// Wait until all previously queued writer operations have been applied.
pub(crate) async fn flush_writes(writer_tx: &mpsc::Sender<WriteOp>) {
    let (tx, rx) = oneshot::channel();
    if let Err(e) = writer_tx.send(WriteOp::Flush { done: tx }).await {
        warn!("Writer channel closed, Flush dropped: {e}");
        return;
    }
    let _ = rx.await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeState {
    Missing,
    Complete,
    StaleStep,
}

async fn get_resume_state(
    db: &Arc<tokio::sync::Mutex<Connection>>,
    slug: &str,
    step_type: &str,
    chunk_index: i64,
    depth: i64,
    node_id: &str,
) -> Result<ResumeState> {
    let slug_owned = slug.to_string();
    let step_type_owned = step_type.to_string();
    let node_id_owned = node_id.to_string();

    db_read(&db, move |conn| {
        if !db::step_exists(
            conn,
            &slug_owned,
            &step_type_owned,
            chunk_index,
            depth,
            &node_id_owned,
        )? {
            return Ok(ResumeState::Missing);
        }

        if db::get_node(conn, &slug_owned, &node_id_owned)?.is_some() {
            Ok(ResumeState::Complete)
        } else {
            Ok(ResumeState::StaleStep)
        }
    })
    .await
}

// ── CONVERSATION PIPELINE ────────────────────────────────────────────────────

/// Build a conversation pyramid (forward -> reverse -> combine -> L1 pairing -> L2 threads -> L3+).
/// Returns the number of node failures (0 = perfect build).
pub async fn build_conversation(
    db: Arc<tokio::sync::Mutex<Connection>>,
    writer_tx: &mpsc::Sender<WriteOp>,
    llm_config: &LlmConfig,
    slug: &str,
    cancel: &CancellationToken,
    progress_tx: &mpsc::Sender<BuildProgress>,
) -> Result<i32> {
    let slug_owned = slug.to_string();
    let num_chunks = db_read(&db, {
        let s = slug_owned.clone();
        move |conn| db::count_chunks(conn, &s)
    })
    .await?;
    if num_chunks == 0 {
        return Err(anyhow!("No chunks found for slug '{slug}'"));
    }

    // Total steps: forward(N) + reverse(N) + combine(N) + L1(N/2) + L2(threads) + L3+(variable)
    // Estimate conservatively; we update total as we discover more.
    let estimated_total = num_chunks * 3 + num_chunks / 2; // forward + reverse + combine + rough L1 estimate
    let _ = progress_tx
        .send(BuildProgress {
            done: 0,
            total: estimated_total,
        })
        .await;
    let mut done: i64 = 0;
    let mut failures: i32 = 0;

    // ── FORWARD PASS ─────────────────────────────────────────────────
    info!("=== FORWARD PASS ({num_chunks} chunks) ===");

    let mut running_context = "Beginning of conversation.".to_string();

    // Recover running_context from last completed forward step
    for ci in 0..num_chunks {
        let exists = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::step_exists(conn, &s, "forward", ci, -1, "")
        })
        .await?;
        if exists {
            let output = db_read(&db, {
                let s = slug_owned.clone();
                move |conn| db::get_step_output(conn, &s, "forward", ci)
            })
            .await?;
            if let Some(output) = output {
                if let Ok(parsed) = serde_json::from_str::<Value>(&output) {
                    if let Some(ctx) = parsed.get("running_context").and_then(|v| v.as_str()) {
                        running_context = format!("{running_context} {ctx}");
                        if running_context.len() > 1500 {
                            running_context = safe_slice_start(&running_context, 1200).to_string();
                        }
                    }
                }
            }
            done += 1;
        } else {
            break;
        }
    }

    for ci in 0..num_chunks {
        if cancel.is_cancelled() {
            return Ok(failures);
        }

        let exists = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::step_exists(conn, &s, "forward", ci, -1, "")
        })
        .await?;
        if exists {
            continue;
        }

        let chunk_content = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::get_chunk(conn, &s, ci)
        })
        .await?
        .ok_or_else(|| anyhow!("Missing chunk {ci} for slug '{slug}'"))?;

        let user_prompt = format!(
            "## RUNNING CONTEXT FROM PRIOR CHUNKS\n{running_context}\n\n## CHUNK {ci}\n{chunk_content}"
        );

        info!("  Forward [{ci:02}/{num_chunks}]");
        let t0 = Instant::now();
        match call_and_parse(
            llm_config,
            FORWARD_PROMPT,
            &user_prompt,
            &format!("forward-{ci}"),
        )
        .await
        {
            Ok(analysis) => {
                let elapsed = t0.elapsed().as_secs_f64();
                let output_json = serde_json::to_string(&analysis)?;
                send_save_step(
                    writer_tx,
                    slug,
                    "forward",
                    ci,
                    -1,
                    "",
                    &output_json,
                    &llm_config.primary_model,
                    elapsed,
                )
                .await;

                // Update running context
                if let Some(ctx) = analysis.get("running_context").and_then(|v| v.as_str()) {
                    running_context = format!("{running_context} {ctx}");
                    if running_context.len() > 1500 {
                        running_context = safe_slice_start(&running_context, 1200).to_string();
                    }
                }
            }
            Err(e) => {
                warn!("  Forward pass failed for chunk {ci}, skipping: {e}");
                failures += 1;
            }
        }

        done += 1;
        let _ = progress_tx
            .send(BuildProgress {
                done,
                total: estimated_total,
            })
            .await;
    }

    // ── REVERSE PASS ─────────────────────────────────────────────────
    info!("=== REVERSE PASS ({num_chunks} chunks) ===");

    let mut running_context = "End of conversation.".to_string();

    for ci in (0..num_chunks).rev() {
        if cancel.is_cancelled() {
            return Ok(failures);
        }

        let exists = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::step_exists(conn, &s, "reverse", ci, -1, "")
        })
        .await?;
        if exists {
            let output = db_read(&db, {
                let s = slug_owned.clone();
                move |conn| db::get_step_output(conn, &s, "reverse", ci)
            })
            .await?;
            if let Some(output) = output {
                if let Ok(parsed) = serde_json::from_str::<Value>(&output) {
                    if let Some(ctx) = parsed.get("running_context").and_then(|v| v.as_str()) {
                        running_context = format!("{ctx} {running_context}");
                        if running_context.len() > 1500 {
                            running_context = safe_slice_end(&running_context, 1200).to_string();
                        }
                    }
                }
            }
            done += 1;
            let _ = progress_tx
                .send(BuildProgress {
                    done,
                    total: estimated_total,
                })
                .await;
            continue;
        }

        let chunk_content = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::get_chunk(conn, &s, ci)
        })
        .await?
        .ok_or_else(|| anyhow!("Missing chunk {ci}"))?;

        let user_prompt = format!(
            "## RUNNING CONTEXT FROM FUTURE CHUNKS\n{running_context}\n\n## CHUNK {ci}\n{chunk_content}"
        );

        info!("  Reverse [{ci:02}/{num_chunks}]");
        let t0 = Instant::now();
        match call_and_parse(
            llm_config,
            REVERSE_PROMPT,
            &user_prompt,
            &format!("reverse-{ci}"),
        )
        .await
        {
            Ok(analysis) => {
                let elapsed = t0.elapsed().as_secs_f64();
                let output_json = serde_json::to_string(&analysis)?;
                send_save_step(
                    writer_tx,
                    slug,
                    "reverse",
                    ci,
                    -1,
                    "",
                    &output_json,
                    &llm_config.primary_model,
                    elapsed,
                )
                .await;

                if let Some(ctx) = analysis.get("running_context").and_then(|v| v.as_str()) {
                    running_context = format!("{ctx} {running_context}");
                    if running_context.len() > 1500 {
                        running_context = safe_slice_end(&running_context, 1200).to_string();
                    }
                }
            }
            Err(e) => {
                warn!("  Reverse pass failed for chunk {ci}, skipping: {e}");
                failures += 1;
            }
        }

        done += 1;
        let _ = progress_tx
            .send(BuildProgress {
                done,
                total: estimated_total,
            })
            .await;
    }

    // ── COMBINE (forward + reverse -> L0) ────────────────────────────
    info!("=== COMBINE -> L0 ({num_chunks} chunks) ===");

    for ci in 0..num_chunks {
        if cancel.is_cancelled() {
            return Ok(failures);
        }

        let node_id = format!("L0-{ci:03}");

        match get_resume_state(&db, slug, "combine", ci, 0, &node_id).await? {
            ResumeState::Complete => {
                done += 1;
                let _ = progress_tx
                    .send(BuildProgress {
                        done,
                        total: estimated_total,
                    })
                    .await;
                continue;
            }
            ResumeState::StaleStep => {
                warn!("  Combine step exists but node is missing for {node_id}; rebuilding chunk {ci}");
            }
            ResumeState::Missing => {}
        }

        let fwd_json = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::get_step_output(conn, &s, "forward", ci)
        })
        .await?;
        let rev_json = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::get_step_output(conn, &s, "reverse", ci)
        })
        .await?;

        // If forward or reverse step is missing (prior failure), skip this combine
        let (fwd_json, rev_json) = match (fwd_json, rev_json) {
            (Some(f), Some(r)) => (f, r),
            _ => {
                warn!("  Combine skipped for chunk {ci}: missing forward/reverse step (prior failure)");
                failures += 1;
                done += 1;
                let _ = progress_tx
                    .send(BuildProgress {
                        done,
                        total: estimated_total,
                    })
                    .await;
                continue;
            }
        };

        let fwd: Value = serde_json::from_str(&fwd_json)?;
        let rev: Value = serde_json::from_str(&rev_json)?;

        let user_prompt = format!(
            "## FORWARD (STONE)\n{}\n\n## REVERSE (WATER)\n{}\n\nCombine into L0.",
            serde_json::to_string_pretty(&fwd)?,
            serde_json::to_string_pretty(&rev)?
        );

        info!("  Combine [{ci:02}/{num_chunks}]");
        let t0 = Instant::now();
        match call_and_parse(
            llm_config,
            COMBINE_PROMPT,
            &user_prompt,
            &format!("combine-{ci}"),
        )
        .await
        {
            Ok(analysis) => {
                let elapsed = t0.elapsed().as_secs_f64();
                let output_json = serde_json::to_string(&analysis)?;
                send_save_step(
                    writer_tx,
                    slug,
                    "combine",
                    ci,
                    0,
                    &node_id,
                    &output_json,
                    &llm_config.primary_model,
                    elapsed,
                )
                .await;

                let node = node_from_analysis(&analysis, &node_id, slug, 0, Some(ci), vec![]);
                send_save_node(writer_tx, node, None).await;
            }
            Err(e) => {
                warn!("  Combine failed for chunk {ci}, skipping: {e}");
                failures += 1;
            }
        }

        done += 1;
        let _ = progress_tx
            .send(BuildProgress {
                done,
                total: estimated_total,
            })
            .await;
    }

    // ── L1: Positional pairing ───────────────────────────────────────
    let l1_failures = build_l1_pairing(
        db.clone(),
        writer_tx,
        llm_config,
        slug,
        cancel,
        progress_tx,
        &mut done,
        estimated_total,
    )
    .await?;
    failures += l1_failures;

    // ── Update progress total now that L1 is done ─────────────────────
    let l1_count = {
        let slug_s = slug.to_string();
        db_read(&db, {
            let s = slug_s;
            move |conn| db::count_nodes_at_depth(conn, &s, 1)
        })
        .await
        .unwrap_or(0)
    };
    // Estimate: L2 ~ l1_count/2 threads, L3+ ~ log2 upper layers
    let est_l2 = (l1_count + 1) / 2;
    let mut est_upper = 0i64;
    let mut remaining = est_l2;
    while remaining > 1 {
        remaining = (remaining + 1) / 2;
        est_upper += remaining;
    }
    let estimated_total = done + est_l2 + est_upper;
    let _ = progress_tx
        .send(BuildProgress {
            done,
            total: estimated_total,
        })
        .await;

    // ── L2: Thread clustering ────────────────────────────────────────
    let l2_failures = build_threads_layer(
        db.clone(),
        writer_tx,
        llm_config,
        slug,
        cancel,
        progress_tx,
        &mut done,
        estimated_total,
    )
    .await?;
    failures += l2_failures;

    // ── L3+: Normal pairing until apex ───────────────────────────────
    let upper_failures = build_upper_layers(
        db.clone(),
        writer_tx,
        llm_config,
        slug,
        2,
        cancel,
        progress_tx,
        &mut done,
        estimated_total,
    )
    .await?;
    failures += upper_failures;

    // Update slug stats
    writer_tx
        .send(WriteOp::UpdateStats {
            slug: slug.to_string(),
        })
        .await
        .ok();

    if failures > 0 {
        warn!("Conversation pyramid build for '{slug}' completed with {failures} failure(s)");
    } else {
        info!("Conversation pyramid build complete for '{slug}'");
    }
    Ok(failures)
}

// ── CODE PIPELINE ────────────────────────────────────────────────────────────

/// Build a code pyramid (mechanical passes -> concurrent L0 -> import clustering L1 -> L2 threads -> L3+).
/// Returns the number of node failures (0 = perfect build).
pub async fn build_code(
    db: Arc<tokio::sync::Mutex<Connection>>,
    writer_tx: &mpsc::Sender<WriteOp>,
    llm_config: &LlmConfig,
    slug: &str,
    cancel: &CancellationToken,
    progress_tx: &mpsc::Sender<BuildProgress>,
) -> Result<i32> {
    let slug_owned = slug.to_string();
    let num_chunks = db_read(&db, {
        let s = slug_owned.clone();
        move |conn| db::count_chunks(conn, &s)
    })
    .await?;
    if num_chunks == 0 {
        return Err(anyhow!("No chunks found for slug '{slug}'"));
    }

    let estimated_total = num_chunks + num_chunks / 2; // L0 + rough L1 estimate
    let _ = progress_tx
        .send(BuildProgress {
            done: 0,
            total: estimated_total,
        })
        .await;
    let mut done: i64 = 0;
    let mut failures: i32 = 0;

    // ── Step 1: Mechanical passes (import graph, spawns, strings) ────
    let import_graph = db_read(&db, {
        let s = slug_owned.clone();
        let wtx = writer_tx.clone();
        move |conn| extract_import_graph(conn, &s, Some(&wtx))
    })
    .await?;

    // ── Step 2: Concurrent L0 extraction ─────────────────────────────
    info!("=== CODE L0: EXTRACT {num_chunks} files ===");

    // Collect work items
    let mut work_items: Vec<(i64, String, String)> = Vec::new();
    for ci in 0..num_chunks {
        let node_id = format!("C-L0-{ci:03}");
        match get_resume_state(&db, slug, "code_extract", ci, 0, &node_id).await? {
            ResumeState::Complete => {
                done += 1;
                continue;
            }
            ResumeState::StaleStep => {
                warn!("  Code extract step exists but node is missing for {node_id}; rebuilding chunk {ci}");
            }
            ResumeState::Missing => {}
        }
        let content = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::get_chunk(conn, &s, ci)
        })
        .await?;
        if let Some(content) = content {
            work_items.push((ci, node_id, content));
        }
    }

    let _ = progress_tx
        .send(BuildProgress {
            done,
            total: estimated_total,
        })
        .await;

    if !work_items.is_empty() {
        let concurrency = work_items.len().min(3);
        info!(
            "  {concurrency} concurrent workers for {} files",
            work_items.len()
        );

        // Spawn concurrent extraction tasks
        let (result_tx, mut result_rx) =
            mpsc::channel::<Result<(i64, String, Value, String, f64)>>(concurrency * 2);

        let mut handles = Vec::new();
        for (ci, node_id, content) in work_items {
            let config = llm_config.clone();
            let ig = import_graph.clone();
            let tx = result_tx.clone();

            let handle = tokio::spawn(async move {
                let is_config = content.contains("## TYPE: config");
                let prompt = if is_config {
                    CONFIG_EXTRACT_PROMPT
                } else {
                    CODE_EXTRACT_PROMPT
                };

                // Parse file path from chunk header
                let file_path = content
                    .lines()
                    .take(4)
                    .find(|l| l.starts_with("## FILE: "))
                    .map(|l| {
                        l.trim_start_matches("## FILE: ")
                            .split(" [")
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string()
                    })
                    .unwrap_or_default();

                // Append mechanical metadata
                let mut user_content = content.clone();
                if !file_path.is_empty() && !is_config {
                    let mut meta_parts = Vec::new();
                    if let Some(spawns) = ig.spawn_counts.get(&file_path) {
                        meta_parts.push(format!(
                            "## MECHANICAL: {} async spawn/timer calls found:",
                            spawns.len()
                        ));
                        for s in spawns {
                            meta_parts.push(format!(
                                "  - {} near line {}: {}",
                                s.call_type, s.line, s.context
                            ));
                        }
                    }
                    if let Some(resources) = ig.string_resources.get(&file_path) {
                        meta_parts.push(format!(
                            "## MECHANICAL: {} string literal resources found:",
                            resources.len()
                        ));
                        for r in resources {
                            meta_parts.push(format!("  - {r}"));
                        }
                    }
                    if let Some(comp) = ig.complexity.get(&file_path) {
                        meta_parts.push(format!(
                            "## MECHANICAL: {} lines, {} functions, {} spawns",
                            comp.lines, comp.functions, comp.spawn_count
                        ));
                    }
                    if !meta_parts.is_empty() {
                        user_content = format!("{user_content}\n\n{}", meta_parts.join("\n"));
                    }
                }

                let t0 = Instant::now();
                let analysis =
                    call_and_parse(&config, prompt, &user_content, &format!("code-l0-{ci}")).await;
                let elapsed = t0.elapsed().as_secs_f64();

                match analysis {
                    Ok(a) => {
                        let _ = tx.send(Ok((ci, node_id, a, file_path, elapsed))).await;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                    }
                }
            });
            handles.push(handle);
        }
        drop(result_tx); // Close sender so receiver terminates when all tasks done

        // Collect results and write to DB
        let mut l0_failures: i64 = 0;
        while let Some(result) = result_rx.recv().await {
            match result {
                Ok((ci, node_id, analysis, file_path, elapsed)) => {
                    // Build topics for code node
                    let purpose = analysis
                        .get("purpose")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let mut entities: Vec<String> = Vec::new();
                    if let Some(exports) = analysis.get("exports").and_then(|v| v.as_array()) {
                        for exp in exports {
                            if let Some(name) = exp.get("name").and_then(|v| v.as_str()) {
                                entities.push(name.to_string());
                            }
                        }
                    }
                    if let Some(key_types) = analysis.get("key_types").and_then(|v| v.as_array()) {
                        for kt in key_types {
                            if let Some(name) = kt.get("name").and_then(|v| v.as_str()) {
                                entities.push(name.to_string());
                            }
                        }
                    }
                    if let Some(key_fns) = analysis.get("key_functions").and_then(|v| v.as_array())
                    {
                        for kf in key_fns {
                            if let Some(name) = kf.get("name").and_then(|v| v.as_str()) {
                                entities.push(name.to_string());
                            }
                        }
                    }
                    entities.sort();
                    entities.dedup();

                    let topic_name = if file_path.is_empty() {
                        format!("Chunk {ci}")
                    } else {
                        file_path.clone()
                    };

                    let topics_json = serde_json::to_string(&serde_json::json!([{
                        "name": topic_name,
                        "current": purpose,
                        "entities": entities,
                        "corrections": [],
                        "decisions": [],
                    }]))?;

                    let output_json = serde_json::to_string(&analysis)?;
                    send_save_step(
                        writer_tx,
                        slug,
                        "code_extract",
                        ci,
                        0,
                        &node_id,
                        &output_json,
                        &llm_config.primary_model,
                        elapsed,
                    )
                    .await;

                    let mut node =
                        node_from_analysis(&analysis, &node_id, slug, 0, Some(ci), vec![]);
                    node.distilled = purpose;
                    send_save_node(writer_tx, node, Some(topics_json)).await;

                    done += 1;
                    let _ = progress_tx
                        .send(BuildProgress {
                            done,
                            total: estimated_total,
                        })
                        .await;
                }
                Err(e) => {
                    l0_failures += 1;
                    tracing::warn!("  Code L0 extraction error (failure #{}): {e}", l0_failures);
                }
            }
        }

        // Wait for all spawn handles
        for h in handles {
            let _ = h.await;
        }

        // Check for missing L0 nodes
        let actual_l0 = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::count_nodes_at_depth(conn, &s, 0)
        })
        .await?;
        if actual_l0 < num_chunks {
            tracing::warn!(
                "Code L0 extraction: {actual_l0}/{num_chunks} nodes created ({} missing, {} failures)",
                num_chunks - actual_l0, l0_failures
            );
        }
        failures += l0_failures as i32;
    }

    // ── Step 3: L1 — Import-graph clustering ─────────────────────────
    let clusters = cluster_by_imports(&import_graph);
    info!(
        "=== CODE L1: {} clusters from import graph ===",
        clusters.len()
    );

    for (ci_idx, cluster_files) in clusters.iter().enumerate() {
        if cancel.is_cancelled() {
            return Ok(failures);
        }

        let node_id = format!("C-L1-{ci_idx:03}");
        let exists = db_read(&db, {
            let s = slug_owned.clone();
            let nid = node_id.clone();
            move |conn| db::step_exists(conn, &s, "synth", -1, 1, &nid)
        })
        .await?;
        if exists {
            done += 1;
            let _ = progress_tx
                .send(BuildProgress {
                    done,
                    total: estimated_total,
                })
                .await;
            continue;
        }

        // Gather child data from L0 nodes that correspond to cluster files
        let mut child_ids = Vec::new();
        let mut child_data: Vec<Value> = Vec::new();

        for chunk_ci in 0..num_chunks {
            let content = db_read(&db, {
                let s = slug_owned.clone();
                move |conn| db::get_chunk(conn, &s, chunk_ci)
            })
            .await?;
            if let Some(content) = content {
                let file_path = content
                    .lines()
                    .take(4)
                    .find(|l| l.starts_with("## FILE: "))
                    .map(|l| {
                        l.trim_start_matches("## FILE: ")
                            .split(" [")
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string()
                    })
                    .unwrap_or_default();

                if cluster_files.contains(&file_path) {
                    let l0_id = format!("C-L0-{chunk_ci:03}");
                    let l0_node = db_read(&db, {
                        let s = slug_owned.clone();
                        let lid = l0_id.clone();
                        move |conn| db::get_node(conn, &s, &lid)
                    })
                    .await?;
                    if let Some(l0_node) = l0_node {
                        child_ids.push(l0_id);
                        let topics_val: Value = serde_json::to_value(&l0_node.topics)?;
                        child_data.push(topics_val);
                    }
                }
            }
        }

        if child_data.is_empty() {
            continue;
        }

        // Build cluster metadata
        let cluster_imports: HashMap<&str, &Vec<String>> = cluster_files
            .iter()
            .filter_map(|f| {
                import_graph
                    .imports
                    .get(f.as_str())
                    .map(|v| (f.as_str(), v))
            })
            .collect();

        let cluster_ipc: Vec<&IpcBinding> = import_graph
            .ipc_map
            .iter()
            .filter(|ipc| {
                cluster_files.contains(&ipc.frontend) || cluster_files.contains(&ipc.backend)
            })
            .collect();

        let user_prompt = format!(
            "## FILES IN THIS CLUSTER\n{}\n\n## IMPORT GRAPH\n{}\n\n## IPC BINDINGS\n{}\n\n## FILE EXTRACTIONS\n{}",
            serde_json::to_string_pretty(&cluster_files)?,
            serde_json::to_string_pretty(&cluster_imports)?,
            serde_json::to_string_pretty(&cluster_ipc)?,
            serde_json::to_string_pretty(&child_data)?,
        );

        info!("  L1 cluster [{ci_idx}] ({} files)", cluster_files.len());
        let t0 = Instant::now();
        match call_and_parse(
            llm_config,
            CODE_GROUP_PROMPT,
            &user_prompt,
            &format!("code-l1-{ci_idx}"),
        )
        .await
        {
            Ok(analysis) => {
                let elapsed = t0.elapsed().as_secs_f64();
                let topics_json = serde_json::to_string(
                    analysis.get("topics").unwrap_or(&serde_json::json!([])),
                )?;
                let output_json = serde_json::to_string(&analysis)?;
                send_save_step(
                    writer_tx,
                    slug,
                    "synth",
                    -1,
                    1,
                    &node_id,
                    &output_json,
                    &llm_config.primary_model,
                    elapsed,
                )
                .await;

                let node =
                    node_from_analysis(&analysis, &node_id, slug, 1, None, child_ids.clone());
                send_save_node(writer_tx, node, Some(topics_json)).await;

                for cid in &child_ids {
                    send_update_parent(writer_tx, slug, cid, &node_id).await;
                }
            }
            Err(e) => {
                warn!("  Code L1 cluster [{ci_idx}] failed, skipping: {e}");
                failures += 1;
            }
        }

        done += 1;
        let _ = progress_tx
            .send(BuildProgress {
                done,
                total: estimated_total,
            })
            .await;
    }

    // ── Update progress total now that L1 is done ─────────────────────
    let l1_count_code = {
        let slug_s = slug.to_string();
        db_read(&db, {
            let s = slug_s;
            move |conn| db::count_nodes_at_depth(conn, &s, 1)
        })
        .await
        .unwrap_or(0)
    };
    let est_l2_code = (l1_count_code + 1) / 2;
    let mut est_upper_code = 0i64;
    let mut remaining_code = est_l2_code;
    while remaining_code > 1 {
        remaining_code = (remaining_code + 1) / 2;
        est_upper_code += remaining_code;
    }
    let estimated_total = done + est_l2_code + est_upper_code;
    let _ = progress_tx
        .send(BuildProgress {
            done,
            total: estimated_total,
        })
        .await;

    // ── L2: Thread clustering ────────────────────────────────────────
    let l2_failures = build_threads_layer(
        db.clone(),
        writer_tx,
        llm_config,
        slug,
        cancel,
        progress_tx,
        &mut done,
        estimated_total,
    )
    .await?;
    failures += l2_failures;

    // ── L3+: Normal pairing ──────────────────────────────────────────
    let upper_failures = build_upper_layers(
        db.clone(),
        writer_tx,
        llm_config,
        slug,
        2,
        cancel,
        progress_tx,
        &mut done,
        estimated_total,
    )
    .await?;
    failures += upper_failures;

    // Update slug stats
    writer_tx
        .send(WriteOp::UpdateStats {
            slug: slug.to_string(),
        })
        .await
        .ok();

    if failures > 0 {
        warn!("Code pyramid build for '{slug}' completed with {failures} failure(s)");
    } else {
        info!("Code pyramid build complete for '{slug}'");
    }
    Ok(failures)
}

// ── DOCUMENT PIPELINE ────────────────────────────────────────────────────────

/// Build a document pyramid (concurrent L0 -> entity clustering L1 -> L2 threads -> L3+).
/// Returns the number of node failures (0 = perfect build).
pub async fn build_docs(
    db: Arc<tokio::sync::Mutex<Connection>>,
    writer_tx: &mpsc::Sender<WriteOp>,
    llm_config: &LlmConfig,
    slug: &str,
    cancel: &CancellationToken,
    progress_tx: &mpsc::Sender<BuildProgress>,
) -> Result<i32> {
    let slug_owned = slug.to_string();
    let num_chunks = db_read(&db, {
        let s = slug_owned.clone();
        move |conn| db::count_chunks(conn, &s)
    })
    .await?;
    if num_chunks == 0 {
        return Err(anyhow!("No chunks found for slug '{slug}'"));
    }

    let estimated_total = num_chunks + num_chunks / 2; // L0 + rough L1 estimate
    let _ = progress_tx
        .send(BuildProgress {
            done: 0,
            total: estimated_total,
        })
        .await;
    let mut done: i64 = 0;
    let mut failures: i32 = 0;

    // ── L0: Concurrent document extraction ───────────────────────────
    info!("=== DOC L0: EXTRACT {num_chunks} documents ===");

    let mut work_items: Vec<(i64, String, String)> = Vec::new();
    for ci in 0..num_chunks {
        let node_id = format!("L0-{ci:03}");
        match get_resume_state(&db, slug, "doc_extract", ci, 0, &node_id).await? {
            ResumeState::Complete => {
                done += 1;
                continue;
            }
            ResumeState::StaleStep => {
                warn!("  Doc extract step exists but node is missing for {node_id}; rebuilding chunk {ci}");
            }
            ResumeState::Missing => {}
        }
        let content = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::get_chunk(conn, &s, ci)
        })
        .await?;
        if let Some(content) = content {
            work_items.push((ci, node_id, content));
        }
    }

    let _ = progress_tx
        .send(BuildProgress {
            done,
            total: estimated_total,
        })
        .await;

    if !work_items.is_empty() {
        let concurrency = work_items.len().min(3);
        info!(
            "  {concurrency} concurrent workers for {} documents",
            work_items.len()
        );

        let (result_tx, mut result_rx) =
            mpsc::channel::<Result<(i64, String, Value, f64)>>(concurrency * 2);

        let mut handles = Vec::new();
        for (ci, node_id, content) in work_items {
            let config = llm_config.clone();
            let tx = result_tx.clone();

            let handle = tokio::spawn(async move {
                let lines = content.matches('\n').count() + 1;
                let chars = content.len();
                let user_prompt =
                    format!("## METADATA\nLines: {lines}, Characters: {chars}\n\n{content}");

                let t0 = Instant::now();
                let analysis = call_and_parse(
                    &config,
                    DOC_EXTRACT_PROMPT,
                    &user_prompt,
                    &format!("doc-l0-{ci}"),
                )
                .await;
                let elapsed = t0.elapsed().as_secs_f64();

                match analysis {
                    Ok(a) => {
                        let _ = tx.send(Ok((ci, node_id, a, elapsed))).await;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                    }
                }
            });
            handles.push(handle);
        }
        drop(result_tx);

        let mut l0_failures: i64 = 0;
        while let Some(result) = result_rx.recv().await {
            match result {
                Ok((ci, node_id, analysis, elapsed)) => {
                    // Build entities from characters + locations
                    let mut entities: Vec<String> = Vec::new();
                    if let Some(chars) = analysis.get("characters").and_then(|v| v.as_array()) {
                        for c in chars {
                            if let Some(name) = c.get("name").and_then(|v| v.as_str()) {
                                entities.push(name.to_string());
                            }
                        }
                    }
                    if let Some(locs) = analysis.get("locations").and_then(|v| v.as_array()) {
                        for l in locs {
                            if let Some(name) = l.get("name").and_then(|v| v.as_str()) {
                                entities.push(name.to_string());
                            }
                        }
                    }

                    let purpose = analysis
                        .get("purpose")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Document");
                    let summary = analysis
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let topics_json = serde_json::to_string(&serde_json::json!([{
                        "name": purpose,
                        "current": summary,
                        "entities": entities,
                        "corrections": [],
                        "decisions": [],
                    }]))?;

                    let output_json = serde_json::to_string(&analysis)?;
                    send_save_step(
                        writer_tx,
                        slug,
                        "doc_extract",
                        ci,
                        0,
                        &node_id,
                        &output_json,
                        &llm_config.primary_model,
                        elapsed,
                    )
                    .await;

                    let mut node =
                        node_from_analysis(&analysis, &node_id, slug, 0, Some(ci), vec![]);
                    node.distilled = summary.to_string();
                    node.terms = entities
                        .iter()
                        .map(|e| Term {
                            term: e.clone(),
                            definition: String::new(),
                        })
                        .collect();
                    send_save_node(writer_tx, node, Some(topics_json)).await;

                    done += 1;
                    let _ = progress_tx
                        .send(BuildProgress {
                            done,
                            total: estimated_total,
                        })
                        .await;
                }
                Err(e) => {
                    l0_failures += 1;
                    tracing::warn!("  Doc L0 extraction error (failure #{}): {e}", l0_failures);
                }
            }
        }

        for h in handles {
            let _ = h.await;
        }

        // Check for missing L0 nodes
        let actual_l0 = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::count_nodes_at_depth(conn, &s, 0)
        })
        .await?;
        if actual_l0 < num_chunks {
            tracing::warn!(
                "Doc L0 extraction: {actual_l0}/{num_chunks} nodes created ({} missing, {} failures)",
                num_chunks - actual_l0, l0_failures
            );
        }
        failures += l0_failures as i32;
    }

    // ── L1: Entity-overlap clustering ────────────────────────────────
    let l0_nodes = db_read(&db, {
        let s = slug_owned.clone();
        move |conn| db::get_nodes_at_depth(conn, &s, 0)
    })
    .await?;
    let l1_count = db_read(&db, {
        let s = slug_owned.clone();
        move |conn| db::count_nodes_at_depth(conn, &s, 1)
    })
    .await?;

    if l1_count == 0 && l0_nodes.len() > 1 {
        info!("=== DOC L1: CLUSTER {} documents ===", l0_nodes.len());

        // Build entity sets per node
        let node_entities: HashMap<String, HashSet<String>> = l0_nodes
            .iter()
            .map(|n| {
                let ents: HashSet<String> = n
                    .topics
                    .iter()
                    .flat_map(|t| t.entities.iter())
                    .map(|e| e.to_lowercase().trim().to_string())
                    .collect();
                (n.id.clone(), ents)
            })
            .collect();

        // Cluster by entity overlap (>=1 shared entity)
        let mut used: HashSet<String> = HashSet::new();
        let mut clusters: Vec<Vec<&PyramidNode>> = Vec::new();

        for node in &l0_nodes {
            if used.contains(&node.id) {
                continue;
            }
            let mut cluster = vec![node];
            used.insert(node.id.clone());
            let mut cluster_entities = node_entities.get(&node.id).cloned().unwrap_or_default();

            for other in &l0_nodes {
                if used.contains(&other.id) {
                    continue;
                }
                let other_ents = node_entities.get(&other.id).cloned().unwrap_or_default();
                let overlap: HashSet<_> = cluster_entities.intersection(&other_ents).collect();
                if !overlap.is_empty() {
                    cluster.push(other);
                    used.insert(other.id.clone());
                    cluster_entities.extend(other_ents);
                }
            }
            clusters.push(cluster);
        }

        info!("  {} clusters from entity overlap", clusters.len());

        for (ci_idx, cluster) in clusters.iter().enumerate() {
            if cancel.is_cancelled() {
                return Ok(failures);
            }

            let node_id = format!("L1-{ci_idx:03}");
            match get_resume_state(&db, slug, "doc_group", -1, 1, &node_id).await? {
                ResumeState::Complete => {
                    done += 1;
                    let _ = progress_tx
                        .send(BuildProgress {
                            done,
                            total: estimated_total,
                        })
                        .await;
                    continue;
                }
                ResumeState::StaleStep => {
                    warn!("  Doc group step exists but node is missing for {node_id}; rebuilding");
                }
                ResumeState::Missing => {}
            }

            let child_ids: Vec<String> = cluster.iter().map(|n| n.id.clone()).collect();
            let child_data: Vec<Value> = cluster
                .iter()
                .map(|n| serde_json::to_value(&n.topics).unwrap_or(serde_json::json!([])))
                .collect();

            // Get document names from chunks
            let mut doc_names = Vec::new();
            for n in cluster.iter() {
                if let Some(ci) = n.chunk_index {
                    let content = db_read(&db, {
                        let s = slug_owned.clone();
                        move |conn| db::get_chunk(conn, &s, ci)
                    })
                    .await?;
                    if let Some(content) = content {
                        let name = content
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim_start_matches("## DOCUMENT: ")
                            .to_string();
                        doc_names.push(name);
                    }
                }
            }

            let user_prompt = format!(
                "## DOCUMENTS IN THIS CLUSTER\n{}\n\n## CONTENT\n{}",
                doc_names.join(", "),
                serde_json::to_string_pretty(&child_data)?
            );

            info!("  L1 cluster [{ci_idx}] ({} docs)", cluster.len());
            let t0 = Instant::now();
            match call_and_parse(
                llm_config,
                DOC_GROUP_PROMPT,
                &user_prompt,
                &format!("doc-l1-{ci_idx}"),
            )
            .await
            {
                Ok(analysis) => {
                    let elapsed = t0.elapsed().as_secs_f64();
                    let topics_json = serde_json::to_string(
                        analysis.get("topics").unwrap_or(&serde_json::json!([])),
                    )?;
                    let output_json = serde_json::to_string(&analysis)?;
                    send_save_step(
                        writer_tx,
                        slug,
                        "doc_group",
                        -1,
                        1,
                        &node_id,
                        &output_json,
                        &llm_config.primary_model,
                        elapsed,
                    )
                    .await;

                    let node =
                        node_from_analysis(&analysis, &node_id, slug, 1, None, child_ids.clone());
                    send_save_node(writer_tx, node, Some(topics_json)).await;

                    for cid in &child_ids {
                        send_update_parent(writer_tx, slug, cid, &node_id).await;
                    }
                }
                Err(e) => {
                    warn!("  Doc L1 cluster [{ci_idx}] failed, skipping: {e}");
                    failures += 1;
                }
            }

            done += 1;
            let _ = progress_tx
                .send(BuildProgress {
                    done,
                    total: estimated_total,
                })
                .await;
        }
    }

    // ── Update progress total now that L1 is done ─────────────────────
    let l1_count_doc = {
        let slug_s = slug.to_string();
        db_read(&db, {
            let s = slug_s;
            move |conn| db::count_nodes_at_depth(conn, &s, 1)
        })
        .await
        .unwrap_or(0)
    };
    let est_l2_doc = (l1_count_doc + 1) / 2;
    let mut est_upper_doc = 0i64;
    let mut remaining_doc = est_l2_doc;
    while remaining_doc > 1 {
        remaining_doc = (remaining_doc + 1) / 2;
        est_upper_doc += remaining_doc;
    }
    let estimated_total = done + est_l2_doc + est_upper_doc;
    let _ = progress_tx
        .send(BuildProgress {
            done,
            total: estimated_total,
        })
        .await;

    // ── L2: Thread clustering ────────────────────────────────────────
    let l2_failures = build_threads_layer(
        db.clone(),
        writer_tx,
        llm_config,
        slug,
        cancel,
        progress_tx,
        &mut done,
        estimated_total,
    )
    .await?;
    failures += l2_failures;

    // ── L3+: Normal pairing ──────────────────────────────────────────
    let upper_failures = build_upper_layers(
        db.clone(),
        writer_tx,
        llm_config,
        slug,
        2,
        cancel,
        progress_tx,
        &mut done,
        estimated_total,
    )
    .await?;
    failures += upper_failures;

    // Update slug stats
    writer_tx
        .send(WriteOp::UpdateStats {
            slug: slug.to_string(),
        })
        .await
        .ok();

    if failures > 0 {
        warn!("Document pyramid build for '{slug}' completed with {failures} failure(s)");
    } else {
        info!("Document pyramid build complete for '{slug}'");
    }
    Ok(failures)
}

// ── SHARED PIPELINE STAGES ───────────────────────────────────────────────────

/// L1 positional pairing: pair adjacent L0 nodes with DISTILL_PROMPT.
/// Returns the number of node failures.
async fn build_l1_pairing(
    db: Arc<tokio::sync::Mutex<Connection>>,
    writer_tx: &mpsc::Sender<WriteOp>,
    llm_config: &LlmConfig,
    slug: &str,
    cancel: &CancellationToken,
    progress_tx: &mpsc::Sender<BuildProgress>,
    done: &mut i64,
    total: i64,
) -> Result<i32> {
    let mut failures: i32 = 0;
    let slug_owned = slug.to_string();
    let l0_nodes = db_read(&db, {
        let s = slug_owned.clone();
        move |conn| db::get_nodes_at_depth(conn, &s, 0)
    })
    .await?;
    if l0_nodes.len() <= 1 {
        return Ok(0);
    }

    let expected_l1 = (l0_nodes.len() + 1) / 2;
    let existing_l1 = db_read(&db, {
        let s = slug_owned.clone();
        move |conn| db::count_nodes_at_depth(conn, &s, 1)
    })
    .await?;
    if existing_l1 >= expected_l1 as i64 {
        info!("  L1: {} nodes (already complete)", existing_l1);
        *done += existing_l1;
        let _ = progress_tx.send(BuildProgress { done: *done, total }).await;
        return Ok(0);
    }

    info!(
        "=== DEPTH 1: DISTILL {} -> {} ===",
        l0_nodes.len(),
        expected_l1
    );

    let mut pair_idx = 0usize;
    let mut i = 0usize;
    while i < l0_nodes.len() {
        if cancel.is_cancelled() {
            return Ok(failures);
        }

        let node_id = format!("L1-{pair_idx:03}");

        match get_resume_state(&db, slug, "synth", -1, 1, &node_id).await? {
            ResumeState::Complete => {
                pair_idx += 1;
                i += 2;
                *done += 1;
                let _ = progress_tx.send(BuildProgress { done: *done, total }).await;
                continue;
            }
            ResumeState::StaleStep => {
                warn!("  L1 synth step exists but node is missing for {node_id}; rebuilding");
            }
            ResumeState::Missing => {}
        }

        if i + 1 < l0_nodes.len() {
            let left = &l0_nodes[i];
            let right = &l0_nodes[i + 1];

            let left_payload = child_payload_json(left);
            let right_payload = child_payload_json(right);

            let user_prompt = format!(
                "## SIBLING A (earlier)\n{}\n\n## SIBLING B (later)\n{}",
                serde_json::to_string_pretty(&left_payload)?,
                serde_json::to_string_pretty(&right_payload)?
            );

            info!("  [{} + {}] -> {node_id}", left.id, right.id);
            let t0 = Instant::now();
            match call_and_parse(
                llm_config,
                DISTILL_PROMPT,
                &user_prompt,
                &format!("l1-{pair_idx}"),
            )
            .await
            {
                Ok(analysis) => {
                    let elapsed = t0.elapsed().as_secs_f64();
                    let topics_json = serde_json::to_string(
                        analysis.get("topics").unwrap_or(&serde_json::json!([])),
                    )?;
                    let output_json = serde_json::to_string(&analysis)?;
                    send_save_step(
                        writer_tx,
                        slug,
                        "synth",
                        -1,
                        1,
                        &node_id,
                        &output_json,
                        &llm_config.primary_model,
                        elapsed,
                    )
                    .await;

                    let node = node_from_analysis(
                        &analysis,
                        &node_id,
                        slug,
                        1,
                        None,
                        vec![left.id.clone(), right.id.clone()],
                    );
                    send_save_node(writer_tx, node, Some(topics_json)).await;

                    send_update_parent(writer_tx, slug, &left.id, &node_id).await;
                    send_update_parent(writer_tx, slug, &right.id, &node_id).await;
                }
                Err(e) => {
                    warn!("  L1 pairing failed for {node_id}, carrying left node: {e}");
                    failures += 1;
                    // Carry left node as fallback
                    let mut node = left.clone();
                    node.id = node_id.clone();
                    node.depth = 1;
                    node.chunk_index = None;
                    node.children = vec![left.id.clone(), right.id.clone()];
                    send_save_node(writer_tx, node, None).await;
                    send_update_parent(writer_tx, slug, &left.id, &node_id).await;
                    send_update_parent(writer_tx, slug, &right.id, &node_id).await;
                }
            }

            i += 2;
        } else {
            // Odd node — carry up
            let carry = &l0_nodes[i];
            info!("  Carry up: {} -> {node_id}", carry.id);

            let mut node = carry.clone();
            node.id = node_id.clone();
            node.depth = 1;
            node.chunk_index = None;
            node.children = vec![carry.id.clone()];
            send_save_node(writer_tx, node, None).await;
            send_update_parent(writer_tx, slug, &carry.id, &node_id).await;

            i += 1;
        }

        pair_idx += 1;
        *done += 1;
        let _ = progress_tx.send(BuildProgress { done: *done, total }).await;
    }

    Ok(failures)
}

/// L2 thread clustering: collect all L1 topics, cluster into threads, synthesize thread narratives.
/// Returns the number of node failures.
async fn build_threads_layer(
    db: Arc<tokio::sync::Mutex<Connection>>,
    writer_tx: &mpsc::Sender<WriteOp>,
    llm_config: &LlmConfig,
    slug: &str,
    cancel: &CancellationToken,
    progress_tx: &mpsc::Sender<BuildProgress>,
    done: &mut i64,
    total: i64,
) -> Result<i32> {
    let mut failures: i32 = 0;
    let slug_owned = slug.to_string();
    let l1_nodes = db_read(&db, {
        let s = slug_owned.clone();
        move |conn| db::get_nodes_at_depth(conn, &s, 1)
    })
    .await?;
    if l1_nodes.len() < 2 {
        return Ok(0);
    }

    // Check if L2 already built
    let l2_count = db_read(&db, {
        let s = slug_owned.clone();
        move |conn| db::count_nodes_at_depth(conn, &s, 2)
    })
    .await?;

    // Step 1: Cluster topics
    let tc_exists = db_read(&db, {
        let s = slug_owned.clone();
        move |conn| db::step_exists(conn, &s, "thread_cluster", -1, 1, "")
    })
    .await?;
    let clusters: Value = if tc_exists {
        let output = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::get_step_output(conn, &s, "thread_cluster", -1)
        })
        .await?
        .unwrap_or_else(|| "{}".to_string());
        serde_json::from_str(&output)?
    } else {
        // Build topic inventory from L1 nodes
        let mut topic_inventory: Vec<Value> = Vec::new();
        for node in &l1_nodes {
            if node.topics.is_empty() {
                topic_inventory.push(serde_json::json!({
                    "source_node": node.id,
                    "topic_index": 0,
                    "name": node.headline,
                    "entities": [],
                }));
            } else {
                for (idx, topic) in node.topics.iter().enumerate() {
                    let top_entities: Vec<&str> =
                        topic.entities.iter().take(6).map(|s| s.as_str()).collect();
                    topic_inventory.push(serde_json::json!({
                        "source_node": node.id,
                        "topic_index": idx,
                        "name": topic.name,
                        "entities": top_entities,
                    }));
                }
            }
        }

        info!(
            "=== HORIZONTAL SCAN: {} topics across {} L1 nodes ===",
            topic_inventory.len(),
            l1_nodes.len()
        );

        let inv_json = serde_json::to_string_pretty(&topic_inventory)?;
        let est_tokens = inv_json.len() / 4;
        let batch_threshold = 30_000usize;

        let t0 = Instant::now();

        let clustering_result = if est_tokens > batch_threshold {
            // Batched clustering
            let batch_size = topic_inventory.len() / ((est_tokens / batch_threshold) + 1);
            let batches: Vec<Vec<Value>> = topic_inventory
                .chunks(batch_size.max(1))
                .map(|c| c.to_vec())
                .collect();
            info!(
                "  Splitting into {} batches (~{batch_size} topics each)",
                batches.len()
            );

            let mut batch_results: Vec<Value> = Vec::new();
            let mut batch_failed = false;
            for (bi, batch) in batches.iter().enumerate() {
                if cancel.is_cancelled() {
                    return Ok(failures);
                }
                info!(
                    "  Batch {}/{} ({} topics)",
                    bi + 1,
                    batches.len(),
                    batch.len()
                );
                let batch_json = serde_json::to_string_pretty(batch)?;
                match call_and_parse(
                    llm_config,
                    THREAD_CLUSTER_PROMPT,
                    &batch_json,
                    &format!("thread-batch-{bi}"),
                )
                .await
                {
                    Ok(bc) => batch_results.push(bc),
                    Err(e) => {
                        warn!("  Thread clustering batch {bi} failed: {e}");
                        batch_failed = true;
                        break;
                    }
                }
            }

            if batch_failed || batch_results.is_empty() {
                Err(anyhow!("Thread clustering batch failed"))
            } else {
                // Merge batch results
                let per_batch_threads: Vec<Value> = batch_results
                    .iter()
                    .filter_map(|bc| bc.get("threads").cloned())
                    .collect();
                let merge_input = serde_json::to_string_pretty(&per_batch_threads)?;

                info!("  Merging {} batch results", batch_results.len());
                call_and_parse(llm_config, MERGE_PROMPT, &merge_input, "thread-merge").await
            }
        } else {
            // Single call
            call_and_parse(
                llm_config,
                THREAD_CLUSTER_PROMPT,
                &inv_json,
                "thread-cluster",
            )
            .await
        };

        // If clustering failed, fall back to simple positional pairing (same as L1)
        let result = match clustering_result {
            Ok(r) => r,
            Err(e) => {
                warn!("Thread clustering LLM failed, falling back to positional pairing: {e}");
                failures += 1;
                // Build synthetic threads by pairing adjacent L1 nodes
                let mut fallback_threads: Vec<Value> = Vec::new();
                for (idx, chunk) in l1_nodes.chunks(2).enumerate() {
                    let assignments: Vec<Value> = chunk
                        .iter()
                        .enumerate()
                        .flat_map(|(_, node)| {
                            if node.topics.is_empty() {
                                vec![serde_json::json!({
                                    "source_node": node.id,
                                    "topic_index": 0,
                                    "topic_name": node.headline,
                                })]
                            } else {
                                node.topics
                                    .iter()
                                    .enumerate()
                                    .map(|(ti, t)| {
                                        serde_json::json!({
                                            "source_node": node.id,
                                            "topic_index": ti,
                                            "topic_name": t.name,
                                        })
                                    })
                                    .collect()
                            }
                        })
                        .collect();
                    fallback_threads.push(serde_json::json!({
                        "name": format!("Group {}", idx + 1),
                        "description": "Positional grouping (clustering fallback)",
                        "assignments": assignments,
                    }));
                }
                serde_json::json!({ "threads": fallback_threads })
            }
        };

        let elapsed = t0.elapsed().as_secs_f64();
        let output_json = serde_json::to_string(&result)?;
        send_save_step(
            writer_tx,
            slug,
            "thread_cluster",
            -1,
            1,
            "",
            &output_json,
            &llm_config.primary_model,
            elapsed,
        )
        .await;

        result
    };

    let threads = clusters
        .get("threads")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    if threads.is_empty() {
        info!("  No threads found!");
        return Ok(failures);
    }

    // Validate all L1 nodes are assigned to at least one thread
    let assigned_l1_ids: HashSet<String> = threads
        .iter()
        .flat_map(|t| {
            t.get("assignments")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
                .filter_map(|a| {
                    a.get("source_node")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
        })
        .collect();
    let all_l1_ids: HashSet<String> = l1_nodes.iter().map(|n| n.id.clone()).collect();
    let orphaned: Vec<&String> = all_l1_ids.difference(&assigned_l1_ids).collect();
    let mut threads = threads;
    if !orphaned.is_empty() {
        tracing::warn!(
            "Thread clustering left {} L1 nodes unassigned: {:?}. Adding to 'Other' catch-all thread.",
            orphaned.len(),
            orphaned
        );
        let other_assignments: Vec<Value> = orphaned
            .iter()
            .map(|id| serde_json::json!({ "source_node": id, "topic_index": 0, "topic_name": "Unassigned" }))
            .collect();
        threads.push(serde_json::json!({
            "name": "Other",
            "description": "Catch-all thread for topics not assigned by the LLM clustering step.",
            "assignments": other_assignments,
        }));
    }

    if l2_count >= threads.len() as i64 {
        info!("  L2: {} thread nodes (already complete)", l2_count);
        *done += l2_count;
        let _ = progress_tx.send(BuildProgress { done: *done, total }).await;
        return Ok(failures);
    }

    // Step 2: Build L2 nodes from thread narratives
    info!("=== DEPTH 2: BUILD {} THREAD NARRATIVES ===", threads.len());

    for (thread_idx, thread) in threads.iter().enumerate() {
        if cancel.is_cancelled() {
            return Ok(failures);
        }

        let node_id = format!("L2-{thread_idx:03}");

        let thread_result: Result<()> = async {
            match get_resume_state(&db, slug, "synth", -1, 2, &node_id).await? {
                ResumeState::Complete => {
                    *done += 1;
                    let _ = progress_tx.send(BuildProgress { done: *done, total }).await;
                    return Ok(());
                }
                ResumeState::StaleStep => {
                    warn!("  Thread synth step exists but node is missing for {node_id}; rebuilding");
                }
                ResumeState::Missing => {}
            }

            let thread_name = thread
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Thread");

            let assignments = thread
                .get("assignments")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Gather full topic data
            let mut assigned_topics: Vec<Value> = Vec::new();
            let mut contributing_nodes: Vec<String> = Vec::new();

            for assignment in &assignments {
                let src_node_id = assignment
                    .get("source_node")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let topic_idx = assignment
                    .get("topic_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;

                let l1_node = db_read(&db, { let s = slug_owned.clone(); let snid = src_node_id.to_string(); move |conn| db::get_node(conn, &s, &snid) }).await?;
                if let Some(l1_node) = l1_node {
                    if topic_idx < l1_node.topics.len() {
                        let mut topic_val = serde_json::to_value(&l1_node.topics[topic_idx])?;
                        if let Some(obj) = topic_val.as_object_mut() {
                            obj.insert("source_node".to_string(), serde_json::json!(src_node_id));
                        }
                        assigned_topics.push(topic_val);
                    } else {
                        assigned_topics.push(serde_json::json!({
                            "name": l1_node.headline,
                            "current": l1_node.distilled,
                            "entities": [],
                            "corrections": l1_node.corrections,
                            "decisions": l1_node.decisions,
                            "source_node": src_node_id,
                        }));
                    }

                    if !contributing_nodes.contains(&src_node_id.to_string()) {
                        contributing_nodes.push(src_node_id.to_string());
                    }
                }
            }

            if assigned_topics.is_empty() {
                info!("  {node_id} ({thread_name}): no topics found, skipping");
                return Ok(());
            }

            // Sort by source_node for chronological order, add order numbers + temporal authority
            assigned_topics.sort_by(|a, b| {
                let sa = a.get("source_node").and_then(|v| v.as_str()).unwrap_or("");
                let sb = b.get("source_node").and_then(|v| v.as_str()).unwrap_or("");
                sa.cmp(sb)
            });
            let late_threshold = (assigned_topics.len() as f64 * 0.7) as usize;
            for (idx, topic) in assigned_topics.iter_mut().enumerate() {
                if let Some(obj) = topic.as_object_mut() {
                    obj.insert("order".to_string(), serde_json::json!(idx + 1));
                    if idx >= late_threshold {
                        obj.insert(
                            "temporal_authority".to_string(),
                            serde_json::json!("LATE — AUTHORITATIVE"),
                        );
                    }
                }
            }

            let user_prompt = format!(
                "## THREAD: {thread_name}\n\n## TOPICS (chronological — higher order = later = more authoritative)\n{}",
                serde_json::to_string_pretty(&assigned_topics)?
            );

            info!("  {node_id} ({thread_name}, {} topics)", assigned_topics.len());
            let t0 = Instant::now();
            match call_and_parse(llm_config, THREAD_NARRATIVE_PROMPT, &user_prompt, &format!("thread-{thread_idx}")).await {
                Ok(analysis) => {
                    let elapsed = t0.elapsed().as_secs_f64();
                    let topics_json = serde_json::to_string(
                        analysis.get("topics").unwrap_or(&serde_json::json!([]))
                    )?;
                    let output_json = serde_json::to_string(&analysis)?;
                    send_save_step(
                        writer_tx, slug, "synth", -1, 2, &node_id, &output_json,
                        &llm_config.primary_model, elapsed,
                    ).await;

                    let node = node_from_analysis(&analysis, &node_id, slug, 2, None, contributing_nodes.clone());
                    send_save_node(writer_tx, node, Some(topics_json)).await;

                    // Update parent_id for each contributing L1 node
                    for child_id in &contributing_nodes {
                        send_update_parent(writer_tx, slug, child_id, &node_id).await;
                    }
                }
                Err(e) => {
                    warn!("  Thread narrative failed for {node_id} ({thread_name}), skipping: {e}");
                    failures += 1;
                }
            }

            *done += 1;
            let _ = progress_tx.send(BuildProgress { done: *done, total }).await;
            Ok(())
        }.await;

        if let Err(e) = thread_result {
            warn!("  Thread {node_id} processing failed, skipping: {e}");
            failures += 1;
            *done += 1;
            let _ = progress_tx.send(BuildProgress { done: *done, total }).await;
        }
    }

    Ok(failures)
}

/// Build upper layers (L3+) by pairing adjacent nodes until only one apex remains.
/// Returns the number of node failures.
async fn build_upper_layers(
    db: Arc<tokio::sync::Mutex<Connection>>,
    writer_tx: &mpsc::Sender<WriteOp>,
    llm_config: &LlmConfig,
    slug: &str,
    start_depth: i64,
    cancel: &CancellationToken,
    progress_tx: &mpsc::Sender<BuildProgress>,
    done: &mut i64,
    total: i64,
) -> Result<i32> {
    let mut failures: i32 = 0;
    let slug_owned = slug.to_string();
    let mut depth = start_depth;

    loop {
        let current_nodes = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::get_nodes_at_depth(conn, &s, depth)
        })
        .await?;
        if current_nodes.len() <= 1 {
            if let Some(apex) = current_nodes.first() {
                info!("=== APEX: {} ===", apex.id);
            }
            break;
        }

        depth += 1;
        let expected = (current_nodes.len() + 1) / 2;
        let existing = db_read(&db, {
            let s = slug_owned.clone();
            move |conn| db::count_nodes_at_depth(conn, &s, depth)
        })
        .await?;
        if existing == expected as i64 {
            info!("  Depth {depth}: {existing} nodes (already complete)");
            *done += existing;
            let _ = progress_tx.send(BuildProgress { done: *done, total }).await;
            continue;
        }

        info!(
            "=== DEPTH {depth}: DISTILL {} -> {expected} ===",
            current_nodes.len()
        );

        let mut pair_idx = 0usize;
        let mut i = 0usize;
        while i < current_nodes.len() {
            if cancel.is_cancelled() {
                return Ok(failures);
            }

            let node_id = format!("L{depth}-{pair_idx:03}");

            match get_resume_state(&db, slug, "synth", -1, depth, &node_id).await? {
                ResumeState::Complete => {
                    pair_idx += 1;
                    i += 2;
                    *done += 1;
                    let _ = progress_tx.send(BuildProgress { done: *done, total }).await;
                    continue;
                }
                ResumeState::StaleStep => {
                    warn!("  Upper-layer synth step exists but node is missing for {node_id}; rebuilding");
                }
                ResumeState::Missing => {}
            }

            if i + 1 < current_nodes.len() {
                let left = &current_nodes[i];
                let right = &current_nodes[i + 1];

                let left_payload = child_payload_json(left);
                let right_payload = child_payload_json(right);

                let user_prompt = format!(
                    "## SIBLING A (earlier)\n{}\n\n## SIBLING B (later)\n{}",
                    serde_json::to_string_pretty(&left_payload)?,
                    serde_json::to_string_pretty(&right_payload)?
                );

                info!("  [{} + {}] -> {node_id}", left.id, right.id);
                let t0 = Instant::now();

                // Retry up to 3 times on LLM failure, then carry left node on final failure
                let mut analysis_result = None;
                for attempt in 0..3 {
                    match call_and_parse(
                        llm_config,
                        DISTILL_PROMPT,
                        &user_prompt,
                        &format!("synth-d{depth}-{pair_idx}"),
                    )
                    .await
                    {
                        Ok(a) => {
                            analysis_result = Some(a);
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "  Synthesis attempt {}/{} failed for {node_id}: {e}",
                                attempt + 1,
                                3
                            );
                            if attempt < 2 {
                                tokio::time::sleep(std::time::Duration::from_secs(
                                    2u64.pow(attempt as u32 + 1),
                                ))
                                .await;
                            }
                        }
                    }
                }

                if let Some(analysis) = analysis_result {
                    let elapsed = t0.elapsed().as_secs_f64();

                    let topics_json = serde_json::to_string(
                        analysis.get("topics").unwrap_or(&serde_json::json!([])),
                    )?;
                    let output_json = serde_json::to_string(&analysis)?;
                    send_save_step(
                        writer_tx,
                        slug,
                        "synth",
                        -1,
                        depth,
                        &node_id,
                        &output_json,
                        &llm_config.primary_model,
                        elapsed,
                    )
                    .await;

                    let node = node_from_analysis(
                        &analysis,
                        &node_id,
                        slug,
                        depth,
                        None,
                        vec![left.id.clone(), right.id.clone()],
                    );
                    send_save_node(writer_tx, node, Some(topics_json)).await;

                    send_update_parent(writer_tx, slug, &left.id, &node_id).await;
                    send_update_parent(writer_tx, slug, &right.id, &node_id).await;
                } else {
                    // All retries failed — carry left node up as fallback
                    tracing::error!(
                        "  All 3 synthesis attempts failed for {node_id}. Carrying left node."
                    );
                    failures += 1;
                    let mut node = left.clone();
                    node.id = node_id.clone();
                    node.depth = depth;
                    node.chunk_index = None;
                    node.children = vec![left.id.clone(), right.id.clone()];
                    send_save_node(writer_tx, node, None).await;
                    send_update_parent(writer_tx, slug, &left.id, &node_id).await;
                    send_update_parent(writer_tx, slug, &right.id, &node_id).await;
                }

                i += 2;
            } else {
                // Carry up odd node
                let carry = &current_nodes[i];
                info!("  Carry up: {} -> {node_id}", carry.id);

                let mut node = carry.clone();
                node.id = node_id.clone();
                node.depth = depth;
                node.chunk_index = None;
                node.children = vec![carry.id.clone()];
                send_save_node(writer_tx, node, None).await;
                send_update_parent(writer_tx, slug, &carry.id, &node_id).await;

                i += 1;
            }

            pair_idx += 1;
            *done += 1;
            let _ = progress_tx.send(BuildProgress { done: *done, total }).await;
        }
    }

    Ok(failures)
}

/// Build a JSON payload from a node, preferring topics if available.
pub(crate) fn child_payload_json(node: &PyramidNode) -> Value {
    if !node.topics.is_empty() {
        serde_json::json!({
            "headline": node.headline,
            "distilled": node.distilled,
            "topics": node.topics,
        })
    } else {
        serde_json::json!({
            "headline": node.headline,
            "distilled": node.distilled,
            "corrections": node.corrections,
            "decisions": node.decisions,
            "terms": node.terms,
        })
    }
}

// ── MECHANICAL PASSES (CODE PIPELINE) ────────────────────────────────────────

/// Data structures for the import graph / mechanical analysis.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ImportGraph {
    pub imports: HashMap<String, Vec<String>>,
    pub exports: HashMap<String, Vec<String>>,
    pub ipc_frontend: HashMap<String, Vec<String>>,
    pub ipc_backend: HashMap<String, Vec<String>>,
    pub ipc_map: Vec<IpcBinding>,
    pub spawn_counts: HashMap<String, Vec<SpawnEntry>>,
    pub string_resources: HashMap<String, Vec<String>>,
    pub complexity: HashMap<String, FileComplexity>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IpcBinding {
    pub frontend: String,
    pub command: String,
    pub backend: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpawnEntry {
    pub call_type: String,
    pub context: String,
    pub line: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileComplexity {
    pub lines: i64,
    pub functions: i64,
    pub imports: i64,
    pub exports: i64,
    pub spawn_count: i64,
}

/// Mechanical pass: extract import graph, IPC map, spawn counts, string resources,
/// and per-file complexity from code chunks.  Pure regex, no LLM.
fn extract_import_graph(
    conn: &Connection,
    slug: &str,
    writer_tx: Option<&mpsc::Sender<WriteOp>>,
) -> Result<ImportGraph> {
    // Check if already computed
    if db::step_exists(conn, slug, "import_graph", -1, -1, "")? {
        if let Some(output) = db::get_step_output(conn, slug, "import_graph", -1)? {
            if let Ok(graph) = serde_json::from_str::<ImportGraph>(&output) {
                return Ok(graph);
            }
        }
    }

    let num_chunks = db::count_chunks(conn, slug)?;
    let mut graph = ImportGraph::default();

    static RUST_USE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"use\s+(?:crate::)?(\w+)").unwrap());
    static RUST_MOD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"mod\s+(\w+)\s*;").unwrap());
    static RUST_PUB_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"pub\s+(?:async\s+)?(?:fn|struct|enum|type)\s+(\w+)").unwrap()
    });
    static RUST_TAURI_CMD_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"#\[tauri::command\]\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)").unwrap()
    });
    static TS_IMPORT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"import\s+.*?from\s+['"]([^'"]+)['"]"#).unwrap());
    static TS_EXPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"export\s+(?:default\s+)?(?:function|const|class|interface|type|enum)\s+(\w+)")
            .unwrap()
    });
    static TS_INVOKE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"invoke\s*[<(]\s*['"](\w+)['"]"#).unwrap());

    static SPAWN_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:tokio::)?(?:async_runtime::)?spawn\s*\(").unwrap());
    static TIMER_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(setInterval|setTimeout)\s*\(").unwrap());
    static TOKIO_TIME_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"tokio::time::(interval|sleep)\s*\(").unwrap());

    static TABLE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\.from\s*\(\s*['"](\w+)['"]"#).unwrap());
    static STORAGE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"storage\s*\(\s*\)\s*\.from\s*\(\s*['"]([^'"]+)['"]"#).unwrap()
    });
    static URL_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"['"]((https?://[^'"]+)|(\/api\/[^'"]+))['"]"#).unwrap());
    static FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"['"]([^'"]*\.(json|toml|db|sqlite|log|txt|png|jpg|mp3|wav|ogg))['"]"#)
            .unwrap()
    });
    static RUST_FN_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:pub\s+)?(?:async\s+)?fn\s+\w+").unwrap());
    static TS_FN_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:export\s+)?(?:default\s+)?(?:function|const\s+\w+\s*=\s*(?:async\s+)?\()")
            .unwrap()
    });

    let rust_use_re = &*RUST_USE_RE;
    let rust_mod_re = &*RUST_MOD_RE;
    let rust_pub_re = &*RUST_PUB_RE;
    let rust_tauri_cmd_re = &*RUST_TAURI_CMD_RE;
    let ts_import_re = &*TS_IMPORT_RE;
    let ts_export_re = &*TS_EXPORT_RE;
    let ts_invoke_re = &*TS_INVOKE_RE;
    let spawn_re = &*SPAWN_RE;
    let timer_re = &*TIMER_RE;
    let tokio_time_re = &*TOKIO_TIME_RE;
    let table_re = &*TABLE_RE;
    let storage_re = &*STORAGE_RE;
    let url_re = &*URL_RE;
    let file_re = &*FILE_RE;
    let rust_fn_re = &*RUST_FN_RE;
    let ts_fn_re = &*TS_FN_RE;

    for ci in 0..num_chunks {
        let content = match db::get_chunk(conn, slug, ci)? {
            Some(c) => c,
            None => continue,
        };

        // Parse header
        let mut file_path = String::new();
        let mut language = String::new();
        for line in content.lines().take(4) {
            if let Some(fp) = line.strip_prefix("## FILE: ") {
                file_path = fp.split(" [").next().unwrap_or("").trim().to_string();
            }
            if let Some(lang) = line.strip_prefix("## LANGUAGE: ") {
                language = lang.trim().to_string();
            }
        }
        if file_path.is_empty() {
            continue;
        }

        let mut file_imports: Vec<String> = Vec::new();
        let mut file_exports: Vec<String> = Vec::new();

        match language.as_str() {
            "rust" => {
                for m in rust_use_re.captures_iter(&content) {
                    file_imports.push(m[1].to_string());
                }
                for m in rust_mod_re.captures_iter(&content) {
                    file_imports.push(m[1].to_string());
                }
                for m in rust_pub_re.captures_iter(&content) {
                    file_exports.push(m[1].to_string());
                }
                if content.contains("#[tauri::command]") {
                    for m in rust_tauri_cmd_re.captures_iter(&content) {
                        graph
                            .ipc_backend
                            .entry(file_path.clone())
                            .or_default()
                            .push(m[1].to_string());
                    }
                }
            }
            "typescript" | "javascript" => {
                for m in ts_import_re.captures_iter(&content) {
                    file_imports.push(m[1].to_string());
                }
                for m in ts_export_re.captures_iter(&content) {
                    file_exports.push(m[1].to_string());
                }
                for m in ts_invoke_re.captures_iter(&content) {
                    graph
                        .ipc_frontend
                        .entry(file_path.clone())
                        .or_default()
                        .push(m[1].to_string());
                }
            }
            _ => {}
        }

        file_imports.sort();
        file_imports.dedup();
        file_exports.sort();
        file_exports.dedup();

        if !file_imports.is_empty() {
            graph
                .imports
                .insert(file_path.clone(), file_imports.clone());
        }
        if !file_exports.is_empty() {
            graph
                .exports
                .insert(file_path.clone(), file_exports.clone());
        }

        // Spawn/timer detection
        let lines: Vec<&str> = content.lines().collect();
        let mut spawns = Vec::new();
        for (li, line) in lines.iter().enumerate() {
            if spawn_re.is_match(line) {
                let ctx: String = lines[li..lines.len().min(li + 3)]
                    .iter()
                    .map(|l| l.trim())
                    .collect::<Vec<_>>()
                    .join(" ");
                spawns.push(SpawnEntry {
                    call_type: "spawn".into(),
                    context: safe_slice_end(&ctx, 120).to_string(),
                    line: li,
                });
            }
            if let Some(m) = timer_re.captures(line) {
                let ctx: String = lines[li..lines.len().min(li + 2)]
                    .iter()
                    .map(|l| l.trim())
                    .collect::<Vec<_>>()
                    .join(" ");
                spawns.push(SpawnEntry {
                    call_type: m[1].to_string(),
                    context: safe_slice_end(&ctx, 120).to_string(),
                    line: li,
                });
            }
            if let Some(m) = tokio_time_re.captures(line) {
                let ctx: String = lines[li..lines.len().min(li + 2)]
                    .iter()
                    .map(|l| l.trim())
                    .collect::<Vec<_>>()
                    .join(" ");
                spawns.push(SpawnEntry {
                    call_type: format!("tokio::{}", &m[1]),
                    context: safe_slice_end(&ctx, 120).to_string(),
                    line: li,
                });
            }
        }
        if !spawns.is_empty() {
            graph.spawn_counts.insert(file_path.clone(), spawns);
        }

        // String resource extraction
        let mut resources: Vec<String> = Vec::new();
        for m in table_re.captures_iter(&content) {
            resources.push(format!("table: {}", &m[1]));
        }
        for m in storage_re.captures_iter(&content) {
            resources.push(format!("storage bucket: {}", &m[1]));
        }
        for m in url_re.captures_iter(&content) {
            resources.push(format!("url: {}", &m[1]));
        }
        for m in file_re.captures_iter(&content) {
            resources.push(format!("file: {}", &m[1]));
        }
        resources.sort();
        resources.dedup();
        if !resources.is_empty() {
            graph.string_resources.insert(file_path.clone(), resources);
        }

        // Per-file complexity
        let fn_count = if language == "rust" {
            rust_fn_re.find_iter(&content).count() as i64
        } else {
            ts_fn_re.find_iter(&content).count() as i64
        };

        graph.complexity.insert(
            file_path.clone(),
            FileComplexity {
                lines: content.matches('\n').count() as i64 + 1,
                functions: fn_count,
                imports: graph
                    .imports
                    .get(&file_path)
                    .map(|v| v.len() as i64)
                    .unwrap_or(0),
                exports: graph
                    .exports
                    .get(&file_path)
                    .map(|v| v.len() as i64)
                    .unwrap_or(0),
                spawn_count: graph
                    .spawn_counts
                    .get(&file_path)
                    .map(|v| v.len() as i64)
                    .unwrap_or(0),
            },
        );
    }

    // Build IPC map
    let mut backend_cmds: HashMap<String, String> = HashMap::new();
    for (f, cmds) in &graph.ipc_backend {
        for cmd in cmds {
            backend_cmds.insert(cmd.clone(), f.clone());
        }
    }
    for (f, cmds) in &graph.ipc_frontend {
        for cmd in cmds {
            if let Some(backend) = backend_cmds.get(cmd) {
                graph.ipc_map.push(IpcBinding {
                    frontend: f.clone(),
                    command: cmd.clone(),
                    backend: backend.clone(),
                });
            }
        }
    }

    // Save the import graph step via writer channel if available, else direct write
    let output = serde_json::to_string(&graph)?;
    if let Some(tx) = writer_tx {
        tx.blocking_send(WriteOp::SaveStep {
            slug: slug.to_string(),
            step_type: "import_graph".to_string(),
            chunk_index: -1,
            depth: -1,
            node_id: String::new(),
            output_json: output,
            model: "mechanical".to_string(),
            elapsed: 0.0,
        })
        .map_err(|e| anyhow!("Failed to send SaveStep for import_graph: {e}"))?;
    } else {
        db::save_step(
            conn,
            slug,
            "import_graph",
            -1,
            -1,
            "",
            &output,
            "mechanical",
            0.0,
        )?;
    }

    info!(
        "Mechanical analysis: {} files with imports, {} IPC bindings, {} spawn sites",
        graph.imports.len(),
        graph.ipc_map.len(),
        graph.spawn_counts.values().map(|v| v.len()).sum::<usize>()
    );

    Ok(graph)
}

/// Cluster files by import relationships. Returns list of file groups.
fn cluster_by_imports(graph: &ImportGraph) -> Vec<Vec<String>> {
    let all_files: HashSet<String> = graph
        .imports
        .keys()
        .chain(graph.exports.keys())
        .cloned()
        .collect();

    // Map module names to file paths
    let mut file_by_module: HashMap<String, String> = HashMap::new();
    for f in &all_files {
        let stem = Path::new(f)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        file_by_module.insert(stem, f.clone());
    }

    // Build undirected adjacency
    let mut adjacency: HashMap<String, HashSet<String>> = all_files
        .iter()
        .map(|f| (f.clone(), HashSet::new()))
        .collect();

    for (f, imported_modules) in &graph.imports {
        for mod_name in imported_modules {
            // Rust: module name -> module.rs
            if let Some(target) = file_by_module.get(mod_name) {
                if target != f {
                    adjacency
                        .entry(f.clone())
                        .or_default()
                        .insert(target.clone());
                    adjacency
                        .entry(target.clone())
                        .or_default()
                        .insert(f.clone());
                }
            }
            // TS/JS relative imports
            if mod_name.starts_with("./") || mod_name.starts_with("../") {
                let last_part = mod_name.rsplit('/').next().unwrap_or("");
                for candidate in &all_files {
                    if candidate != f && candidate.contains(last_part) {
                        adjacency
                            .entry(f.clone())
                            .or_default()
                            .insert(candidate.clone());
                        adjacency
                            .entry(candidate.clone())
                            .or_default()
                            .insert(f.clone());
                    }
                }
            }
        }
    }

    // BFS to find connected components
    let mut visited: HashSet<String> = HashSet::new();
    let mut clusters: Vec<Vec<String>> = Vec::new();

    for start in &all_files {
        if visited.contains(start) {
            continue;
        }
        let mut cluster = Vec::new();
        let mut queue = vec![start.clone()];
        while let Some(node) = queue.pop() {
            if visited.contains(&node) {
                continue;
            }
            visited.insert(node.clone());
            cluster.push(node.clone());
            if let Some(neighbors) = adjacency.get(&node) {
                for n in neighbors {
                    if !visited.contains(n) {
                        queue.push(n.clone());
                    }
                }
            }
        }
        cluster.sort();
        clusters.push(cluster);
    }

    // Split clusters > 8 files
    let mut final_clusters = Vec::new();
    for cluster in clusters {
        if cluster.len() <= 8 {
            final_clusters.push(cluster);
        } else {
            // Split by directory or just chunk into groups of 6
            for chunk in cluster.chunks(6) {
                final_clusters.push(chunk.to_vec());
            }
        }
    }

    final_clusters
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_node(slug: &str, node_id: &str, depth: i64) -> PyramidNode {
        PyramidNode {
            id: node_id.to_string(),
            slug: slug.to_string(),
            depth,
            chunk_index: None,
            headline: "Test".to_string(),
            distilled: "Test distilled".to_string(),
            topics: Vec::new(),
            corrections: Vec::new(),
            decisions: Vec::new(),
            terms: Vec::new(),
            dead_ends: Vec::new(),
            self_prompt: String::new(),
            children: Vec::new(),
            parent_id: None,
            superseded_by: None,
            created_at: String::new(),
        }
    }

    async fn setup_resume_test_db() -> Arc<tokio::sync::Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        db::init_pyramid_db(&conn).unwrap();
        db::create_slug(
            &conn,
            "resume-test",
            &ContentType::Conversation,
            "/tmp/resume-test",
        )
        .unwrap();
        Arc::new(tokio::sync::Mutex::new(conn))
    }

    #[tokio::test]
    async fn resume_state_marks_step_without_node_as_stale() {
        let db = setup_resume_test_db().await;
        {
            let conn = db.lock().await;
            db::save_step(
                &conn,
                "resume-test",
                "synth",
                -1,
                3,
                "L3-000",
                "{\"headline\":\"x\",\"distilled\":\"y\"}",
                "test-model",
                0.1,
            )
            .unwrap();
        }

        let state = get_resume_state(&db, "resume-test", "synth", -1, 3, "L3-000")
            .await
            .unwrap();

        assert_eq!(state, ResumeState::StaleStep);
    }

    #[tokio::test]
    async fn resume_state_marks_step_with_node_as_complete() {
        let db = setup_resume_test_db().await;
        {
            let conn = db.lock().await;
            db::save_step(
                &conn,
                "resume-test",
                "synth",
                -1,
                3,
                "L3-000",
                "{\"headline\":\"x\",\"distilled\":\"y\"}",
                "test-model",
                0.1,
            )
            .unwrap();
            db::save_node(&conn, &test_node("resume-test", "L3-000", 3), None).unwrap();
        }

        let state = get_resume_state(&db, "resume-test", "synth", -1, 3, "L3-000")
            .await
            .unwrap();

        assert_eq!(state, ResumeState::Complete);
    }
}
