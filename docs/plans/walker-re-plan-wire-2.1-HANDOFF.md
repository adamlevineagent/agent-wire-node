# Walker Re-Plan Wire 2.1 — Implementation Handoff

**From:** planning thread (2026-04-20 → 2026-04-21)
**To:** implementation thread (fresh agent)
**Plan:** `docs/plans/walker-re-plan-wire-2.1.md` rev 0.3 — READY FOR IMPLEMENTATION
**Status:** Adam GO'd. Stage 1 + Stage 2 audits applied.

---

## TL;DR

Build a node-side LLM-dispatcher walker that collapses three hardcoded phases in `call_model_unified_with_audit_and_ctx` into one unified loop over a routing list. Market branch uses Wire rev 2.1's three-RPC flow (`/quote → /purchase → /fill`). 6 waves, ~2950 LOC, 3-5 sessions. Target: single operator (Adam), wipe-and-fresh-install, no migration preservation. Plan has absorbed 2 audit rounds + 15 concrete fixes; it's the complete spec.

Read the plan file in full before starting. The plan's §2.5 (systemic helpers) and rev 0.3 audit-history list are load-bearing.

---

## Mandatory operational discipline

**You MUST maintain two logs throughout implementation. Both live in `docs/plans/` alongside the plan. Both update after EVERY committed change so the next agent can pick up cold from either one if tokens run out mid-run or overnight.**

### 1. Implementation log — `walker-re-plan-wire-2.1-IMPL-LOG.md`

