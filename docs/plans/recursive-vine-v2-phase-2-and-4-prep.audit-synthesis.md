# Phase 2 + 4 Prep Doc — Audit Synthesis

> **Date:** 2026-04-07
> **Stage 1 informed audit:** auditors K, L
> **Stage 2 discovery audit:** auditors M, N
> **Reports:** `/tmp/phase-2-4-audit-K.md`, `L.md`, `M.md`, `N.md`
>
> All four auditors converged strongly. Prep doc has 5 critical compile-blocking issues, 8 major design issues, and one architecture flaw that breaks the entire Phase 2 value loop. Phase 4 has a hard prerequisite (Wire-side payment infra) that doesn't exist yet.

---

## Critical findings (3-4 auditors agree)

### CRIT-1 — `RemotePyramidClient::remote_search` signature mismatch (K5/K6, L-08, M2/M3, N1)

**Plan claim:** `client.remote_search(slug, gap, max_nodes).await` returning typed `Vec<{node_id, headline, snippet}>`.

**Reality:** `wire_import.rs:775` defines `remote_search(slug: &str, query: &str) -> Result<RemoteSearchResponse>` where `RemoteSearchResponse.results: Vec<serde_json::Value>` — untyped JSON values.

**Fix:** Phase 4.2 must:
- Drop `max_nodes` from the call (or extend `RemotePyramidClient::remote_search` to accept it as a query param if the remote endpoint supports it)
- Define a `RemoteSearchHit` struct that mirrors what the remote `/search` endpoint actually returns, OR navigate the `serde_json::Value` results manually with `.get("node_id").and_then(|v| v.as_str())`
- Ideally: extend the `RemoteSearchResponse` type with strongly-typed results, since this is the same shape used by `resolve_remote_web_edges`

### CRIT-2 — `mint_token` does not exist (K7, L-09, M8, N2)

**Plan claim:** `wire_client.mint_token(slug, "vine_evidence", stamp_amount, access_amount).await` mints a payment token.

**Reality:** No `mint_token` function exists anywhere in `src-tauri/src/pyramid/`. `wire_import.rs:1043+` has `remote_*_with_cost` methods but they're all `TODO(WS-ONLINE-H): Integrate payment-intent/token flow when Wire server ready`. The Wire-side payment integration **is not built yet** — it's an open WS-ONLINE-H workstream.

**Fix:** Phase 4.4 (credit flow integration) **cannot ship until WS-ONLINE-H lands**. Two options:
- **(a)** Block Phase 4 until WS-ONLINE-H lands. Document the dependency clearly.
- **(b)** Ship Phase 4 with a "free queries only" mode that skips paid pyramids entirely. Vines can query `'public'` access tier sources but error out cleanly on `'priced'`/`'embargoed'`. When WS-ONLINE-H lands, swap the error for a token mint.

**Recommendation:** option (b). It's the maximal path that ships value today and trivially upgrades when payment lands. Document the gate clearly so the user knows vines can't query priced pyramids until WS-ONLINE-H.

### CRIT-3 — Variables out of scope at the proposed escalation site (K2, L-01, M1, N3)

**Plan claim (Phase 2.3):** the `can_escalate` block reads `pyramid_slugs`, `pyramid_count`, `combined`, `evidence_count` at the dispatcher level.

**Reality:** these variables are defined inside the `db_read(...)` blocking closure at `chain_executor.rs:5312-5351`. Outside the closure, only `resolved_files` exists (the merged result). The plan's snippet references variables that don't exist at the call site.

**Fix:** Phase 2.3 must either:
- Expand the closure to also compute the partition + escalation decision and return them as part of the closure's return tuple, OR
- Change the closure return shape to also include `pyramid_slugs` and `pyramid_count` so the outer scope can use them, then call the async escalation function from the outer scope where `state` is available

