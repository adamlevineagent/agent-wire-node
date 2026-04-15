## DOCUMENT: architecture/delta-chain-implementation-plan-v3.md

# Delta-Chain Knowledge Pyramid — Complete Implementation Plan (v3)

> **v3 — incorporates corrections from two full audit cycles (8 independent auditors, 2 cycles). Architecture validated as structurally sound.**

**Supersedes:** `delta-chain-implementation-plan.md` (v1, 840 lines), `delta-chain-implementation-plan-v2.md`
**Audit basis:** Stage 1 informed audit, 2026-03-22 — 4 critical, 12 major, ~10 minor findings applied. Stage 2 deep audit — 8 auditors across 2 cycles, additional corrections below.

## Prerequisites

Read these first:
1. `/docs/architecture/recursive-delta-chain-knowledge-pyramid.md` — the architecture
2. `/docs/architecture/partner-context-system-v2.md` — the partner memory model
3. `/docs/architecture/pyramid-annotation-api-plan.md` — the annotation overlay system
4. The conversation pyramid at slug `mainclaudeconversation` in the node's pyramid.db (1164 nodes covering the full design evolution)

## Current State (What Exists)

### agent-wire-node (`/Users/adamlevine/AI Project Files/agent-wire-node`)
- **Pyramid engine** (9 Rust modules in `src-tauri/src/pyramid/`): db.rs, types.rs, query.rs, ingest.rs, build.rs, slug.rs, llm.rs, routes.rs, mod.rs
- **Partner system** (4 Rust modules in `src-tauri/src/partner/`): mod.rs, context.rs, conversation.rs, routes.rs
- **Admin UI** (React/Tauri): PyramidDashboard, AddWorkspace, BuildProgress, PyramidSettings, PyramidFirstRun
- All three build pipelines working (conversation, code, documents)
- 18 pyramid HTTP endpoints + 5 partner HTTP endpoints
- Dennis responds to messages using the nav skeleton + pyramid brain
- **NOT working**: Dennis's tool calls (pyramid_query, context_schedule) — format issues with OpenRouter function-calling API
- **Known partner seam bugs** (see Phase 0): BrainState mismatch, API path params, PartnerResponse alignment, DennisState serialization, UTF-8 panic in context.rs, duplicate node_from_row, unbounded session cache, duplicate auth middleware

### vibesmithy (`/Users/adamlevine/AI Project Files/vibesmithy`)
- Space view with level tabs, drill-down, entity highlighting, navigation history
- ChatPanel, MessageBubble, DennisAvatar, BrainStateBar
- Settings, Pyramids selector, Search, Desktop sidebar
- usePartner hook with optimistic UI
- 18 pyramid API functions + 5 partner API functions in node-client.ts

### Pyramid databases
- `pyramid.db` at `~/Library/Application Support/wire-node/pyramid.db`
  - Slugs: mainclaudeconversation (1164 nodes), agent-wire-node (137 nodes), docs (150 nodes), mega-project (2735 nodes)
- `partner.db` at `~/Library/Application Support/wire-node/partner.db`
  - Sessions table for Dennis conversations

---

## Phase 0: Fix Partner Seam Bugs

**Priority: Immediate. These are blocking real-world testing.**

### Bug List

These are known issues found during audit where the partner system's seams don't align with the rest of the codebase:

1. **BrainState mismatch** — the `BrainState` enum in `partner/mod.rs` uses variants that don't match what vibesmithy expects. Align the enum values with the frontend's `BrainStateBar` component.

2. **API path params** — `partner/routes.rs` registers some routes with `:param` style but the handler extracts them as query params (or vice versa). Audit all 5 partner routes for consistency.

3. **PartnerResponse alignment** — the JSON shape returned by `handle_message` doesn't match the TypeScript `PartnerResponse` type in vibesmithy's `node-client.ts`. Fields are missing or misnamed.

4. **DennisState serialization** — `DennisState` derives `Serialize` but some fields use types that don't implement `Serialize` (e.g., raw `Connection` handles). These fields need `#[serde(skip)]` or the struct needs restructuring.

5. **UTF-8 panic in context.rs** — `assemble_context_window` slices a string by byte index without checking UTF-8 boundaries. Replace with `str::floor_char_boundary()` or equivalent safe slicing.

6. **Duplicate node conversion** — there are two implementations: `node_from_row` in `db.rs` and `row_to_node` in `query.rs` that have drifted apart. Consolidate to one canonical version.

7. **Unbounded session cache** — `sessions: HashMap<String, Session>` in the partner state grows without bound. Add LRU eviction or session expiry (e.g., 24 hours idle).

8. **Duplicate auth middleware** — auth token check is applied in both the route handler and a middleware layer, causing double-validation. Remove one.

### Approach

For each bug: identify the file, write the fix, test with a curl call or vibesmithy interaction. These are all small targeted fixes — no architectural changes.

**Time estimate: 4-6 hours**

---

## Phase 1: Fix Dennis Tool Execution

**Can proceed in parallel with Phase 0.**

### Problem
Dennis generates tool call text (`<pyramid_query>...</pyramid_query>`) in his responses instead of using the OpenRouter function-calling API properly. The `call_partner` function sends `tools` in the request but the model ignores them and puts tool calls inline in content.

### Root Cause
The model being used (`anthropic/claude-sonnet-4-20250514` or `xiaomi/mimo-v2-pro`) may not support OpenRouter's function-calling format. Different models on OpenRouter handle tools differently. Some require `tool_choice: "auto"`, some need the Anthropic-specific format.

### Fix

**File: `src-tauri/src/partner/conversation.rs`**

Option A (preferred): Add `tool_choice: "auto"` to the request payload:
```rust
// In call_partner, add to the request body:
if let Some(ref tools) = tools {
    body["tools"] = tools.clone();
    body["tool_choice"] = serde_json::json!("auto");
}
```

Option B (fallback): If the model still puts tools inline, parse them from the content text:
```rust
fn parse_inline_tool_calls(content: &str) -> (String, Vec<ToolCall>) {
    // Regex for <tool name="pyramid_query">{...}</tool>
    // Extract tool name and JSON arguments
    // Return cleaned content + parsed tool calls
}
```

Option C (nuclear): Switch to a model known to support function calling well. Test with:
- `anthropic/claude-sonnet-4-20250514` (native tool support)
- `openai/gpt-4o` (native function calling)
- `google/gemini-2.5-flash` (native function calling)

### Verification
```bash
curl -X POST -H "Authorization: Bearer vibesmithy-test-token" \
  -H "Content-Type: application/json" \
  http://localhost:8765/partner/message \
  -d '{"session_id":"SESSION_ID","message":"Search the pyramid for authentication"}'
```

Expected: Response includes executed tool results, not raw `<pyramid_query>` text.

**Time estimate: 2-4 hours**

