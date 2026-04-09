# Vine Episodic Memory — Three Gap Fix (MPS-Audited)

## Context

The vine build system is hybrid: L0 bedrocks use the new chain executor with `conversation-episodic.yaml` (correct), but L1 clustering and L2+/apex synthesis still use the legacy hardcoded-prompt pipeline (`THREAD_NARRATIVE_PROMPT`, `DISTILL_PROMPT`, `node_from_analysis`). This produces generic topic-merged summaries instead of episodic memory. Additionally, the nav page UI can't display ANY HTTP-fetched data due to missing auth headers, and vine builds don't trigger vocabulary refresh.

**Key architectural fact:** There are two build infrastructures. The OLD system has hardcoded prompt constants in `build.rs` and uses `call_and_parse()` + `node_from_analysis()`. The NEW system uses YAML chain definitions, prompt files on disk, `chain_executor.rs`, and `build_node_from_output()` from `chain_dispatch.rs`. Everything non-conversation defaults to `question-pipeline` (question.yaml). The old `code.yaml` and `document.yaml` chains are dead code — unwired from all defaults.

---

## Workstream 0: Vine L0 assembly drops episodic fields (MPS finding, prerequisite)

**File:** `src-tauri/src/pyramid/vine.rs` lines 454-473 (apex L0) and 493-512 (penultimate L0)

**Problem:** `assemble_vine_l0()` constructs vine L0 nodes with explicit field assignment + `..Default::default()`. It copies `topics`, `corrections`, `decisions`, `terms` from the bunch apex — but NOT `narrative`, `entities`, `key_quotes`, `transitions`, `time_range`, or `weight`. Those all fall through to `Default::default()` (empty). The bunch apexes built by the episodic chain DO have these fields populated, but they're silently discarded at vine L0 assembly.

**Why this is critical:** Without this fix, the vine L0 nodes that feed L1/upper synthesis have empty narrative, empty entities, empty key_quotes, no time_range, zero weight. The episodic prompt (`synthesize_recursive.md`) expects these fields as guaranteed input. Switching prompts without fixing the data pipeline = garbage in.

**Fix:** In both the apex L0 node (line 454) and penultimate L0 node (line 493), add before the `..Default::default()`:

```rust
narrative: apex.narrative.clone(),  // (or pn.narrative.clone() for penultimate)
entities: apex.entities.clone(),
key_quotes: apex.key_quotes.clone(),
transitions: apex.transitions.clone(),
time_range: apex.time_range.clone(),
weight: apex.weight,
```

**ALSO (Cycle 2 audit finding):** A SECOND vine L0 assembly path exists in `notify_vine_of_bunch_change()` at lines ~2998-3017 (apex L0) and ~3035-3054 (penultimate L0). This delta/staleness reassembly path mirrors `assemble_vine_l0` and has the exact same `..Default::default()` pattern. Apply the identical 6-field fix there too, or vine rebuilds triggered by bunch changes will lose episodic fields.

---

## Workstream 1: Vine upper synthesis → episodic prompt (Gap 3, critical)

The product-level fix. Replace legacy prompts + node builder with episodic equivalents.

### 1A. New episodic child payload function

**File:** `src-tauri/src/pyramid/build.rs` (after `child_payload_json` at line 2825)

Add `pub(crate) fn episodic_child_payload_json(node: &PyramidNode) -> Value` that includes:
- `headline`, `topics` (always)
- `narrative` (from `node.narrative.levels.first().map(|l| l.text.as_str())`, fallback to `node.distilled`) — **CRITICAL (backend audit C3): must use `.first()` not `[0]` to avoid panic on empty levels Vec**
- `time_range`, `weight`, `decisions`, `entities`, `key_quotes`, `transitions` (when non-empty)

This is needed because `child_payload_json` strips narrative/entities/key_quotes/transitions — the exact fields `synthesize_recursive.md` expects as input.

### 1B. Compile-time prompt constant

**File:** `src-tauri/src/pyramid/vine.rs` (top of file)