**Cleanest:** add a small dispatcher struct returned from the closure: `struct GapResolution { combined: Vec<(String, String, String)>, pyramid_slugs: Vec<String>, file_slugs: Vec<String> }`. The outer scope unpacks it.

### CRIT-4 — `recursion_depth` cannot be threaded via `ChainContext.initial_params` (K4, L-04, M4, N5)

**Plan claim (Phase 2.5):** "Add `recursion_depth: u32` to the `ChainContext` initial_params (which is already passed through `run_decomposed_build` → `execute_chain_from`)."

**Reality:** `run_decomposed_build` at `build_runner.rs:776` (NOT 702 — line numbers stale) builds `initial_context` from a HARDCODED list of fields (`apex_question`, `granularity`, `max_depth`, `from_depth`, `content_type`, `audience`, `characterize`, `is_cross_slug`, `referenced_slugs`, `build_id`). It has **no parameter** for caller-injected extra params. The child build always sees `recursion_depth = 0` — **infinite recursion guaranteed**.

**Fix:** add a new parameter to `run_decomposed_build`:
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
) -> Result<(String, i32, Vec<StepActivity>)>
```

The function merges `extra_initial_params` into `initial_context` after building the standard fields. All existing call sites pass `None`. The Phase 2 escalation passes `Some(map_with_recursion_depth)`.

OR thread `recursion_depth` as a top-level parameter (cleaner type, but more existing call sites to update).

### CRIT-5 — Phase 2 value loop is broken: child question pyramid nodes are tagged with the CHILD slug, not the source slug (L-18)

**Plan claim:** "After escalation, evidence is re-resolved from the enriched sources." The plan implies that spawning a child question pyramid enriches the source pyramid with new nodes.

**Reality:** when `run_decomposed_build` builds the child slug `{source}--ask-{hash}`, all the new nodes are tagged with `slug = "{source}--ask-{hash}"`, NOT `slug = "{source}"`. The `resolve_pyramids_for_gap` re-resolve walks `[source_slug]` only, so it sees the same nodes it saw before. The child's new evidence is invisible.

**Note:** auditor K verified that the *file-based* `targeted_reexamination` path (the existing Phase 1 → file path) DOES save back to the source slug at `chain_executor.rs:5454`. But that's a different code path. Phase 2's child question pyramid doesn't use `targeted_reexamination`; it uses `run_decomposed_build` which writes to the child slug.

**Fix:** two options:

- **(a) Walk the referrer graph in re-resolve.** After escalation, the parent's resolve_pyramids_for_gap is called with `[source_slug]` AS WELL AS every question pyramid that references the source slug. `db::get_slug_referrers(source_slug)` returns this set. Filter to question content_type pyramids that were created by escalation (use a marker on the slug name OR a new column on `pyramid_slug_references` like `reference_type = 'vine-escalation'`).

- **(b) Save child build's nodes to the source slug.** Modify `run_decomposed_build` to accept a `target_persist_slug` parameter so child builds can persist their nodes under the source slug's namespace. This is invasive — touches every node-write path.

**Recommendation:** option (a). Cleaner separation; doesn't change `run_decomposed_build`'s persistence model; uses the existing reference graph. The re-resolve becomes "walk source pyramid AND any escalation-child pyramids that reference it."

Implementation:
```rust
// After escalation completes:
let mut all_evidence_slugs = pyramid_source_slugs.clone();
for source in pyramid_source_slugs {
    let referrers = db::get_slug_referrers(conn, source)?;
    for referrer in referrers {
        // Only walk question pyramids created by vine escalation
        if let Ok(Some(info)) = db::get_slug(conn, &referrer) {
            if info.content_type.as_str() == "question"
                && referrer.contains("--ask-")
            {
                all_evidence_slugs.push(referrer);
            }
        }
    }
}
let re_resolved = resolve_pyramids_for_gap(conn, &all_evidence_slugs, gap_description, max_nodes)?;
```

This is the most important finding in the audit. **Without this fix, Phase 2 doesn't actually do anything useful** — the recursive ask completes but the parent never sees the new evidence.

---

## Major findings

### MAJ-1 — Async/sync mismatch in Phase 4 dispatcher integration (K20, L-14, M6)

The existing dispatcher at `chain_executor.rs:5307+` resolves evidence inside a `db_read(...)` blocking closure. Phase 4's `resolve_remote_pyramids_for_gap` is async (HTTP calls). You can't `.await` inside a blocking closure.

**Fix:** the Phase 4 remote resolution must happen OUTSIDE the `db_read` closure. Restructure the dispatcher:
1. Closure does the file + local pyramid resolution (blocking).
2. Closure also returns the list of remote_refs to fetch.
3. Outer async scope walks the remote_refs and calls `resolve_remote_pyramids_for_gap` async.
4. Outer scope merges all three results before calling `targeted_reexamination`.

### MAJ-2 — Lock acquisition deadlock risk (L-03)

The escalation function holds `state.writer.lock()` to create the child slug, then calls `run_decomposed_build` which itself locks `state.reader` and `state.writer`. If the writer guard isn't dropped before the recursive call, deadlock.

**Fix:** explicit `drop(conn)` or scope `{ }` around the slug-creation block before the recursive `run_decomposed_build` call.

### MAJ-3 — Cycle detection: depth bound is insufficient (L-02)

Depth bound of 2 still allows a vine A → vine B → vine A escalation cycle within the bound. The cycle wastes work but completes without infinite loop. A visited set per top-level build is cheap insurance.

**Fix:** thread a `HashSet<String>` of visited slugs through the escalation chain alongside `recursion_depth`. Skip slugs already in the set.

### MAJ-4 — Child question pyramid needs question tree initialization (L-05)

`run_decomposed_build` at `build_runner.rs:776+` reads `db::get_question_tree(conn, slug_name)` and errors if not present (`build_runner.rs:241+` shows the pattern: "Question slug '{slug}' has no stored question tree"). The escalation creates the child slug via `db::create_slug` but doesn't populate a question tree, so subsequent re-builds of the child fail.

**Fix:** the escalation function must call `db::save_question_tree(conn, child_slug, &tree_json)` with a synthesized minimal tree, OR call `run_decomposed_build` with a path that auto-creates the tree on first build. Verify by reading lines 241-262 of build_runner.rs (the conversation dispatch shows how question trees are auto-created on first build).

### MAJ-5 — `is_querier_allowed` references nonexistent `querier_op_id` (L-11, M9, N)

The Phase 4.3 access tier check uses `querier_op_id` as a parameter to `is_querier_allowed`. There is no such variable in the local dispatcher scope. Locally, the querier IS the operator running the build — there's no remote requester.

**Fix:** drop the `querier_op_id` parameter for local-only resolution. Restrict the local check to "skip embargoed pyramids" (which the operator's own builds shouldn't query). For circle-scoped, the operator is always in their own circles. For priced, the operator pays themselves nothing (no-op locally; only matters for cross-operator queries which is Phase 4 remote).

### MAJ-6 — `pyramid_unredeemed_tokens` populator never identified (K7)

The plan says vine queries should insert into `pyramid_unredeemed_tokens` but never identifies WHERE the existing populator is. Auditors searched and found the rows are written via the existing payment paths in wire_publish.rs / wire_import.rs, but those paths are also `TODO(WS-ONLINE-H)`.

**Fix:** linked to CRIT-2. Until WS-ONLINE-H lands, vine queries can't insert tokens because the minting infrastructure doesn't exist. Defer Phase 4.4 entirely until WS-ONLINE-H ships.

### MAJ-7 — Sequential per-gap × per-source build fan-out (K15, L-13)

Phase 2's escalation runs sequentially: for each gap, for each source pyramid, spawn a child build, wait, re-resolve. With N gaps and M sources, N×M sequential builds. Each child build is itself a multi-LLM-call operation. Total latency could 10-100x existing build times.

**Fix:** parallelize the source-pyramid escalation per gap. `tokio::task::JoinSet` of child builds, await all, then re-resolve once. Bound by `state.operational.tier3.vine_max_parallel_escalations` (default 3).

### MAJ-8 — Re-resolve loop is `// ...` placeholder (L-20)

