# recursive-vine-v2 — Phase 2 + Phase 4 Prep (v2)

> **Status:** v2, written 2026-04-07 after Stage 1 informed audit (auditors K, L) + Stage 2 discovery audit (auditors M, N) on the v1 prep doc. 5 critical findings + 8 majors, all addressed below.
>
> **Supersedes:** `recursive-vine-v2-phase-2-and-4-prep.md` (v1).
> **Audit synthesis:** `recursive-vine-v2-phase-2-and-4-prep.audit-synthesis.md`.
>
> Every claim grounded in verified file paths + line numbers (re-verified after the audit).

---

## Section 0 — Verified facts (re-checked after audit)

### 0.1 `run_decomposed_build` actual signature

**File:** `build_runner.rs:776-787`

```rust
pub async fn run_decomposed_build(
    state: &PyramidState,
    slug_name: &str,
    apex_question: &str,
    granularity: u32,
    max_depth: u32,
    from_depth: i64,
    characterization: Option<CharacterizationResult>,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    layer_tx: Option<mpsc::Sender<LayerEvent>>,
) -> Result<(String, i32, Vec<super::types::StepActivity>)>
```

10 parameters. Returns `(build_id, node_count, step_activities)` where `build_id` is a `qb-{8hex}` string.

### 0.2 `initial_context` is built from a hardcoded list

**File:** `build_runner.rs:913-961`

The function constructs `initial_context: HashMap<String, serde_json::Value>` from these fixed keys: `apex_question`, `granularity`, `max_depth`, `from_depth`, `content_type`, `audience`, `characterize`, `is_cross_slug`, `referenced_slugs`, `build_id`. There is **no parameter for caller-injected extras**. This is the CRIT-4 finding.

**v2 fix:** add an `extra_initial_params: Option<HashMap<String, serde_json::Value>>` parameter to the signature. Merge it into `initial_context` after the standard fields are built. All existing call sites pass `None`.

### 0.3 `execute_process_gaps` dispatcher loop

**File:** `chain_executor.rs:5183-5455` (`async fn execute_process_gaps`)

Verified structure:
- Line 5237: `referenced_slugs: Vec<String>` is in scope at the function level (loaded from `load_prior_state_val`)
- Line 5253-5258: `unresolved_gaps` loaded via `db_read`
- Line 5274: `for gap in &unresolved_gaps` outer loop
- Line 5301-5305: `base_slugs_for_gap` computed inside the loop
- Line 5312-5351: the `db_read` blocking closure that does the partition + file/pyramid resolution
- Line 5316-5326: the closure declares `file_slugs` and `pyramid_slugs` as LOCAL variables — they are NOT in outer scope
- Line 5353: outer scope receives only `resolved_files: Result<Vec<(String, String, String)>>`

This is CRIT-3. The v2 dispatcher needs to either return more from the closure, or move the partition outside the closure.

### 0.4 `RemotePyramidClient::remote_search` actual signature

**File:** `wire_import.rs:775` (and surrounding)

```rust
pub async fn remote_search(&self, slug: &str, query: &str) -> Result<RemoteSearchResponse>
```

**Two arguments**, not three. `RemoteSearchResponse.results: Vec<serde_json::Value>` — untyped JSON. The plan's `hit.headline`, `hit.snippet`, `hit.node_id` are fictional struct fields. CRIT-1.

### 0.5 `mint_token` does not exist

Confirmed by grep: no function named `mint_token` anywhere in `src-tauri/src/`. The cost-aware methods at `wire_import.rs:1043+` (`remote_search_with_cost`, `remote_drill_with_cost`, etc.) all carry `TODO(WS-ONLINE-H): Integrate payment-intent/token flow when Wire server ready`. **The Wire-side payment infrastructure is unbuilt** and tracked as the WS-ONLINE-H workstream. CRIT-2.

### 0.6 Child question pyramid persistence

**File:** `chain_executor.rs:5417-5454` (the existing file-path save logic)

The existing `targeted_reexamination` flow saves new L0 nodes to the SOURCE slug (`base_slug` in scope), not the question slug. Confirmed at line 5422 (`db::save_node(&c, node, None)?` with `node.slug = base_slug_owned`).

But this is the FILE path. Phase 2's child-question-pyramid path uses `run_decomposed_build`, which persists nodes under the CHILD slug (`{source}--ask-{hash}`). The child's nodes are tagged with the child slug, not the source slug. CRIT-5.

### 0.7 `Tier3Config` location

**File:** `mod.rs:414+`

Tier3Config is defined in `src-tauri/src/pyramid/mod.rs`. All fields use `#[serde(default)]` for backwards compat. Adding 2 new fields is additive and safe.

### 0.8 `ChainContext.initial_params`

**File:** `chain_resolve.rs:61`

`ChainContext` has an `initial_params: HashMap<String, Value>` field. `execute_chain_from` accepts `Option<HashMap<String, Value>>` as its last parameter and stores it on the context. Verified by reading `build_runner.rs:973-975` which passes `Some(initial_context)`.

So extending `run_decomposed_build` with `extra_initial_params` and merging into `initial_context` flows naturally into `ChainContext.initial_params` for the child build.

### 0.9 `db::get_slug_referrers`

**File:** `db.rs:1607-1614`

