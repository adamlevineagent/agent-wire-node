# Friction Log — Three-Market Build

**Started:** 2026-04-16 evening
**Covers:** doc unification → audit-until-clean → Phase 2 implementation → onward

Format: each entry = timestamp + what slowed me down or surprised me + whether/how it was resolved. This is deliberately separate from the decision log — decisions are what we pick; friction is what we hit. Friction that reveals a systemic issue gets cross-referenced to a decision or flagged for Adam.

Taxonomy: `SPEC-GAP` (doc didn't specify something needed), `CODE-DRIFT` (doc says X, code says Y), `SCOPE-AMBIG` (unclear whether something is in-scope for the current phase), `DECISION-FORK` (genuine design choice blocking progress), `TOOL-FRICTION` (tooling/CLI/environment slowed the work), `REDO` (had to undo something I'd done).

---

## Session 1: Doc Unification Pass

### 2026-04-16 ~21:20 · CODE-DRIFT · Thread B1 invented CallbackKind variant names that don't exist in shipped code

I renamed `CallbackKind` variants in `compute-market-architecture.md` §III during Thread B1 (2026-04-16 afternoon) to `WireBootstrap / RequesterTunnel / RelayChain / FleetPeer`. The shipped enum at `fleet.rs:582` has `Fleet / MarketStandard / Relay`. Auditors B and D caught the divergence. Three-auditor confirmation.

**Root cause:** I didn't verify the shipped enum before renaming the docs. Wrote "what the variants should be called" in the docs without checking.

**Resolution:** DD-B reverts docs to shipped names. Added a memory: verify "shipped foundation" claims by checking the actual struct/enum/migration before writing about it. Related existing memory: `feedback_verify_prior_infra_upfront.md` — that one is about PRE-WRITING workstream prompts; this is the same principle applied to writing docs. One principle, two applications.

**Cost:** ~1 hour of audit time caught what 30 seconds of grep would have prevented. Classic systemic lesson.

### 2026-04-16 ~21:25 · SPEC-GAP · "Shipped" claims about fleet MPS diverge from actual struct

Seams §VIII and `project_async_fleet_shipped.md` memory both claimed "Fleet MPS WS1+WS2 shipped (commit 4ae01c0)." Reality per `local_mode.rs:1719-1727`: 5-field struct (no `allow_market_dispatch`), contribution scaffold only. ServiceDescriptor / AvailabilitySnapshot three-objects do not exist in code. Auditors A, B, D all caught.

**Root cause:** I overstated "shipped" claims without verification. Same class of mistake as the CallbackKind naming.

**Resolution:** Systemic root #2 (ground-truth shipped claims) is one of the five big fixes. Will produce a "Deployed Foundation Status" reference that every plan doc can cite, replacing scattered "(shipped)" annotations.

### 2026-04-16 ~21:40 · TOOL-FRICTION · Overlay-vs-body pattern produces contradictions faster than humans catch them

Storage and relay plans got refresh overlays rather than body rewrites (Thread C1 + C2, 2026-04-16 afternoon). The overlay+body pattern SOUNDED like a transparent way to update without losing context. In practice: auditors found 7 overlay-vs-body inconsistencies in storage alone. The body still uses `String` where overlay says `TunnelUrl`; body describes sync streaming where overlay says async outbox; etc.