The plan's Phase 2.6 just has `// ...` for the re-resolve code. The actual logic (walk all_evidence_slugs, call resolve_pyramids_for_gap, merge, check if threshold now met) was never written.

**Fix:** write the re-resolve loop explicitly in v2 of the prep doc.

---

## Minor findings

- **MIN-1** Stale line numbers throughout (K11, M11, N): `run_decomposed_build` at 776 not 702; `pyramid_unredeemed_tokens` at 1136 not 976; etc. v2 prep doc must use correct line numbers.
- **MIN-2** `pyramid_remote_slug_references` UNIQUE constraint excludes `remote_tunnel_url` (M12). Doesn't matter unless multiple operators publish at the same handle path.
- **MIN-3** Slug name 128-char limit (K12, L-06, N4): `{source}--ask-{8hex}` could exceed 128 if source is already long. Mitigate with length cap or shorter hash prefix.
- **MIN-4** `slugify` collapses `--` to `-` (M5, N4): existence check uses raw `{source}--ask-{hash}` but `db::create_slug` bypasses slug validation while `slug::create_slug` would normalize. Race-prone if both paths used. Pin to one — `db::create_slug` (raw) since the name doesn't need slugify normalization.
- **MIN-5** `Tier3Config` location verified at `mod.rs:414` not `tiers.rs` (K confirmed).
- **MIN-6** `db::create_slug` bypasses `slug::validate_slug` validation (K12, M5, N6). For escalation-child slugs we want the bypass (length cap is the only concern); for user-facing creation we want validation. Use `db::create_slug` directly in escalation.
- **MIN-7** `wire_jwt` source (`config.auth_token`) is the local desktop API token, NOT a Wire-issued JWT (N also). Existing `build_runner.rs:486` has the same misuse. Track as separate auth-cleanup ticket.
- **MIN-8** `HandlePath::parse` requires 3 segments (`slug/depth/node_id`); a remote vine source ref is just a slug or `slug/apex` (K8). Need a `parse_remote_slug_ref` helper that accepts shorter shapes.
- **MIN-9** Race when multiple gaps in one parent build escalate to the same source pyramid simultaneously (N missing-piece): simultaneous child builds for different gaps but same source could collide on the slug name or duplicate work. Mitigate by deduping by source slug per parent-build-id.
- **MIN-10** Progress streaming for recursive builds (N missing-piece): the user has no visibility into "vine is escalating to source X." Add a `LayerEvent::VineEscalation { source_slug, depth }` event the UI can render.
- **MIN-11** Rate limiting on remote queries for Phase 4 (L-13, N): add a per-remote-tunnel-URL rate limiter, mirroring the existing `absorption_gate` pattern in `build_runner.rs:80+`.

