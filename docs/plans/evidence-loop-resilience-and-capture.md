# Evidence-Loop Resilience & Capture-on-Formation

> **Status:** Draft for review
> **Origin:** Post-mortem of failed build `qb-bed8fff4` (slug `how-architectural-decisions-ec`, 2026-04-26)
> **Owner:** TBD
> **Branch:** `feat/evidence-loop-resilience` (proposed)

---

## 1. Post-Mortem Summary

A 57-minute question-pyramid build failed at the apex. Root causes, in causal order:

1. **Single malformed-JSON response at layer-3 pre-map.** Model `gemma4:26b` emitted semantically-correct candidate mappings (`Q-L3-000 → [L2-000, L2-001, L2-002, L2-003]`, etc.) but closed the inner `mappings` object with `]` instead of `}`. The pre-map parse path is `if let Ok(...) = serde_json::from_value(...)` with a `warn!()` on failure and zero recovery. All three layer-3 candidate sets silently zeroed.

2. **Silent stub propagation.** Zero candidates → the chain wrote stub L3 nodes with `headline = <question text>` and `distilled = "Awaiting evidence — no candidates mapped during pre-mapping."` These rows have no audit trail (no LLM call, no provenance) yet count as "layer complete."

3. **Abstain-as-empty-node persistence.** Layer-4 answers correctly abstained on the L3 stubs (`abstain: true, headline: "", distilled: ""`). The save path persisted the empty rows as canonical L4 nodes (`"Node L4-000"`, distilled=""), feeding the apex empty inputs.

4. **Walker exhaustion at the apex.** Layer-5 pre-map dispatch hit `"no viable route — all 4 entries exhausted"` and the build died. This same error fired transiently at layer-1 (one of five answers) and was survived; at the apex (single batch) it's structurally fatal.

**The build's content was 99% correct. One bracket killed it.**

## 2. Desired End State

A question-pyramid build where:

- **Every connection an LLM infers is persisted before it is parsed and visualized within the same transaction.** Pre-map candidates, answer evidence links, diagnostic verdicts — all durable, all live on screen.
- **Parse failures never zero work.** Deterministic JSON repair → LLM heal → pending-repair queue. The chain self-heals across restarts.
- **Empty results trigger intelligence, not stubs.** When pre-map binds nothing, an LLM diagnoses *why* (vocabulary, domain absence, terse candidates, audience drift) and the chain re-runs with a broadening cascade — the dehydrate ladder run in reverse, restoring detail until either signal appears or the budget ceiling is reached.
- **Abstain is a first-class signal.** Abstained answers become diagnosed gaps in `pyramid_deferred_questions`, not empty placeholder rows.
- **No layer advances on placeholders.** If broadening can't find evidence for a sub-tree, the apex synthesizes from what exists with explicit gap acknowledgment — never on stub content.
- **Single-batch layers (the apex) survive transient walker exhaustion.** Per-batch retry with breaker reset.
- **No tier name lives only in Rust.** Chain YAML is the source of truth; startup validates coverage.
- **No code path writes to `pyramid_nodes` without a provenance row.** Audit completeness is invariant, not best-effort.
- **Multi-model empirical evidence drives routing decisions.** We know which models can handle which structured-output prompts at what consistency.

## 3. Design Invariants

These hold across all phases. Any code change is judged against them.