```rust
const SYNTHESIZE_RECURSIVE_PROMPT: &str = include_str!("../../../chains/prompts/conversation-episodic/synthesize_recursive.md");
```

`include_str!` keeps it in sync with the canonical prompt file.

### 1C. Replace DISTILL_PROMPT in `build_vine_upper()` (vine.rs:1530-1606)

Three changes at this location:
1. **Line 1535-1536**: Replace `build::child_payload_json(left/right)` → `build::episodic_child_payload_json(left/right)`
2. **Line 1552**: Replace `build::DISTILL_PROMPT` → `SYNTHESIZE_RECURSIVE_PROMPT`
3. **Line 1598-1605**: Replace `build::node_from_analysis(...)` → `chain_dispatch::build_node_from_output(...)` + set `.children` after. **Note (backend audit C2):** `build_node_from_output` returns `Result<PyramidNode>`, not `PyramidNode` — add `?` for error propagation. On error, treat like a synthesis failure (increment failures, carry left node).

`build_node_from_output` (chain_dispatch.rs:324) already parses the full episodic schema: narrative→NarrativeMultiZoom, entities as Entity structs, key_quotes with speaker_role, decisions with stance, time_range, weight, transitions.

**Important (backend audit C1):** `build_node_from_output` sets `distilled` from `orientation`/`distilled`/`purpose` keys, but `synthesize_recursive.md` produces `narrative` instead. Result: `distilled = ""` on vine L1/upper nodes. Fix: after calling `build_node_from_output`, set `node.distilled` from the narrative: `node.distilled = node.narrative.levels.first().map(|l| l.text.clone()).unwrap_or_default();`. This ensures downstream code that reads `distilled` (including the forced apex fallback) works correctly.

### 1D. Replace THREAD_NARRATIVE_PROMPT in `build_vine_l1()` (vine.rs:1361-1432)

Three changes:
1. **Lines 1362-1380**: Replace topic_entries construction with full episodic node payloads via `build::episodic_child_payload_json()`
2. **Line 1397**: Replace `build::THREAD_NARRATIVE_PROMPT` → `SYNTHESIZE_RECURSIVE_PROMPT`
3. **Line 1424-1431**: Replace `build::node_from_analysis(...)` → `chain_dispatch::build_node_from_output(...)?` + set `.children`. Same `distilled` backfill as 1C: `node.distilled = node.narrative.levels.first().map(|l| l.text.clone()).unwrap_or_default();`

**L1 user prompt format (backend audit M3):** Replace the old format with:
```rust
let mut node_payloads = Vec::new();
for (order, node) in l0_nodes.iter().enumerate() {
    let mut payload = build::episodic_child_payload_json(node);
    payload["input_order"] = serde_json::json!(order);
    node_payloads.push(payload);
}
let user_prompt = format!(
    "## CLUSTER: {cluster_name}\n## INPUT NODES ({} nodes)\n{}",
    node_payloads.len(),
    serde_json::to_string_pretty(&node_payloads)?
);
```

Note: `VINE_CLUSTER_PROMPT` (line 1252) stays unchanged — it's a structural grouping step, not content synthesis. Token budgets and auto-dehydration are YAML-configured per model — no manual thresholds here.

### 1F. Fix forced apex fallback path (backend audit M4)

**File:** `src-tauri/src/pyramid/vine.rs` lines ~1684-1695

The forced apex fallback (when synthesis failures prevent convergence) creates a node with `..Default::default()`, dropping episodic fields from the top-depth nodes. Since we're touching the surrounding code, fix this to merge episodic fields from the top nodes — copy `narrative`, `entities`, `key_quotes`, `transitions`, `time_range` from the first/best top node.

**Also (Cycle 2 discovery finding):** Line ~1689 uses `&combined_headline[..197]` which is byte-slicing — panics on multi-byte UTF-8 (em dashes, smart quotes, CJK). Replace with `combined_headline.chars().take(197).collect::<String>()` or use the existing `truncate_text` helper from build.rs.

### 1G. Add `speaker` field to KeyQuote struct (Cycle 2 audit finding)