`pub fn get_slug_referrers(conn: &Connection, slug: &str) -> Result<Vec<String>>` exists. Reads from `pyramid_slug_references` where `referenced_slug = ?1`. This is the helper Phase 2's referrer-walk re-resolve uses.

### 0.10 `pyramid_slug_references` schema

**File:** `db.rs:702-712`

```sql
CREATE TABLE IF NOT EXISTS pyramid_slug_references (
    slug TEXT NOT NULL REFERENCES pyramid_slugs(slug),
    referenced_slug TEXT NOT NULL REFERENCES pyramid_slugs(slug),
    reference_type TEXT NOT NULL DEFAULT 'base',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (slug, referenced_slug)
);
```

Has a `reference_type` column with default `'base'`. Phase 2 uses `'vine-escalation'` as a new value to mark child-question-pyramid refs created by escalation. Phase 4 can use `'vine-remote'` for remote source refs (or use a separate table per below).

**No CASCADE on either FK.** `purge_slug` cleans references explicitly via the manual DELETE pattern at `db.rs:1701+`.

### 0.11 `slug::validate_slug` rules

**File:** `slug.rs:36-49`

```rust
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() { return Err(...); }
    if slug.len() > 128 { return Err(...); }
    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(...);
    }
    Ok(())
}
```

128-char limit, `[a-z0-9-]` charset only. **No consecutive-hyphen check** — `--ask-` is legal under validate_slug. But `slug::slugify` at `:17-33` collapses runs of `-` to a single `-`. So if escalation goes through `slug::create_slug` (which calls slugify), `{source}--ask-{hash}` normalizes to `{source}-ask-{hash}`. If it goes through `db::create_slug` (raw insert), no normalization.

**v2 fix:** escalation calls `db::create_slug` directly with a name that already passes the 128-char limit. Use `--ask-` (double hyphen) so it's distinguishable from any user-typed slug. Cap source slug length to 110 chars to leave room (`{110}--ask-{8}` = 124 chars).

### 0.12 `HandlePath::parse`

**File:** `types.rs:613` (verified by audit M)

`HandlePath::parse(s: &str) -> Option<HandlePath>` requires exactly 3 segments separated by `/`: `slug/depth/node_id`. A bare slug or `slug/apex` returns None. CRIT-1 sub-issue.

**v2 fix:** for remote vine sources, define a sibling `RemoteSlugRef` struct with separate `slug: String` and `tunnel_url: String` fields, and a parser that accepts `wire://{tunnel_url}/{slug}` format (or just store them as separate columns in the new table).

### 0.13 `pyramid_unredeemed_tokens` actual line

**File:** `db.rs:1136` (auditor confirmed; v1 said 976, stale)

The table exists. Schema covers nonce, payment_token, querier_operator_id, slug, query_type, stamp_amount, access_amount, total_amount, etc. But its WRITE sites are all the `_with_cost` methods that have the `TODO(WS-ONLINE-H)` block. Confirmed.

### 0.14 `db::get_access_tier`

**File:** `db.rs:5333` (auditor confirmed; helper exists)

Returns `(tier, price, allowed_circles)` for a slug. Used by Phase 4.3.

---

## Section 1 — Phase 2: Recursive ask escalation (corrected)

### 2.0 — Pre-flight: extend `run_decomposed_build` with `extra_initial_params`

**File:** `build_runner.rs:776`

```rust
pub async fn run_decomposed_build(
    state: &PyramidState,
    slug_name: &str,
    apex_question: &str,
    granularity: u32,
    max_depth: u32,
    from_depth: i64,
    characterization: Option<CharacterizationResult>,
    cancel: &CancellationToken,
    progress_tx: Option<mpsc::Sender<BuildProgress>>,
    layer_tx: Option<mpsc::Sender<LayerEvent>>,
    extra_initial_params: Option<HashMap<String, serde_json::Value>>,  // NEW
) -> Result<(String, i32, Vec<super::types::StepActivity>)>
```

After building `initial_context` from the hardcoded fields (lines 916-930), merge:

```rust
if let Some(extras) = extra_initial_params {
    for (k, v) in extras {
        // Don't let extras override standard fields — they're load-bearing.
        if !initial_context.contains_key(&k) {
            initial_context.insert(k, v);
        }
    }
}
```

**Existing callers** (need to update each to pass `None`):
- `build_runner.rs:218-230` (Question dispatch in `run_build_from`)
- `build_runner.rs:264-275` (Conversation dispatch in `run_build_from`)
- The `run_chain_build` and `run_ir_build` paths do NOT call `run_decomposed_build` — they call `chain_executor::execute_chain_from` directly. No update needed.

**Check:** grep `run_decomposed_build(` to confirm the caller list before editing.

### 2.1 — Tier3Config new fields

**File:** `mod.rs:414+`

