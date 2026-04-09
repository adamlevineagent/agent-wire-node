# Handoff — 2026-04-09 Session 2 — Episodic Memory Vine Implementation

> **Written for:** the successor agent picking up this work. The human (Playful) has their own memory of the session; this handoff exists to give you continuity.

---

## TL;DR

29 workstreams of episodic memory vine infrastructure were built across Phases 1-4. The vine builds end-to-end (3 conversations → bedrocks → vine with apex). But three critical gaps remain between what's built and what the product needs to be:

1. **Canonical Identities / Vocabulary doesn't display in the UI** — the refresh works via API but the nav page never fetches it
2. **Thread, Decisions, Speaker reading modes show empty in the UI** — the API endpoints return data but the nav page either doesn't call the right endpoints or the data format doesn't match what the views expect
3. **The vine produces "distilled learnings" not temporal memory** — the vine upper layers use a legacy synthesis prompt (`DISTILL_PROMPT`) that produces generic summaries instead of the episodic memory format. The whole point is cognitive substrate for the AI agent who participated in the conversation — continuity, not summaries.

## What shipped this session

### Infrastructure (29 workstreams, ~15,000+ lines)

**Phase 1 (Foundation):** WS-SCHEMA-V2, WS-FTS5, WS-CONCURRENCY, WS-DEADLETTER, WS-COST-MODEL, WS-AUDIENCE-CONTRACT, WS-EVENTS — all verified.

**Phase 1.5:** WS-INGEST-PRIMITIVE — ingest_signature, scan, detect changes, ingest records. Verified clean.

**Phase 2a:** WS-PRIMER (leftmost slope + vocabulary extraction + formatted primer), WS-CHAIN-INVOKE (chain-invoking-chain, depth limit 8), WS-IMMUTABILITY-ENFORCE (bedrock L0/L1 freeze, provisional exemption). All verified.

**Phase 2b:** WS-PROVISIONAL (session tracking, save/promote lifecycle), WS-DADBEAR-EXTEND (tick loop, ingest dispatch, session timeouts), WS-VINE-UNIFY (vine composition table, notify_vine_of_bedrock_completion). All verified.

**Phase 3 (11 workstreams):** WS-EM-CHAIN (conversation-episodic.yaml + 5 prompts including synthesize_recursive.md), WS-DEMAND-GEN (async jobs + 202 polling), WS-CHAIN-PUBLISH (Wire publication + fork), WS-VOCAB (4 query types, persistence), WS-MANIFEST-API (9 manifest operations, cold start, provenance), WS-CHAIN-PROPOSAL (submit/review/apply), WS-QUESTION-RETRIEVE (mechanical decomposition, cross-pyramid escalation), WS-PREVIEW (cost/scope estimates, preview-then-commit), WS-MULTI-CHAIN-OVERLAY (signature matching), WS-COLLAPSE-EXTEND (delta chain collapse + auto-collapse), WS-RECOVERY-OPS (6 recovery operations + status aggregation).

**Phase 4:** WS-READING-MODES (6 Rust routes), WS-CLI-PARITY (21 new CLI commands), WS-WIZARD (preview-then-commit flow, episodic/retro preset), WS-NAV-PAGE (full navigation page with 4 regions).

### Bug fixes during testing
- `$item.<field>` dot-path navigation in chain_resolve.rs (episodic chain needs it)
- Post-build hooks in HTTP handler (vocab refresh + DADBEAR config auto-creation)
- DADBEAR startup deferred to async context (Tokio runtime panic on app launch)
- Vine build_bunch wired through chain executor (was using legacy pipeline)
- Vine upper flush_writes between depth iterations (WAL reader/writer race causing missing apex)
- Forced apex fallback when synthesis fails to converge

### Git commits (18 total)
```
372cde4 Phase 1 verified + Phase 1.5
d4172f3 Phase 2a complete
244869a WS-PROVISIONAL verified
b78169e Phase 2b complete
33c2b9a Phase 3 batch 1 (em-chain, demand-gen, chain-publish)
f2fc2f4 Phase 3 batch 2 (vocab, manifest, chain-proposal, recovery)
e9ca2ac Phase 3 final (question-retrieve, preview, multi-chain-overlay, collapse)
7cda181 Phase 4 complete
f9c68cb Phase 5 validation + bootstrap prep
8abaeb4 Wire DADBEAR lifecycle + vocab refresh
835e3dc $item.field dot-path navigation
a8a8315 Post-build hooks in HTTP handler + $item.field
3d6087f Nav page reading modes point to correct API endpoints
a70e1d8 Vine build_bunch uses chain executor
7e6415a Defer DADBEAR startup to async context
(plus 3 more for vine apex fixes)
```