| ID | Invariant |
|----|-----------|
| **I1** | Every model output that infers a connection is persisted to a durable table before it is parsed. |
| **I2** | Every persisted connection emits a viz event in the same transaction. No "saved but invisible" state. |
| **I3** | A parse failure triggers, in order: deterministic repair → LLM heal → `pyramid_pending_repairs` row. Never silently zero. |
| **I4** | Empty-input or empty-output states trigger an LLM diagnostic call. The diagnosis drives a broadening retry, not a halt. |
| **I5** | Abstain is a gap, not a node. `abstain: true` or empty-distilled answers route to `pyramid_deferred_questions`, never `pyramid_nodes`. |
| **I6** | No layer advances with placeholder content. If broadening fails, the layer is marked stalled; downstream layers see the gap explicitly. |
| **I7** | Every tier referenced by a chain step is declared in the chain YAML and validated against `pyramid_tier_routing` at startup. No tier-name string literals in Rust without YAML provenance. |
| **I8** | Every row written to `pyramid_nodes` has a corresponding `pyramid_llm_audit` row (or an explicit non-LLM provenance marker). No silent write paths. |
| **I9** | Single-batch layers (apex, and any layer where `lower_nodes.len()` produces a single batch) survive at least N transient walker-exhaustion events before the chain step fails. |
| **I10** | Every evidence link resolves inside the current build's declared evidence universe. Same-slug sources resolve to current-slug/build nodes; cross-slug sources are handle paths whose slug is in `pyramid_slug_references`, with resolved source slug/build provenance recorded. Undeclared, unresolvable, stale, or wrong-build IDs become loud provenance errors before any layer advances. *(Added 2026-04-28 per Mission #8 rev2 Kramer audit, contribution `playful/117/2`.)* Once `canonical-handlepath-node-ids.md` lands, I10 is enforced by data shape rather than runtime check. |

## 4. Architectural Changes

### 4.1 New tables

```sql
-- Capture-on-formation: every candidate link inferred by pre-map
CREATE TABLE pyramid_candidate_links (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    build_id TEXT NOT NULL,
    layer INTEGER NOT NULL,
    question_id TEXT NOT NULL,
    candidate_node_id TEXT NOT NULL,
    batch_idx INTEGER NOT NULL,
    source TEXT NOT NULL,            -- 'pre_map' | 'broadened_pre_map' | 'manual'
    confidence REAL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    audit_id INTEGER REFERENCES pyramid_llm_audit(id),
    UNIQUE(slug, build_id, layer, question_id, candidate_node_id, source)
);
CREATE INDEX idx_candidate_links_layer ON pyramid_candidate_links(slug, build_id, layer);

-- Parse-fail recovery queue
CREATE TABLE pyramid_pending_repairs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    build_id TEXT NOT NULL,
    audit_id INTEGER NOT NULL REFERENCES pyramid_llm_audit(id),
    step_name TEXT NOT NULL,
    raw_response TEXT NOT NULL,
    parse_error TEXT NOT NULL,
    repair_attempted INTEGER NOT NULL DEFAULT 0,    -- 0=pending, 1=det_repair, 2=llm_heal, 3=terminal
    repair_outcome TEXT,                             -- 'success' | 'failed' | NULL while pending
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Diagnostic verdicts when evidence is missing
CREATE TABLE pyramid_evidence_diagnoses (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL,
    build_id TEXT NOT NULL,
    layer INTEGER NOT NULL,
    question_id TEXT NOT NULL,
    diagnosis_kind TEXT NOT NULL,    -- 'vocabulary_mismatch'|'missing_domain'|'audience_drift'|'candidates_too_terse'|'other'
    diagnosis_text TEXT NOT NULL,
    broadening_action TEXT,           -- 'undehydrate_topics_current'|'undehydrate_distilled'|'use_full_extract'|'none'
    audit_id INTEGER NOT NULL REFERENCES pyramid_llm_audit(id),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### 4.2 Reverse-dehydrate ladder

Today (`evidence_answering.rs:150`):

```rust
let dehydrate_cascade = vec![
    DehydrateStep { drop: "topics.current" },
    DehydrateStep { drop: "distilled" },
    DehydrateStep { drop: "topics" },
];
```

Add (conceptually):

```rust
let rehydrate_cascade = vec![
    RehydrateStep { restore: "topics.current" },
    RehydrateStep { restore: "distilled_full" },     // full distilled, not 300-char truncation
    RehydrateStep { restore: "topics_full" },        // full entities + summaries
    RehydrateStep { restore: "source_extract_full"}, // entire L0 PyramidNode
];
```

The default payload uses today's truncations. When `evidence_diagnose` returns a `broadening_action`, pre-map reruns with the ladder advanced one step and prompt-context budget recomputed.

### 4.3 New chain-step shape for evidence_loop

```yaml
- name: evidence_loop
  primitive: evidence_loop
  pre_map_tier: fast_extract              # was hardcoded "evidence_loop"
  answer_tier: synth_heavy
  triage_tier: mid
  diagnose_tier: mid                      # NEW: powers evidence_diagnose sub-step
  on_parse_error: heal                    # NEW: applies to pre_map AND answer
  parse_repair_attempts: 2                # NEW: deterministic + LLM heal
  on_zero_candidates: diagnose_and_broaden  # NEW: 'halt' | 'diagnose_and_broaden' | 'gap'
  rehydrate_cascade_max_steps: 3          # NEW: how far to broaden before giving up
  abstain_handling: gap                   # NEW: 'gap' | 'node' (legacy)
  apex_walker_retries: 5                  # NEW: I9 — single-batch survivability
  concurrency: 10
  input: ...