**File:** `src-tauri/src/pyramid/types.rs` (KeyQuote struct, ~line 157)

The `synthesize_recursive.md` prompt produces key_quotes with both `speaker` (name/label) and `speaker_role` (human/agent). The `KeyQuote` struct only has `speaker_role` — `speaker` is silently dropped by `build_node_from_output`. Speaker identity is lost at every synthesis layer.

**Fix:** Add `pub speaker: String` to `KeyQuote` with `#[serde(default)]`. In `chain_dispatch.rs` (~line 538), add extraction: `speaker: q.get("speaker").and_then(|s| s.as_str()).unwrap_or("").to_string()`. The `key_quotes_json` column stores JSON-serialized KeyQuote vectors, so the new field persists automatically via serde.

### 1H. Fix weight serialization in `episodic_child_payload_json` (Cycle 2 audit finding)

The prompt expects `weight: {tokens, turns, fraction_of_parent}` but `PyramidNode.weight` is a bare `f64` (tokens only, flattened by `build_node_from_output`). When `episodic_child_payload_json` serializes weight as a number, the downstream LLM sees `"weight": 1234.0` instead of the expected object shape.

**Fix:** In `episodic_child_payload_json`, wrap weight back into the expected object: `payload["weight"] = serde_json::json!({"tokens": node.weight, "turns": 0, "fraction_of_parent": 0.0})`.

### 1E. Add import

**File:** `src-tauri/src/pyramid/vine.rs` (imports section)

```rust
use super::chain_dispatch;
```

---

## Workstream 2: Vocabulary refresh after vine builds (Gap 1)

**File:** `src-tauri/src/pyramid/routes.rs` line 5440

In the `handle_vine_build` spawned task, after the `Ok(apex_id)` arm's `tracing::info!` (line 5440), before the status return (line 5441), add:

```rust
{
    let conn = state_clone.writer.lock().await;
    match super::vocabulary::refresh_vocabulary(&conn, &slug_clone) {
        Ok((_, count)) => tracing::info!("Post-vine-build: vocabulary refreshed ({} entries)", count),
        Err(e) => tracing::warn!("Post-vine-build: vocabulary refresh failed: {}", e),
    }
}
```

Same pattern as `handle_build` at line 3243.

---

## Workstream 3: Nav page — ALL HTTP fetches broken (Gap 2, reframed)

**Reframing from MPS audit:** This is not just a reading-modes problem. `with_auth_state` (routes.rs:64-94) rejects ALL requests with no `Authorization` header — line 76 returns `Err(reject)` on `None`. Every HTTP fetch in PyramidNavPage has been silently failing since day one: vocabulary, DADBEAR status, recovery status, vine bedrocks, AND reading modes. Only IPC-based features (slug list, tree, Memoir, Walk) ever worked.

### 3A. Auth token IPC command

**File:** `src-tauri/src/main.rs`

Add new Tauri command near `pyramid_get_config` (line 5267):

```rust
#[tauri::command]
async fn pyramid_get_auth_token(state: tauri::State<'_, SharedState>) -> Result<String, String> {
    let config = state.pyramid.config.read().await;
    Ok(config.auth_token.clone())
}
```

Register it in the `.invoke_handler(tauri::generate_handler![...])` list (around line 7356).

### 3B. Frontend auth headers (with race condition fix — audit finding C1)

**File:** `src/components/PyramidNavPage.tsx`

- Add state: `const [authToken, setAuthToken] = useState('')`
- Fetch on mount: `invoke<string>('pyramid_get_auth_token').then(setAuthToken)`
- Build headers: `const authHeaders = authToken ? { 'Authorization': \`Bearer ${authToken}\` } : {}`
- **CRITICAL (C1):** Add `authToken` to the dependency array of the useEffect that fires HTTP fetches (the one at ~line 254 that runs on slug change). Without this, all mount-time fetches fire before the IPC token arrives → 401. When the token loads, the useEffect re-runs and fetches succeed.
- Add `{ headers: authHeaders }` to ALL fetch calls:
  - Line 287: vocabulary fetch
  - Line 312: DADBEAR status fetch
  - Line 318: recovery status fetch
  - Line 324: vine bedrocks fetch
  - Line 350: decisions fetch
  - Line 356: speaker fetch
  - Line 363: thread fetch
  - Line 403: question/search fetch
  - Line 425: search fetch