```rust
pub struct Tier3Config {
    // ... existing fields ...

    /// recursive-vine-v2 Phase 2: maximum depth a vine's recursive ask can
    /// escalate before stopping. depth=0 disables escalation entirely.
    /// depth=2 (default) allows one nested ask.
    #[serde(default = "default_vine_max_recursion_depth")]
    pub vine_max_recursion_depth: u32,

    /// recursive-vine-v2 Phase 2: minimum number of pyramid evidence nodes
    /// from resolve_pyramids_for_gap to consider Stage 1 successful. If
    /// Stage 1 returns fewer matches, escalate to recursive ask.
    #[serde(default = "default_vine_evidence_threshold")]
    pub vine_evidence_threshold: usize,

    /// recursive-vine-v2 Phase 2: maximum parallel child question builds
    /// when a single gap escalates to multiple source pyramids. Bounded
    /// to avoid LLM-rate-limit blowups during deep escalation.
    #[serde(default = "default_vine_max_parallel_escalations")]
    pub vine_max_parallel_escalations: usize,
}
fn default_vine_max_recursion_depth() -> u32 { 2 }
fn default_vine_evidence_threshold() -> usize { 3 }
fn default_vine_max_parallel_escalations() -> usize { 3 }
```

### 2.2 — Recursion-depth + visited-set threading via `extra_initial_params`

The escalation function passes recursion state through `extra_initial_params`:

```rust
let mut extras: HashMap<String, serde_json::Value> = HashMap::new();
extras.insert("vine_recursion_depth".to_string(), serde_json::json!(parent_depth + 1));
extras.insert("vine_visited_slugs".to_string(),
    serde_json::json!(visited_so_far.iter().collect::<Vec<_>>()));
```

The child build's `execute_process_gaps` reads them from `ctx.initial_params`:

```rust
let current_recursion_depth: u32 = ctx.initial_params
    .get("vine_recursion_depth")
    .and_then(|v| v.as_u64())
    .map(|n| n as u32)
    .unwrap_or(0);

let visited: HashSet<String> = ctx.initial_params
    .get("vine_visited_slugs")
    .and_then(|v| serde_json::from_value(v.clone()).ok())
    .unwrap_or_default();
```

Visited set MAJ-3 fix: prevents A→B→A cycles within the depth bound.

### 2.3 — Restructure the dispatcher loop to expose partition results

**File:** `chain_executor.rs:5274-5455` (the `for gap in &unresolved_gaps` loop)

The `db_read` closure currently returns only `Vec<(String, String, String)>`. Change it to return a struct with the partition results AND the combined evidence:

```rust
struct GapResolution {
    combined: Vec<(String, String, String)>,
    file_slugs: Vec<String>,
    pyramid_slugs: Vec<String>,
}

let resolution = db_read(&state.reader, {
    let base_slugs = base_slugs_for_gap.clone();
    let gap_desc = gap.description.clone();
    let max_files = state.operational.tier2.gap_resolution_max_files;
    move |conn| -> Result<GapResolution> {
        let mut file_slugs: Vec<String> = Vec::new();
        let mut pyramid_slugs: Vec<String> = Vec::new();
        for s in &base_slugs {
            match db::has_file_hashes(conn, s) {
                Ok(true) => file_slugs.push(s.clone()),
                Ok(false) => pyramid_slugs.push(s.clone()),
                Err(_) => file_slugs.push(s.clone()),
            }
        }
        let mut combined: Vec<(String, String, String)> = Vec::new();
        if !file_slugs.is_empty() {
            combined.extend(super::evidence_answering::resolve_files_for_gap(
                conn, &file_slugs, &gap_desc, &[], max_files,
            )?);
        }
        if !pyramid_slugs.is_empty() {
            combined.extend(super::evidence_answering::resolve_pyramids_for_gap(
                conn, &pyramid_slugs, &gap_desc, max_files,
            )?);
        }
        Ok(GapResolution { combined, file_slugs, pyramid_slugs })
    }
}).await;

let mut resolution = match resolution {
    Ok(r) => r,
    Err(e) => { warn!(slug, error = %e, "gap evidence resolution failed"); continue; }
};
```

Now `resolution.pyramid_slugs` is in scope at the outer level, where `state` is accessible for the async escalation call.

### 2.4 — Trigger escalation when below threshold

After the closure returns:

```rust
let pyramid_evidence_count = resolution.combined.iter()
    .filter(|(slug, _, _)| resolution.pyramid_slugs.contains(slug))
    .count();

let max_depth = state.operational.tier3.vine_max_recursion_depth;
let threshold = state.operational.tier3.vine_evidence_threshold;
let current_depth = ctx.initial_params
    .get("vine_recursion_depth")
    .and_then(|v| v.as_u64())
    .map(|n| n as u32)
    .unwrap_or(0);
let visited: HashSet<String> = ctx.initial_params
    .get("vine_visited_slugs")
    .and_then(|v| serde_json::from_value(v.clone()).ok())
    .unwrap_or_default();

let should_escalate = current_depth < max_depth
    && !resolution.pyramid_slugs.is_empty()
    && pyramid_evidence_count < threshold;

if should_escalate {
    info!(
        slug, gap_id = %gap.question_id,
        pyramid_evidence_count, threshold, depth = current_depth,
        "vine: escalating recursive ask"
    );

    let escalation = escalate_gap_to_source_pyramids(
        state,
        &gap,
        &question_text,
        &resolution.pyramid_slugs,
        current_depth + 1,
        &visited,
        cancel,
    ).await;

    match escalation {
        Ok(child_slugs) => {
            // Re-resolve evidence: walk source pyramids AND the new child
            // question pyramids via the referrer graph.
            let re_resolved = db_read(&state.reader, {
                let pyramid_slugs = resolution.pyramid_slugs.clone();
                let gap_desc = gap.description.clone();
                let max_files = state.operational.tier2.gap_resolution_max_files;
                move |conn| -> Result<Vec<(String, String, String)>> {
                    // Walk all evidence-bearing slugs: original sources +
                    // every escalation-child question pyramid that references them.
                    let mut all_slugs = pyramid_slugs.clone();
                    for source in &pyramid_slugs {
                        if let Ok(referrers) = db::get_slug_referrers(conn, source) {
                            for r in referrers {
                                // Only walk vine-escalation children, not arbitrary referrers.
                                if r.contains("--ask-") {
                                    if let Ok(Some(info)) = db::get_slug(conn, &r) {
                                        if info.content_type.as_str() == "question" {
                                            all_slugs.push(r);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    super::evidence_answering::resolve_pyramids_for_gap(
                        conn, &all_slugs, &gap_desc, max_files,
                    )
                }
            }).await;

            if let Ok(new_evidence) = re_resolved {
                let added = new_evidence.len().saturating_sub(pyramid_evidence_count);
                info!(slug, gap_id = %gap.question_id, added,
                      "vine: re-resolve picked up new evidence after escalation");
                resolution.combined = new_evidence;
            }
        }
        Err(e) => {
            warn!(slug, gap_id = %gap.question_id, error = %e,
                  "vine: escalation failed, continuing with stage-1 evidence");
        }
    }
}

let resolved_files = resolution.combined;
// ... rest of existing dispatcher loop unchanged ...
```