---

## Verified-true claims (the parts that survived)

These are the parts of the prep doc that auditors confirmed:

- `Tier3Config` exists at `mod.rs:414` with `#[serde(default)]` — adding 2 new fields is backward-compatible.
- `db::create_slug`, `db::save_slug_references`, `db::get_access_tier` all exist.
- Gap-produced nodes ARE saved to the source `base_slug` for the file path (`chain_executor.rs:5454`) — but only for the file resolution path, not for child-question-pyramid escalation (CRIT-5).
- `RemotePyramidClient::remote_search` and `remote_drill` exist as methods (with the wrong signatures vs the plan, see CRIT-1).
- `pyramid_remote_web_edges` and `pyramid_unredeemed_tokens` tables exist (line numbers stale).
- `slug::validate_slug` accepts `--` (only enforces `[a-z0-9-]` charset, no consecutive-hyphen check).
- `HandlePath::parse` exists at `types.rs:613`.
- `db::get_access_tier` exists at `db.rs:5333`.
- `ChainContext.initial_params` exists at `chain_resolve.rs:61` — but `run_decomposed_build` doesn't accept caller-injected initial params (CRIT-4).

---

## Phase-by-phase verdict

| Phase | Status | Verdict |
|---|---|---|
| **2.1** Tier3Config fields | OK | Verified location, additive |
| **2.2** ChainContext recursion depth | **BROKEN** | CRIT-4 — needs new parameter on `run_decomposed_build` |
| **2.3** Trigger condition in dispatcher | **BROKEN** | CRIT-3 — variables out of scope |
| **2.4** escalate_gap_to_source_pyramids | **BROKEN** | CRIT-5 + MAJ-2 + MAJ-4 — value loop architecture wrong, deadlock risk, missing question tree init |
| **2.5** Recursion threading | **BROKEN** | CRIT-4 — same as 2.2 |
| **2.6** Re-resolve after escalation | **BROKEN** | CRIT-5 + MAJ-8 — placeholder code, wrong evidence-slug walk |
| **4.1** pyramid_remote_slug_references table | OK | Schema sound, line numbers stale |
| **4.2** resolve_remote_pyramids_for_gap | **BROKEN** | CRIT-1 — wrong signature, untyped results |
| **4.3** Access tier enforcement | Partial | MAJ-5 — drop the querier_op_id parameter; restrict to "skip embargoed locally" |
| **4.4** Credit flow | **BLOCKED** | CRIT-2 — depends on WS-ONLINE-H, which is unbuilt |
| **4.5** Dispatcher integration | **BROKEN** | MAJ-1 — async/sync mismatch |
| **4.6** IPC for adding remote sources | OK | Not yet verified but signature looks reasonable |