---

## Phase 1.5: Annotation API

**Standalone — can be built any time after Phase 0. Useful immediately for auditors and workstream agents.**

### Schema

Add to `pyramid.db`:

```sql
CREATE TABLE IF NOT EXISTS pyramid_annotations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    node_id TEXT NOT NULL,               -- which pyramid node this annotates
    annotation_type TEXT NOT NULL,        -- 'correction', 'note', 'connection', 'tag', 'faq'
    payload TEXT NOT NULL,               -- JSON, structure depends on type
    context_question TEXT,               -- what question/task was being worked on when this was discovered
    context_source TEXT NOT NULL,        -- 'auditor-informed-A', 'workstream-auth', 'partner-session-xyz', etc.
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (slug, node_id) REFERENCES pyramid_nodes(slug, id),
    CHECK(annotation_type IN ('correction', 'note', 'connection', 'tag', 'faq'))
);

CREATE INDEX idx_annotations_node ON pyramid_annotations(slug, node_id);
CREATE INDEX idx_annotations_type ON pyramid_annotations(slug, annotation_type);
CREATE INDEX idx_annotations_source ON pyramid_annotations(context_source);
```

### Payload Structures by Type

**correction:**
```json
{"wrong": "auth uses password login", "right": "auth uses magic-link via Supabase", "confidence": "high"}
```

**note:**
```json
{"text": "This module has a subtle race condition when two sessions access the writer simultaneously", "severity": "major"}
```

**connection:**
```json
{"target_node_id": "L2-005", "relationship": "auth tokens validated in pipeline middleware", "bidirectional": true}
```

**tag:**
```json
{"tags": ["security", "needs-review", "performance-sensitive"]}
```

**faq:**
```json
{"question": "How does Partner read from the pyramid?", "answer": "Via context.rs assemble_context_window which loads the nav skeleton + hydrated nodes", "answer_node_ids": ["L2-003", "L1-042"]}
```

### HTTP Endpoints

**Write:**
```
POST /pyramid/:slug/annotate
Body: {
    "node_id": "L2-003",
    "annotation_type": "correction",
    "payload": {...},
    "context_question": "auditing the auth flow for race conditions",
    "context_source": "auditor-informed-A"
}
Response: { "id": 42, "status": "created" }
```

**Read:**
```
GET /pyramid/:slug/annotations                          — all annotations for slug
GET /pyramid/:slug/annotations?node_id=L2-003           — annotations for a specific node
GET /pyramid/:slug/annotations?type=correction           — all corrections (annotation + build-produced)
GET /pyramid/:slug/annotations?source=auditor-informed-A — all from a specific agent
GET /pyramid/:slug/annotations?question=auth             — search by context question
```

**Existing query endpoints get annotation merging:**
- `GET /pyramid/:slug/corrections` — merges build-produced corrections + annotation corrections
- `GET /pyramid/:slug/entities` — merges build-produced entities + connection annotations
- `GET /pyramid/:slug/node/:id` — includes annotation count and latest annotations in response

### FAQ Edge Meta-Process

When an annotation is created:

1. **If type is `faq`:** Store directly as a FAQ edge. No further processing needed.

2. **If type is `correction`, `note`, or `connection`:** Fire a lightweight meta-process:
   - Read the annotation's `context_question` + `payload`
   - Check existing FAQ annotations for this node: is there already a FAQ that covers this question?
   - If no: create a new FAQ annotation synthesizing the question->answer pair
   - If yes: update the existing FAQ with the new information (supersede)

3. **The FAQ edge is queryable:** When Partner (or any agent) is working near a node that has FAQ annotations, the context assembly includes relevant FAQs. "Last time someone was working on auth, they discovered X."

**V1 (immediate):** Simple keyword matching — does the new annotation's context_question overlap with existing FAQs for this node? If no match, create new FAQ.

**V2 (with delta chains):** The annotation triggers a delta on the node's thread. The distillation rewrite incorporates the annotation. The FAQ becomes a web edge.

### Files to Create/Modify
- `src-tauri/src/pyramid/db.rs` — add `insert_annotation()`, `get_annotations()`, `get_annotations_by_type()`
- `src-tauri/src/pyramid/types.rs` — add `Annotation` struct, `AnnotationType` enum
- `src-tauri/src/pyramid/query.rs` — modify `collect_corrections()`, `collect_entities()` to merge annotations
- `src-tauri/src/pyramid/routes.rs` — add `handle_annotate()`, `handle_get_annotations()` endpoints

### Integration with Agent Prompts

Add to the PYRAMID-FIRST ONBOARDING BLOCK in all skills:
```
ANNOTATION WRITE-BACK: When you discover something the pyramid doesn't know — a correction, a connection between modules, a gotcha, or an answer to a question you had to work hard to find — write it back:

curl -X POST -H "Authorization: Bearer vibesmithy-test-token" \
  -H "Content-Type: application/json" \
  http://localhost:8765/pyramid/<slug>/annotate \
  -d '{"node_id": "<relevant_node>", "annotation_type": "note", "payload": {"text": "your discovery"}, "context_question": "what you were investigating", "context_source": "<your-workstream-name>"}'

Types: correction, note, connection, tag, faq
This makes the pyramid smarter for every agent that comes after you.
```

**Time estimate: 4-6 hours**

---

## Phase 2: Delta Chain System

**This is the core innovation. Everything else builds on it.**

### New Table: `pyramid_threads`

The delta chain keys on `thread_id` — a stable identity that survives supersession. Without this table, L2 nodes are just regular `pyramid_nodes` at depth 2 with no stable thread concept.

```sql
CREATE TABLE IF NOT EXISTS pyramid_threads (
    slug TEXT NOT NULL,
    thread_id TEXT NOT NULL,             -- stable identity (e.g., 'thread-auth', 'thread-pipeline')
    thread_name TEXT NOT NULL,           -- human-readable name
    current_canonical_id TEXT NOT NULL,  -- current chain tip node ID in pyramid_nodes
    depth INTEGER NOT NULL DEFAULT 2,
    delta_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (slug, thread_id),
    FOREIGN KEY (slug, current_canonical_id) REFERENCES pyramid_nodes(slug, id)
);
```

### Migration for Existing Pyramids

Existing pyramids have L2+ nodes but no thread entries. Run this migration:

```sql
-- Add supersession tracking to pyramid_nodes
ALTER TABLE pyramid_nodes ADD COLUMN superseded_by TEXT DEFAULT NULL;

-- Create initial thread entries from existing L2+ nodes
INSERT INTO pyramid_threads (slug, thread_id, thread_name, current_canonical_id, depth)
SELECT slug, id,
       COALESCE(json_extract(topics, '$[0].name'), 'Untitled-' || id),
       id, depth
FROM pyramid_nodes
WHERE depth >= 2 AND superseded_by IS NULL;
```

