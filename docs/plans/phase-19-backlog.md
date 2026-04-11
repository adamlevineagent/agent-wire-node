# Phase 19 Backlog

**Created:** 2026-04-11 after Phase 18 shipped
**Status:** Unscheduled — items accumulated during Phase 18 wanderer passes that were deferred rather than fixed in-branch

---

## Provenance

All items below were surfaced by Phase 18 wanderer agents (2026-04-11 overnight run). Each item has been explicitly deferred because:
- It's a UX polish issue, not a correctness bug (Phase 18c items)
- It's pre-existing Phase 17 design debt exposed by Phase 18e's nested vine hierarchy
- It's a pre-existing hardcoded LLM parameter that belongs in a cross-cutting Pillar 37 sweep, not a one-off fix

These items are not blockers for Phase 18 to ship. They're the next-run candidates for when Adam wants to polish the initiative.

---

## Group A — Phase 18c UX polish (6 items)

Source: 18c wanderer pass (`047c222`), friction log Phase 18c wanderer section.

### A1. L4 silent fallback when opting in on slug=null contribution

**What:** User checks "Include cache manifest" on a global (slug=null) contribution. Backend at `main.rs:8360-8389` correctly detects that there's no slug to export cache from, logs a warning, and drops the opt-in. But the UI shows the same success state as if the opt-in had landed, and the response's `cache_manifest_entries` field is null without explanation.

**Fix:** Either (a) add a post-publish warning toast "Opt-in ignored because this contribution has no slug to export from" driven by a new response field `opt_in_ignored_reason`, or (b) gate the checkbox in the UI to disable when the dry-run report indicates slug=null.

**Scope:** 1 new response field, 1 toast component, 1 conditional render. ~30 min.

### A2. DryRunReport missing `slug` field (enables A1)

**What:** The dry-run report that drives the PublishPreviewModal lacks a `slug: Option<String>` field. Without it, the frontend can't conditionally gate the opt-in checkbox based on whether the contribution is slug-bound.

**Fix:** Add `slug: Option<String>` to `DryRunReport`. Populate from the contribution row. Gate the checkbox disabled state on `slug.is_none()`. Fixes A1 via UI path instead of post-hoc toast.

**Scope:** 1 struct field, 1 populator, 1 conditional prop. ~20 min.

### A3. CrossPyramidTimeline banner Resume forces scope='all'

**What:** After a user pauses DADBEAR via the scoped modal (e.g., scope=folder `/a`), a green banner appears with a "Resume" button. The banner only stores the affected count, not the scope. Clicking Resume fires `pyramid_resume_dadbear_all` with `scope='all'` — which would wake up pyramids the user never intended to pause, potentially including pyramids paused in prior sessions.

The implementer flagged this in a code comment and recommended the DADBEAR Oversight page for scoped resume. But the banner is the first thing a user reaches for, so this is a real footgun.

**Fix:** Store the scope + scope_value alongside the affected count when the banner renders. Resume uses the same scope. Or: remove the banner's Resume button entirely and force users through the Oversight page for correctness.

**Scope:** 1 state addition + Resume handler signature change. Or deletion. ~15 min.

### A4. CrossPyramidTimeline missing refetch after pause/resume

**What:** `DadbearOversightPage` correctly calls `refetchOverview()` after a pause/resume completes. `CrossPyramidTimeline` doesn't — it only sets a banner + toast. The cross-pyramid live counts become stale until the user navigates away and back.

**Fix:** Add a refetch call after the pause/resume IPC completes. Parity with DadbearOversightPage.

**Scope:** 1 line. ~5 min.

### A5. Whitespace-only folder input reaches count IPC

**What:** `DadbearPauseScopeModal`'s folder input has a `trim().length === 0` check on the confirm button, but the live count preview IPC fires on every input change without trimming. A whitespace-only input sends empty-string to `pyramid_count_dadbear_scope` which returns 0 harmlessly but wastes a round trip.