This is the **CRIT-5 fix** — re-resolve walks the referrer graph, picks up the child question pyramid's nodes, and merges them into the combined evidence the parent feeds to `targeted_reexamination`.

### 2.5 — `escalate_gap_to_source_pyramids` function

**File:** `chain_executor.rs` (new private async function near `execute_process_gaps`)

```rust
/// recursive-vine-v2 Phase 2: spawn a child question pyramid on each source
/// pyramid with the gap as the apex question. Builds via run_decomposed_build,
/// which creates a sibling question pyramid that references the source. The
/// dispatcher's re-resolve step then walks the referrer graph to pick up the
/// new evidence (since child nodes are tagged with the child slug, not the
/// source slug — see CRIT-5 in the audit synthesis).
///
/// Bounded by:
///   - tier3.vine_max_recursion_depth (passed in via current_depth+1)
///   - tier3.vine_max_parallel_escalations (caps simultaneous child builds)
///   - visited set (prevents A→B→A cycles within the depth bound)
async fn escalate_gap_to_source_pyramids(
    state: &PyramidState,
    gap: &super::types::GapReport,
    question_text: &str,
    source_pyramid_slugs: &[String],
    next_depth: u32,
    visited: &HashSet<String>,
    cancel: &CancellationToken,
) -> Result<Vec<String>> {
    use sha2::{Digest, Sha256};

    let mut child_slugs: Vec<String> = Vec::new();
    let mut to_escalate: Vec<String> = Vec::new();

    // Filter out visited slugs (cycle detection)
    for source in source_pyramid_slugs {
        if !visited.contains(source) {
            to_escalate.push(source.clone());
        }
    }
    if to_escalate.is_empty() {
        info!("vine: all source pyramids already visited at this recursion level");
        return Ok(child_slugs);
    }

    // Build the visited set the children will inherit
    let mut next_visited: HashSet<String> = visited.clone();
    next_visited.extend(to_escalate.iter().cloned());

    // Stable hash of the gap so re-asks are idempotent
    let gap_hash_full = format!("{:x}", Sha256::digest(gap.description.as_bytes()));
    let gap_hash = &gap_hash_full[..8];

    // Bound parallelism
    let max_parallel = state.operational.tier3.vine_max_parallel_escalations;
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max_parallel));

    let mut tasks = tokio::task::JoinSet::new();
    for source_slug in to_escalate {
        // Truncate source slug to leave room for the suffix (--ask-XXXXXXXX = 13 chars)
        let truncated_source = if source_slug.len() > 110 {
            source_slug[..110].to_string()
        } else {
            source_slug.clone()
        };
        let child_slug = format!("{}--ask-{}", truncated_source, gap_hash);

        // Idempotency: skip if child already exists
        let already_exists = {
            let conn = state.reader.lock().await;
            db::get_slug(&conn, &child_slug)?.is_some()
        };

        if !already_exists {
            // Create the child slug in its own short writer-lock scope
            // (CRITICAL: drop the writer guard before the recursive build)
            {
                let conn = state.writer.lock().await;
                db::create_slug(
                    &conn,
                    &child_slug,
                    &super::types::ContentType::Question,
                    "",
                )?;
                db::save_slug_references(&conn, &child_slug, &[source_slug.clone()])?;
                info!(child_slug = %child_slug, source = %source_slug,
                      "vine: created child question pyramid for escalation");
            }
        }

        // Spawn the child build
        let state_clone = state.clone();
        let cancel_clone = cancel.clone();
        let question_text_owned = question_text.to_string();
        let child_slug_owned = child_slug.clone();
        let next_visited_clone = next_visited.clone();
        let sem = semaphore.clone();

        tasks.spawn(async move {
            let _permit = match sem.acquire_owned().await {
                Ok(p) => p,
                Err(_) => return Err(anyhow!("semaphore closed")),
            };

            let mut extras: HashMap<String, serde_json::Value> = HashMap::new();
            extras.insert(
                "vine_recursion_depth".to_string(),
                serde_json::json!(next_depth),
            );
            extras.insert(
                "vine_visited_slugs".to_string(),
                serde_json::json!(next_visited_clone.iter().collect::<Vec<_>>()),
            );

            // Recursive call. Note: state is &PyramidState; we cloned it above.
            // Box::pin to break the recursive future-size loop.
            Box::pin(crate::pyramid::build_runner::run_decomposed_build(
                &state_clone,
                &child_slug_owned,
                &question_text_owned,
                3, // granularity — sub-questions stay shallow
                2, // max_depth — bounded tree
                0, // from_depth
                None, // characterization (auto)
                &cancel_clone,
                None, // progress_tx
                None, // layer_tx
                Some(extras),
            ))
            .await
            .map(|(build_id, _, _)| (child_slug_owned, build_id))
        });
    }

    // Collect results
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok((slug, build_id))) => {
                info!(child_slug = %slug, build_id = %build_id,
                      "vine: escalation child build complete");
                child_slugs.push(slug);
            }
            Ok(Err(e)) => {
                warn!(error = %e, "vine: escalation child build failed");
            }
            Err(e) => {
                warn!(error = %e, "vine: escalation task panicked");
            }
        }
    }

    Ok(child_slugs)
}
```