Existing pyramids are frozen at `build_version = 1` and become eligible for delta chains immediately — new content creates deltas against these threads rather than requiring a full rebuild.

### SQL View for Live Node Filtering

Instead of modifying 12+ individual queries to add `AND build_version > 0 AND superseded_by IS NULL`:

```sql
CREATE VIEW live_pyramid_nodes AS
SELECT * FROM pyramid_nodes
WHERE build_version > 0 AND superseded_by IS NULL AND depth >= 0;
```

All public read queries point at this view. Write queries and explicit-ID lookups still target the table directly. Meta-node queries (depth < 0) use the table with `AND superseded_by IS NULL`.

### New Rust Module: `src-tauri/src/pyramid/delta.rs`

This module implements the delta chain — the heartbeat of the living pyramid.

#### Data Structures

```rust
/// A single delta — the diff between consecutive states of understanding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delta {
    pub id: String,                    // UUID
    pub slug: String,
    pub thread_id: String,             // The L2+ thread this delta belongs to
    pub sequence: i64,                 // Order in the chain (0, 1, 2, ...)
    pub content: String,               // What changed
    pub relevance: DeltaRelevance,     // Self-assessed importance
    pub source_node_ids: Vec<String>,  // Which L1 nodes triggered this delta
    pub flag: Option<DeltaFlag>,       // Self-check flag if distillation drifted
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeltaRelevance {
    Low,        // Typo fix, minor detail
    Medium,     // New information, no contradiction
    High,       // Significant change, new capability
    Critical,   // Contradicts existing understanding, security-relevant
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaFlag {
    pub description: String,           // What seems wrong
    pub suggested_check: String,       // Which source nodes to verify against
}

/// The cumulative distillation — rewritten after each delta.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CumulativeDistillation {
    pub slug: String,
    pub thread_id: String,
    pub content: String,               // The current distilled understanding since last collapse
    pub delta_count: i64,              // How many deltas since last collapse
    pub last_delta_id: String,
    pub web_edge_notes: Vec<WebEdgeNote>, // Structured cross-thread connections
    pub updated_at: String,
}

/// Structured web edge note (replaces free-text notes from v1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebEdgeNote {
    pub thread_id: String,             // The target thread
    pub relationship: String,          // Description of the connection change
}

/// A collapse event — when deltas are absorbed into a new canonical.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollapseEvent {
    pub id: String,
    pub slug: String,
    pub thread_id: String,
    pub old_canonical_id: String,      // The node being superseded
    pub new_canonical_id: String,      // The new canonical node
    pub delta_count: i64,              // How many deltas were absorbed
    pub created_at: String,
}
```

#### Database Schema (add to pyramid.db)

```sql
CREATE TABLE IF NOT EXISTS pyramid_deltas (
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    content TEXT NOT NULL,
    relevance TEXT NOT NULL DEFAULT 'medium',
    source_node_ids TEXT NOT NULL DEFAULT '[]',   -- JSON array
    flag_description TEXT,                         -- NULL if no flag
    flag_suggested_check TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(slug, thread_id, sequence)
);

CREATE INDEX idx_deltas_thread ON pyramid_deltas(slug, thread_id, sequence);

CREATE TABLE IF NOT EXISTS pyramid_distillations (
    slug TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    content TEXT NOT NULL,
    delta_count INTEGER NOT NULL DEFAULT 0,
    last_delta_id TEXT,
    web_edge_notes TEXT NOT NULL DEFAULT '[]',    -- JSON array of WebEdgeNote objects
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (slug, thread_id)
);

CREATE TABLE IF NOT EXISTS pyramid_collapse_events (
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    old_canonical_id TEXT NOT NULL,
    new_canonical_id TEXT NOT NULL,
    delta_count INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_collapses_thread ON pyramid_collapse_events(slug, thread_id);
```

#### Core Functions

```rust
/// Create a delta for a thread based on new L1 content.
///
/// 1. Load the thread's most recent delta (or canonical if no deltas)
/// 2. Load the new L1 node(s)
/// 3. Call LLM: "Given the current understanding and this new information, what changed?"
/// 4. Self-assess relevance
/// 5. Save delta to DB (transaction-wrapped sequence assignment)
/// 6. Trigger distillation rewrite (separate concern — drift check lives there)
///
/// If no existing L2+ thread matches the new L1 content:
/// 1. Create new pyramid_node at depth 2 with the L1 content as initial distillation
/// 2. Create pyramid_threads entry with the new node as canonical
/// 3. Log as high-relevance event (triggers upward propagation)
pub async fn create_delta(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    thread_id: &str,
    new_l1_node_ids: &[String],
) -> Result<Delta>
```

**Transaction-wrapped sequence assignment** inside `create_delta`:
```rust
// CRITICAL: read max sequence + insert in a single transaction to prevent duplicates
let writer_lock = writer.lock().await;
let tx = writer_lock.transaction()?;
let next_seq: i64 = tx.query_row(
    "SELECT COALESCE(MAX(sequence), 0) + 1 FROM pyramid_deltas WHERE slug=? AND thread_id=?",
    params![slug, thread_id],
    |r| r.get(0),
)?;
tx.execute(
    "INSERT INTO pyramid_deltas (id, slug, thread_id, sequence, content, relevance, source_node_ids, flag_description, flag_suggested_check, created_at)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    params![delta.id, slug, thread_id, next_seq, delta.content, delta.relevance_str(), source_ids_json, flag_desc, flag_check, delta.created_at],
)?;
tx.commit()?;
drop(writer_lock);
```

```rust
/// Rewrite the cumulative distillation incorporating a new delta.
/// Also performs drift check (moved here from delta creation — CC4).
///
/// 1. Load current distillation (or empty if first delta since collapse)
/// 2. Load the new delta
/// 3. Call LLM with DISTILLATION_REWRITE_PROMPT (includes drift check + token budget)
/// 4. If distillation exceeds 1200 tokens after rewrite, trigger early collapse
/// 5. Save updated distillation
/// 6. Return structured web edge notes for webbing update
pub async fn rewrite_distillation(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    thread_id: &str,
    delta: &Delta,
) -> Result<Vec<WebEdgeNote>>

/// Check if a thread needs collapse (hit threshold).
pub fn needs_collapse(reader: &Arc<Mutex<Connection>>, slug: &str, thread_id: &str, threshold: i64) -> Result<bool>

/// Collapse a thread's delta chain into a new canonical node.
///
/// 1. Load the current canonical (the L2/L3/etc node)
/// 2. Load the cumulative distillation
/// 3. Call frontier LLM with COLLAPSE_PROMPT
/// 4. Create new pyramid node (set old canonical's superseded_by = new_canonical_id)
/// 5. Update pyramid_threads.current_canonical_id
/// 6. Record collapse event
/// 7. Reset distillation and delta counter
/// 8. Check if parent level needs a delta (staleness propagation)
pub async fn collapse_thread(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    thread_id: &str,
) -> Result<CollapseEvent>

/// Propagate staleness upward after a collapse.
///
/// When L2 collapses, check if L3 needs a delta.
/// When L3 accumulates enough deltas, collapse L3.
/// Continue until apex.
///
/// Guards:
/// - visited: HashSet<String> — skip already-visited nodes (cycle prevention)
/// - Max propagation depth: max_depth + 2 (allows meta layer, prevents runaway)
/// - Debounce: if a delta was created for this thread in the last 10 seconds, skip
pub async fn propagate_staleness(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    changed_node_id: &str,
    changed_depth: i32,
    visited: &mut HashSet<String>,
) -> Result<Vec<CollapseEvent>>
```