**Fix:** Trim the input before calling the count IPC. Skip the call entirely if trimmed length is 0.

**Scope:** 1 trim. ~2 min.

### A6. DadbearPauseScopeModal hardcoded datalist id

**What:** The reusable modal uses `list="dadbear-source-paths"` for its `<datalist>`. If the modal is ever rendered in parallel (two instances open at once), the datalist id collides. Not reachable in the current UI (only one modal can be open at a time), but a latent bug.

**Fix:** Generate a unique datalist id per modal instance via `useId()` hook.

**Scope:** 1 hook invocation + id threading. ~5 min.

---

## Group B — Phase 17 design debt (2 items)

Source: 18e wanderer pass (`f38cc60`), friction log Phase 18e wanderer section.

### B1. execute_plan idempotency was broken since Phase 17

**Status:** Already fixed in Phase 18e (`f38cc60`). Not a Phase 19 item — just noted here as a vindication of the wanderer pattern on built systems.

The Phase 17 idempotency check used `msg.contains("already exists")` but sqlite reports `"UNIQUE constraint failed"`, not `"already exists"`, AND `db::create_slug`'s `.with_context()` wrap hides the chain in `{:#}` format so `e.to_string()` returns only the top-level context. The check never matched. Every re-run of a folder ingestion against a populated DB would push fake errors for every op. Invisible to all prior tests because they used fresh in-memory DBs.

**Lesson carried forward:** for any fix-pass on executors that can be re-run, include at least one test exercising the executor against a non-fresh state. The Phase 19 candidate is a hygiene sweep of all `.to_string().contains(` usages on anyhow errors across the codebase, because this landmine pattern may exist elsewhere.

### B2. spawn_initial_builds nested hierarchy dispatch gap

**What:** Phase 17's `spawn_initial_builds` dispatches vines on a 2-second fixed delay, not on leaf completion. `notify_vine_of_child_completion` halts at vines that have zero live nodes. For 18e's nested hierarchy (root vine → CC vine → conversation + memory bedrocks), the dispatch order is:
1. t+0s: dispatch all leaves (conversation bedrocks, memory bedrocks, real-folder bedrocks)
2. t+2s: dispatch all vines (root vine AND CC vines)
3. Root vine builds against children that haven't completed yet — topical chain runs on empty/partial state
4. CC vines build against bedrocks that haven't completed yet — same problem
5. When bedrocks DO eventually complete, `notify_vine_of_child_completion` walks up from each bedrock → CC vine → root vine. But it halts at vines with zero live nodes, which is exactly the state both vines are in.
6. Result: neither the CC vines nor the root vine ever auto-rebuild after their children complete. The user ends up with a root vine whose apex is empty.

**Fix options:**

- **Option 1 (simplest):** event-driven dispatch. When the last leaf in a vine's child set completes, fire a "dispatch this vine" event. The vine build starts with real child content, not empty. Requires wiring a dependency tracker in `spawn_initial_builds`.
- **Option 2 (lazier):** remove the "halts at zero live nodes" guard in `notify_vine_of_child_completion` so bedrock completion re-triggers the vine rebuild even if the vine's first attempt was empty. Simpler but causes redundant builds.
- **Option 3 (heavy):** redesign `spawn_initial_builds` as a topological sort over the plan, dispatching each op only after its dependencies complete. Cleanest but more surgery.

**Scope:** Option 1 is ~100 lines + tests. Option 2 is ~20 lines but causes wasted rebuilds. Option 3 is Phase 19 sized.

**Recommendation:** Option 1. The 2-second delay was a Phase 17 wanderer workaround for "dispatch order matters but we don't have proper signaling yet" — Phase 19 adds the proper signaling.

---

## Group C — Pillar 37 hygiene sweep (1 item)

Source: 18d wanderer pass (`d28f3ff`), friction log Phase 18d wanderer section.

### C1. Hardcoded LLM call parameters in generative + migration config flows

