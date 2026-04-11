# Workstream: Phase 18b — Cache Integrity Retrofit

## Who you are

You are an implementer joining a coordinated fix-pass across the pyramid-folders/model-routing/observability initiative. The original 17 phases shipped to main. Phase 18 reclaims 9 dropped cross-phase handoffs. You are implementing workstream **18b**, claiming ledger entries **L7 and L8** from `docs/plans/deferral-ledger.md`.

Three other Phase 18 workstreams (18a/18c/18d) run in parallel on their own branches. Do not touch files outside your scope. Your commits land on branch `phase-18b-cache-integrity`.

## Context

Phase 12 (evidence triage + cache retrofit sweep) intentionally skipped the `call_model_audited` path because the audited variant writes its own audit row and historically bypassed the cache-aware `..._and_ctx` variant. Phase 12's wanderer flagged this explicitly in the friction log and the Phase 12 workstream prompt called it out as deferred to Phase 13+. Phase 13 focused on build viz expansion and did not pick it up. Phases 14 and 15 also did not. Result: every audited LLM call burns real tokens on every re-run, including retries, force-fresh passes, and resumes. This is real money leaking per build.

Separately, Phase 12 punted `search_hit` demand signal recording because Wire Node had no session/referer mechanism to link a search result click to a subsequent drill. Phase 13 was supposed to add that linkage during build viz event work. It didn't. Result: `user_drill` signals fire on direct drills but `search_hit` never does — one-third of the spec's demand-signal vocabulary is dead code.

## Ledger entries you claim

| L# | Item | Source |
|---|---|---|
| **L7** | **`search_hit` demand signal recording path** — link a search result click to a subsequent drill and record the demand signal with `signal_type = "search_hit"` instead of `user_drill`. | `docs/specs/evidence-triage-and-dadbear.md` Part 2 line 234; `docs/plans/phase-12-workstream-prompt.md` line 107 (explicit deferral); Phase 12 friction-log notes |
| **L8** | **`call_model_audited` cache retrofit** — thread the Phase 6 `StepContext` through `call_model_audited` sites so audited LLM calls hit the `pyramid_step_cache` on repeat. 4 sites in `evidence_answering.rs` + 1 in `chain_dispatch.rs`. | `docs/specs/llm-output-cache.md` "Threading the Cache Context"; `docs/plans/phase-12-workstream-prompt.md` line 421 (explicit deferral); Phase 12 friction-log notes |

## Required reading (in order)

1. `docs/plans/phase-18-plan.md` — Phase 18 structure; skim.
2. `docs/plans/deferral-ledger.md` — entries L7 and L8 in full.
3. **`docs/plans/pyramid-folders-model-routing-friction-log.md`** — search for "call_model_audited" and "search_hit". Phase 12's wanderer wrote a detailed analysis of both deferrals with exact file:line targets. This is your starting map.
4. `docs/plans/phase-12-workstream-prompt.md` lines 80-120 (search_hit framing) + line 421 (audited retrofit deferral) — the exact phrasing I used when deferring, so you know what was intended.
5. **`docs/specs/llm-output-cache.md`** — re-read "StepContext" / "Threading the Cache Context" sections. Phase 6 primitive you are extending reach of.
6. **`docs/specs/evidence-triage-and-dadbear.md` Part 2** (lines ~118-438) — re-read the "Demand Signal Tracking" section (~line 212) and the "Recording Points" table. `search_hit` is one of three signal types spec'd but currently undelivered.

### Code reading