#### Tier 1 Regex Extraction (Zero-Cost)

Run on every incoming message before any LLM call. No cost, feeds session topics and nav skeleton directly.

```rust
/// Extract entities, corrections, and decisions from raw text using regex.
/// No LLM call — pure pattern matching.
pub fn tier1_extract(text: &str) -> Tier1Extractions {
    Tier1Extractions {
        entities: extract_entities(text),
        corrections: extract_corrections(text),
        decisions: extract_decisions(text),
    }
}

pub struct Tier1Extractions {
    pub entities: Vec<String>,      // Capitalized words, @mentions, file paths
    pub corrections: Vec<Correction>, // "no, it's X not Y", "actually X", "wait, X not Y"
    pub decisions: Vec<String>,     // "let's go with X", "we decided X", "the answer is X"
}

fn extract_entities(text: &str) -> Vec<String> {
    // Capitalized multi-word names: /\b[A-Z][a-z]+(?:\s+[A-Z][a-z]+)+\b/
    // @mentions: /@\w+/
    // File paths: /(?:\/[\w.-]+)+\.\w+/
    // Module references: /\b\w+::\w+/
}

fn extract_corrections(text: &str) -> Vec<Correction> {
    // "no, it's X not Y" / "actually, X" / "wait, X not Y" / "correction: X"
    // Returns (wrong, right) pairs where identifiable
}

fn extract_decisions(text: &str) -> Vec<String> {
    // "let's go with X" / "we decided X" / "the answer is X" / "agreed: X"
}
```

#### New Thread Creation

When `create_delta` is called and no existing L2+ thread matches:

```rust
/// Check if new L1 content matches any existing thread.
/// If not, create a new thread.
async fn match_or_create_thread(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    l1_content: &str,
    existing_threads: &[PyramidThread],
) -> Result<String> {  // Returns thread_id
    // Include existing thread names in the prompt
    // If model says "this does not fit any existing thread":
    //   1. Create new pyramid_node at depth 2 with L1 content as initial distillation
    //   2. Create pyramid_threads entry with new node as canonical
    //   3. Return new thread_id
    // Otherwise return matched thread_id
}
```

#### LLM Prompts

**DELTA_PROMPT** (Mercury-2, fast) — produces delta + relevance only (drift check removed per CC4):
```
You are comparing new information against existing understanding.

CURRENT UNDERSTANDING:
{previous_delta_or_canonical}

NEW INFORMATION:
{new_l1_content}

LAST 5 DELTAS (for continuity checking):
{last_5_deltas}

EXISTING THREADS:
{thread_name_list}

What changed? Be specific:
- What's genuinely new
- What contradicts or corrects existing understanding
- What's confirmed or reinforced (briefly)
- Whether this fits the current thread or belongs in a different/new thread

Rate the relevance: low (minor detail), medium (new info), high (significant change), critical (contradicts existing understanding).

Output JSON:
{
  "content": "What changed...",
  "relevance": "medium"
}
```

**DISTILLATION_REWRITE_PROMPT** (Mercury-2, fast) — now includes drift check (from CC4) + token budget (MC1) + structured edges (MC2):
```
You are maintaining a cumulative understanding that gets rewritten after each new delta.

CURRENT DISTILLATION:
{current_distillation}

NEW DELTA:
{delta_content}

EXISTING THREADS (for cross-thread connections):
{thread_id_and_name_list}

Rewrite the distillation incorporating the delta. This is DISTILLATION, not accumulation:
- Produce the current understanding, not a list of everything that happened
- If the delta contradicts something in the distillation, update to the new truth
- If the delta adds detail, integrate it naturally
- Keep the distillation focused and bounded
- Keep the distillation under 800 tokens. If the accumulated understanding exceeds this, prioritize high-relevance changes and note that low-relevance details are recoverable from the raw deltas.

SELF-CHECK: After rewriting, compare your new distillation against the delta chain. If your rewrite has drifted from what the deltas actually say (e.g., you dropped a critical correction, or you invented something not in any delta), flag the discrepancy.

If this change affects connections to other threads, output web_edge_notes referencing specific thread IDs.

Output JSON:
{
  "distillation": "The updated understanding...",
  "web_edge_notes": [
    {"thread_id": "L2-003", "relationship": "Auth now connects to Pipeline via OAuth2 tokens"}
  ],
  "flag": null
}
```

**Mechanical check:** After the LLM returns, count tokens in the `distillation` field. If >1200 tokens, trigger early collapse regardless of delta count threshold.

**COLLAPSE_PROMPT** (Mimo/frontier, quality matters) — fixed field names per MC5:
```
You are producing a new canonical understanding by absorbing accumulated changes.

PREVIOUS CANONICAL:
{canonical_node_content}

CHANGES SINCE CANONICAL (cumulative distillation):
{distillation_content}

Produce the new canonical understanding. This supersedes the previous canonical.
The result should be a complete, self-contained understanding that incorporates
everything from the previous canonical plus all changes. A reader of this node
should have complete understanding without needing to read any deltas.

Deduplicate any corrections — if the same correction appears multiple times, include it only once with the most recent version.

Output JSON matching this exact schema:
{
  "distilled": "Complete understanding of this thread...",
  "topics": [
    {
      "name": "topic name",
      "current": "current state of this topic",
      "entities": ["entity1", "entity2"],
      "corrections": [
        {"wrong": "what was wrong", "right": "what is correct", "who": "delta-chain-collapse"}
      ]
    }
  ],
  "corrections": [
    {"wrong": "...", "right": "...", "who": "delta-chain-collapse"}
  ],
  "decisions": [
    {"decided": "...", "why": "...", "rejected": ""}
  ],
  "terms": [
    {"term": "...", "definition": "..."}
  ],
  "dead_ends": ["approach that was tried and abandoned"]
}
```

**Time estimate: 2-3 days**

---

## Phase 3: Intelligent Webbing

**Depends on Phase 2 (requires delta chain infrastructure: threads, deltas, distillation rewrites).**