Chronicle of what's done. Updated after each commit. One entry per commit with:
- Commit SHA + branch
- Which plan task(s) it lands (cite plan section/task #)
- What changed (1-2 sentences)
- Cargo check + test status (green/red/not-yet-run)
- Any deviation from the plan (with rationale)

**Format (append-only, newest at top):**

```
## 2026-04-22 14:32 — commit abc1234 (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 0 task 4 — `LlmConfig::prepare_for_replay` helper
**Changed:** Added `prepare_for_replay(origin)` method to impl LlmConfig at llm.rs:854. Updated 4 call sites: llm.rs:2104, server.rs:2028, server.rs:3958, dadbear_supervisor.rs:571. Net -18 LOC.
**Cargo check:** clean (default target).
**Cargo test:** `cargo test --lib prepare_for_replay` — 3/3 pass (Local / FleetReceived / MarketReceived).
**Deviation:** None.
```

### 2. Friction log — `walker-re-plan-wire-2.1-FRICTION-LOG.md`

Chronicle of what bit you. Updated in real-time when you hit a surprise. This is how the retro gets written without reconstructing memory. One entry per distinct friction event:
- Timestamp
- Where you were (task/file:line)
- What surprised you
- Root cause (if known) OR "still investigating"
- How you worked around it (or blocked waiting for something)
- Flag whether it suggests a plan error, doc staleness, spec ambiguity, Wire-side bug, or just a foreign-codebase learning moment

**Format (newest at top):**

```
## 2026-04-22 15:47 — Wave 1 task 9

**Context:** Moving HTTP retry loop (lines 2350-2830) into pool-provider branch of walker.
**Surprise:** `provider_impl` mutable borrow collides with walker's per-entry iteration because it's hoisted outside the loop. Today's code `let (mut provider_impl, ...) = build_call_provider(config)?;` at line 1220.
**Root cause:** Walker branch needs per-entry provider_impl (model override might change which provider). Can't hoist.
**Workaround:** Move `build_call_provider` inside the pool-provider branch; drop the outer hoist. One extra provider-trait instantiation per entry, but simplifies ownership.
**Flag:** Plan §4.3 "Dispatch" should have called this out; adding to Wave 5 string-match-audit follow-ups.
```

**Both logs stay in git. Both get committed alongside the code changes they describe.** If you run out of tokens mid-task, the NEXT agent reads:
1. Plan doc for canonical spec.
2. Impl log to know where you stopped.
3. Friction log to know what's weird.

This lets anyone resume cold without deep conversation archaeology.

---

## Where to find the plan

**Primary spec:** `docs/plans/walker-re-plan-wire-2.1.md` rev 0.3. Read sections in this order:
1. §1 one-paragraph statement
2. §2 + §2.5 (what changes vs what survives; three systemic helpers)
3. §3 per-entry walker algorithm (the pseudocode)
4. §4.1/4.2/4.3 per-branch semantics (including error-classification tables)
5. §4.4 compute_queue interaction
6. §4.5 concurrency (especially §4.5.1 walkers-storm-market known issue)
7. §5 chronicle event vocabulary
8. §6 MarketSurfaceCache
9. §7 ProviderPools::try_acquire_owned
10. §8 Implementation waves (your build sequence)
11. §9 Wire-dev questions (Q4 + Q5 affect Wave 3)
12. §10 NOT in scope
13. §11 tradeoffs
14. §12 acceptance criteria
15. §13 audit history (read this — the rev 0.3 fix list at the bottom IS load-bearing)
16. §14 backlog
17. §15 supplemental punchlist

**Cross-repo authoritative docs (READ IN FULL before Wave 3):**
- Wire rev 2.1 spec: `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/plans/compute-market-quote-primitive-spec-2026-04-20.md`
- Bilateral contract rev 2.1: `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/architecture/wire-node-compute-market-contract.md`

Node-side rules:
- `docs/SYSTEM.md` — §12 "do-not wall" (catch reinvention traps)
- Wire-side `docs/wire-pillars.md` for Pillar 37 discipline

---

## What the plan does NOT specify (land your judgment carefully)

The plan is comprehensive on STRUCTURE. A few items are deliberately left to implementer judgment:

1. **Exact test names + file locations.** Plan says "unit tests for prepare_for_replay" — you pick the test module, write idiomatic-for-this-codebase tests.
2. **Chronicle event metadata shapes** — plan lists fields; you pick field ordering and serde names.
3. **Wave 4 Settings panel React patterns** — plan says drag-reorder via up/down buttons, debounced save on Apply; you pick which of the existing Settings.tsx patterns to mirror.
4. **`bundled_contributions.json` JSON Schema shape for `dispatch_policy`** — plan says ~60 LOC JSON. Mirror the `evidence_policy` schema_definition structure (head of the bundled_contributions.json file).

For anything AMBIGUOUS beyond these, log it in friction-log, pick the conservative interpretation, flag for retro.

---

## Wire-dev coordination needed

**Two questions open. Both in plan §9.**

- **Q4 (blocks Wave 3):** `/fill` body — are `input_token_count` and `privacy_tier` still valid fields in rev 2.1? Contract §1.8 example omits them; spec §2.3 implies they stay. Walker can't construct the `/fill` body without the answer.

- **Q5 (optimization, non-blocking):** Add `uuid_job_id` to `/purchase` 200 response body. ~2 LOC Wire-side change at `src/app/api/v1/compute/purchase/route.ts:691-696` — include `commit.job_id` in the response `Response.json({...})`. Saves walker one round-trip per dispatch (the `/jobs/:handle-path` poll fallback). Walker ships with fallback if Q5 doesn't land in time.

**Coordination protocol:** Adam relays between threads. Don't try to page Wire dev directly — add questions to friction-log, Adam carries them across. Wire dev has NOT been compacted recently; he has full context for both sides.

**Wave 3 gate:** Wave 3 task 19 (`compute_quote_flow` RPC bodies) cannot complete without Q4 resolved. If Q4 lands mid-wave, proceed. If Q4 is still open when you hit Wave 3 task 19, STOP and escalate to Adam rather than guessing the body shape.

---

## Build waves (from plan §8)

Serial implementer pattern. One focused agent-effort per wave. Audit + verifier at marked gates.

**Wave 0 — prereqs + systemic helpers (~650 LOC, 10 tasks):**
1. Bundle `dispatch_policy` contribution family (4 entries: seed + schema_def + annotation + skill).
2. `sync_dispatch_policy_to_operational` helper in wire_migration.rs.
3. Boot hydration check (no new code — verify main.rs:11824-11887 already reads).
4. `LlmConfig::prepare_for_replay` helper + update 4 call sites.
5. `branch_allowed` + `RouteBranch` + `classify_branch` helpers.
6. `EntryError { Retryable, RouteSkipped, CallTerminal }` enum.
7. `ProviderPools::try_acquire_owned` + `SlidingWindowLimiter::try_acquire`.
8. `compute_quote_flow` module skeleton (types + unimplemented stubs).
9. `MarketSurfaceCache` skeleton.
10. Verifier pass.

**Wave 1 — walker shell + pool-provider branch (~400 LOC, 5 tasks):**
Includes schema migration for `pyramid_llm_audit.provider_id` (task 11 per plan rev 0.3).

**Wave 2 — fleet branch inlined (~400 LOC, 6 tasks).**

**Wave 3 — market branch inlined + compute_quote_flow bodies (~700-1000 LOC, 8 tasks).**
- **Q4 MUST be resolved by this wave.**
- Walker UUID resolution via `compute_quote_flow::resolve_uuid_from_purchase` (Q5-preferred else fallback poll).

**Wave 4 — MarketSurfaceCache polling + Settings panel (~600 LOC, 7 tasks).**

**Wave 5 — cleanup + deprecation enforcement (~200 LOC, 5 tasks):**
- Delete `compute_requester.rs`.
- Remove deprecated `market_dispatch_eager` / `market_dispatch_threshold_queue_depth` fields + TS interface mirrors.
- String-match audit (sweep `"fleet"` sites for missing `"market"` parallels).
- Retired-event consumer grep.
- Permit-release unit test.
- Final wanderer.

**Total: ~2950 LOC, 3-5 sessions. Single operator. No migration.**

---

## Orchestration pattern — standard

Your job is to RUN the pattern, not just write code. Every workflow-level unit of work ships through:

**Workflow agent → serial verifier → wanderer (where specified)**

### Workflow agent
One focused sub-agent per task (or small cluster of tightly-coupled tasks within a wave). Prompt includes: plan task number, specific files to touch, acceptance bar (cargo check + test + any wave-level gate), and a pointer to the friction log to record surprises in. Do NOT combine multiple concerns into one workflow agent — per `feedback_split_big_agents` + `feedback_one_agent_per_task`, if you have more than ~3 numbered sections in a single prompt, split it.

### Serial verifier (behind EVERY workflow)
A second sub-agent arrives expecting to build, audits what the first agent did with fresh eyes, fixes in place. Per `feedback_serial_verifier`. The verifier:
- Reads the workflow agent's diff.
- Reads the plan tasks that diff was supposed to land.
- Reads the relevant source files the diff touches.
- Flags anything wrong.
- Fixes in place (not just reports — this is implementation, not audit).

Verifier commits alongside the workflow's commit (or as a separate follow-on commit, depending on scope of fix).

### Wanderer (after Wave 1, Wave 3, Wave 4, and final ship)
Per `feedback_wanderer_after_verifier` — a third sub-agent with NO punch list. Given only the feature name + "does this actually work?" Traces end-to-end execution through the built system, catches validators/wiring/dead paths that punch-list verification misses. Per `feedback_wanderers_on_built_systems` — wanderers on built code catch more than any plan-stage audit. Don't skip them.

### Prompts — what to include, what NOT to include

**DO include in every sub-agent prompt:**
- Task definition (plan task N from wave X).
- Files to read (plan sections + source file paths).
- Acceptance bar.
- Friction-log path — "if anything surprises you, append an entry."
- Impl-log path — "document what you committed after commit."

**Do NOT include:**
- Prior audit summaries or verdicts. Audit hygiene per the superseded plan's retro — tainting prompts produces shallow results. Target doc + stated purpose + scope only.
- Pyramid-query skills (pyramid-knowledge, wire-pyramid-ops, etc.). Adam's standing directive — read source files directly, not through pyramid summaries.
- "Ship is a real option" nudges or scope-reduction hints. The plan IS the commitment.

### Parallelism rules
- **Parallel OK** when sub-agents touch DISJOINT files/concerns (e.g. a Rust module + a TS component in Wave 4). Per `feedback_no_worktrees` + `feedback_parallel_agent_atomicity` — parallel agents must commit atomic commits, not leave edits in the working tree.
- **Serial REQUIRED** when sub-agents touch the same file or shared data structure. Most of llm.rs waves (Wave 1, 2, 3) are serial — each wave is one file-region.
- Waves themselves run serial (1 → 2 → 3 → 4 → 5). Wave 0 can partially parallelize (task 1 bundled seed + task 4 prepare_for_replay + task 7 try_acquire_owned touch disjoint files).

### Small-work exception
Per `feedback_direct_over_delegation_small` — for well-specified work under ~500 lines (e.g. adding one helper method to an existing file), write it yourself rather than spinning up a workflow agent. Delegation setup + agent-overload risk doesn't amortize on small diffs. Still run a serial verifier behind it.

## Verification gates

Per the plan:
- Cargo check (default target — not just `--lib`, per `feedback_cargo_check_lib_insufficient_for_binary`).
- Cargo test (specific modules per wave).
- Dev-mode smoke after each wave (`feedback_always_test_dev`).
- Serial verifier after EVERY workflow (see orchestration pattern above).
- Full-feature wanderer after Wave 1 + Wave 3 + Wave 4.
- Final-ship wanderer after Wave 5.

Playful node is the dev test host. Fresh-install rollout: Adam wipes DB before ship; do NOT design for legacy migration.

---

## Known latent issues preserved as intentional

From plan §15 "noted but not acted upon":
- `pyramid_compute_events` has no SQLite CHECK on `event_type` (additive safety; adding walker events is safe).
- Today's `should_enqueue_local_execution` coarse `any(is_local)` check — walker replaces with per-entry tightening; no retroactive fix to pre-walker code.

From §15 "deferred systemic (NOT walker scope)":
- Audit-row schema redesign to parent+attempts split (Wave 1 task 11 is interim; full redesign is post-walker).
- TypeScript-Rust type generation via ts-rs.
- Wire spec/contract doc coherence cleanup (Wire-dev task).
- Wire cooperative-framing in error slugs (Wire-dev task).
- E3 walkers-storm-market single-flight mitigation — v2 after measurement; hook added to Wave 4.
- `branch_allowed(Pool, _) => true` bypasses participation policy for inbound jobs (accepted; documents today's behavior).

Anything else you catch during implementation goes on friction-log. If it's urgent AND not scope-covered, escalate to Adam via friction-log comment.

---

## Escalation triggers

Halt and escalate to Adam if ANY of these:
- Wave 3 reached and Wire-dev Q4 still open.
- Wave 1 schema migration trips on an existing `provider_id` column (means audit-row schema was changed by some other path; reconcile first).
- `cargo check` / `cargo test` breaks for a reason not scoped by the current wave (means a seemingly-orthogonal part of the codebase is coupled; understand before patching).
- Cross-repo divergence: Wire contract doc says X but Wire code does Y (flag as friction immediately + to Adam).
- Any chronicle event emission breaks the BEHEM Chronicle tab rendering (frontend consumers may key on retired event names — plan §15 Wave 5 task 37 does the sweep, but if a current-wave change breaks a surface, stop).

Do NOT halt for: minor drafting errors in the plan, ambiguities you can resolve conservatively, LOC estimates that undershoot. Those go on friction-log and keep moving.

---

## Log templates (copy into place at commit 0 of each log)

### walker-re-plan-wire-2.1-IMPL-LOG.md

```markdown
# Walker Re-Plan Wire 2.1 — Implementation Log

Append-only log of what's done. Newest at top. Updated at every commit.

**Plan:** `docs/plans/walker-re-plan-wire-2.1.md` rev 0.3
**Branch:** `walker-re-plan-wire-2.1` (or per your choice; document here)
**Started:** (date when Wave 0 task 1 commits)

---

## <date time> — commit <sha> (branch <name>)

**Plan task:** <wave X task N>
**Changed:** <1-2 sentences>
**Cargo check:** clean / errors — <summary if errors>
**Cargo test:** <specific module tests run + pass counts>
**Deviation:** None / <rationale>
```

### walker-re-plan-wire-2.1-FRICTION-LOG.md

```markdown
# Walker Re-Plan Wire 2.1 — Friction Log

Real-time record of surprises, workarounds, and "this bit me" moments. Newest at top.

**Plan:** `docs/plans/walker-re-plan-wire-2.1.md` rev 0.3

---

## <date time> — <task or file:line context>

**Context:** <what you were doing>
**Surprise:** <what bit you>
**Root cause:** <if known, else "still investigating">
**Workaround:** <how you got unstuck>
**Flag:** plan error / doc staleness / spec ambiguity / Wire-side / learning moment
```

Create both files as first commit on the implementation branch (empty but template-ready). Then begin Wave 0 task 1.

---

## Pre-compact cross-reference

The planning thread's state is captured in:
- This handoff.
- `docs/plans/walker-re-plan-wire-2.1.md` rev 0.3 (plan).
- `docs/plans/RESUME-HERE-walker-re-plan-post-wire-rev-2.1.md` (pre-planning resumption brief; useful for background but supersededby the plan itself).
- `docs/plans/compute-cascade-build-plan.md` (SUPERSEDED rev 0.5 of the predecessor; design-history only, architecture obsolete).

Don't read the cascade plan unless you're chasing retro lessons. The rev 0.3 audit history at bottom of the plan covers the relevant history compactly.

---

## Pre-flight Q&A (answered by planning thread)

### Scope / process

1. **Branch name:** `walker-re-plan-wire-2.1` confirmed. Keep.
2. **End-state:** fast-forward-merge to main after Wave 5 wanderer clean. No PR review needed (single operator).
3. **Push cadence:** per-commit to `origin/walker-re-plan-wire-2.1`. Safer for cold-resume; cost is zero.
4. **Dev-smoke without Adam:** proceed through Waves 0-3 on cargo-check+test-green; queue dev-smoke checklist entries in impl-log for Adam at wake. **HARD GATE:** dev-smoke on Wave 0-3 must be green before Wave 4 starts (user-visible surfaces).
5. **Wipe-and-fresh-install:** Adam wipes Playful's DB at ship, not during dev. No schema version bump / DADBEAR marker needed from you — the ALTER TABLE uses idempotent `pragma_table_info` pattern. Operator step only at rollout.
6. **Retro cadence:** short retro note in friction-log at end of EACH wave (1-3 sentences). Full retro after Wave 5 using accumulated friction-log.

### Orchestration judgment

7. **Small-work pattern:** direct write + serial verifier only — SKIP workflow agent on <500 LOC well-specified work. Wave 0 tasks 4/5/6/7 all qualify. Workflow agents for Wave 0 task 1 (bundled JSON authoring), task 2 (sync helper), task 8 (compute_quote_flow skeleton).
8. **Wave 3 parallelism:** allowed for disjoint files. Split Wave 3 into:
   - **Wave 3a (PARALLEL):** author `compute_quote_flow.rs` RPC bodies + author `market_surface_cache.rs` polling loop. Two concurrent workflow agents on new files.
   - **Wave 3b (SERIAL, after 3a):** inline market branch into `llm.rs`, remove Phase B pre-loop. llm.rs is single-threaded (shared file).
   - **Wave 3c (SERIAL, after 3b):** error-taxonomy tests + chronicle emission tests + wanderer.
   
   Wave 2 (fleet extract in llm.rs) and Wave 3b (market extract in llm.rs) are strictly serial to each other (overlapping llm.rs regions).
9. **Wave 5 string-match audit:** fix in-scope during Wave 5 if related to walker's flat-string sentinels. Unrelated bugs surfacing in the sweep: friction-log + fix in same PR if <100 LOC, otherwise `mcp__ccd_session__spawn_task` for separate handling.
10. **Unrelated latent bugs during any wave:** fix inline per `feedback_fix_all_bugs`. Exception: multi-file bugs needing their own design doc → friction-log + spawn_task.

### Plan ambiguities

11. **`NO_BUDGET_CAP` sentinel:** add as `pub const NO_BUDGET_CAP: i64 = (1i64 << 53) - 1;` in `dispatch_policy.rs`. Walker reads `entry.max_budget_credits.unwrap_or(NO_BUDGET_CAP)`. Doc-comment the rationale (JS Number.MAX_SAFE_INTEGER, f64 round-trip safety, effectively-no-cap semantic).
12. **`prepare_for_replay(&self, origin: DispatchOrigin)`:** origin named (not underscored). Use it inside the fn via `tracing::debug!(?origin, "preparing replay config")` — gives per-call observability of replay-config derivation AND suppresses unused-variable warnings. Plan §2.5.1 snippet updated to reflect this.
13. **Wave 1 task 11 audit-row:** ONLY `provider_id TEXT` column + extend `complete_llm_audit` AND `fail_llm_audit` signatures with `Option<&str>`. NO `attempt_index` — multi-entry attempt tracking lives in `pyramid_compute_events` chronicle (queryable by slug+build_id+step_name+timestamp-window). Full parent+attempts schema split is post-walker.
14. **Chronicle event naming:** `pub const EVENT_<DOMAIN>_<ACTION>: &str = "<domain_action>";` per existing compute_chronicle.rs pattern. All snake_case. Full list of walker-added constants in the HANDOFF's "Chronicle event constants to add" block below.
15. **CallTerminal audit row:** record the LAST-ATTEMPTED entry's `provider_id`, not "winning" — CallTerminal isn't a win. Plan Wave 1 task 11 revised: Success → `complete_llm_audit(..., Some(winner.provider_id))`; CallTerminal → `fail_llm_audit(..., Some(last_attempted.provider_id))`; Exhaustion → `fail_llm_audit(..., None)`.

---

## Chronicle event constants to add to `src-tauri/src/pyramid/compute_chronicle.rs`

Add near the existing EVENT_NETWORK_* constants (line 164-169 today):

```rust
// ── Walker lifecycle events (rev 2.1 compute dispatch walker) ───────────
pub const EVENT_WALKER_RESOLVED: &str = "walker_resolved";
pub const EVENT_WALKER_EXHAUSTED: &str = "walker_exhausted";
pub const EVENT_WALKER_PATH_DISTRIBUTION: &str = "walker_path_distribution";
pub const EVENT_WALKER_QUOTE_RACE_STATS: &str = "walker_quote_race_stats";

pub const EVENT_NETWORK_ROUTE_SKIPPED: &str = "network_route_skipped";
pub const EVENT_NETWORK_ROUTE_SATURATED: &str = "network_route_saturated";
pub const EVENT_NETWORK_ROUTE_UNAVAILABLE: &str = "network_route_unavailable";
pub const EVENT_NETWORK_ROUTE_RETRYABLE_FAIL: &str = "network_route_retryable_fail";
pub const EVENT_NETWORK_ROUTE_TERMINAL_FAIL: &str = "network_route_terminal_fail";
pub const EVENT_NETWORK_MODEL_UNAVAILABLE: &str = "network_model_unavailable";

pub const EVENT_NETWORK_QUOTED: &str = "network_quoted";
pub const EVENT_NETWORK_PURCHASED: &str = "network_purchased";
pub const EVENT_NETWORK_QUOTE_EXPIRED: &str = "network_quote_expired";
pub const EVENT_NETWORK_PURCHASE_RECOVERED: &str = "network_purchase_recovered";
pub const EVENT_NETWORK_RATE_ABOVE_BUDGET: &str = "network_rate_above_budget";
pub const EVENT_NETWORK_DISPATCH_DEADLINE_MISSED: &str = "network_dispatch_deadline_missed";
pub const EVENT_NETWORK_PROVIDER_SATURATED: &str = "network_provider_saturated";
pub const EVENT_NETWORK_BALANCE_INSUFFICIENT_FOR_MARKET: &str = "network_balance_insufficient_for_market";
pub const EVENT_NETWORK_AUTH_EXPIRED: &str = "network_auth_expired";

pub const EVENT_DISPATCH_POLICY_SUPERSEDED: &str = "dispatch_policy_superseded";
```

Wave 0 task lands these as dead-code constants (allow-unused until walker emit sites wire them up in Wave 1/3). Prevents typo drift between emission sites and consumers.

---

**Ship it right, not fast. Plan commitment is the deadline, not session timing.**
