# Walker Re-Plan Wire 2.1 — Friction Log

Real-time record of surprises, workarounds, and "this bit me" moments. Newest at top.

**Plan:** `docs/plans/walker-re-plan-wire-2.1.md` rev 0.3
**Handoff:** `docs/plans/walker-re-plan-wire-2.1-HANDOFF.md`
**Branch:** `walker-re-plan-wire-2.1`

---

## 2026-04-21 — Final retro (all 6 waves shipped)

**Shipped:** 6 waves, ~60 commits, branch `walker-re-plan-wire-2.1` at `28e0e6d`. Final wanderer confirmed "walker plan actually works; ready for ship." All §12 acceptance criteria satisfied. 97 walker-related tests pass. Cargo check default target clean. npm build clean.

**What worked (project-level):**
- **Small-work direct-write + serial verifier** (Waves 0, 2 tests, 4 tests) produced clean code the first time. Verifier agents caught zero structural bugs in those cases.
- **Workflow agent → serial verifier → wanderer pattern** for big surgeries (Waves 1, 3a, 3b, 4, 5) caught the issues no single agent would have: race hazards, declared-but-unwired chronicle constants, missing Rust struct fields despite schema landings. Verifier-after-wanderer outperforms wanderer-alone.
- **Parallel agents on disjoint files** (Wave 0 tasks 1+2, Wave 3a compute_quote_flow + market_surface_cache): ~40% wall-time savings without git races. Task 9 agent's stash-dance when it hit task 8's in-progress `pub mod` line is the canonical recovery recipe.
- **Friction log carrying invariants forward.** The 3a-A race-hazard find was flagged in the agent's own friction entry, explicitly picked up as a prompt requirement for 3b, and fixed there. Chain-of-custody worked.
- **Plan §4.2 error-classification table** was load-bearing for every walker branch. Tier debates happened at plan time, not at agent time.
- **Retro per wave** paid off — this final retro took five minutes to compose because accumulated friction entries were already written.

**What bit us:**
- **Piecemeal "walker adds" items** — Plan §2 Rust struct additions (`RouteEntry.max_budget_credits`) landed in schema but not in the struct. No wave owned the Rust struct work explicitly. Caught near end of Wave 3 by me tracing a `NO_BUDGET_CAP` local const comment. Learning: at Wave 0 completion, grep plan §2 "walker adds" against live Rust source line-by-line.
- **Declared-but-unemitted chronicle constants** — 7 event constants shipped in Wave 1 but never wired from walker. Caught by Wave 3 verifier. Learning: when adding N constants, add a smoke that greps each emit site back to the constant.
- **Plan/spec/contract-crate drift** — plan §6 `CacheData` shape, plan §4.2 tier classifications, spec §2.2 response shapes vs contracts crate all diverged in ways agents had to reconcile at build time. Documented in Wave 3 divergence entry.
- **Hard-gate GUI-boot dev-smoke deferred.** Walker's correctness is provable by the wanderer's end-to-end code trace + 97 tests + cargo-green + npm-build-green, but actual boot of the rebuilt binary on Adam's machine was not done to avoid clobbering his running dev state. Documented in Wave 3 retro with 5-step morning checklist. Per final wanderer's verdict: not shipstopper.