7. **`src-tauri/src/pyramid/step_context.rs`** — full read. Understand `StepContext`, `cache_is_usable()`, `with_prompt_hash`, `with_model_resolution`. Phase 12 wired this everywhere except the audited path.
8. **`src-tauri/src/pyramid/llm.rs`** — grep for `call_model_audited`. You'll find the legacy function, its signature, and how it differs from `call_model_unified_with_options_and_ctx`. Understand what "audit row" means here — likely a Phase 11 theatre audit record written alongside the LLM call.
9. **`src-tauri/src/pyramid/evidence_answering.rs`** — grep for `call_model_audited`. Expect 4 hits. Each hit is a call site that needs StepContext threaded through. Note which ones are inside the `if let Some(ctx) = audit` audit branch (those are the ones you retrofit; the non-audit branch was already retrofitted in Phase 12).
10. **`src-tauri/src/pyramid/chain_dispatch.rs`** — grep for `call_model_audited`. Expect 1 hit in `dispatch_llm` (Phase 10's v2 legacy path) and/or 1 in `dispatch_ir_llm` (Phase 6 fix pass already retrofitted the non-audited branch). Phase 6 fix pass left an explicit comment at ~line 1159 about the audited arm being deferred — read it.
11. **`src-tauri/src/pyramid/routes.rs`** — grep for `handle_search`, `handle_drill`, and `log_query_usage`. L7 work lives here. You need to correlate "user dropped in a search result" with "user subsequently drilled on one of those result nodes" via either (a) the `Referer` HTTP header, (b) a short-lived session cookie, or (c) a `from_search=true` query param the frontend sets when the drill is launched from a search hit.
12. `src-tauri/src/pyramid/demand_signal.rs` — existing `record_demand_signal` + `insert_demand_signal` helpers. L7 fires `record_demand_signal` with `signal_type = "search_hit"`. Propagation logic is already there — you're just adding the call site.
13. **`src-tauri/src/pyramid/db.rs`** — grep for `pyramid_llm_audit`. Understand how the audit row is written today and whether Phase 6's cache retrofit pattern is compatible with it.
14. `src-tauri/src/pyramid/webbing.rs`, `faq.rs`, `meta.rs`, `delta.rs` — quick scan for any stray `call_model_audited` call sites I missed in the "4+1" estimate. Phase 12's grep may be outdated. Trust your own grep, not the friction-log number.

## What to build

### 1. L8: `call_model_audited` cache retrofit

**The core challenge:** `call_model_audited` exists because the audited path writes a row to `pyramid_llm_audit` alongside the LLM response. Phase 6's cache-aware `call_model_unified_with_options_and_ctx` doesn't write that audit row. So today's code structure is:

- Audit enabled: `call_model_audited` → writes audit row + bypasses cache
- Audit disabled: `call_model_unified_with_options_and_ctx` → uses cache

The fix is to unify these. Three options in order of architectural cleanliness:

**Option A (clean, moderate effort):** Extend `call_model_unified_with_options_and_ctx` to accept an optional `audit_context: Option<&AuditContext>`. When `Some`, it writes the audit row after a fresh LLM call OR after serving from cache (cache hits still need an audit row — they represent a decision point, and the audit trail should show "served from cache" as distinct from "served by HTTP call to model X"). Retire `call_model_audited` as a separate function — make it a thin wrapper that constructs the audit_context and calls the unified function.

**Option B (conservative, smaller diff):** Add `call_model_audited_with_ctx` that accepts both `audit: &AuditContext` and `ctx: Option<&StepContext>`. It's a copy of `call_model_audited` with a cache probe at the top (if `ctx` is cache-usable, return early with the cached result + write an audit row stamped as `cache_hit = true`). Retrofit sites call this new variant; the old `call_model_audited` stays for non-cache call sites.

**Option C (ugliest, smallest diff):** At each retrofit site, do the cache probe manually before calling `call_model_audited`. Pros: most localized. Cons: duplicates cache probe logic across 5 sites, hard to evolve.

**Recommendation: Option A.** It matches how Phase 12 unified the non-audited paths. The cache hit → audit row linkage is important: the audit trail is the thing the DADBEAR Oversight page and cost reconciliation depend on, and a cache hit without an audit row would be a gap.

If you pick A: add a new field to `pyramid_llm_audit` (or the existing `source` / `cache_hit` column if it exists) that distinguishes "wire call" vs "cache hit" vs "cache hit verify-failed (miss)". Already-hit sites from Phase 6/11/12 will show up as `cache_hit = false` (regular wire calls) which is correct for non-audited paths; retrofit this phase for the audited paths.

### Retrofit call sites

For each of the following sites, thread `StepContext` through the caller chain so `call_model_audited_with_ctx` (or the unified function, per your choice above) can be called with a valid cache-usable context:

- `evidence_answering.rs`: 4 audited sites (grep for `call_model_audited`). Each is inside an `if let Some(ctx) = audit { ... }` branch — the non-audit else-branch was already retrofitted in Phase 12. Use the same `LlmConfig::cache_access`-derived StepContext constructor Phase 12 introduced (`make_step_ctx_from_llm_config` or equivalent). `step_name` per the friction log: `"evidence_pre_map"` / `"evidence_answer"` / `"evidence_triage"` / `"evidence_synthesis"` depending on which audit site.
- `chain_dispatch.rs`: the audited arm in `dispatch_ir_llm` (and possibly `dispatch_llm` for v2 chains). Phase 6 fix pass left a TODO comment there. Thread ctx through — the non-audited arm already constructs ctx via `build_cache_ctx_for_ir_step`; the audited arm can reuse the same helper.
- Any other `call_model_audited` sites your grep finds. Do NOT trust the "4+1" friction-log count.

### Acceptance for L8

After retrofit, these greps must flip decisively:

- `grep -c "call_model_audited(" src-tauri/src/pyramid/*.rs | grep -v test` — should be near zero in production code paths (only the function definition + any legacy non-retrofit sites documented as intentionally-bypassed)
- `grep -c "call_model_audited_with_ctx\|call_model_unified_with_options_and_ctx.*audit" src-tauri/src/pyramid/*.rs` — should show the retrofit call sites

### 2. L7: `search_hit` demand signal recording path

Currently, `routes.rs::handle_drill` records a `user_drill` demand signal fire-and-forget. `handle_search` doesn't record anything — search is considered intermediate, and only a drill on a searched node should count as `search_hit`. The spec wants us to distinguish a direct URL-drill from a search-then-drill.

**Implementation approach** — pick one and document in the log:

**Approach A (session-scoped correlation, proper):** Add a tiny in-memory session store keyed by client IP (or auth token's session_id if available): `Arc<Mutex<HashMap<String, SearchContext>>>` where `SearchContext` has `{ node_ids_returned: HashSet<String>, expires_at: Instant }`. When `handle_search` fires, insert/update the session's search context with the hit node IDs and a 5-minute expiry. When `handle_drill` fires, check the session store — if the drilled node_id is in the recent search hits for this session, record `signal_type = "search_hit"` (instead of OR in addition to `user_drill`). Expire entries older than 5 minutes on each insert.

**Approach B (frontend-cooperative, simplest):** Add a `?from=search` query param to the drill endpoint. Update the frontend search result → drill navigation to append `?from=search` to the drill URL. `handle_drill` checks the query param and if present records `signal_type = "search_hit"` on top of `user_drill`.

**Approach C (referer-based, fragile):** Inspect the `Referer` header on the drill request. If it contains `/search` or a search results URL, classify as `search_hit`. Fragile because referer can be stripped by privacy settings.

**Recommendation: Approach B.** Requires a tiny frontend change (one URL edit in whatever component renders search results → drill), is explicit, doesn't fight the browser, and survives privacy-preserving referer policies. Approach A is more correct architecturally but adds a mutex + session registry for a demand-signal heuristic that propagates 50%-attenuated anyway.

If you pick B:
- `handle_drill` in `routes.rs`: accept an optional `from` query param. If `from == "search"`, record `record_demand_signal(conn, slug, node_id, "search_hit", ...)` in addition to the existing `user_drill` recording.
- Frontend: find the search results list in `src/components/` (probably somewhere in `QueryPanel.tsx` or a search results component). When the user clicks a result to drill, append `?from=search` to the drill URL the frontend navigates to.
- Both signal types for the same click is fine per the spec — the policy's `demand_signals` threshold can distinguish `search_hit` vs `user_drill` weight if the user wants.

If you pick A, document the session store's lifetime and cleanup strategy in the log.

### 3. Tests

- **Cache retrofit tests:** per-retrofit-site smoke test. For each newly-retrofitted call site, add a test that (a) populates the cache with a known row for a known (inputs_hash, prompt_hash, model_id), (b) calls the function, (c) asserts the cached result was returned. Follow Phase 12's test pattern for retrofit verification.
- **Audited cache hit path:** a new test that calls the unified function with an audit_context AND a cache-usable step_context, pre-populates the cache, asserts both "cache hit returned" AND "audit row written with cache_hit = true".
- **search_hit path:** a test that posts a drill with `from=search` (or simulates the session store hit for Approach A) and asserts two rows land in `pyramid_demand_signals` (one `user_drill`, one `search_hit`).
- **search_hit propagation:** since propagation is already covered by Phase 12's demand_signal tests, you just need to spot-check that the new signal type propagates identically. One test is enough.

## Scope boundaries

**In scope:**
- `call_model_audited` retrofit in `evidence_answering.rs` (4 sites) and `chain_dispatch.rs` (1-2 sites)
- Unified function signature change OR new `_with_ctx` variant (Option A vs B)
- Audit row `cache_hit` distinction
- `search_hit` demand signal recording path (backend + minimal frontend change)
- Rust tests for both L7 and L8
- Implementation log entry

**Out of scope (other Phase 18 workstreams):**
- Local mode toggle — 18a
- Credential warnings UI — 18a
- `/api/tags` resolver — 18a
- Cache-publish privacy opt-in — 18c
- Pause-all folder/circle scopes — 18c
- Schema migration UI — 18d
- CC memory subfolder ingestion — 18e

**Out of scope permanently:**
- Retrofitting `call_model_direct` (diagnostic path, intentionally bypassed)
- Retrofitting `call_model_unified` in `public_html/routes_ask.rs` (free-form ask, no step context exists)
- Retrofitting the semantic search LLM call in `routes.rs::handle_search` itself (it's the search, not a build step, no cache context makes sense)
- Building a search→drill correlation across multiple HTTP sessions (5-minute expiry is sufficient)

## Verification criteria

1. **Rust clean:** `cargo check --lib` — 3 pre-existing warnings allowed, zero new.
2. **Test count:** `cargo test --lib pyramid` at prior count + new Phase 18b tests.
3. **Retrofit ratio:** document before/after `grep -c "call_model_audited(" src-tauri/src/pyramid/*.rs` counts in the log. After should be ≤ 1 (function definition only) or 0 if you went with Option A and retired it entirely.
4. **Both paths exercised:** log the manual verification: run a build twice, observe that the second run's audited steps cache-hit (faster + cheaper). For L7, manually perform a search → drill flow in the UI and verify via a SQLite dump of `pyramid_demand_signals` that both signal types landed.
5. **No production regressions:** existing Phase 12 tests for cache correctness still pass.

## Deviation protocol

- **Option A vs B for the unified signature:** pick one, document rationale.
- **Approach A vs B vs C for search_hit correlation:** pick one, document.
- **Audit row schema:** if adding `cache_hit` column requires a migration, make it idempotent via `pragma_table_info` check. If the schema already has an equivalent column (check first — grep `pyramid_llm_audit` table definition), reuse it.
- **Retrofit-finds-more-sites:** if your grep turns up audited call sites outside `evidence_answering.rs` and `chain_dispatch.rs`, retrofit them too. Document the full list.

## Mandate

- **Real token savings.** L8 is the retrofit that makes audited paths benefit from the cache. Before your fix, an audited build that rebuilds the same content burns tokens every time. After your fix, it doesn't. This is money.
- **`feedback_always_scope_frontend.md`:** L7 has a tiny frontend touch (appending `?from=search` if you pick Approach B). Don't skip it. Adam tests by feel — if the search→drill flow doesn't actually record `search_hit`, L7 ships incomplete.
- **Fix bugs found in the sweep.** Standard.
- **Retrofit ratio grep is a hard gate.** Don't let a non-retrofit site hide because it's inside a nested `if let Some(audit)` branch. Grep every site, document its status.

## Commit format

Single commit on `phase-18b-cache-integrity`:

```
phase-18b: call_model_audited cache retrofit + search_hit signal path

<5-8 line body summarizing:
- Audited call sites retrofitted (count, files)
- Unified vs _with_ctx variant choice + rationale
- Audit row cache_hit distinction
- search_hit correlation approach (A/B/C) + rationale
- Claims L7 and L8 from deferral-ledger.md>
```

Do not amend. Do not push. Do not merge.

## Implementation log

Append Phase 18b entry to `docs/plans/pyramid-folders-model-routing-implementation-log.md`:
1. Retrofit table (file:line → before/after status)
2. Before/after `grep -c` counts for `call_model_audited(` and `call_model_audited_with_ctx` (or equivalent)
3. Unified vs new-variant choice + rationale
4. `search_hit` correlation approach + rationale
5. Tests added + counts
6. Manual verification steps
7. Status: `awaiting-verification`

## End state

Phase 18b is complete when:
1. All audited call sites in production code paths route through a cache-aware variant
2. Audited cache hits write an audit row stamped as such
3. `search_hit` demand signals fire on search→drill flows
4. `cargo check --lib` + `cargo test --lib pyramid` + `npm run build` clean
5. Single commit on branch `phase-18b-cache-integrity`

Begin with the Phase 12 friction log — it's your map. Then trace each audited call site. Then pick Option A/B/C and commit to it before you start typing. Then retrofit. Then wire search_hit. Then tests.

Good luck.