### New Rust Module: `src-tauri/src/pyramid/webbing.rs`

#### Data Structures

```rust
/// A web edge connecting two threads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebEdge {
    pub id: String,                    // UUID
    pub slug: String,
    pub thread_a_id: String,           // CONSTRAINT: thread_a_id < thread_b_id (alphabetical)
    pub thread_b_id: String,
    pub canonical_content: String,     // How these threads relate
    pub entities_shared: Vec<String>,  // Shared entity names
    pub relevance_score: f64,          // Decays over time without reinforcement
    pub updated_at: String,
}

/// Delta on a web edge (same pattern as thread deltas).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebEdgeDelta {
    pub id: String,
    pub edge_id: String,
    pub sequence: i64,
    pub content: String,               // What changed about this connection
    pub created_at: String,
}
```

#### Database Schema

```sql
CREATE TABLE IF NOT EXISTS pyramid_web_edges (
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL,
    thread_a_id TEXT NOT NULL,
    thread_b_id TEXT NOT NULL,
    canonical_content TEXT NOT NULL,
    entities_shared TEXT NOT NULL DEFAULT '[]',
    distillation TEXT NOT NULL DEFAULT '',
    delta_count INTEGER NOT NULL DEFAULT 0,
    relevance_score REAL NOT NULL DEFAULT 1.0,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    -- Bidirectional constraint: always store with a < b to prevent duplicate edges
    UNIQUE(slug, thread_a_id, thread_b_id),
    CHECK(thread_a_id < thread_b_id)
);

CREATE TABLE IF NOT EXISTS pyramid_web_edge_deltas (
    id TEXT PRIMARY KEY,
    edge_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(edge_id, sequence)
);
```

#### Bidirectional Constraint

Always normalize edge storage so `thread_a_id < thread_b_id` (alphabetical comparison):

```rust
fn normalize_edge_pair(a: &str, b: &str) -> (String, String) {
    if a < b { (a.to_string(), b.to_string()) }
    else { (b.to_string(), a.to_string()) }
}
```

This prevents duplicate edges (A->B and B->A) and simplifies lookups.

#### Relevance Decay and Edge Limits

- **Decay:** Each edge has a `relevance_score` (starts at 1.0). On each distillation rewrite that does NOT reinforce the edge, decay by 0.05. On reinforcement, reset to 1.0.
- **Max edges per thread:** 10. When a thread exceeds 10 edges, drop the edge with the lowest relevance_score.
- **Pruning:** Edges with `relevance_score < 0.2` are candidates for removal during crystallization.

#### Core Functions

```rust
/// Process structured web edge notes from a distillation rewrite.
///
/// For each WebEdgeNote:
/// 1. Normalize the thread pair (a < b)
/// 2. Find or create the web edge between the two threads
/// 3. Enforce max 10 edges per thread (drop lowest relevance if exceeded)
/// 4. Create a delta on that edge
/// 5. Rewrite the edge's distillation
/// 6. Reset relevance_score to 1.0 (reinforced)
pub async fn process_web_edge_notes(
    reader: &Arc<Mutex<Connection>>,
    writer: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    source_thread_id: &str,
    notes: &[WebEdgeNote],
) -> Result<()>

/// Get all web edges for a thread (for the partner's brain map).
pub fn get_thread_edges(reader: &Arc<Mutex<Connection>>, slug: &str, thread_id: &str) -> Result<Vec<WebEdge>>

/// Get all web edge chain tips for a slug (for the nav skeleton).
pub fn get_all_edge_tips(reader: &Arc<Mutex<Connection>>, slug: &str) -> Result<Vec<WebEdge>>

/// Decay unreinforced edges. Call during crystallization.
pub fn decay_edges(writer: &Arc<Mutex<Connection>>, slug: &str, reinforced_edge_ids: &HashSet<String>) -> Result<usize>
```

**Time estimate: 1 day**

---

## Phase 4: Progressive Crystallization

### Modify: `src-tauri/src/partner/warm.rs`

**Add module declaration** in `partner/mod.rs`:
```rust
pub mod warm;
pub mod crystal;
```

Build the warm pass to:
1. Track conversation buffer position (`warm_cursor`)
2. Every ~100 lines of conversation, run the forward pass on the new chunk
3. Produce provisional L0 node (`build_version = -1`)
4. Pair provisional L0s into provisional L1 topics
5. Run Tier 1 regex extraction on the chunk
6. Feed new L1 topics into the delta chain system (Phase 2)

```rust
/// Check if warm pass is needed.
pub fn needs_warm_pass(session: &Session) -> bool {
    let lines_since_warm = estimate_lines(&session.conversation_buffer[session.warm_cursor..]);
    lines_since_warm >= 100
}

/// Run warm pass on accumulated conversation.
///
/// 1. Extract the conversation since warm_cursor
/// 2. Run Tier 1 regex extraction (entities, corrections, decisions)
/// 3. Run FORWARD_PROMPT → provisional L0
/// 4. Save with build_version = -1
/// 5. If 2+ provisional L0s exist, pair into provisional L1
/// 6. For each new L1 topic, check against existing L2 threads via delta chain
/// 7. If no thread matches, create new thread
/// 8. Update warm_cursor
pub async fn warm_pass(
    session: &mut Session,
    pyramid_reader: &Arc<Mutex<Connection>>,
    pyramid_writer: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
) -> Result<WarmPassResult>

pub struct WarmPassResult {
    pub l0_nodes_created: usize,
    pub l1_nodes_created: usize,
    pub deltas_created: usize,
    pub session_topics: Vec<SessionTopic>,
    pub tier1_extractions: Tier1Extractions,
}
```

### Warm Pass Concurrency Fix (CC3)

The warm pass runs as a background task. You cannot pass `&mut session` across a `tokio::spawn` boundary. The fix: clone the session, mutate the clone, merge back via mutex.

**Concurrent-execution guard:** Only one warm pass should run per session at a time. Use a per-session `AtomicBool` or `tokio::sync::Semaphore(1)` to prevent overlapping warm passes if messages arrive faster than the warm pass completes.

In `partner/conversation.rs`, after `handle_message`:

```rust
// After adding partner response to buffer:

// 1. Check if warm pass needed
if warm::needs_warm_pass(&session) {
    let warm_session_id = session_id.clone();
    let warm_reader = partner_state.pyramid.reader.clone();
    let warm_writer = partner_state.pyramid.writer.clone();
    let warm_config = config.clone();
    let warm_sessions = partner_state.sessions.clone();

    tokio::spawn(async move {
        // Load a FRESH copy of the session from the mutex
        let session = {
            let sessions = warm_sessions.lock().await;
            sessions.get(&warm_session_id).cloned()
        };
        if let Some(mut session) = session {
            if let Ok(result) = warm::warm_pass(&mut session, &warm_reader, &warm_writer, &warm_config).await {
                // Write ONLY the warm-pass mutations back
                let mut sessions = warm_sessions.lock().await;
                if let Some(live_session) = sessions.get_mut(&warm_session_id) {
                    live_session.warm_cursor = session.warm_cursor;
                    // Don't overwrite conversation_buffer — it may have changed
                    // Don't overwrite other session state — only warm_cursor is ours
                }
            }
        }
    });
}

// 2. Reset idle timer for crystallization (using CancellationToken — MC9)
session.reset_idle_timer();
```