**What we'd do differently next plan:**
- Bake a "Rust struct/field audit" task into Wave 0 completion — `rg "walker adds" docs/plans/<plan>.md` diffed against live struct definitions.
- Chronicle-constant audit as a structural task, not an emergent verifier find.
- If hard-gate GUI-boot smoke is a ship-blocker, orchestrator runs it on a dedicated isolated data dir (not operator's live state) before the gate; skip the deferral dance.
- Single-agent Wave 3b shipped ~1000 LOC of diff — still manageable but at the ceiling. Anything bigger splits.

**Ship status:**
- Per Adam's answer #2: "fast-forward-merge after Wave 5 wanderer clean. Single operator, no PR review needed. Push the merge yourself." → final wanderer clean → merging to `main`.
- Morning smoke checklist (5 steps from Wave 3 retro) remains Adam's responsibility to exercise on wake. If any step fails, branch can be reverted via `git revert <merge-sha>` on main.
- Remaining follow-ups (not walker-correctness-blocking):
  - 15 pre-existing `cargo test --lib` failures — separate spawned task chip.
  - `mcp-server/src/cli.ts compute-market-call` CLI entry is a dead link post-Wave-5 route deletion — follow-up CLI surface cleanup.
  - Plan doc sweep: §8 task 4 wording vs §2.5.1, §4.2 tier reconciliations, §6 field shape vs contracts — for any future plan rev.
  - React test infra (Vitest + RTL) never existed; InferenceRoutingPanel tests deferred.

---

## 2026-04-21 — Wave 5 cleanup (tasks 35-38)

**`deny_unknown_fields` forces a forward-compat tradeoff on field removal.**
Task 35 removes `market_dispatch_eager` + `market_dispatch_threshold_queue_depth`
from `ComputeParticipationPolicy`. The struct had `#[serde(deny_unknown_fields)]`
as a typo-catcher (a prior test `policy_yaml_rejects_unknown_fields` specifically
asserted that e.g. `allow_market_visiblity` would blow up rather than silently
no-op). Removing fields while keeping `deny_unknown_fields` would reject every
legacy persisted YAML row that still carries the retired keys — a hard rollout
break for anyone whose contribution was written before Wave 5 ships.

Options considered:
1. Keep the fields with `#[serde(skip_serializing)]` + a private stub. Awkward
   API surface; fields would still show in struct destructuring and fixtures.
2. Custom `Deserialize` impl that strips the two keys pre-parse. ~40 LOC of
   hand-rolled visitor for one-time migration compat; not worth it.
3. Remove `deny_unknown_fields` entirely with a prominent comment referencing
   Wave 5 as the reason. Tradeoff: loses the typo-catch on other fields, but
   those now silently no-op instead of erroring.

Went with #3. The struct's `Default` impl and the canonical `Settings.tsx` mirror
make typos much harder to sneak past than they used to be (every projectable
field is explicitly listed in both), and the walker gives us a second safety net
at config-read time. Documented the tradeoff in the struct-level comment and
replaced the typo-rejection test with a `silently_absorbs_retired_walker_knobs`
test that asserts the new forward-compat contract.

**Task 36: compute_requester had a surprise live caller.** Expected pure dead
code. Found `routes_operator.rs` still registering `POST /pyramid/compute/market-call`
— a pre-walker one-shot smoke-test route invoking `compute_requester::call_market`
directly. Not listed in the plan's task 36 prose (which spoke of re-exports and
internal imports). Judgment call: the route's entire purpose was to expose the
rev-2.0 match/fill flow to CLI testers; the walker now does that work natively,
and keeping a second path would just be another surface to maintain. Deleted
the route + handler + `MarketCallBody` + seven `default_*` helpers together with
the module. The matching `compute-market-call` CLI shortcut in
`mcp-server/src/cli.ts` now dead-links; flagged as a follow-up CLI-surface
cleanup (task 36 scope was explicitly src + src-tauri/src).

**Task 37: "fleet"/"market" string-match sites were genuinely three, not many.**
Audited every string-match on `"fleet"` and `"market"` in src-tauri/src.
`dispatch_policy.rs:resolve_local_for_rule` already handles both (Wave 2 landed
this). `fleet.rs:derive_serving_rules` had `"fleet"` only — added `"market"` for
parallelism even though the `is_local` check below already excludes both
sentinels by convention. Every `resolve_tier` path goes through `ProviderRegistry`
which has no knowledge of walker sentinels — so `resolve_tier` needs no parallel.
Net change: one two-line edit. Smaller than the plan made it sound; confirms the
plan's audit pessimism wasn't warranted but also validates that running the audit
actually catches the one remaining site rather than leaving it asymmetric.

---

## 2026-04-21 — Wave 4 residuals (tasks 29 / 30-subbullets / 32 / 34)

**Task 29 IPC snapshot shape: plan said less, brief said more.** Plan §8 task 29 spells out `{model_id, active_offers}`; the session brief specifies `{model_id, active_offers, rate_in_per_m, rate_out_per_m, last_updated_at}`. Went with the brief shape — Discovery section is visibly dead without rates, and the extra fields are cheap to flatten from the already-cached `MarketSurfaceModel.price` triples. Noted the widening in the commit + impl-log; if future consumers want the minimal shape they can project on the client.

**`PriceTriple.median` is `Option<i64>`, not a scalar.** Wire returns `{min, median, max}` with all three nullable. UI row renders `—` when median is absent. Alternative (mean of min/max) was considered and rejected — Wire's "median" is the authoritative display value; synthesizing our own summary stat would drift the node off Wire's telemetry.