**Key correctness points:**
- **Lock scope** — the writer guard for slug creation is in its own `{ }` block that drops before the recursive `run_decomposed_build` call. MAJ-2 fix.
- **`Box::pin`** on the recursive call to break the unbounded future-size that recursive async functions normally trigger.
- **`PyramidState` is cloneable** (verify) OR the caller passes a wrapped `Arc<PyramidState>` — check `mod.rs` to see which. If `state` is already `Arc`-wrapped at the call site, the clone is cheap; otherwise the function takes `&PyramidState` and the recursive call needs to lift it. Most likely the call is fine because `PyramidState` already contains `Arc`'d members for `reader`, `writer`, `config`. Verify by reading the struct definition.
- **Idempotency** — re-asks of the same gap (same gap text, same source) reuse the existing child slug rather than creating duplicates.
- **Parallelism** — `JoinSet` with semaphore caps simultaneous child builds at `vine_max_parallel_escalations` (default 3). MAJ-7 fix.
- **Visited set** — children inherit the visited set including their parent sources, blocking A→B→A cycles. MAJ-3 fix.

### 2.6 — Question tree initialization for child builds

**Concern (MAJ-4):** `run_decomposed_build` reads `db::get_question_tree(conn, slug_name)` early. If the tree doesn't exist, it errors.

**Verify by reading:** `build_runner.rs:200-217` (Question dispatch path). The Question dispatch DOES read the question tree and errors if missing. But that's the `run_build_from` path. The `run_decomposed_build` function called directly from escalation doesn't go through `run_build_from`.

**Read `run_decomposed_build:776-1000` to confirm.** From the verified read above (Section 0.1), `run_decomposed_build` does NOT pre-fetch the question tree — it builds the tree fresh via the chain (`decompose` step). The tree is saved at the end of the build, not read at the start.

**Verification step:** before shipping, run a test build of a fresh question slug created with empty source_path and confirm the chain succeeds. If it fails on missing question tree, add a `db::save_question_tree(&conn, &child_slug, &serde_json::json!({}))` call before the recursive build.

Actually — re-reading `build_runner.rs:200-217` more carefully:

```rust
if content_type == ContentType::Question {
    // Retrieve the stored apex question and config from the question tree
    let (apex_question, stored_granularity, stored_max_depth) = {
        let conn = state.reader.lock().await;
        let tree_json = db::get_question_tree(&conn, slug_name)?.ok_or_else(|| {
            anyhow!(
                "Question slug '{}' has no stored question tree. \
                 Use the question build endpoint to set the initial question.",
                slug_name
            )
        })?;
        ...
```

This is the `run_build_from` Question early-return. It requires a pre-existing tree because `run_build_from` doesn't accept an apex_question parameter. But escalation calls `run_decomposed_build` DIRECTLY with the apex_question as a parameter, bypassing `run_build_from`. So this check is irrelevant.

**However**, the wizard's `pyramid_question_build` IPC command (or whichever IPC creates new question pyramids today) probably calls a flow that DOES save the tree. We need to mirror it. **Action:** read `pyramid_question_build` IPC handler in `main.rs` to see what it does after `pyramid_create_slug` — it likely calls a helper that writes the initial tree row. The escalation function should call the same helper.

Add to v2 done criteria: **before merging Phase 2, verify the escalation child build works without explicit `save_question_tree`. If not, replicate the tree-init path from the existing `pyramid_question_build` IPC.**

### 2.7 — `vine_max_recursion_depth = 0` test case

The recursion bound check is:
```rust
let should_escalate = current_depth < max_depth && ...
```

With `max_depth = 0`, this is `current_depth < 0` which is always false (`current_depth: u32`). Escalation never fires. Verified safe.

### 2.8 — Phase 2 done criteria