**Root cause:** Overlays are deltas; bodies are ground truth for readers who skip the overlay. An implementer reads the body FIRST (it's where the details are) and copies the stale pattern before reading the overlay note.

**Resolution:** Systemic root #1 — fold overlays into bodies, retire the overlay pattern. Never again.

**Generalization:** "Correction by annotation" is a docs anti-pattern. If the fix is small, do it inline; if it's big, rewrite the section. Stacking annotations creates its own confusion.

---

## Session 1 continuing — entries appended as friction surfaces.

### 2026-04-16 ~22:30 · SPEC-GAP · `MarketIdentity` check is genuinely different from `FleetIdentity`, not just "parallel"

Cycle 1's D auditor flagged this: `FleetIdentity` verifies `claims.op == self_operator_id`. Market can't use that check — requester and provider are intentionally different operators (self-dealing is forbidden). I wrote DD-F to resolve: market's equivalent check is `claims.pid == self.node_id` (provider identity match), not an operator-equality check.

**Generalization worth keeping:** "Parallel to X" in a spec is NEVER sufficient when X has assumption that's load-bearing for its correctness. If FleetIdentity's `op == self_operator_id` check encodes "fleet = same operator," then ANY "parallel" pattern either needs the same invariant (which might not hold) or needs an explicit replacement. Always spec the full check surface, don't gesture at "same shape."

### 2026-04-16 ~22:45 · SCOPE-AMBIG · Outbox share-vs-split was a non-decision masquerading as a decision

Cycle 1 quoted Phase 2 §III: "Same schema — the outbox holds market jobs and fleet jobs in the same table (or a parallel `compute_result_outbox` if schema separation is cleaner; implementer's call)." This is the Cycle 1 B3 blocker. "Implementer's call" in a canonical spec doc is a spec gap, not flexibility. I closed it with DD-D (reuse `fleet_result_outbox`, extend CallbackKind validation).

**Generalization worth keeping:** Phrases like "implementer's call", "TBD", "to be determined", "either/or depending on taste" in a doc that claims to be canonical are bugs. They defer a decision that HAS to be made before implementation starts, and deferring it inside a doc means the decision gets made implicitly (and inconsistently) across the implementation. Make the call in the doc.

### 2026-04-16 ~23:00 · TOOL-FRICTION · `cargo check --lib` gap rediscovered, indirectly

Not actually hit in this session, but: I was about to write a Phase 2 admission-check test harness and thought "ok just run cargo check." Then I remembered `feedback_cargo_check_lib_insufficient_for_binary.md` and `feedback_cargo_test_not_just_check.md`. Good — the memories are doing their job. But the friction is that I now have THREE distinct cargo invocations I need to remember at phase-end (`cargo check` default target, `cargo test --lib <feature>`, `cargo tauri dev`). A small shell helper or a phase-end checklist skill would remove the need to remember all three.

**Future improvement worth noting (don't build now):** A `phase-end-verify` skill that runs all three and reports status.

### 2026-04-16 ~23:30 · REDO · Had to re-verify the slug sweep after Phase 4 edit

I did the slug sweep in one pass, then edited Phase 4's bridge section, which involved writing a new block with a slug reference. Had to re-verify the sweep was still complete. Not a big deal (single grep), but a reminder that sweeps need re-verification after ANY edit in the target files, not just once.

**Generalization worth keeping:** When doing a global sweep-and-replace, add the pattern you're sweeping for to a "do not reintroduce" list that lives in the decision log. For DD-A this is just `"compute-market"` / `"storage-market"` / `"relay-market"` / `"compute-market-bridge"`. Future edits should be checked against that list.

### 2026-04-16 ~23:45 · SPEC-GAP · "parallel to async-fleet-dispatch" claim keeps needing unpacking

I've now spec'd several things as "parallel to X": `MarketIdentity` (parallel to `FleetIdentity`), `market_delivery_policy` (parallel to `fleet_delivery_policy`), `MarketDispatchContext` (parallel to `FleetDispatchContext`). Each "parallel" needs to spell out what's identical, what's different, and WHERE the difference lives. Cycle 1 D caught this as a spec gap and I resolved each one specifically — but the pattern is recurring.

**Generalization worth keeping:** Any "parallel to X" claim in a spec needs a concrete "parallel surface" spec that enumerates what's identical (reused directly) and what's different (specific replacement with stated reason). Plain "parallel" without the enumeration is a spec gap dressed as a resolution.

### 2026-04-17 early · REDO · Cycle 2 caught major propagation failures; audit-correction tables at phase-doc bottoms are the primary miss-surface

Cycle 2 A surfaced that the unification pass updated decision homes (architecture §VIII.6 + DD-J RPC table) cleanly, but the actual SQL bodies in Phase 3 §II and Phase 5 §VIII kept using the pre-rename function names. Similarly, audit-correction tables at the bottom of phase docs (Phase 2 §VIII, Phase 3 §VIII, Phase 5 §XVI) kept pre-SOTA language even after the canonical sections above them were rewritten.

**Root cause:** When applying a systemic decision (DD-A through DD-O), I updated the section that first said the thing, assumed the rest of the doc followed suit, and moved on. But a typical phase doc has THREE surfaces where the same claim shows up:
1. The canonical section (where the decision is stated)
2. The SQL body / Rust struct (where the decision is implemented)
3. The audit-correction table at the bottom (where the history of the decision is documented)

If the sweep hits only #1, the doc contradicts itself between top and bottom. Cycle 2 caught exactly this.

**Generalization worth keeping (and maybe pulling into a feedback memory):** When applying a systemic decision across a doc, grep the ENTIRE doc for every instance of the old pattern — including SQL bodies AND audit-correction tables AND cross-references. Don't assume the canonical-section edit propagates. This is the "global search on correction" principle (`feedback_global_search_on_correction.md`) applied to multi-surface docs.

### 2026-04-17 early · SPEC-GAP · Silent deferral in cancel_compute_job's relay-fee branch violated feedback_no_deferral_creep

The `NULL; -- Implementation: ...` stub in Phase 3's `cancel_compute_job` was on the Cycle 1 list as MJ-6. The unification pass did NOT address it. Cycle 2 A caught it again as M7. This was a case of "the unification pass focused on criticals and left majors in place" — which is normal for an initial sweep but means the re-audit catches the leftovers.

**Now fixed structurally:** Replaced `NULL;` with a `RAISE EXCEPTION` that makes the deferral loud. If anyone ships relay market without extending cancel, every attempted cancel of a filled relay job will error out visibly rather than silently fail to refund. Matches the `feedback_no_deferral_creep` discipline: silent deferrals are bugs; loud deferrals are OK.

**Generalization worth keeping:** Every `NULL; -- TODO:` / `NULL; -- Implementation:` in a SQL body is a silent-deferral bomb waiting to fire. Replace with `RAISE EXCEPTION` at write time, not at ship time.

### 2026-04-17 early · SPEC-GAP · DD framework didn't include a Pillar 37 hardcoded-number grep

Cycle 2 B's NM-4/NM-5/NM-6 are all Pillar 37 violations that the unification pass didn't catch because Pillar 37 wasn't an explicit DD sweep target. DD-A through DD-O covered slug naming, CallbackKind variants, RPC locations, handle predicates, struct field sets, status CHECK, paneling state transitions, min_replicas defaults — good coverage of architectural concerns. But "grep for hardcoded numbers in SQL + Rust struct definitions" wasn't on the list.

**Root cause:** I treated Pillar 37 compliance as a spot-check concern ("if I notice a hardcoded value I'll flag it") rather than a systematic sweep. The audit caught three that survived: `best_provider_rate: u64` (not flagged because I was looking at CallbackKind naming when I last touched that file), `interval '2 minutes'` in relay (not flagged because relay overlay refresh was about architecture, not SQL), `100 MB` hedging in the unification summary (not flagged because unification was about structural consolidation).

**Generalization worth keeping:** When doing a doc unification pass, the DD list should explicitly include Pillar 37 as a distinct sweep item: "grep every doc for hardcoded seconds, minutes, percentages, byte thresholds, counts — confirm each is either (a) read from a contribution with fallback-as-bootstrap-sentinel, or (b) explicitly flagged as a Pillar 37 violation to fix." Without this, Pillar 37 hedges that were carried forward from earlier drafts survive systematic sweeps.

**Implied sub-lesson:** DD list completeness is a single point of failure for sweep coverage. Every systematic thing the audit cares about must have a DD equivalent in the apply-pass plan, or the sweep misses it.

### 2026-04-17 early · SPEC-GAP · Seams §VIII treated as "reference" rather than "primary-edit target"

Cycle 2 B's NC-2 (build ordering graph still wrong) + NC-3 (parallel outbox tables still listed) are both entirely in seams §VIII. The unification pass treated seams §VIII as a doc that consumes decisions from architecture + phase docs, rather than as a primary canonical surface that needs co-edits.

**Root cause:** Seams §VIII was Thread D output from the original 2026-04-16 session — I wrote it last, after the other docs were done. Treating it as "a summary of what the other docs say" rather than "a canonical cross-market spec that needs to stay in sync" meant the apply pass skipped it.

**Generalization worth keeping:** Any cross-cutting / cross-doc summary section is a canonical surface, NOT a projection. Edits to anything it summarizes MUST update the summary in the same pass. Treating summaries as derived (and therefore editable post-hoc) is how inconsistencies slip through multi-doc refactors.

### 2026-04-17 early · CODE-DRIFT · DD-D rested on three false assumptions about shipped code — I violated my own `feedback_verify_prior_infra_upfront` memory

Cycle 2 D caught that DD-D claims `fleet_result_outbox` has a `callback_kind` column (it doesn't), that `AuthState.self_node_id` holds the Wire's node_id (that field doesn't exist), and that `validate_callback_url` is a "single-site extension" (it's currently `KindNotImplemented` for MarketStandard/Relay). All three are grounded in my reading of the async-fleet-dispatch plan doc — not the shipped code.

**Root cause:** I wrote `reference_async_dispatch_pattern.md` memory after the async-fleet-dispatch session, treating the plan doc as ground truth. When I returned to write DD-D, I trusted the memory. The memory was fine for understanding the architectural pattern but wrong about the specific shipped schema + code surface.

**This is a second instance of the same systemic failure** the Cycle 1 CallbackKind rename caught (I invented variant names that didn't exist in code). `feedback_verify_prior_infra_upfront.md` exactly warns about this class of mistake when WRITING workstream prompts. The lesson applies equally to WRITING design decisions: "parallel to X" / "reuse scaffolding" / "single-site change" claims all need to be grep-verified against the actual code before they land in a canonical spec.

**Fixes applied:**
1. Added DD-Q to architecture §VIII.6 specifying the concrete ALTER migration + code deltas to make DD-D actually buildable.
2. Updated `reference_async_dispatch_pattern.md` memory with a prominent CAUTION block listing the specific shipped-vs-target deltas (outbox column, AuthState fields, validate_callback_url stub).
3. Rewrote DD-D to honestly acknowledge what it got wrong and cross-reference DD-Q for the prereqs.

**Generalization worth keeping:** Memory entries that describe "shipped" state must be updated whenever the code diverges from plan. Treating a memory as write-once-canonical is an invitation for drift. Update memory when verifying against code reveals a gap.

**Also worth keeping:** "Parallel to X" claims need concrete grep-verification of X before landing as a decision. Not "I remember X works this way" — "I opened X, confirmed, paste the relevant lines into the DD for future readers."

### 2026-04-17 early · TOOL-FRICTION · Cycle 2 auditors ran in parallel with my fix pass; 3 of 4 flagged already-fixed issues

Cycle 2 A launched, found issues, I started fixing. Cycle 2 B/C launched in parallel; they read files while my edits were in flight. B and C's reports flagged several "already fixed" findings because their read happened before my write. A and D's reports were clean (A ran fast; D ran slow and caught my fixes).

**Not a blocker — the fixes are cheap to re-verify** — but it cost ~20 minutes of my synthesis time because I had to grep each claim against current state instead of trusting the auditor's line references.

**Generalization worth keeping:** When running parallel audit agents on a state I'm actively editing, either (a) freeze edits until all agents return, or (b) label each agent's output with the file-state timestamp it read (if the agent framework supports that). Current framework doesn't expose file-state-at-read-time. Workaround: sequential audits during active edit phases, parallel audits only during stable/committed phases.

---

## Session 2 Frictions (WS0 implementation)

### 2026-04-17 · WINS · Verifier + wanderer pattern validated on WS0

Ran the verifier-then-wanderer pattern per `feedback_wanderer_after_verifier`. Both caught real bugs the implementer (me) missed:

- Verifier: 2 MAJOR outbox filter gaps that would have cross-starved fleet/market admission budgets in production.
- Wanderer: 1 ordering bug (`CREATE INDEX` before `ALTER`) that would have broken every operator's upgrade.

Neither bug would have been caught by the pre-existing test suite. Neither was in the canonical spec — they were implementation drift that only surfaces when you try to build the thing. `feedback_wanderers_on_built_systems.md` says "wanderers on built systems catch more than wanderers on plans" — validated again.

### 2026-04-17 · FRICTION · I shipped an HTTP-scheme acceptance deviation from canonical spec

When implementing `validate_callback_url` for MarketStandard/Relay, canonical DD-Q part 3 says `!= "https"`. My commit accepted both http and https with a comment justifying "single-host dev rigs." Verifier caught it; tightened to spec.

**Root cause:** I was thinking about dev ergonomics while writing code, and the local "I want this to work without TLS" thought overrode the global "canonical spec says https" constraint. Classic local-optimization-vs-global-constraint friction.

**Generalization:** When implementing a specific spec'd decision, re-read the exact spec text for that decision immediately before writing the code. Don't rely on memory of the spec — memory drifts toward local context at write time.

### 2026-04-17 · WIN · Small, incremental commits kept verifier scope clean

WS0 landed as three commits (`6e414bf` infra, `7b303c0` verifier, `fba3723` wanderer). Each has a clear intent and a testable scope. Verifier was able to reason about WS0's delta cleanly because the infrastructure commit was isolated from the fix-pass commits. If I had bundled everything into one mega-commit, the verifier's report would have been harder to interpret (is this finding about the infra or the verifier's own additions?).

### 2026-04-17 · OBSERVATION · DD-Q's code-level specificity paid off

DD-Q wrote out the specific ALTER migration + code deltas needed to make DD-D buildable. Implementing WS0 from DD-Q was straightforward — every item in the spec mapped to a concrete code change.

Compare to earlier DDs (A-O) which were more architectural and required translation. DD-Q's "here is the exact ALTER statement" / "here is the exact match arm" shape is the right level of specificity for implementer-facing canonical decisions. For decisions that close an implementability gap (not a design fork), include the exact code or SQL. Don't leave the implementer to re-derive.

### 2026-04-17 · FRICTION · Adding the new ALTER upgrade-path test found a real bug — tests FOR tests matter

I wrote `test_fleet_outbox_pre_ws0_alter_upgrade_path` specifically because the wanderer flagged that the PRAGMA-guarded ALTER was never exercised (`test_fleet_outbox_table_creation_idempotent` only tested fresh-DB init). Writing the test surfaced an ordering bug I would otherwise have shipped (CREATE INDEX before ALTER → "no such column" on upgrade).

This is a case where adding a test that a wanderer recommended paid off immediately — the test is load-bearing in a way the original test suite wasn't. Write upgrade-path tests FIRST when shipping schema changes, not as an afterthought.