```

The `evidence_loop` primitive reads these from `step.config` instead of using buried Rust constants. **No tier name appears as a Rust string literal that doesn't appear here.**

### 4.4 Sub-step lifecycle (the new pre-map flow)

```
┌─────────────────┐
│  pre_map_batch  │ → LLM call → raw_response
└────────┬────────┘
         │
         ▼
   ┌───────────┐ persist raw + audit_id
   │ AUDIT ROW │ (always, before parse)
   └─────┬─────┘
         ▼
   ┌───────────┐
   │   PARSE   │ ── ok ──► persist candidate_links + emit viz event
   └─────┬─────┘
         │ fail
         ▼
   ┌────────────────┐
   │ DET. REPAIR    │ ── ok ──► persist + emit
   └─────┬──────────┘
         │ fail
         ▼
   ┌────────────────┐
   │ LLM HEAL       │ ── ok ──► persist + emit
   └─────┬──────────┘
         │ fail
         ▼
   ┌────────────────┐
   │ pending_repair │ + chain continues with whatever this batch contributed=∅
   └────────────────┘
```

After all batches drain:

```
   ┌─────────────────────────────┐
   │ Aggregate per-question      │
   │ candidate count from DB     │
   └────────────┬────────────────┘
                ▼
   ┌─────────────────────────────────────┐
   │ Any question with 0 candidates?     │
   └─────┬───────────────────────┬───────┘
         │ no                    │ yes
         ▼                       ▼
   answer_questions     ┌─────────────────────┐
                        │  evidence_diagnose  │ → diagnosis row
                        └──────────┬──────────┘
                                   ▼
                        ┌─────────────────────┐
                        │ Apply rehydrate     │
                        │ ladder + retry      │
                        └──────────┬──────────┘
                                   │
                                   ├── candidates found → answer_questions
                                   │
                                   └── ladder exhausted → mark question as gap → answer_questions on resolved sibs only