- [ ] `Tier3Config` has `vine_max_recursion_depth`, `vine_evidence_threshold`, `vine_max_parallel_escalations` with defaults
- [ ] `run_decomposed_build` accepts `extra_initial_params: Option<HashMap<String, serde_json::Value>>`
- [ ] All existing callers of `run_decomposed_build` updated to pass `None`
- [ ] `execute_process_gaps` dispatcher refactored to return `GapResolution` struct with partition exposed
- [ ] `escalate_gap_to_source_pyramids` function exists
- [ ] Recursion-depth + visited-set threading works (verify with `vine_max_recursion_depth = 0` returns immediately, `= 2` allows one nested ask, A→B→A cycle blocked by visited set)
- [ ] Re-resolve walks the referrer graph (`db::get_slug_referrers` + filter to question content_type + `--ask-` substring)
- [ ] Child question pyramids tree-init verified (see 2.6 — possibly needs `save_question_tree` call)
- [ ] Parallelism via `JoinSet` + semaphore at `vine_max_parallel_escalations`
- [ ] Lock scope correct: writer guard dropped before recursive `run_decomposed_build` call
- [ ] Manual smoke test: create vine pyramid → trigger gap → escalation runs → child pyramid created → re-resolve picks up new evidence count > pre-escalation count

---

## Section 2 — Phase 4: Cross-operator vines (corrected, scoped)

### 4.0 — Scope correction

The v1 prep doc framed Phase 4 as a single workstream. The audit found that **Phase 4.4 (credit flow) depends on the unbuilt WS-ONLINE-H Wire-side payment infrastructure**. v2 splits Phase 4 into two stages:

- **Phase 4-local:** remote source refs, remote evidence resolution, dispatcher integration, public-only mode (no paid queries)
- **Phase 4-paid (deferred):** credit flow integration. Blocked on WS-ONLINE-H landing on the GoodNewsEveryone (Wire server) repo. Tracked as a separate ticket.

Phase 4-local ships in this session if time permits. Phase 4-paid ships later.

### 4.1 — `pyramid_remote_slug_references` table

**File:** `db.rs::init_pyramid_db`

```rust
let _ = conn.execute(
    "CREATE TABLE IF NOT EXISTS pyramid_remote_slug_references (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        slug TEXT NOT NULL REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
        remote_handle_path TEXT NOT NULL,
        remote_tunnel_url TEXT NOT NULL,
        reference_type TEXT NOT NULL DEFAULT 'vine-source',
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        UNIQUE(slug, remote_handle_path, remote_tunnel_url)
    )",
    [],
);
let _ = conn.execute(
    "CREATE INDEX IF NOT EXISTS idx_remote_slug_refs_slug ON pyramid_remote_slug_references(slug)",
    [],
);
```