---

## Recommendation

Phase 2 and Phase 4 are achievable but the prep doc needs a v2 with these corrections baked in. Specifically:

1. **Fix `run_decomposed_build` signature** to accept `extra_initial_params: Option<HashMap<String, serde_json::Value>>` so recursion_depth can be threaded. This is the smallest possible signature change that makes Phase 2 viable.
2. **Fix the escalation re-resolve to walk the referrer graph** (option (a) in CRIT-5). Don't try to make the child build write to the source slug.
3. **Fix the dispatcher partition** to return the partition results from the closure, then do remote resolution in the outer async scope (CRIT-3 + MAJ-1).
4. **Add visited-set cycle detection** alongside depth bound (MAJ-3).
5. **Defer Phase 4.4 (credit flow) until WS-ONLINE-H lands.** Ship Phase 4 with public-only mode (free queries, error on priced/embargoed). Document the upgrade path.
6. **Fix the `remote_search` integration** to use the actual signature + handle untyped JSON results (CRIT-1).
7. **Drop `is_querier_allowed`** for local checks; the local self-resolution case is a no-op for tier enforcement (MAJ-5).
8. **Parallelize child builds** within a gap (MAJ-7).
9. **Refresh all line numbers** to match current source.

Estimated v2 prep doc size: ~80% rewrite of Phase 2.x sections, ~50% rewrite of Phase 4.x sections, with explicit "Phase 4.4 deferred to WS-ONLINE-H" section. Then implement against v2.

**Most critical insight from this audit:** CRIT-5 (the value loop architecture flaw) is the difference between Phase 2 doing useful work and Phase 2 being a no-op. The prep doc framed escalation as "spawn child build → source enriched → re-resolve picks up new nodes," but child builds don't enrich the source — they create a sibling pyramid that *references* the source. The re-resolve has to walk both the source AND the sibling. This is the architectural correction the v2 prep doc most needs to internalize.