### 3B-2. Vocabulary response shape mismatch (audit finding C2)

**Problem:** Backend returns `VocabularyCatalog`:
```json
{ "slug": "...", "topics": [{name, liveness, ...}], "entities": [...], "decisions": [...], "terms": [...], "practices": [...] }
```

Frontend expects flat array of `VocabEntry` with `canonical_name` (string) and `live` (boolean). The parsing at line 291 tries `data.entries`, `data.vocabulary`, or raw array — none match the actual response shape.

**Fix (choose one):**
- **Option A (frontend adapts to backend):** In the vocabulary fetch handler, flatten the categorized response and map field names:
  ```typescript
  const catalog = await r.json();
  const entries = [
    ...(catalog.topics || []),
    ...(catalog.entities || []),
    ...(catalog.decisions || []),
    ...(catalog.terms || []),
    ...(catalog.practices || []),
  ].map(e => ({
    canonical_name: e.name,
    category: e.category || 'unknown',
    importance: e.importance || 0,
    live: e.liveness === 'live',
    aliases: [],
  }));
  setVocabulary(entries);
  ```
- **Option B (backend adapts to frontend):** Add an `entries` field to `VocabularyCatalog` serialization that flattens + renames. More work, changes the API contract.

Option A is recommended — it's a frontend-only change and the backend shape is structurally richer.

### 3B-3. AddWorkspace.tsx also needs auth headers (Cycle 2 audit finding)

**File:** `src/components/AddWorkspace.tsx` lines 372, 422, 446

Same broken pattern as PyramidNavPage — makes HTTP calls (preview, commit, DADBEAR watch) without auth headers. All use `with_auth_state` and silently fail.

**Fix:** Same pattern as 3A/3B — fetch auth token via IPC, include in all fetch calls. Either share the token via React context or duplicate the IPC call in this component.

### 3B-4. MemoirView renders wrong decision field name (Cycle 2 audit finding)

**File:** `src/components/PyramidNavPage.tsx` line ~802

MemoirView renders decisions as `d.question ?? d.name ?? d.description ?? JSON.stringify(d)`. Backend `Decision` struct has `d.decided`, not `question`/`name`/`description`. Every decision falls through to `JSON.stringify(d)`.

**Fix:** Change to `d.decided ?? d.question ?? d.name ?? JSON.stringify(d)`. Same fix in NodeDetailView at ~line 1167.

### 3B-5. Search results use wrong field name for node ID (Cycle 2 audit finding)

**File:** `src/components/PyramidNavPage.tsx` lines ~1027, ~1052

Search result click handler uses `r.id` but backend `SearchHit`/`SearchReadingHit` returns `node_id`. Clicking search results does nothing.

**Fix:** Change `r.id` to `r.node_id ?? r.id` at both locations.

### 3B-6. Empty auth_token on server rejects all requests (Cycle 2 audit finding)

**File:** `src-tauri/src/pyramid/routes.rs` line 87

If `config.auth_token` is empty (fresh install), `with_auth_state` rejects ALL requests because `auth_token.is_empty()` short-circuits before token comparison. No HTTP endpoints work regardless of what the client sends.

**Fix (recommended):** If `auth_token` is empty on the server, skip auth for localhost-origin requests. This is appropriate for a desktop app where the HTTP server binds to 127.0.0.1 only. Alternatively, show a message in the nav page when token is empty.

### 3B-7. Vocabulary: match frontend types to backend (Cycle 2 audit finding, replaces 3B-2)

Instead of the brittle flatten+map in 3B-2, update the frontend `VocabEntry` interface to match the backend struct directly:

```typescript
interface VocabEntry {
    name: string;           // was canonical_name
    category: string | null;
    importance: number | null;
    liveness: string;       // was live: boolean — "live" | "mooted"
    detail?: any;
}
```

Then update all rendering code: `v.canonical_name` → `v.name`, `v.live` → `v.liveness === 'live'`, `e.live` → `e.liveness === 'live'`.

The flatten from `VocabularyCatalog` categories to flat array is still needed:
```typescript
const catalog = await r.json();
const entries = [
    ...(catalog.topics || []),
    ...(catalog.entities || []),
    ...(catalog.decisions || []),
    ...(catalog.terms || []),
    ...(catalog.practices || []),
];
setVocabulary(entries);
```

### 3B-8. All three HTTP useEffects need authToken in deps (Cycle 2 clarification)

The plan's 3B mentions adding `authToken` to "the useEffect at ~line 254." There are actually THREE separate useEffects that make HTTP calls:
- Vocabulary fetch (~line 282)
- DADBEAR/recovery/bedrocks fetch (~line 302)
- Reading mode fetch (~line 335)

ALL THREE need `authToken` in their dependency arrays, not just one.

### 3B-9. MemoirView must render narrative multi-zoom (Cycle 2 discovery finding)

**File:** `src/components/PyramidNavPage.tsx` lines ~770-809

MemoirView renders `data.distilled` but completely ignores `data.narrative` (the `NarrativeMultiZoom` structure). After WS-1 makes the vine produce rich episodic narrative, the UI won't display it. The narrative IS returned by the `pyramid_apex` IPC — MemoirView just doesn't render it.

**Fix:** Add a narrative section that renders `data.narrative?.levels` (array of `{zoom, text}`). Show the first/deepest zoom level's text as the primary prose. The narrative is the core differentiator of memoir mode — without it, memoir is just the distilled summary.

### 3C. Thread wildcard fix

**File:** `src-tauri/src/pyramid/reading_modes.rs` line 120

Add early check: when `identity` is empty or `*`, match everything:

```rust
let show_all = identity.is_empty() || identity == "*";
// Then in each loop: if show_all || name.contains(&identity_lower)
```

### 3D. Thread field name fix

**File:** `src/components/PyramidNavPage.tsx` line 914

Change `m.matched_field` → `m.matched_text`

### 3E. Thread default to all (frontend)

**File:** `src/components/PyramidNavPage.tsx` line 363

Keep `identity=*` (works after 3C). Optionally add a vocabulary-based topic picker later.

---

## Implementation order

1. **WS-0** (vine L0 assembly) — prerequisite for WS-1, without this the prompt swap gets empty data
2. **WS-1** (Gap 3) — product-critical change that makes vine produce memory
3. **WS-2** (Gap 1) — simple, independent, one insertion
4. **WS-3** (Gap 2) — UI fixes, all HTTP fetches need auth

## Verification

1. `cargo check` — must compile clean
2. `cargo test` — 785+ tests pass, 0 new regressions
3. Rebuild app: `cargo tauri build` or run dev server
4. Delete existing vine test slug, rebuild vine with 3 test conversations
5. Check:
   - Vine apex has `narrative` (prose, not empty), `decisions` with `stance`, `entities` with `role`/`importance`, `key_quotes` — not just generic topic bundles
   - Vocabulary panel populates after vine build
   - DADBEAR status panel shows data (was silently broken before)
   - Decisions reading mode shows decisions
   - Thread reading mode shows mentions
   - Speaker reading mode shows quotes (if LLM produced key_quotes)

## Follow-up workstreams (after this plan lands)

### WS-F1: ts-rs type generation
Add `#[derive(TS)]` to all structs that cross the HTTP boundary. Generate TypeScript types, delete hand-written interfaces. Eliminates the entire class of frontend/backend type drift bugs. Also adds DrillResult.remote_web_edges and .gaps to frontend.

### WS-F2: Chain executor `dispatch_pair` episodic fix
`chain_executor.rs:7663` uses `child_payload_json` for recursive pair synthesis, stripping episodic fields from per-session pyramid builds too. Same root cause as the vine fix, affects the chain executor path. Every per-session pyramid's L1→apex synthesis is getting stripped payloads.