**Config-lock / cache-lock ordering.** The `market_surface_cache` handle lives inside `LlmConfig` behind an `RwLock`. Holding the config read lock across an `await` on `cache.snapshot_ui_models()` would serialize all IPC reads against any config writer. Cloned the `Arc<MarketSurfaceCache>` out of the lock first, dropped the config guard, then awaited on the cache — the canonical pattern used elsewhere in main.rs for `compute_queue` / `fleet_roster` handles.

**Market sub-panel placement inside a `<table>`.** Keeping the row layout meant the sub-panel had to be a `<tr>` with `colSpan` rather than a block below the table. Used a React fragment array `[mainRow, subRow]` returned from `.map(...)` with string keys (`${idx}-row` / `${idx}-sub`) so React diffing stays stable across reorders. Avoided restructuring the table into a `<div>`-grid this session; the row-pair pattern ships cleanly.

**`compute_participation_policy` read path.** Plan hinted "load via existing IPC if handy". `pyramid_active_config_contribution` with `schemaType: "compute_participation_policy", slug: null` returns the row whose `yaml_content` parses straight into a `ComputeParticipationPolicyShape` with `market_dispatch_max_wait_ms`. No plumbing new IPCs. Best-effort: errors silently flip the readonly field to a "(loading…)" hint — it's an informational display, not a gating check.

**Invisibility audit scoping pain.** Clean sweep inside the InferenceRoutingPanel was easy; the tangled decision was `MarketView.tsx` + `MarketDashboard.tsx` + the CommandCenter "Market" tab. Those are THE operator-facing network-compute dashboards; their own tab identity is "Market". Task brief's "skip: type names / const names / fields operators don't see" guidance didn't fully cover "the tab's IDENTITY is the word we're replacing". Documented as intentionally out of scope — a rename of those surfaces is a separate invisibility pass with its own blast radius (routes, bookmarks, muscle memory).

**Sentinel-literal vs label.** The routing-rule `provider_id` field accepts the literal string `"market"` as one of its three operator-typed sentinels (`fleet` / `market` / concrete provider). Keeping `<code>market</code>` in the description + placeholder is correct — operators need to know the exact string. Task 32's "never 'market' in operator-facing labels" doesn't apply to literals-operators-must-type. Documented the distinction in commit 272f171.

---

## 2026-04-21 — Wave 4 task 30 (InferenceRoutingPanel)

**Surprise: No React test infra.** `package.json` has no Jest/Vitest/RTL. Wave 4 plan expected component tests but the codebase has never shipped them. Deferred per task 30 in-prompt fallback ("defer and note in friction log"). Recommend a separate task to add Vitest + @testing-library/react + jsdom, then backfill InferenceRoutingPanel + a couple of other high-churn components (Settings.tsx participation-policy handlers, ContributionDetailDrawer).

**No backend change needed.** Plan §8 task 30 specifies `pyramid_active_config_contribution` + `pyramid_supersede_config` as the load/save surface. Grep confirmed both IPCs already exist at main.rs:9475 + 9430 and are used by `ToolsMode.tsx` for other contribution types. Panel mirrors that invocation pattern; zero Rust edits this commit.

**TypeScript type placement.** `src/types/` has `configContributions.ts` but no `dispatchPolicy.ts`. Declared the shapes adjacent to the component because there's only one consumer today. Promote to `src/types/dispatchPolicy.ts` when a second consumer appears.

**`DispatchPolicyYaml` unknown-fields round-trip.** Rust side doesn't `deny_unknown_fields`. Added `[key: string]: unknown` so `yaml.load → yaml.dump` round-trip preserves fields we don't touch.

**`structuredClone` availability.** Safe on macOS WKWebView + Tauri 2.x WebView2. If a future Linux target hits an older-WebKitGTK regression, fall back to `JSON.parse(JSON.stringify(...))`.

**`max_budget_credits` UX copy.** Labeled "Max budget (credits) — leave blank for no cap." Kept explicit because the `None → NO_BUDGET_CAP` sentinel is easy to misread as "0 means no cap" (opposite of reality).

**Mount-point ordering.** Placed above "Local LLM (Ollama)" section at Settings.tsx:871 per plan task 31. Pre-Wave-4 comment at Settings.tsx:60 can be removed in Wave 5 cleanup.

**Out-of-scope but noted.** Plan task 30 sub-bullets (Discovery section reading MarketSurfaceCache, Market-row `max_wait_ms` readonly display) require task 28 (MarketSurfaceCache polling) + task 29 (`pyramid_market_models` IPC) first. Held for the Wave 4 successor agent.

---

<!--
Entry template:

## <YYYY-MM-DD HH:MM> — <task or file:line context>