### Session Idle Timer (MC9)

Use `tokio_util::sync::CancellationToken` instead of a serializable timer.

**Note:** SessionTimer lives on PartnerState, not Session, because it contains non-serializable async handles (JoinHandle, CancellationToken) that cannot be stored on the serializable Session struct. PartnerState holds a `HashMap<String, SessionTimer>` keyed by session_id.

```rust
use tokio_util::sync::CancellationToken;

pub struct SessionTimer {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

impl SessionTimer {
    pub fn start(
        session_id: String,
        sessions: Arc<Mutex<HashMap<String, Session>>>,
        pyramid_reader: Arc<Mutex<Connection>>,
        pyramid_writer: Arc<Mutex<Connection>>,
        llm_config: LlmConfig,
        timeout: Duration,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(timeout) => {
                    // Timer fired — run crystallization
                    if let Err(e) = crystal::crystallize_session(
                        &session_id, &sessions, &pyramid_reader, &pyramid_writer, &llm_config
                    ).await {
                        warn!("Crystallization failed for session {}: {}", session_id, e);
                    }
                }
                _ = cancel_clone.cancelled() => {
                    // Timer cancelled — session got a new message or was cleaned up
                }
            }
        });

        SessionTimer { cancel, handle }
    }

    pub fn reset(&mut self, /* same params */) {
        self.cancel.cancel();
        // Start a new timer
        *self = Self::start(/* same params */);
    }
}
```

The idle timer (5 minute default) triggers `crystal::crystallize` when it fires. Reset on each message. On session cleanup, call `cancel.cancel()`.

### Modify: `src-tauri/src/partner/crystal.rs`

Build the meta-reverse crystallization:

```rust
/// Run crystallization on a slug.
///
/// 1. Find all warm nodes (build_version = -1)
/// 2. For each warm L1 topic, it's already been fed into the delta chain by warm_pass
/// 3. Check if any threads need collapse (delta threshold reached)
/// 4. Run collapses
/// 5. Propagate staleness upward (with visited set + max depth + debounce guards)
/// 6. Promote warm nodes to crystal (build_version = 1)
/// 7. Decay unreinforced web edges
/// 8. Update nav skeleton cache
pub async fn crystallize(
    slug: &str,
    pyramid_reader: &Arc<Mutex<Connection>>,
    pyramid_writer: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
) -> Result<CrystallizationResult>

pub struct CrystallizationResult {
    pub warm_nodes_promoted: usize,
    pub collapses: Vec<CollapseEvent>,
    pub threads_unchanged: usize,
    pub threads_updated: usize,
    pub threads_created: usize,
    pub apex_changed: bool,
}
```

### Update Context Window Assembly (MC11)

**File: `src-tauri/src/partner/context.rs`**

Modify `assemble_context_window` to include delta chain knowledge. Without this, Partner cannot access any delta chain data.

```rust
pub fn assemble_context_window(
    session: &Session,
    pyramid_reader: &Arc<Mutex<Connection>>,
    slug: &str,
    // ... existing params
) -> Result<ContextWindow> {
    // Existing: nav skeleton, hydrated nodes, conversation history
    // ADD:
    // - Thread chain tips for topics matching current conversation
    let active_threads = find_matching_threads(pyramid_reader, slug, &session.session_topics)?;
    // - Cumulative distillations for those threads
    let distillations = load_distillations(pyramid_reader, slug, &active_threads)?;
    // - Web edge chain tips connecting those threads
    let web_edges = load_connecting_edges(pyramid_reader, slug, &active_threads)?;

    // Include in context window alongside existing data
    context.delta_chain_tips = active_threads;
    context.distillations = distillations;
    context.web_edges = web_edges;

    Ok(context)
}
```

**Time estimate: 2-3 days**

---

## Phase 5: Configuration System

### New file: `src-tauri/src/pyramid/config.rs`

Replace hardcoded values with a YAML-driven configuration system.

**Dependency note:** Add `serde_yaml` to `Cargo.toml` dependencies for YAML config parsing:
```toml
[dependencies]
serde_yaml = "0.9"
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PyramidActionChain {
    pub slug_template: String,
    pub version: i32,

    pub l0: L0Config,
    pub l1: L1Config,
    pub delta: DeltaConfig,
    pub l2_plus: L2PlusConfig,
    pub meta: MetaConfig,
    pub scaling: ScalingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L0Config {
    pub strategy: String,           // "sequential" | "concurrent"
    pub concurrency: usize,         // 1 for sequential, 10 for concurrent
    pub model: String,
    pub prompts: L0Prompts,
    pub mechanical_passes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L0Prompts {
    pub forward: Option<String>,
    pub reverse: Option<String>,
    pub combine: Option<String>,
    pub extract: Option<String>,
    pub config_extract: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1Config {
    pub strategy: String,           // "positional_pairing" | "import_graph" | "entity_overlap"
    pub model: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaConfig {
    pub model: String,
    pub collapse_threshold: i64,
    pub self_check_window: usize,
    pub distillation_token_budget: usize,     // 800 default
    pub distillation_early_collapse: usize,   // 1200 — trigger collapse if exceeded
    pub distillation_prompt: String,
    pub collapse_prompt: String,
    pub relevance_scoring: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2PlusConfig {
    pub model: String,
    pub thread_prompt: String,
    pub cluster_prompt: String,
    pub webbing_prompt: String,
    pub max_edges_per_thread: usize,          // 10 default
    pub edge_relevance_decay: f64,            // 0.05 per unreinforced cycle
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaConfig {
    pub timeline_forward: Option<String>,
    pub timeline_backward: Option<String>,
    pub narrative: Option<String>,
    pub quickstart: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalingConfig {
    pub l1_bundle_threshold: usize,
    pub l2_bundle_threshold: usize,
    pub delta_collapse_threshold: i64,
    pub web_edge_collapse_threshold: i64,
}
```

Ship three default configs:
- `conversation_default.yaml`
- `code_default.yaml`
- `document_default.yaml`

**Note on Fiction content type:** Fiction uses the `document` pipeline with a fiction-specific config (`document_fiction.yaml`). No separate Fiction content type is needed — the config system handles the variation.

Store in the node's data directory. Users can edit. Custom configs can be created for new content types.