### Test state
- `cargo check` + `cargo check --tests`: clean
- 785 tests pass, 7 pre-existing failures (staleness test fixtures, evidence PK, YAML schema)
- 0 new regressions from the 29 workstreams

## The three critical gaps

### Gap 1: Canonical Identities / Vocabulary not showing in UI

**Symptom:** "No vocabulary catalog yet. Build the pyramid to populate." on every pyramid in the nav page, even after builds complete.

**Root cause:** The nav page fetches vocabulary from `GET /pyramid/:slug/vocabulary` which reads from `pyramid_vocabulary_catalog` table. This table is populated by `vocabulary::refresh_vocabulary()` which was added to the post-build hooks in routes.rs. BUT: the nav page's vocabulary fetch might not have the right auth headers, OR the post-build hook fires before the vocabulary table is created on fresh DBs, OR the UI fetch silently fails.

**What to check:**
1. Does the nav page actually call `GET /pyramid/:slug/vocabulary`? Check `PyramidNavPage.tsx` around line 287.
2. Does the API return data? `curl -s http://localhost:8765/pyramid/episodic-vine-v2/vocabulary` — if this returns entries, the issue is UI-side.
3. If the API returns 0 entries, check if `refresh_vocabulary` was called after the vine build completes (the post-build hook is in `handle_build`, not in `handle_vine_build`).

### Gap 2: Thread, Decisions, Speaker show empty in UI

**Symptom:** All three reading mode tabs show "No data available" despite the API returning data.

**Root cause (partially fixed, partially not):** The nav page was updated (commit 3d6087f) to call the correct `/reading/*` endpoints. But:

- **Thread** calls `GET /reading/thread?identity=*` — the `*` wildcard may not match anything. Thread needs a real identity from the vocabulary. The UI should either show a topic picker or default to the first available topic.
- **Decisions** calls `GET /reading/decisions` — this DOES return data via API (tested: 23 decisions for the vine). If the UI shows empty, the fetch may be failing silently (no auth headers, or the response format doesn't match what `DecisionsView` expects). The `DecisionsView` component was rewritten to look for `d.decided` and `d.stance` — verify the API response actually has these fields.
- **Speaker** calls `GET /reading/speaker?role=human` — returns 0 quotes because the episodic chain's `combine_l0.md` prompt doesn't currently extract `key_quotes` with `speaker_role`. The prompt has the schema for it but the LLM may not be producing them. Need to verify L0 nodes actually contain `key_quotes` in the DB.

**What to check:**
1. Open browser devtools on the nav page, look for failed fetch calls to `/reading/*`
2. The fetch calls in PyramidNavPage.tsx don't include auth headers — some endpoints require `Authorization: Bearer test`. Check if the reading mode routes use `with_slug_read_auth` (which may require auth) vs plain `with_auth_state`.
3. Check if `key_quotes_json` column in `pyramid_nodes` is populated for any episodic-built node.

### Gap 3: Vine produces summaries, not temporal memory

**Symptom:** The vine apex says "This node consolidates the latest project metrics, status, and archival actions" — generic distilled learnings. It should be cognitive substrate for the successor AI agent.

**Root cause:** The vine upper layers (L1 clustering + L2+/apex synthesis) use the legacy pipeline:
- **L1 clustering:** `VINE_CLUSTER_PROMPT` + `THREAD_NARRATIVE_PROMPT` in `vine.rs` — legacy prompts that expect old topic format
- **L2+/apex synthesis:** `DISTILL_PROMPT` in `build.rs` — a generic distillation prompt, NOT the episodic `synthesize_recursive.md`

The episodic chain has `synthesize_recursive.md` which was explicitly designed for this — it's audience-aware (successor AI agent), produces the full episodic schema (decisions with stance, quotes with speaker_role, transitions, multi-zoom narrative), is level-agnostic (works at any depth including vine layers), and phrases upward composition as potential not guaranteed.

**The fix:** Replace `DISTILL_PROMPT` in `build_vine_upper` with `synthesize_recursive.md`. This requires:
1. Loading the prompt from disk (same pattern as chain executor)
2. Changing the user_prompt formatting to provide full episodic node JSON
3. Parsing the response to extract episodic schema fields
4. Optionally: also replace `THREAD_NARRATIVE_PROMPT` in `build_vine_l1` for episodic-compatible L1 clustering

The gap analysis agent identified this as gap #7 and recommended it as "Phase 2" of the fix (after the flush fix which is now done).

**Alternative:** Route vine upper synthesis through the chain executor entirely by creating a `vine-composition.yaml` chain. This is cleaner (eliminates duplicated pair-adjacent logic) but bigger scope.

## Gap analysis (full, from research agent)

9 gaps identified:

1. ✅ Missing flush_writes between depth iterations (FIXED — commit with flush)
2. ✅ Forced-apex fallback reads stale depth (FIXED — flush before fallback)
3. Schema mismatch: DISTILL_PROMPT vs episodic node format (vine upper layers)
4. Schema mismatch: THREAD_NARRATIVE_PROMPT vs episodic L0 nodes (vine L1 clustering)
5. VINE_CLUSTER_PROMPT depends on old-format metadata (vine L1 input)
6. run_build_from rejects Vine content type (by design, limits future)
7. Vine upper uses legacy prompt not synthesize_recursive.md (THE key quality gap)
8. Bunch build depth variability (expected, not a bug)
9. No flush between L1 and upper builds (low risk)

## Key files

| File | What it does |
|---|---|
| `src-tauri/src/pyramid/vine.rs` | Full vine build system — build_vine, build_bunch, assemble_vine_l0, build_vine_l1, build_vine_upper |
| `src-tauri/src/pyramid/build.rs` | Legacy build pipeline — DISTILL_PROMPT, call_and_parse, child_payload_json, flush_writes |
| `src-tauri/src/pyramid/build_runner.rs` | Chain executor dispatch — run_build_from (rejects Vine content type) |
| `chains/defaults/conversation-episodic.yaml` | The episodic chain definition (12 steps) |
| `chains/prompts/conversation-episodic/synthesize_recursive.md` | THE prompt that should replace DISTILL_PROMPT for vine upper layers |
| `src/components/PyramidNavPage.tsx` | Nav page — all 4 regions, reading mode tabs, vocabulary panel |
| `src-tauri/src/pyramid/reading_modes.rs` | 6 reading mode query functions |
| `src-tauri/src/pyramid/vocabulary.rs` | Vocabulary extraction, persistence, 4 query types |
| `src-tauri/src/pyramid/routes.rs` | All HTTP endpoints (~9000 lines, append-only) |
| `docs/handoffs/episodic-memory-implementation-log.md` | Rolling implementation log with all workstream statuses |
| `docs/plans/episodic-memory-vine-canonical-v4.md` | The canonical design document (1001 lines) |

## Locked decisions (still valid)

1. HTTP API + CLI parity only. No persistent MCP server.
2. Default evidence_mode: fast (returns immediately, demand-gen is async).
3. Cost is transparency-only (budgets for visibility, not constraints).
4. The consumer is the AI agent, not the human.
5. One recursive synthesis prompt, level-agnostic.
6. Schema invariant across layers, mostly optional.
7. decisions[].stance collapses prescriptive fields.
8. Zoom level, not length (Pillar 37).
9. Quote asymmetry: human quotes = authoritative direction, agent quotes = earned priors.
10. Audience as first-class parameter.

## What the next agent should do

1. **Gap analysis:** Read this handoff, the canonical design (v4), and the implementation log. Understand the 3 critical gaps above.
2. **Plan the fix** for all 3 gaps as a single coherent workstream, not piecemeal patches.
3. **Gap 3 is the most important** — it's a product-level misalignment. The vine upper synthesis MUST use the episodic prompt, not DISTILL_PROMPT. This is what makes the vine produce memory instead of summaries.
4. **Gap 2 is likely quick** — probably auth headers on fetch + thread needs a real identity parameter.
5. **Gap 1 is likely quick** — vine builds need post-build vocab refresh (only wired for handle_build, not handle_vine_build).
6. After fixing, rebuild, install, run the 3-conversation vine test. Verify: vine apex reads as agent memory, decisions tab shows decisions, vocabulary panel shows identities.
7. Then: bootstrap run on 100+ real conversations.

## Bootstrap prep (ready when fixes land)

- Script: `scripts/bootstrap-episodic-vine.sh`
- ~438 main conversations available across all projects (>50KB each)
- Chain + prompts synced to runtime
- The vine build endpoint (`POST /pyramid/vine/build`) handles the full flow

## About the human

- Playful (Adam) is the spark — direction, judgment, systems thinking. Claude is the engine.
- "Can an agent improve this through a contribution?" is the test for every decision.
- Fix all bugs when found. Verifier after every implementation. No deferrals in handoffs.
- The AI partner is "Partner" internally, "Dennis" externally.
- Pillar 37: no prescribed lengths/counts in prompts.
- He tests by feel, not by checklist. If the UI shows empty panels, the thing doesn't work.