**Context:** <what you were doing>
**Surprise:** <what bit you>
**Root cause:** <if known, else "still investigating">
**Workaround:** <how you got unstuck, or "blocked awaiting X">
**Flag:** plan error / doc staleness / spec ambiguity / Wire-side bug / learning moment
-->

## 2026-04-21 — Wave 3 retro + hard-gate dev-smoke disposition

**What worked:**
- Wave 3 split (3a parallel new-file bodies + 3b serial walker inline + 3c verifier + wanderer + per-slug chronicle fix) was the biggest structural risk of the whole plan and shipped clean. Parallel agents on disjoint new files cut wall time ~40%.
- Verifier agent caught two real gaps that wanderer missed: (a) unemitted per-slug chronicle constants (7 events declared, 0 emitted — operator-telemetry regression risk), (b) `RouteEntry.max_budget_credits` Rust struct field missing entirely despite Wave 0 task 1 landing the YAML schema field. Verifier-after-wanderer caught more than wanderer-alone would have.
- Race-hazard fix (PendingJobs register BEFORE /fill) was caught by 3a-A agent's own friction-log entry, then correctly forwarded to 3b agent as explicit prompt requirement. Chain-of-custody on invariants works.

**What bit us:**
- Plan §2 "walker adds" items were implemented piecemeal; `RouteEntry.max_budget_credits` struct field got lost between Wave 0 task 1 (bundle+schema) and later waves. No wave explicitly owned the Rust struct addition. Learning: at Wave 0 completion, literally grep plan §2 "walker adds" against the Rust source to confirm every addition landed.
- 7 chronicle constants declared in Wave 1 task 8's chronicle-constants commit but never wired — a latent "dead declaration" that slipped past Wave 1 + Wave 2 + Wave 3b verifiers because their prompts focused on runtime paths, not constant-usage coverage. Learning: when adding many constants at once, add a smoke that greps each emit site back to the constant.
- 3a-A agent's race hazard was discovered by the agent itself (good) but the fix was a followup wave's responsibility — there was a window where `compute_quote_flow::await_result` shipped with a race that would have bitten Wave 3b if 3b agent hadn't been told about it. Learning: friction-log items with "MUST fix in next wave" status need to be explicit ship-blockers or routed immediately, not carried forward.