### Loading

```rust
impl PyramidActionChain {
    pub fn load_for_content_type(content_type: &str, data_dir: &Path) -> Result<Self> {
        let custom_path = data_dir.join(format!("{}_config.yaml", content_type));
        if custom_path.exists() {
            // Load custom config
            let yaml = std::fs::read_to_string(&custom_path)?;
            Ok(serde_yaml::from_str(&yaml)?)
        } else {
            // Use built-in default
            Ok(Self::default_for(content_type))
        }
    }

    pub fn default_for(content_type: &str) -> Self {
        match content_type {
            "conversation" => Self::conversation_default(),
            "code" => Self::code_default(),
            "document" => Self::document_default(),
            "fiction" => Self::fiction_default(),  // document pipeline + fiction config
            _ => Self::document_default(),
        }
    }
}
```

**Time estimate: 1 day**

---

## Phase 6: Meta Analysis Layers

### Timeline, Narrative, and Quickstart passes

These are the meta-analysis layers that sit on top of the foundation pyramid. They use the same delta chain pattern — they're just special nodes at the highest level.

#### Implementation

Add to `src-tauri/src/pyramid/meta.rs`:

```rust
/// Generate the timeline forward pass.
/// Reads L2 threads in chronological order, produces the event sequence.
pub async fn timeline_forward(
    reader: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
) -> Result<String>

/// Generate the timeline backward pass.
/// Reads the forward timeline, marks what actually mattered.
pub async fn timeline_backward(
    reader: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    forward_timeline: &str,
) -> Result<String>

/// Generate the narrative.
/// Reads both timelines, produces the story arc.
pub async fn narrative(
    reader: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    forward_timeline: &str,
    backward_timeline: &str,
) -> Result<String>

/// Generate the quickstart.
/// Reads the narrative + both timelines + foundation pyramid chain tips.
/// Produces the compressed full grok.
pub async fn quickstart(
    reader: &Arc<Mutex<Connection>>,
    llm_config: &LlmConfig,
    slug: &str,
    narrative: &str,
    forward_timeline: &str,
    backward_timeline: &str,
) -> Result<String>
```

These are stored as special nodes in pyramid_nodes with depth = -1 (meta level) and node IDs like `META-timeline-forward`, `META-narrative`, `META-quickstart`.

They get deltas like everything else — when threads change, the quickstart's delta chain fires.

Queries that should NOT return meta nodes use the `live_pyramid_nodes` view (which filters `depth >= 0`). Queries that specifically want meta nodes use `WHERE depth < 0 AND superseded_by IS NULL`.

**Time estimate: 1 day**

---

## Phase 7: Frontend Integration

### Vibesmithy Updates

**Brain state visualization in Space view:**
- Hydrated nodes glow bright
- Warm (provisional) nodes have a pulsing dashed border
- Crystal nodes are solid
- Web edges render as luminous threads between connected marbles

**Delta activity feed:**
- New component: `DeltaFeed.tsx` — shows recent deltas across all threads
- Lives in the Space view sidebar or as a toggleable overlay
- Each delta shows: thread name, relevance badge, content preview, timestamp

**Dennis brain state improvements:**
- `BrainStateBar` shows: buffer usage, hydrated thread count, warm pending indicator, crystal pending indicator
- Dennis avatar transitions: thinking (tool call in progress), crystallizing (warm/crystal pass running)
- Context bridge links work: entity pills in chat jump to Space view nodes

### New API Endpoints

```
GET  /pyramid/:slug/threads                            → list all threads with canonical chain tips
GET  /pyramid/:slug/threads/:thread_id/deltas?limit=20 → recent deltas for a thread
GET  /pyramid/:slug/distillation/:thread_id            → current cumulative distillation
GET  /pyramid/:slug/web-edges                          → all web edge chain tips
GET  /pyramid/:slug/meta/quickstart                    → the quickstart document
GET  /pyramid/:slug/meta/narrative                     → the narrative
GET  /pyramid/:slug/meta/timeline                      → the timeline
POST /pyramid/:slug/crystallize                        → trigger crystallization
POST /pyramid/:slug/annotate                           → write annotation (Phase 1.5)
GET  /pyramid/:slug/annotations                        → query annotations (Phase 1.5)
```

**Time estimate: 2-3 days**

---

## Build Order

### Revised Phase Dependencies

```
Phase 0:   Fix partner seam bugs         (4-6 hours)  — immediate, unblocks real-world testing
Phase 1:   Fix Dennis tool execution     (2-4 hours)  — can parallel with Phase 0
Phase 1.5: Annotation API               (4-6 hours)  — standalone, useful immediately
Phase 2:   Delta chain system            (2-3 days)   — with all corrections applied
Phase 3:   Intelligent webbing           (1 day)      — depends on Phase 2
Phase 4:   Progressive crystallization   (2-3 days)   — depends on Phase 2
Phase 5:   Configuration system          (1 day)      — can parallel with Phase 4
Phase 6:   Meta layers                   (1 day)      — depends on Phase 2
Phase 7:   Frontend integration          (2-3 days)   — depends on all backend phases
```

Parallelism opportunities:
- Phase 0 + Phase 1 (independent fixes)
- Phase 4 + Phase 5 (config informs thresholds but can be hardcoded first)

### Track A: Backend (agent-wire-node)
```
Phase 0: Fix partner seam bugs          (4-6 hours)
  → BrainState enum alignment
  → API path param audit
  → PartnerResponse shape fix
  → DennisState serialization
  → UTF-8 safe slicing in context.rs
  → Consolidate node_from_row (db.rs) / row_to_node (query.rs)
  → Session LRU eviction
  → Remove duplicate auth middleware
Phase 1: Fix Dennis tools               (2-4 hours)
Phase 1.5: Annotation API               (4-6 hours)
  → pyramid_annotations table
  → Annotation struct + AnnotationType enum
  → POST /pyramid/:slug/annotate
  → GET /pyramid/:slug/annotations
  → FAQ edge meta-process
  → Merge with existing correction/entity queries
Phase 2: Delta chain system             (2-3 days)
  → pyramid_threads table + migration
  → superseded_by column + live_pyramid_nodes view
  → delta.rs (data structures + schema + core functions)
  → Transaction-wrapped sequence assignment
  → Tier 1 regex extraction
  → Thread matching + new thread creation
  → DELTA_PROMPT (delta + relevance only)
  → DISTILLATION_REWRITE_PROMPT (rewrite + drift check + token budget + structured edges)
  → COLLAPSE_PROMPT (fixed field names + dedup corrections)
  → Staleness propagation with guards (visited set, max depth, 10s debounce)
  → Integration with build pipeline (new L1 → delta chain)
Phase 3: Webbing                        (1 day)
  → webbing.rs
  → Bidirectional constraint (thread_a_id < thread_b_id)
  → Relevance decay + max edge count (10)
  → Integration with distillation rewrite
Phase 4: Progressive crystallization    (2-3 days)
  → Add `pub mod warm; pub mod crystal;` to partner/mod.rs
  → warm.rs (warm pass during conversations)
  → Warm pass concurrency fix (clone session, merge back via mutex)
  → Warm pass concurrent-execution guard (Semaphore or AtomicBool per session)
  → crystal.rs (meta-reverse + collapse + edge decay)
  → Session idle timer with CancellationToken (on PartnerState, not Session)
  → Update assemble_context_window (chain tips + distillations + web edges)
Phase 5: Config system                  (1 day)
  → config.rs + YAML loading (requires serde_yaml dependency)
  → 4 default configs (conversation, code, document, fiction)
Phase 6: Meta layers                    (1 day)
  → meta.rs (timeline + narrative + quickstart)
  → Storage as META- nodes (depth = -1)
  → Delta chain integration for meta nodes
```