### WS-F3: Watcher new-bunch path (CRITICAL for live mode)
When `VineJSONLWatcher` discovers a NEW JSONL file, it calls `build_bunch` but never registers in `vine_bunches`, never creates vine L0 nodes, never triggers L1 recluster. New sessions are silently invisible to the vine. Also `force_rebuild_vine_upper` only rebuilds L2+, not L1. Entire live-mode incremental path needs fixing.

### WS-F4: Bug fix & tech debt sweep (run twice)

Everything discovered by the audit cycle that isn't in a named workstream. Run this workstream, verify, then run it again to catch anything missed on first pass.

**Rust / backend:**
- Decision struct enrichment: add `by`, `at`, `context`, `ties_to` fields + extraction in `build_node_from_output`. Currently silently dropped, progressive loss of decision attribution at each synthesis layer.
- `parse_date_gap` (vine.rs:2703) uses `month * 30` approximation — use `chrono::NaiveDate` for real date math. Affects ERA boundary detection near month transitions.
- `notify_vine_of_bunch_change` chunk_index gaps (vine.rs:2968) — reassembled L0 nodes get non-temporal indices via `MAX(chunk_index)+1`. Either reuse superseded indices or reassign all in temporal order.
- `build_vine_l1` prompt references "L1-XXX" IDs but receives L0-XXX nodes (vine.rs:1365) — moot after prompt replacement, but verify.
- `build_bunch` (vine.rs:926) has an inline writer drain that duplicates `spawn_write_drain` (line 39). Consolidate.
- Vocabulary routes use `with_auth_state` (local-only) while reading routes use `with_slug_read_auth` (dual auth). Inconsistent for read-only endpoints — vocabulary should also use dual auth for Wire Online.

**Frontend / nav page:**
- MemoirView enrichment: show entities, terms, key_quotes, dead_ends from apex (currently only distilled + topics + decisions).
- MemoirView topic tags: show `t.current` status like NodeDetailView does (line 787 vs 1150).
- DecisionsView/SpeakerView: make `source_node_id` clickable (add `onNodeClick` prop like ThreadView has).
- Memoir/Walk use IPC while Thread/Decisions/Speaker use HTTP — consider switching all to HTTP `/reading/*` endpoints for consistency (once auth works), or document the split.
- DrillResult TypeScript interface missing `remote_web_edges` and `gaps` — data silently dropped. (ts-rs fixes this permanently, but can also quick-fix by adding the fields.)
- DADBEAR status never polls — goes stale after initial load. Add `setInterval` (30-60s) or listen for Tauri events.
- Reading modes `reading_thread`/`reading_decisions`/`reading_speaker` load entire node set O(N). Push filtering into SQL for large pyramids.
- Dead imports: `useAppContext`, `useRef` (PyramidNavPage.tsx lines 13, 15).
- Dead state: `overlayTab`/`setOverlayTab` (line 226).
- Dead data: `setReadingData(tree)` for walk mode (line 346) — WalkView doesn't consume readingData.
- `readingData` for walk mode captures `tree` reference that isn't in the dependency array — stale closure.

## Key files

| File | Role |
|---|---|
| `src-tauri/src/pyramid/vine.rs` | L0 assembly fix + L1/upper synthesis replacement |
| `src-tauri/src/pyramid/build.rs` | New episodic payload function |
| `src-tauri/src/pyramid/chain_dispatch.rs:324` | `build_node_from_output` — reuse, don't modify |
| `chains/prompts/conversation-episodic/synthesize_recursive.md` | The correct prompt — reuse via include_str! |
| `src-tauri/src/pyramid/routes.rs` | Vocab refresh in vine build handler |
| `src/components/PyramidNavPage.tsx` | Auth headers on ALL fetches, field fix, thread default |
| `src-tauri/src/pyramid/reading_modes.rs` | Thread wildcard handling |
| `src-tauri/src/main.rs` | Auth token IPC command |