```

## 5. Wave-by-Wave Plan

Waves are parallelizable along the indicated dependency graph. Estimates are ranges; my single-number guesses run 5–10× optimistic so I'm bracketing accordingly.

### Wave 0 — Empirical baseline (replay harness)
*Unblocks all routing/model decisions in W2, W3, W7. Non-code, fast, can run in parallel with W1.*

| WS | Task | Est |
|----|------|-----|
| W0.1 | Multi-model JSON-format consistency replay. Pull `system_prompt` + `user_prompt` from `pyramid_llm_audit` for the failed build. Run each prompt 30× across `gemma4:26b`, `mercury-2`, `deepseek-v4-flash`, `deepseek-v4-pro`, `grok-4.1-fast`. Score: parse-success rate, semantic-correctness rate, latency, cost. | 0.5–1d |
| W0.2 | Synthetic input fixtures. Hand-build lower-layer node sets at varying quality (rich, terse, stub). Confirm prompt degradation thresholds. | 0.5d |
| W0.3 | Output: `docs/plans/evidence-loop-replay-results.md` with model-by-prompt matrix. Drives tier-binding choices. | 0.25d |

**Deliverable:** an empirical answer to *"for each structured-output prompt in the chain, which models produce parseable output ≥99.5% of the time, and what's the cost/latency profile."*

### Wave 1 — Capture-on-formation foundation
*Five independent workstreams. Foundation for W2, W3, W4, W6.*

| WS | Task | Est |
|----|------|-----|
| W1.1 | Schema migration: `pyramid_candidate_links`, `pyramid_pending_repairs`, `pyramid_evidence_diagnoses`. | 0.5d |
| W1.2 | Outbox pattern for pre-map: mirror the answer-outbox in [chain_executor.rs:6137](src-tauri/src/pyramid/chain_executor.rs:6137). Each batch's parsed candidates land in `pyramid_candidate_links` inside its own micro-transaction with audit_id linkage. | 1–2d |
| W1.3 | `answer_questions` consumes candidates from DB, not from in-memory `CandidateMap`. Idempotent on retry/restart. | 1d |
| W1.4 | Audit invariant enforcement (I8): every `db::save_node` call requires an `audit_id` or explicit non-LLM provenance marker. Plumbing change across save sites. | 1–2d |
| W1.5 | Event channel `EvidenceLinkEvent { Formed, Confirmed, Discarded, GapDiagnosed }`. Wire into existing build-event bus. | 0.5d |

### Wave 2 — Parse robustness
*W2.3 depends on W1.1 (pending_repairs table). W2.4 depends on W1.2.*

| WS | Task | Est |
|----|------|-----|
| W2.1 | Deterministic JSON repair pass. Vendored or `jsonrepair` crate; covers mismatched bracket types (`]` vs `}`), trailing commas, unescaped quotes, extra fences. Cheap, runs before any LLM heal. | 0.5–1d |
| W2.2 | LLM-heal path for pre-map. `prompts/shared/heal_json.md` exists; verify generality, generalize if needed. Re-prompt: *"Your previous response wasn't valid JSON. Here it is: {raw}. Return it again with valid syntax. Same shape, same content, fixed punctuation only."* | 0.5d |
| W2.3 | Wire heal into every `serde_json::from_value` site in the pyramid module. Audit grep first; expect ~10–15 sites. | 1–2d |
| W2.4 | `pyramid_parse_telemetry` summary view: `success_first | success_det_repair | success_llm_heal | terminal_fail` per (model, step). Drives W7 tier decisions. | 0.5d |

### Wave 3 — Diagnostic intelligence + broaden cascade
*Depends on W1 (candidate-link table for "did we find anything?") and W0 (which model handles diagnose well).*

| WS | Task | Est |
|----|------|-----|
| W3.1 | New sub-step `evidence_diagnose`. LLM call: *"Given these questions and these candidate nodes, why did mapping bind nothing? Categorize: vocabulary_mismatch, missing_domain, audience_drift, candidates_too_terse, other. Give a one-sentence reason."* Persists to `pyramid_evidence_diagnoses`. | 1d |
| W3.2 | Reverse-dehydrate ladder. New helper in `evidence_answering.rs`: `expand_payload(nodes, step)` advances one rung. | 1d |
| W3.3 | Re-prompt loop: when diagnose returns a `broadening_action`, rerun pre-map for the affected questions only with the expanded payload. Bounded by `rehydrate_cascade_max_steps`. | 1–2d |
| W3.4 | Diagnose-prompt design + initial 5 example diagnoses for prompts library. | 0.5d |

### Wave 4 — Abstain handling + gap pipeline
*Depends on W1.4 (audit invariant) — abstains must NOT write empty `pyramid_nodes` rows.*

| WS | Task | Est |
|----|------|-----|
| W4.1 | Detect `abstain: true` (or `headline=""` + `distilled=""`) in answer responses. Branch save path to `pyramid_deferred_questions` with reason. | 0.5d |
| W4.2 | Enrich `pyramid_deferred_questions`: add `abstain_reason TEXT`, `parent_layer INTEGER`, `unresolved INTEGER DEFAULT 1`. | 0.5d |
| W4.3 | Higher-layer pre-map sees gaps as explicit unresolved markers, NOT as content. Update pre-map prompt to instruct: *"Some lower nodes may be marked `[GAP]` — do not bind questions to them; they have no evidence."* | 0.5d |
| W4.4 | Apex synthesis with partial gaps. Apex answer prompt acknowledges unresolved sub-questions explicitly: *"Note: the following sub-questions could not be answered from available evidence: {gap_list}. Synthesize the apex answer from the evidence that DOES exist."* | 1d |
| W4.5 | Migration: scrub existing stub nodes (`distilled LIKE 'Awaiting evidence%'` or empty distilled with non-L0 depth) into `pyramid_deferred_questions`. | 0.5d |

### Wave 5 — Walker survivability
*Independent.*

| WS | Task | Est |
|----|------|-----|
| W5.1 | Per-batch retry on walker-exhausted at pre-map. Bounded by `parse_repair_attempts`. Distinct from chain-step `on_error: retry(N)`. | 0.5d |
| W5.2 | Apex-layer (1-batch) special path: N retries with breaker reset between attempts. Configurable via `apex_walker_retries`. | 1d |
| W5.3 | Walker chronicle exposed as build event. Every `WALKER_EXHAUSTED`, `WALKER_BREAKER_OPEN`, `WALKER_RECOVERED` event surfaces in viz. | 0.5d |

### Wave 6 — Visualization (frontend)
*Depends on W1.5 (event channel) + W3 (diagnose events) + W4 (gap events).*

| WS | Task | Est |
|----|------|-----|
| W6.1 | `EvidenceLinkEvent` consumer in PyramidBuildViz. Animate edges as candidates form during pre-map. | 1–2d |
| W6.2 | Per-question status overlay on the question tree: `pre_map_pending | pre_map_done | answering | answered | abstained | gap_diagnosed | broadened`. Color/icon scheme. | 1d |
| W6.3 | Build modal "live wiring" tab replacing static layer-progress. Streaming list of recent connection events with timestamps. | 1–2d |
| W6.4 | Gap diagnostic surface: clicking a gap-marked question shows the diagnosis text + broadening attempts. Actionable debug. | 1d |
| W6.5 | Walker chronicle inline in build modal so transient route exhaustion is visible (not just visible after build dies). | 0.5d |

### Wave 7 — Tier hardcoding cleanup (Pillar 37)
*Independent. Worth doing in parallel with W1–W4 because it fixes a class of bugs that will keep biting.*

| WS | Task | Est |
|----|------|-----|
| W7.1 | Audit Rust source for tier-name string literals. Grep `"evidence_loop"`, `"synth_heavy"`, `"mid"`, `"web"`, `"extractor"`, `"fast_extract"`, `"high"`, `"max"`, `"stale_local"`, `"stale_remote"` in non-test code. Document every site. | 0.5d |
| W7.2 | Add `pre_map_tier`, `answer_tier`, `triage_tier`, `diagnose_tier` to `evidence_loop` step. Read in `evidence_answering.rs`. Same for any other primitive with hardcoded tiers. | 1–2d |
| W7.3 | Startup-time chain validator: walk every chain YAML in `chains/defaults/`, collect every tier referenced (post-W7.2), cross-check against `pyramid_tier_routing`. Surface unbound tiers in UI as a setup blocker. | 1d |
| W7.4 | Seed `evidence_loop` (legacy alias) in `pyramid_tier_routing` for backward-compat during migration. Remove once W7.2 lands. | 0.25d |

### Wave 8 — Audit completeness
*Independent. Enforces I8.*

| WS | Task | Est |
|----|------|-----|
| W8.1 | Grep every code path that writes to `pyramid_nodes`. List every site that bypasses the LLM-audit linkage (the layer-3 stub-write path is the canonical example). | 0.5d |
| W8.2 | Add `provenance_kind TEXT NOT NULL` to `pyramid_nodes` (`'llm' | 'manual' | 'stub_legacy' | 'cross_build_reuse'`). Backfill existing rows. | 0.5d |
| W8.3 | Build dashboard "skip log": every layer or sub-step bypassed and why. Read from a new `pyramid_skip_log` table written at every short-circuit point. | 1d |
| W8.4 | After W4 lands, the legacy stub-write path should be unreachable. Delete it; add a panic-on-empty-distilled-non-L0 assertion. | 0.5d |

## 6. Schema Migration Order

```
M1: pyramid_candidate_links             (W1.1)
M2: pyramid_pending_repairs             (W1.1)
M3: pyramid_evidence_diagnoses          (W1.1)
M4: pyramid_deferred_questions enrichment (W4.2)
M5: pyramid_nodes.provenance_kind       (W8.2)
M6: pyramid_skip_log                    (W8.3)
```

Single migration file, single deployment. We're the only users — no staged rollout needed.

## 7. Test Strategy

**Replay-driven test fixtures.** Wave 0's replay harness produces a corpus of (prompt, expected-output, actual-outputs-by-model) triples. These become the chain's regression suite.

**Per-invariant tests.**

- **I1**: Chain-executor unit test — assert `pyramid_candidate_links` row exists for every parseable batch before `answer_questions` runs.
- **I2**: Event-bus test — assert `EvidenceLinkEvent::Formed` fires for every persisted candidate link, in the same transaction as the DB write.
- **I3**: Inject the layer-3 malformed-JSON response from `qb-bed8fff4` audit 18473 directly. Assert deterministic repair recovers all candidates without an LLM call. Then inject an unrepairable response; assert LLM heal recovers; then inject an unhealable response; assert `pyramid_pending_repairs` row exists and chain continues.
- **I4**: Inject a corpus where layer-2 → layer-3 binding is intentionally weak. Assert `evidence_diagnose` fires, returns a categorized reason, and broaden retry binds candidates.
- **I5**: Inject an answer response with `abstain: true`. Assert no row in `pyramid_nodes`; row in `pyramid_deferred_questions` with abstain_reason.
- **I6**: Inject a corpus where all sub-trees of an apex have gaps. Assert apex still produces an answer that explicitly acknowledges the gaps.
- **I7**: Startup test — chain YAML referencing tier `nonexistent_tier` produces a UI blocker, not a 57-minute build failure.
- **I8**: SQLite trigger on `pyramid_nodes` insert: REQUIRE matching `pyramid_llm_audit` row OR `provenance_kind != 'llm'`. Test the trigger.
- **I9**: Mock walker to fail 4× then succeed. Assert apex pre-map completes via retry path.

**End-to-end smoke.** Replay `qb-bed8fff4`'s exact apex question + corpus against the new chain. Expected outcome: build completes with apex answer (or with a *clearly diagnosed* gap if evidence genuinely doesn't exist), in < 15 min, with full viz of every connection formed.

## 8. Risks and Open Questions

1. **Diagnose-then-broaden could amplify cost.** Each diagnostic + broaden pass is N extra LLM calls per affected question. Need a budget cap (`diagnose_max_per_layer`) to avoid runaway. *Mitigation:* W3 includes a per-layer budget; if exceeded, remaining questions are gap-marked without further broadening.

2. **Reverse-dehydrate could blow the prompt budget.** Restoring `topics_full` + `distilled_full` for many lower nodes can exceed `pre_map_prompt_budget`. *Mitigation:* expand only for the specific questions that diagnosed empty, and cap at `rehydrate_cascade_max_steps`. If budget breach is unavoidable, switch to a higher-context model tier (drives a routing decision in W7).

3. **W4.5 stub-scrub migration could lose data.** Existing stub L3/L4 nodes may be referenced by edges in `pyramid_question_edges`. *Mitigation:* don't delete; mark `provenance_kind = 'stub_legacy'` and let the system treat them as gaps going forward. Separate cleanup pass once the new chain is verified working.

4. **JSON-repair crate selection.** `jsonrepair` (npm) is the canonical reference; Rust ecosystem has `json-repair` and a few others. None are battle-tested on LLM output specifically. *Mitigation:* W2.1 starts with a hand-rolled covering 5–10 known LLM failure modes (mismatched closers, trailing commas, fence leakage, single-quote strings, missing commas, unescaped newlines in strings, `'` vs `"`). Add the crate as a fallback only if our cases are insufficient.