### Track B: Frontend (vibesmithy)
```
Delta feed component                    (4-6 hours)
Brain state visualization upgrades      (4-6 hours)
Web edge rendering in Space             (4-6 hours)
Thread list view                        (2-4 hours)
Quickstart/narrative display            (2-4 hours)
Dennis tool call UX (loading states)    (2-4 hours)
Annotation UI (optional)               (4-6 hours)
```

### Track C: Testing & Validation
```
After each phase:
  - Build knowledge pyramid of the changed code
  - Run 2 pyramid-powered auditors
  - Fix findings
  - Rebuild pyramid

End-to-end test:
  - Start node with pyramid
  - Open Vibesmithy, connect
  - Talk to Dennis for 30+ minutes
  - Verify warm passes fire (session topics appear)
  - Verify Tier 1 extractions appear in session state
  - Verify deltas accumulate
  - Verify new thread creation when topic doesn't match
  - Wait 5 min idle, verify crystallization
  - Verify web edges form between related threads
  - Verify edge decay on unreinforced edges
  - Check that Space view shows updated threads
  - Check that quickstart regenerated
  - Check cost logs for actual vs estimated spend
```

---

## Model Configuration

### Build pipeline (speed + cost):
- **Primary:** `inception/mercury-2` (128K context, ~700 tps, ~$0.001/call)
- **Fallback 1:** `qwen/qwen3.5-flash-02-23` (1M context, ~170 tps, ~$0.02/call)
- **Fallback 2:** `x-ai/grok-4.20-beta` (2M context, expensive)

### Model fallback criteria:
Mercury-2 returns 5xx response 3 times in a row → switch to Fallback 1. Fallback 1 returns 5xx 3 times → switch to Fallback 2. Log all fallback events. Alert if primary model has been unavailable for >10 minutes.

### Delta chain (speed + cost):
- **Deltas + distillation rewrites:** `inception/mercury-2`
- **Collapses:** `xiaomi/mimo-v2-pro` (frontier quality, open source)

### Understanding layers (quality):
- **L2+ thread clustering + narratives:** `xiaomi/mimo-v2-pro`
- **Meta passes (timeline, narrative, quickstart):** `xiaomi/mimo-v2-pro`

### Partner conversation (quality + personality):
- **Dennis:** `xiaomi/mimo-v2-pro` (configurable, stored in `pyramid_config.json` as `partner_model`)

---

## Cost Model (Revised)

### Per-operation costs (estimated, verify on OpenRouter):

| Operation | Model | Est. Cost |
|-----------|-------|-----------|
| Delta creation | Mercury-2 | $0.001-0.002 |
| Distillation rewrite | Mercury-2 | $0.001-0.002 |
| Tier 1 regex extraction | None (CPU) | $0.000 |
| Thread matching | Mercury-2 | $0.001 |
| Collapse | mimo-v2-pro | $0.02-0.05 |
| Web edge delta | Mercury-2 | $0.001 |
| Meta pass (quickstart etc.) | mimo-v2-pro | $0.02-0.05 |

### Per-conversation-turn average:
- **Typical (no cascade):** Tier 1 extract + delta + distill = ~$0.003-0.005
- **With propagation cascade:** 4 levels x ($0.002 delta + $0.002 distill) + 1 collapse = ~$0.03-0.05
- **Worst case (full apex rewrite):** ~$0.15-0.20

### Cost monitoring:
- Log actual spend per operation to a `cost_log` table:
  ```sql
  CREATE TABLE IF NOT EXISTS cost_log (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      slug TEXT NOT NULL,
      operation TEXT NOT NULL,    -- 'delta', 'distill', 'collapse', 'web_edge', 'meta'
      model TEXT NOT NULL,
      input_tokens INTEGER,
      output_tokens INTEGER,
      cost_usd REAL,
      created_at TEXT NOT NULL DEFAULT (datetime('now'))
  );
  ```
- Alert (log warning) if any single operation costs >2x its estimate.
- Weekly cost summary queryable via API.

---

## Success Criteria

The system is complete when:

1. Dennis can have a multi-hour conversation with persistent memory
2. Tier 1 regex extraction fires on every message (zero-cost entity/correction/decision capture)
3. The warm layer fires every ~100 lines, producing provisional nodes
4. Deltas accumulate against L2 threads automatically
5. New threads are created when content doesn't match existing threads
6. Collapses happen when thresholds are reached (delta count OR distillation token budget exceeded)
7. Web edges update as side effects of distillation rewrites (structured, not free-text)
8. Web edge relevance decays without reinforcement; max 10 edges per thread
9. The quickstart regenerates when the apex changes
10. A fresh Dennis instance can read the quickstart + nav skeleton and have complete understanding
11. The Space view shows warm/crystal/hydrated states visually
12. The whole system runs at ~$0.003-0.005 per content chunk and ~$0.03-0.05 per conversation turn
13. Cost monitoring logs actual spend per operation
14. Configuration is YAML-driven, not hardcoded
15. Annotations can be written back to the pyramid by any agent
16. Staleness propagation terminates (visited set, max depth, debounce)

---

## Future Work

### L2.5 Grouping
When the number of L2 threads exceeds ~15-20 for a slug, introduce an intermediate grouping layer (L2.5) that clusters related threads. Monitor thread count per slug and trigger when threshold is exceeded. This is a natural extension of the existing thread/collapse pattern — L2 threads cluster into L2.5 groups the same way L1 topics cluster into L2 threads.

### Deferred Items
- **Fiction content type:** Uses Document pipeline with fiction-specific config (`document_fiction.yaml`). No separate pipeline needed.
- **Intelligent user matching** (Midnight Protocol): Currently random 3-user matching. Integrate pyramid-based expertise matching once delta chains are stable.
- **Payment processing:** Paid tier for Midnight Protocol.