**What:** `run_migration_llm_call` hardcodes `temperature: 0.2` and `max_tokens: 4096`. `generative_config.rs::run_generation_llm_call` hardcodes the same values. These are Pillar 37 violations per `feedback_pillar37_no_hedging.md`: "A number constraining LLM output is always a Pillar 37 violation. No 'reasonable default' exceptions."

The 18d wanderer correctly noted that fixing it in 18d alone would be inconsistent with `generative_config.rs` — this is a cross-cutting sweep, not a one-off.

**Fix:** Move temperature + max_tokens into the `skill` contributions that carry each generative/migration prompt. The skill YAML already exists; add `call_params: { temperature: 0.2, max_tokens: 4096 }` fields. Resolve at call-time via contribution lookup. Users can then refine temperature per-skill via the generative flow (meta-self-reference: refining the migration prompt's temperature via the generative flow's own temperature).

**Scope:** 2 skill contributions extended, 2 call sites refactored, 1 resolver helper. ~1-2 hours.

**Also:** same sweep should cover any other hardcoded LLM params across the codebase. `grep -rn "temperature: 0\." src-tauri/src/pyramid/` would surface the full list.

---

## Group D — Codebase hygiene (2 items)

### D1. `.to_string().contains(` on anyhow errors

**What:** Phase 17's `execute_plan` idempotency bug (B1, already fixed in 18e) stemmed from checking `.to_string().contains("already exists")` on an anyhow error. Because anyhow wraps errors with `.with_context()`, `.to_string()` returns only the top-level context — the chain lives in `format!("{:#}", e)`. The check silently never matched.

**Fix:** `grep -rn "\.to_string()\.contains(" src-tauri/src/` — inspect each hit for whether the error is anyhow-chained. Replace with `format!("{:#}", err).contains(...)` where needed.

**Scope:** Inspection-only grep pass, fixes per hit. Maybe ~10 sites total, each <5 min. ~1 hour budget.

### D2. 112 GB `target/` growth

**What:** During Phase 18, the main repo's `target/` ballooned to 112 GB and blocked task output writes. Parallel worktrees each grow their own `target/` independently. Over the course of Phase 18's 15 agent dispatches (5 implementers + 5 verifiers + 5 wanderers), total disk usage peaked at ~17 GB across worktrees + 112 GB in main = 129 GB.

**Fix options:**
- Set `CARGO_TARGET_DIR` to a shared location for parallel worktrees so incremental builds amortize (has race-condition risks when two `cargo build --release` run concurrently)
- Automate `cargo clean --release` on completed worktrees after they wander clean
- Add a pre-flight disk check in conductor workflows: if free space < 50 GB, clean + warn
- Add a `.cargo/config.toml` with `target-dir = "/private/tmp/agent-wire-target"` to centralize

**Scope:** Configuration + automation, not code. ~30 min.

---

## Recommendation on scheduling

Group A (Phase 18c polish) is cheap to bundle into a single small workstream. All 6 items together: ~1-2 hours, one branch, one agent.

Group B2 (spawn_initial_builds topology) is the biggest item and the most architecturally significant. Deserves its own workstream with implementer+verifier+wanderer ceremony. Option 1 takes ~half a day.

Group C (Pillar 37 sweep) touches two flows but is surgical. ~1-2 hours. Can bundle with Group A in the same "Phase 18 polish" workstream OR run in parallel.

Group D (hygiene) is conductor-level infrastructure, not code. Can run before Phase 19 formally starts as prep.

**Suggested Phase 19 structure (if Adam greenlights):**

- **19a** — Phase 18c polish (Group A) + Pillar 37 sweep (Group C) + `.to_string().contains(` hygiene (D1). One workstream, ~3-4 hours. Single implementer.
- **19b** — spawn_initial_builds topological dispatch (B2). One workstream, ~4-8 hours. Single implementer with full ceremony.
- **Pre-19** — `CARGO_TARGET_DIR` + disk hygiene automation (D2). Conductor-level, not a workstream.

Total: ~1 day of parallel work for 2 workstreams + pre-run conductor prep.