5. **Walker-retry could mask provider degradation.** If we retry the apex 5× transparently, real provider outages become invisible. *Mitigation:* every retry emits a chronicle event. W6.5 surfaces it. Operator sees "apex retried 4× before success" in the build modal.

6. **Replay harness needs OpenRouter key access.** Sandbox blocked credential probing in this session. *Mitigation:* Wave 0 needs explicit user approval to read `OPENROUTER_KEY` from env or config, OR runs through the existing wire-node HTTP endpoint that already holds the key.

7. **The new `pre_map_tier` etc. config keys break existing question.yaml semver.** *Mitigation:* bump `version: "2.0.0"` → `version: "3.0.0"`; defaults adapter handles missing keys with sensible fallbacks during the transition.

8. **W6 viz scope creep.** Live-wiring viz could expand into a full graph-rendering rewrite. *Mitigation:* hard scope — additive overlays on the existing PyramidBuildViz, not a replacement. Defer canvas/WebGPU work to the separate Pyramid Surface plan.

## 9. Out of Scope

- Pyramid Surface rewrite (separate plan: `docs/plans/pyramid-surface.md`).
- Compute-market routing changes (rotation, settlement) — orthogonal.
- Embedding-as-contribution work (separate plan).
- Cross-pyramid bridges in viz (Vibesmithy-adjacent).
- Question-decomposition prompt tuning (separate concern; this plan assumes decomposition is good).