**Notes:**
- `ON DELETE CASCADE` only on the LOCAL slug, not on remote (we don't own remote pyramids)
- UNIQUE includes `remote_tunnel_url` so the same handle path on different operators is two distinct refs (MIN-2 fix)
- `reference_type = 'vine-source'` distinguishes from any future remote ref types

Helpers in `db.rs`:
```rust
pub fn save_remote_slug_reference(
    conn: &Connection,
    slug: &str,
    remote_handle_path: &str,
    remote_tunnel_url: &str,
) -> Result<()> { ... }

pub struct RemoteSlugRef {
    pub remote_handle_path: String,
    pub remote_tunnel_url: String,
}
pub fn get_remote_slug_references(
    conn: &Connection,
    slug: &str,
) -> Result<Vec<RemoteSlugRef>> { ... }
```

### 4.2 — `resolve_remote_pyramids_for_gap` (corrected against actual signatures)

**File:** `evidence_answering.rs`, alongside `resolve_pyramids_for_gap`

```rust
/// recursive-vine-v2 Phase 4: resolve evidence from REMOTE pyramid sources
/// (other operators' published pyramids) by fetching nodes via
/// RemotePyramidClient::remote_search. Mirrors `resolve_pyramids_for_gap`'s
/// (slug, pseudo_path, content) shape so the dispatcher can swap freely.
///
/// PHASE 4-LOCAL ONLY: this function uses the public `remote_search` (no
/// payment token). When WS-ONLINE-H lands and `mint_token` is wired up, swap
/// to `remote_search_with_cost` and add token-flow logic per Phase 4-paid.
pub async fn resolve_remote_pyramids_for_gap(
    remote_refs: &[crate::pyramid::db::RemoteSlugRef],
    gap_description: &str,
    max_nodes: usize,
    wire_jwt: &str,
    wire_server_url: &str,
) -> Result<Vec<(String, String, String)>> {
    let keywords: Vec<String> = gap_description
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(String::from)
        .collect();
    if keywords.is_empty() { return Ok(Vec::new()); }

    use std::collections::HashMap;
    use crate::pyramid::wire_import::RemotePyramidClient;
    let mut clients: HashMap<String, RemotePyramidClient> = HashMap::new();

    // (handle_path_used_as_id, pseudo_path, body, score)
    let mut scored: Vec<(String, String, String, usize)> = Vec::new();

    for rref in remote_refs {
        let client = clients.entry(rref.remote_tunnel_url.clone()).or_insert_with(|| {
            RemotePyramidClient::new(
                rref.remote_tunnel_url.clone(),
                wire_jwt.to_string(),
                wire_server_url.to_string(),
            )
        });

        // remote_handle_path for vine sources is just the slug name on the
        // remote operator (e.g., "their-pyramid"), not a 3-segment HandlePath.
        // We use it directly as the slug arg to remote_search.
        let remote_slug = &rref.remote_handle_path;

        match client.remote_search(remote_slug, gap_description).await {
            Ok(response) => {
                // RemoteSearchResponse.results is Vec<serde_json::Value>.
                // Navigate untyped — defensive against shape drift.
                for hit in response.results.iter().take(max_nodes) {
                    let node_id = hit.get("node_id")
                        .or_else(|| hit.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let headline = hit.get("headline")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let snippet = hit.get("snippet")
                        .or_else(|| hit.get("distilled"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let pseudo_path = format!(
                        "wire://{}/{}/{}",
                        rref.remote_tunnel_url, remote_slug, node_id
                    );
                    let body = format!(
                        "## REMOTE SOURCE NODE {}\n\n## HEADLINE\n{}\n\n## SNIPPET\n{}",
                        pseudo_path, headline, snippet
                    );
                    let text = body.to_lowercase();
                    let score = keywords.iter().filter(|kw| text.contains(kw.as_str())).count();
                    if score > 0 {
                        scored.push((remote_slug.clone(), pseudo_path, body, score));
                    }
                }
            }
            Err(e) => {
                warn!(
                    handle = %rref.remote_handle_path,
                    tunnel = %rref.remote_tunnel_url,
                    error = %e,
                    "vine: remote pyramid query failed"
                );
            }
        }
    }

    scored.sort_by(|a, b| b.3.cmp(&a.3));
    scored.truncate(max_nodes);

    info!(
        remote_count = remote_refs.len(),
        result_count = scored.len(),
        "vine: resolved remote pyramid nodes for gap"
    );

    Ok(scored.into_iter()
        .map(|(slug, path, content, _)| (slug, path, content))
        .collect())
}
```

CRIT-1 fixed: actual signature, untyped JSON navigation with defensive `.get().and_then()` chains.

### 4.3 — Local access tier check (corrected, no `querier_op_id`)

For LOCAL pyramid resolution (Phase 1.1 `resolve_pyramids_for_gap`), the only enforceable tier check is "skip embargoed pyramids" — the operator querying their own pyramids has implicit access to public/circle/priced.

```rust
// Inside resolve_pyramids_for_gap, before walking each source slug:
if let Ok(slug_info) = db::get_slug(conn, source_slug) {
    if let Some(info) = slug_info {
        // Phase 4.3: skip embargoed pyramids (defense-in-depth; the Wire
        // server is the authority for non-local queries).
        if let Ok((tier, _price, _circles)) = db::get_access_tier(conn, source_slug) {
            if tier == "embargoed" {
                warn!(slug = %source_slug, "vine: skipping embargoed source pyramid");
                continue;
            }
        }
    }
}
```

For REMOTE queries (Phase 4.2), the remote operator's Wire Node enforces tier on its end. No local check needed — the remote will return 403/404 for non-public pyramids.

MAJ-5 fixed: dropped `querier_op_id` and `is_querier_allowed`.

### 4.4 — Dispatcher integration (async/sync mismatch fixed)

**File:** `chain_executor.rs::execute_process_gaps`

The blocking `db_read` closure does file + local pyramid resolution. Remote resolution happens in the OUTER async scope after the closure returns:

```rust
// Inside the for-gap loop, AFTER the GapResolution closure (Section 2.3)
// completes and we have `resolution.combined`:

// Phase 4 (chain-binding-v2.5 + recursive-vine-v2): also resolve from remote
// pyramid sources. The remote calls are async HTTP, so they happen here in
// the outer scope rather than inside the blocking db_read closure.
let remote_refs = db_read(&state.reader, {
    let s = slug.to_string();
    move |conn| db::get_remote_slug_references(conn, &s)
}).await.unwrap_or_default();

if !remote_refs.is_empty() {
    let wire_jwt = state.config.read().await.auth_token.clone();
    let wire_server_url = std::env::var("WIRE_URL")
        .unwrap_or_else(|_| "https://newsbleach.com".to_string());

    let max_files = state.operational.tier2.gap_resolution_max_files;
    match super::evidence_answering::resolve_remote_pyramids_for_gap(
        &remote_refs,
        &gap.description,
        max_files,
        &wire_jwt,
        &wire_server_url,
    ).await {
        Ok(remote_results) => {
            resolution.combined.extend(remote_results);
        }
        Err(e) => {
            warn!(slug, error = %e, "vine: remote evidence resolution failed");
        }
    }
}
```

MAJ-1 fixed: async resolution outside the blocking closure.

### 4.5 — `pyramid_add_remote_source` IPC

**File:** `main.rs`

```rust
#[tauri::command]
async fn pyramid_add_remote_source(
    state: tauri::State<'_, SharedState>,
    slug: String,
    remote_handle_path: String,
    remote_tunnel_url: String,
) -> Result<(), String> {
    if remote_handle_path.trim().is_empty() || remote_tunnel_url.trim().is_empty() {
        return Err("remote_handle_path and remote_tunnel_url are required".to_string());
    }
    let conn = state.pyramid.writer.lock().await;
    wire_node_lib::pyramid::db::save_remote_slug_reference(
        &conn,
        slug.trim(),
        remote_handle_path.trim(),
        remote_tunnel_url.trim(),
    ).map_err(|e| e.to_string())
}
```

Register in the invoke_handler list alongside the other vine IPCs.

### 4.6 — Phase 4-local done criteria

- [ ] `pyramid_remote_slug_references` table created via existing init pattern
- [ ] `db::save_remote_slug_reference`, `db::get_remote_slug_references`, `db::RemoteSlugRef` struct exist
- [ ] `evidence_answering::resolve_remote_pyramids_for_gap` exists and uses correct `RemotePyramidClient::remote_search` signature
- [ ] Defensive untyped JSON navigation handles shape drift in `RemoteSearchResponse.results`
- [ ] Local access tier check skips `embargoed` pyramids in `resolve_pyramids_for_gap`
- [ ] Dispatcher loop calls remote resolution in outer async scope (not inside `db_read` closure)
- [ ] `pyramid_add_remote_source` IPC + invoke_handler registration
- [ ] Manual smoke test: create a local vine slug, add a remote source via the IPC, trigger build with a gap, verify remote nodes appear in the resolved evidence (vs Wire-published test pyramid on a sibling Wire Node)
- [ ] Public-only mode: priced/embargoed remote pyramids return errors gracefully (no token-mint attempt)

### 4-paid — Deferred to WS-ONLINE-H

Out of scope for this session. Tracked as separate ticket. When WS-ONLINE-H lands:

1. Find `mint_token` (it will exist in `wire_import.rs` or a new payment module)
2. Replace `client.remote_search(...)` calls in `resolve_remote_pyramids_for_gap` with `client.remote_search_with_cost(...)` + token mint
3. Add `query_type: "vine_evidence"` to the token mint
4. Insert into `pyramid_unredeemed_tokens` with the same query_type
5. **Cross-repo:** the Wire server (`GoodNewsEveryone`) needs to accept `vine_evidence` in its `/redeem_token` query_type allowlist. This is a separate ticket on the GoodNewsEveryone repo: ~~"Wire server: accept vine_evidence in redeem_token query_type allowlist"~~.

---

## Section 3 — Sequencing

```
Phase 2 — recursive ask escalation (LOCAL)
   2.0 run_decomposed_build extra_initial_params parameter
   2.1 Tier3Config new fields
   2.2 Recursion-depth + visited-set threading helpers
   2.3 GapResolution struct + dispatcher refactor
   2.4 Trigger condition + re-resolve via referrer graph
   2.5 escalate_gap_to_source_pyramids function
   2.6 Verify question tree initialization for child builds
   │
Phase 4-local — remote source resolution (NO PAYMENT)
   4.1 pyramid_remote_slug_references table + helpers
   4.2 resolve_remote_pyramids_for_gap (uses public remote_search)
   4.3 Local access tier check (skip embargoed)
   4.4 Dispatcher async-scope integration
   4.5 pyramid_add_remote_source IPC
   │
──── Phase 4-paid deferred to WS-ONLINE-H ────
──── done; smoke test + hand off ────
```

Phase 2 ships first. Phase 4-local ships after if time permits. Both go through cargo check at every phase boundary.

## Risks (revised)

1. **CRIT-5 fix correctness:** the referrer-graph re-resolve depends on every escalation child slug containing `--ask-` AND being question-typed. If any other code path creates question pyramids referencing source slugs with `--ask-` in their names, false positives leak in. Mitigation: use a dedicated `reference_type = 'vine-escalation'` value on `pyramid_slug_references` and filter on that, instead of the substring check. **Recommended for v2 implementation.**
2. **Question tree initialization for child builds:** Section 2.6 flagged this as needing live verification. The first child build will reveal whether `run_decomposed_build` requires a pre-existing tree row. If it does, escalation must call `db::save_question_tree(conn, &child_slug, &empty_tree_json)` before the recursive build.
3. **Lock-acquisition deadlock:** the writer guard for slug creation is in its own `{ }` scope that drops before the recursive call (MAJ-2 fix). Test by running escalation under heavy concurrent load.
4. **Parallel child build LLM rate limits:** `vine_max_parallel_escalations` defaults to 3. Higher values fan out faster but may hit OpenRouter rate limits.
5. **Remote query latency in Phase 4:** remote pyramid HTTP calls have no timeout in the current `RemotePyramidClient::remote_search`. Verify by reading the function. If no timeout, add a default 30s.
6. **Embargoed-tier semantics:** the local check skips embargoed pyramids in Stage 1 evidence resolution. But escalation (Phase 2) currently doesn't check tiers — should it? If a vine references an embargoed source pyramid, escalation will spawn a child build that queries it. Recommended: add the same `tier == "embargoed"` check at the top of `escalate_gap_to_source_pyramids`.

## Done criteria (overall)

- [ ] All Phase 2 done criteria met
- [ ] All Phase 4-local done criteria met
- [ ] `cargo build` clean
- [ ] `cargo test --lib` baseline parity (same 7 pre-existing failures, no new failures)
- [ ] Manual smoke test 1: vine pyramid → escalation → child build → re-resolve picks up new evidence
- [ ] Manual smoke test 2: vine pyramid with remote source → remote nodes appear in resolved evidence
- [ ] Manual smoke test 3: `vine_max_recursion_depth = 0` blocks escalation cleanly
- [ ] Manual smoke test 4: A→B→A cycle blocked by visited set
- [ ] Manual smoke test 5: embargoed source pyramid skipped with logged warning
- [ ] Phase 4-paid deferred ticket filed (or GitHub issue created on this repo + GoodNewsEveryone)