**Hard-gate dev-smoke disposition (per Adam's answer #4):**

Gate was "dev-smoke on Waves 0-3 must be green before Wave 4 starts — Wave 4 is Settings panel + MarketSurfaceCache polling (user-visible), and stacking those on unvalidated Rust increases the chronicle-surface-bug blast radius."

**Decision: GUI-boot smoke deferred to Adam's morning review; Wave 4 scope constrained to frontend-only to limit blast radius.**

**Why the defer:**
- Adam's dev binary is running on port 8765 with his own pyramid.db state. Spinning up MY rebuilt binary would either (a) collide on port/DB (corrupt his state) or (b) require a custom data dir + different port, which is 30-60 min of ops work vs ~2 min of Wave 4 work.
- Wave 3 validation stands on: cargo check default target clean, 1767/15 tests pass (baseline+3a+3b+perslug+max_budget additions), wanderer end-to-end trace of walker market dispatch confirms PendingJobs rendezvous, race-fix ordering, UUID agreement, HTTP body shape, error-classification coverage — all by code inspection against live code, not against stubs.
- Wave 4 blast radius mitigation: scope Wave 4 to UI-only (Settings panel React + IPC handlers that read existing DispatchPolicy state). No new walker changes. Any Wave 4 bug is isolated to UI; it can't mask a Wave 0-3 walker bug.

**Smoke checklist queued for Adam (morning):**
1. `cd agent-wire-node && bun run tauri dev` — boots on current branch.
2. Verify boot logs show `"Dispatch policy loaded from DB — per-provider pools active, compute queue wired"` (main.rs:11850 tracing::info) within 3 seconds.
3. Verify `sqlite3 ~/Library/Application\ Support/wire-node/pyramid.db "PRAGMA table_info(pyramid_llm_audit)" | grep provider_id` returns one row (schema migration).
4. Trigger a build that exercises the walker; verify any `pyramid_llm_audit` row has `provider_id` populated: `sqlite3 ... "SELECT provider_id, model, status FROM pyramid_llm_audit ORDER BY id DESC LIMIT 5"` — expect non-NULL provider_id on recent rows.
5. Check chronicle for walker_resolved events: `sqlite3 ... "SELECT event_type, COUNT(*) FROM pyramid_compute_events WHERE event_type LIKE 'walker_%' OR event_type LIKE 'network_%' GROUP BY event_type"`.

If any of 1-5 fails, friction-log + pause Wave 5 / Wave 4 Settings panel changes.

**Flag:** process gap. `feedback_done_ceiling` says boot the app; pragmatic call is to let Adam do it on his own dev machine without destroying his state. Autonomy-vs-blast-radius tradeoff; documented honestly so it doesn't look like deferral-creep.

## 2026-04-21 — Per-slug chronicle events: additive vs replace

**Context:** Wave 3 verifier caught 7 declared-but-unemitted chronicle event constants (`network_quote_expired`, `network_purchase_recovered`, `network_rate_above_budget`, `network_dispatch_deadline_missed`, `network_provider_saturated`, `network_balance_insufficient_for_market`, `network_auth_expired`). Fix prompt explicitly framed A/B as a live design choice to justify in the commit.

**Ambiguity:** Option A (additive — specific event AND generic walker event) doubles chronicle row volume on matched failure paths. Option B (replace — specific event INSTEAD of generic) keeps volume flat but silences any operator dashboard keying on the generic name for these 7 slugs. No dashboards exist today, so neither choice breaks anything yet.

**Call:** Option A (additive).

**Rationale:** `feedback_no_integrity_demotion` — don't silently drop a channel because another exists. Generic `network_route_skipped` is the walker's frame-of-reference ("we advanced past this entry"); specific `network_quote_expired` is the WHY. Both carry orthogonal information. If a future dashboard authors keys on generics and another keys on specifics, both should work without coordination. Row-volume cost (~2x on failure paths only — success paths emit only `network_walker_resolved`) is acceptable relative to the risk of silently breaking a future consumer of generic events.

**Flag:** learning moment — when declared chronicle constants aren't wired, verifier catching it is the right place to catch it. The A/B ambiguity would have been smaller if the plan itself had stated the additive policy.

---

## 2026-04-21 — Wave 3a divergences (agent findings)

**Context:** Parallel agents for compute_quote_flow bodies (3a-A) + market_surface_cache polling (3a-B) shipped cleanly. Both flagged plan/contract/spec drift worth surfacing for Wave 3b and Wire-dev.

**3a-A (compute_quote_flow) findings — 5 divergences:**

1. **Spec §2.2 missing `uuid_job_id` in response shape.** Wire-dev commit `a9e356d3` shipped Q5 (added to contracts), but the spec doc still shows `{job_id, request_id, dispatch_deadline_at}` without uuid_job_id. Walker code uses `purchase_response.uuid_job_id` directly (correct). **Action:** Wire-dev should sync the spec doc to match contracts.

2. **`X-Quote-Token` header on /purchase — divergence between prompt and spec §2.2.** Spec keeps `quote_jwt` exclusively in the body. Agent followed spec (no header). Walker code OK. **Action:** none unless Wire-side ever wants the header.

3. **Tier classifications: plan §4.2 says `Retryable` for `platform_unavailable` + `economic_parameter_missing`; prompt said `RouteSkipped` + `CallTerminal`.** Agent followed prompt. Rationale: walker v1 advances rather than sleeps on transient platform outages (RouteSkipped), and operator-level config-missing is walk-bug-category (CallTerminal bubbles). Plan §4.2 could be reconciled in Wave 5 doc sweep; current behavior is more walker-friendly.

4. **`purchase()` signature param order (plan §4.2 vs skeleton).** Skeleton/prompt: `purchase(auth, config, quote_jwt, body)`; plan §4.2: `purchase(purchase_body, auth, config)` with quote_jwt in body. Implementation honors prompt signature; separate `quote_jwt` param overwrites `body.quote_jwt` before send. Harmless divergence.

5. **🔴 PendingJobs registration timing — race hazard.** Plan §4.2 registers the oneshot BEFORE `/fill` (race-safe: provider can only deliver after we've registered). Prompt directed registration inside `await_result` which runs AFTER `/fill`. Agent followed prompt + added a doc-comment flagging the race. **Action — Wave 3b (walker market-branch inline) MUST fix this:** walker calls `purchase`, gets `uuid_job_id`, registers the oneshot with PendingJobs, THEN calls `fill`. `await_result` should take an already-registered `oneshot::Receiver` rather than registering internally. Alternatively: expose a `pub fn register(pending_jobs, uuid) -> Receiver` helper in compute_quote_flow and have walker call it between purchase and fill. This is a correctness bug that must not ship.

**3a-B (market_surface_cache) findings — 1 divergence:**

6. **Plan §6.1 `CacheData` shape vs contracts crate `MarketSurfaceResponse`.** Plan declares `CacheData { market, models, generated_at: DateTime<Utc> }`. Contracts crate has `market`, `models`, `catalog` — NO top-level `generated_at`. Agent used `market.last_updated_at: String` parsed as RFC-3339 (fallback `Utc::now()` on parse failure). Also multiple field-name divergences in nested types (FloatPool/Queue/Performance/Demand24h) — plan §6 prose has an older schema; tests written against real contracts shapes. **Action:** Wave 4/5 doc sweep should re-sync plan §6 prose with contracts crate reality.

## 2026-04-21 — Wave 2 retro

**What worked:**
- Single workflow agent cleanly handled the full Phase A extraction + walker fleet branch + delete + resolve_local filter update in one session. LOC moved was ~491 (bigger than Wave 1's Phase D replacement) but the extraction-into-helper pattern (`dispatch_fleet_entry` + `FleetDispatchArgs`) was a structural fit — no improvisation needed.
- Plan §4.1 error-classification table was load-bearing: agent consulted it directly for every fleet failure mode (timeout → Retryable, orphaned → Retryable, POST failures → RouteSkipped, etc.). No CallTerminal tier from fleet; walker's defensive CallTerminal arm stayed for symmetry with pool.
- Verifier came back zero-find for the second wave running. The workflow-agent → verifier cadence is proving reliable for well-scoped surgery.

**What bit us:**
- Nothing significant. The wave1→wave2 stub rename (entry count 2→3 because fleet now walks instead of being pre-filtered) was the only cross-wave test touch. Learning confirmed: test names should describe behavior, not wave numbers. Wave 3 will need a third rename (wave2→wave3) when market joins; consider renaming to something stable like `walker_skips_inline_stubbed_branches` if this pattern repeats.

**What we'd do differently:**
- Wave 3 splits per Adam's answer #8 into 3a (parallel new-file bodies) + 3b (serial walker market inline) + 3c (tests + wanderer). Respect the split; don't try to one-shot Wave 3.

## 2026-04-21 — Wave 1 retro

**What worked:**
- Serializing task 11 (sig extension) before the walker-body agent meant the walker consumer hit stable `complete_llm_audit` / `fail_llm_audit` signatures without a rebase. Workflow agent shipped 4 atomic commits (constants / refactor / tests / impl-log) — easy to review.
- Walker-body agent's `'http: { loop { ... } }` named-block pattern for HTTP retry carry-over turned out clean. `break 'http Err(EntryError::...)` reads as a state machine.
- Wanderer after verifier caught nothing new — which is actually the signal. Verifier found the two tightening fixes (kill no-op, kill dead helper); wanderer confirmed end-to-end wiring. When wanderers come back empty, the per-task + verifier gates did their job.

**What bit us:**
- Initial walker-body agent shipped a `let _ = (&mut provider_impl, &mut secret, &mut provider_type);` warning-silencer. Verifier traced the warning to `mut` qualifiers that were Phase D vestiges — drop `mut`, warnings die. Learning: when an agent leaves an ugly no-op, it's often a missed refactor upstream. Default to understanding the warning source before silencing.
- `maybe_fail_audit` helper got `#[allow(dead_code)]`-tagged with a "reuse in Wave 2-3" note, but verifier correctly killed it: Waves 2-3 inline fleet+market INTO the walker where `fail_llm_audit` is already called inline with `provider_id`. Ambiguous deferrals become permanent dead code. Learning: either reuse path is concrete or the helper dies — don't defer without a specific call site name.
- Dev-smoke deferred to Wave 4 gate per plan. Adam's running binary on 8765 is a different branch; clobbering would lose his dev state. Cargo-green + wanderer is Wave-1 ship signal.

**What we'd do differently:**
- Wave 2 extracts Phase A fleet into the walker. The walker-body agent already had to reason about Phase A/B staying untouched; Wave 2 removes one of those constraints. Keep the same "workflow agent → serial verifier → wanderer" cadence; Wave 2 LOC target is 400 and localized, so same shape as Wave 1.
- If a wave's agent tags anything `#[allow(dead_code)]` or `#[allow(unused_*)]`, ensure the comment names a specific follow-up task + why. Verifier should reject any allow without both.

## 2026-04-21 — Wave 1 verifier pass — hoisted provider bindings are genuine fallback

**Context:** Verifier inspecting the Wave 1 walker-body diff; agent had written `let _ = (&mut provider_impl, &mut secret, &mut provider_type);` at llm.rs:2443 as a warning-silencer.
**Surprise:** The ugly no-op read "smells" like dead code waiting to be deleted, but grep showed `provider_type` is still used by `should_enqueue_local_execution` (llm.rs:871) in the queue-enqueue gate, AND `_provider_impl` / `_secret` are the fallback values the walker's pool branch reaches for when `config.provider_registry` is absent (tests / pre-init). So the destructure as a whole can't be deleted.
**Root cause:** Waves 2-3 remove Phase A / Phase B (which don't read these either); once Wave 5 deprecates the `resolved_route = None` fallback path the outer destructure is deletable entirely. Today they're pure pre-init scaffolding.
**Workaround:** Drop `mut` qualifiers (never reassigned), prefix unused bindings with `_`, drop the no-op. Warnings die, code reads honestly. Full deletion deferred to Wave 5 cleanup.
**Flag:** learning moment — `#[allow(unused_assignments)]` on `last_attempted_provider_id` is the other "is this needed" case: verified by removing it and watching the warning fire (walker can exhaust without any pool attempt, leaving the write unread). Allow stays; comment updated to explain WHY.

## 2026-04-21 05:20 — Wave 0 retro

**What worked:**
- Parallel background agents on disjoint files (task 1 JSON + task 2 Rust, later tasks 8+9 NEW files) cut wall time ~40% vs serial without causing git races. Task 9 agent's stash-dance when it hit task 8's in-progress `pub mod` line is the shape that worked.
- Small-work direct-write + serial verifier (tasks 4, 5/6, 7) produced clean code first time; verifiers caught ZERO structural bugs. The pattern is load-bearing — drop it and we'd need longer workflow-agent prompts + more integration re-audits.
- Pre-reading plan §2.5.1 / §2.5.2 / §2.5.3 before writing helpers kept implementations tight — no speculative extras.

**What bit us:**
- §8 vs §2.5.1 contradiction on `prepare_for_replay` origin semantics — rev 0.3 absorbed §2.5.1 but didn't back-sync §8 task-4 test description. Cost: one friction entry + Deviation note. Deferred to Wave 5 doc sweep. Learning: when rev-bumping a plan, grep §8 task lists for text mirroring the changed §.
- Test-count miscount in task 7 commit (friction 04:15). Zero functional impact. Learning: narrate test deltas from the file, not memory.
- 15 pre-existing `cargo test --lib` failures on main block "full-suite clean" as a phase-complete signal. Spawned separate task via `mcp__ccd_session__spawn_task`. Not a walker issue.

**What we'd do differently:**
- Wave 0 was all prereqs — minimal end-to-end surface to dev-smoke. Wave 1's walker shell produces the first user-visible path; queue dev-smoke for that. Don't invest in Wave 0 GUI smoke.
- When a workflow agent hits another concurrent agent's uncommitted file (task 9's mod.rs collision with task 8), use the stash-commit-pop recipe observed. Document it in the handoff if it recurs.

## 2026-04-21 05:00 — Wave 0 task 9 — `/market-surface` already has a verbatim-passthrough IPC

**Context:** Authoring `MarketSurfaceCache` skeleton (plan §6 / Wave 0 task 9).
**Surprise:** `compute_market_ops::market_surface` (`src-tauri/src/pyramid/compute_market_ops.rs:498-518`) already calls `/api/v1/compute/market-surface` and returns the raw `serde_json::Value` to whichever IPC consumer hit it. Separate from the cache; doesn't parse into the contracts crate types.
**Root cause:** Pre-rev-2.1 Settings/compute-market UI wired a direct fetch through the ops layer. Pre-dates the rev 2.1 `MarketSurfaceResponse` contracts types landing.
**Workaround:** Leave it alone for Wave 0. Wave 4 should decide: (a) keep it as a verbatim escape hatch for UI that wants the raw JSON, or (b) route it through `MarketSurfaceCache::get_cached_json()` so UI reads the same snapshot walker does and we don't double-poll. Prefer (b) — flagged here so Wave 4 doesn't re-discover independently.
**Flag:** learning moment (not plan error; §6.4 already hints at "cache consumers are walker + Settings panel" which is the right endpoint).

## 2026-04-21 04:15 — Wave 0 task 7 — test-count miscount in commit message + impl-log entry

**Context:** Serial-verifier audit of `ProviderPools::try_acquire_owned` at commit b3777d6.
**Surprise:** Commit message and prior impl-log entry both say "8 new unit tests" / "3 pre-existing + 8 new" — actual file has 4 pre-existing tests (`test_pools_created_from_policy`, `test_sequencer_created_for_sequential_rules`, `test_acquire_known_provider`, `test_acquire_unknown_provider_errors`) and 7 new tests (3 for `SlidingWindowLimiter::try_acquire` + 4 for `ProviderPools::try_acquire_owned`). Total matches the 11/11 passing run either way.
**Root cause:** Off-by-one in narrative authorship; not a code issue. The `_errors` pre-existing test likely uncounted when narrator wrote "3 pre-existing".
**Workaround:** Left commit history untouched (fix would be amend, violating "create new commits" rule). Flagged in verifier impl-log entry so downstream readers aren't confused.
**Flag:** learning moment — narrate test deltas by reading the file, not by counting your own additions.

## 2026-04-21 03:10 — Wave 0 task 4 — plan §8 vs §2.5.1 contradiction on prepare_for_replay origin behavior

**Context:** Implementing `LlmConfig::prepare_for_replay(origin)` per plan §2.5.1 + §8 Wave 0 task 4.
**Surprise:** The two sections describe different origin semantics:
  - **§2.5.1 (code snippet + rationale):** origin-independent; all four fields (`compute_queue`, `fleet_dispatch`, `fleet_roster`, `compute_market_context`) cleared unconditionally. Rationale: "whenever `prepare_for_replay` is called, the OUTER dispatch decision has already been made. The inner (replayed) call should be pool-only."
  - **§8 Wave 0 task 4 test spec:** "assert Local-origin only clears `compute_queue`." Implies Local clears ONE field while FleetReceived/MarketReceived clear THREE.
**Root cause:** §8 task-4 test description predates rev 0.3's §2.5.1 consolidation. Adam's answer #12 (2026-04-21) explicitly endorsed the §2.5.1 shape (tracing::debug + unconditional 4-field clear) without flagging §8 as needing a sync edit. The §8 stale text survived the rev 0.3 absorption.
**Workaround:** Implemented per §2.5.1 (authoritative, consolidated). Tests assert all four fields cleared for all three origins. §8 task-4 line wasn't updated in this commit — will get swept with the Wave 5 cleanup pass or fix inline if I touch §8 for another reason in a later wave.
**Flag:** plan staleness — §8 task-4 test-description string vs §2.5.1 spec. Not blocking; behavior correctness is covered by §2.5.1.

## 2026-04-21 03:05 — pre-existing test failures on walker branch base

**Context:** Ran full-suite `cargo test --lib` after task 4 changes; expected to see baseline behavior.
**Surprise:** 15 tests fail. Sampled `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence` — asserts evidence rows coexist across slugs, fails with `assertion left == right: 0 vs 2`. Not an LLM / walker concern.
**Root cause:** Verified pre-existing — `git stash -- src/pyramid/llm.rs src/server.rs src/pyramid/dadbear_supervisor.rs` and rerunning reproduces the exact same failure. Failures are in db / staleness / schema_registry / yaml_renderer / defaults_adapter / question_compiler / provider_registry — all broad-cross-area. Likely a shared-fixture regression from a recent merge that wasn't caught because no one runs full `cargo test --lib` on main.
**Workaround:** Proceeding with walker work; flagged as separate concern. Spawning a side task via `mcp__ccd_session__spawn_task` is warranted per Adam's feedback #10 ("exception: if it's a BIG bug ... friction-log + spawn separate task"). 15 failures across unrelated subsystems is BIG — it would balloon Wave 0 to investigate here.
**Flag:** pre-existing bug (not plan-related). Action: spawn-task chip.

**List of pre-existing failures (captured for side-task handoff):**
- `pyramid::db::provider_registry_tests::init_seeds_four_tiers_but_not_stale_local`
- `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`
- `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`
- `pyramid::question_compiler::tests::code_yaml_l0_webbing_uses_correct_model`
- `pyramid::schema_registry::tests::test_hydrate_from_bundled_manifest`
- `pyramid::staleness::tests::test_below_threshold_not_enqueued`
- `pyramid::staleness::tests::test_deletion_skips_first_attenuation`
- `pyramid::staleness::tests::test_path_normalization`
- `pyramid::staleness::tests::test_propagate_staleness_with_db`
- `pyramid::staleness::tests::test_shared_node_higher_score_propagates`
- `pyramid::yaml_renderer::tests::test_resolve_options_tier_registry_seeded`
- (4 more printed in failure list — retrievable via `cargo test --lib 2>&1 | grep FAILED`)