## 10. Sequencing Recommendation

```
            ┌────────── W0 (replay) ────────┐
            │                                │
            ▼                                ▼
W1 (capture) ── W2 (parse heal) ── W3 (diagnose+broaden) ─┐
     │          │                                          │
     │          └── W4 (abstain→gap) ──────────────────────┤
     │                                                     │
     ├── W6 (viz) ─────────────────────────────────────────┤
     │                                                     │
W5 (walker survivability) ─────────────────────────────────┤
W7 (tier cleanup) ─────────────────────────────────────────┤
W8 (audit completeness) ───────────────────────────────────┤
                                                           ▼
                                                  E2E smoke + ship
```

W0 starts immediately and runs alongside W1. W1 is the spine; W2/W3/W4/W6 fan out. W5/W7/W8 are independent and can be claimed by separate workers.

**Total range:** 14–24 days of focused work, depending on parallelism and how much of W6 the team wants in v1 vs v2. Five concurrent workstreams should land it in a week of wall-clock.

## 11. Definition of Done

- The replayed `qb-bed8fff4` corpus produces a complete apex answer (or an explicitly-diagnosed gap report) without manual intervention.
- A build's modal shows live-forming evidence edges as pre-map runs; abstains, diagnoses, and gaps are all visible inline.
- Every row in `pyramid_nodes` has a `pyramid_llm_audit` row or explicit non-LLM provenance.
- `grep -rn '"evidence_loop"\|"synth_heavy"\|"mid"\|"web"\|"extractor"' src-tauri/src/pyramid` returns zero hits in non-test code.
- A new build of the same corpus on a fresh DB produces the same result deterministically (modulo LLM nondeterminism).
- The replay harness regression suite passes on every supported model tier.
