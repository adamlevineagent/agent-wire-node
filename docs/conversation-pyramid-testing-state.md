# Conversation Pyramid Testing — State as of 2026-04-07

Status: four iterations of test pyramids built from the same source (a single Claude Code session `.jsonl`, ~95 chunks). Iterating on the question pipeline as applied to sequential transcripts. This doc captures what worked, what didn't, and what's next.

## Source material

All four test pyramids built from the same input file:
- `~/.claude/projects/-Users-adamlevine-AI-Project-Files-agent-wire-node/3642d847-492c-4534-aefe-20b800ca9264.jsonl`
- A live, in-progress Claude Code session (the very session where we did all this work)
- Ingested by `pyramid::ingest::ingest_conversation` into ~95 chunks of ~100 lines each
- Every line is prefixed with `--- PLAYFUL [iso-timestamp] ---` or `--- CONDUCTOR [iso-timestamp] ---` markers (verified in `ingest.rs:171-238`)

## Run history

### Run 1 — `claudeconvotest`
- **Apex question:** "What are the key themes, decisions, and evolution across these conversations?" (the legacy default)
- **Result:** crash before build started. `characterize` step failed with `Source path '...jsonl' is not accessible and no L0 fallback available`. Root cause: `build_folder_map` in `question_decomposition.rs:2060` only handled directory inputs, not single-file sources. Conversation pyramids point at a single .jsonl file.
- **Fix:** `build_folder_map` now handles file paths by emitting a one-line "Source file: ... Name: ... Size: ... bytes" map. Commit `48cd70b`.

### Run 2 — `claudeconvotest2`
- **Apex question:** same legacy default.
- **Result:** built clean. 114 nodes. Tested with a haiku agent.
- **Verdict:** **8/10** — the pyramid usefully represented the session. The haiku tester found:
  - Apex correctly carved the session into the five real workstreams (audit, parallel work, web UI, auth, pyramid architecture)
  - Captured specific bugs by name and fix: `display_headline` truncation, `build_folder_map` single-file crash, `updated_at` schema mismatch, CSRF placeholder, stale slug-stats apex bug
  - Captured architectural decisions: unified question-pipeline routing, apex-first home redesign, private-as-default tier
  - Pulled real commit SHAs (2dc2a40, 48cd70b)
- **Gaps the haiku tester called out:** doesn't isolate prompt-injection defense as its own topic (gets lumped into generic "hardening"); no systematic git history; missing manual-test repro steps and perf metrics.

### Run 3 — `claudeconvotest3chronofocusconcurrent`
- **Apex question (much improved):** "Tell the story of this chat session in chronological order: what was attempted, what failed, what was learned, what was decided, and what shipped? What was true at the beginning of the sequence that was not true at the end, and vice-versa?"
- **First build:** 115 nodes, 1 failure.
- **Rebuild:** 109 nodes, 0 failures.
- **Result:** **the rebuild was worse than the first run.** The L2 apex was excellent ("From disciplined planning to a shipped multi-modal app: the session's chronological story" — used real names, real commit SHAs, real workstream codes WS-A through WS-K, named what was true at start vs end). But the **leftmost L1 became a hallucinated meta-node**: "Purpose and Value of This Chat Session", with every single L0 evidence child marked DISCONNECT, and a distilled body of pure generic copy ("The main goal of this conversation is to turn the technical logs into a short, plain-language briefing for project stakeholder Adam Levine...").
- **Two compounding bugs identified:**
  1. The decompose step (`decompose.md`) was allowed to ask meta-questions about the transcript-as-artifact ("what is the purpose of this chat session?") instead of about its contents. When the decomposer hits a sequential transcript, it falls into the same "what does this system DO" framing it uses for code repos and produces unanswerable meta-questions.
  2. The answer step (`answer.md`) had no all-disconnect guard. When every assigned L0 returned DISCONNECT, the synthesizer fabricated an answer over an empty evidence base instead of refusing.
- **Fixes applied (commit `b3e42dd`):**
  - `answer.md` — added `abstain: true` response path: if every verdict is DISCONNECT, abstain instead of fabricating.
  - `decompose.md` — added explicit "WHEN THE CORPUS IS A SEQUENTIAL TRANSCRIPT" branch: don't decompose into meta-questions about the artifact; decompose into questions about what was actually discussed inside it. Preserves narrative framing when the apex question is itself temporal.
  - `extraction_schema.md` — when the source is a sequential transcript, the generated extraction prompt and topic_schema MUST capture `speaker` and `at`/`timestamp` on every finding. The chunks already contain the markers; the extractor wasn't preserving them.
  - `source_extract.md` — fallback temporal directive on the per-file extract path so capture happens even if `extraction_schema` doesn't fire.
  - `synthesis_prompt.md` — generated synth/answer prompts must order temporally-anchored evidence chronologically, cite speaker + timestamp at moments of change, and frame answers as narrative when the question is narrative-shaped.
- All edits Pillar 37-clean (no numerical ranges, no min/max, only truth conditions and what-to-preserve directives).
- **Side decision:** to stop the gated-clauses-leaking-into-other-pyramids risk, forked `chains/prompts/question/` → `chains/prompts/question-conversation/` (commit `e9c9c7f`). The conversation pyramid will eventually load from its own prompt directory so the conversation-specific pipeline can develop without contaminating code/document pyramids. **Routing change is not yet wired** — the executor still loads from `prompts/question/` for everyone. Both pipelines currently read from the gated original. This is the "derived_from contrib" pattern in chain form.

### Run 4 — `claudeconvotest4temporallabelingupdate`
- **Apex question:** same as Run 3.
- **Result:** 122 nodes, no failures. Tested with a haiku agent.
- **Verdict:** **6/10 — three of four fixes landed, the most important one didn't.**

| Fix | Result |
|---|---|
| 1. Abstain on empty evidence | ✅ Functionally working — no fabricated nodes, all L1s grounded |
| 2. No meta-questions in decompose | ✅ PASS — no "purpose of this chat" garbage L1s present |
| 3. **Temporal capture (speaker + timestamp on L0)** | ❌ **FAIL — L0 nodes have NO `speaker` or `at` fields** |
| 4. Chronological framing in apex/synth | ✅ PASS — apex opens "The session began with...", uses "first/then/later/by the end", explicitly contrasts start vs end |

The apex itself reads beautifully ("The initial assumptions—single-consumer channel, public-by-default pyramids, and no need for extra security—were proven false, and the final state reflects a more robust, secure, and production-ready system"). The narrative shape works.

But the L0 schema is unchanged: `id, depth, headline, distilled, topics, corrections, decisions, terms, dead_ends, ...` — no speaker, no timestamp. The chunks contain the markers, but the extractor isn't writing them to the topic schema.

**Why fix #3 failed:** the temporal directive lives in `extraction_schema.md`, which is the prompt that *generates* a per-pyramid extraction prompt at build start. The generator is supposed to inject `speaker` and `at` fields into the `topic_schema` it produces, but it likely treated the new clause as optional guidance and produced the same generic schema as before. The gate ("If the source material is a sequential transcript") may have fired, but the LLM didn't translate it into an actual schema field addition. Meta-prompting is leaky.

## Score trajectory

| Run | Apex question | Nodes | Verdict | Notes |
|---|---|---|---|---|
| 1 | legacy default | crash | n/a | `build_folder_map` bug |
| 2 | legacy default | 114 | 8/10 | unexpectedly good first pass |
| 3 (rebuild) | chronological steelman | 109 | regression | meta-node fabrication exposed |
| 4 | chronological steelman | 122 | 6/10 | abstain + meta-fix landed; temporal capture didn't |

Run 4 scored lower than Run 2 not because Run 4 is worse but because the haiku tester held it to a higher bar — Run 4 was being explicitly evaluated against the four claimed fixes, and the temporal-capture fix didn't land. Narratively Run 4's apex is the strongest of all four.

## Open observations from the test arc

1. **The question pipeline treats `.jsonl` conversations the same way it treats code repos.** It runs the same `decompose → extraction_schema → source_extract → answer` flow with the same prompts. When those prompts were written they were tuned for code/document corpora. Sequential transcripts need different treatment at multiple layers — characterize, decompose, extraction schema, synthesis. We can patch via gated clauses (what we did) or fork the pipeline (started but not wired).

2. **Meta-prompting is leaky.** The temporal-capture fix lives in `extraction_schema.md`, which generates *another* prompt at build time. The LLM doing the schema generation didn't reliably translate "MUST include speaker and at fields" into actual JSON schema modifications. Two layers of indirection is one too many for this kind of structural requirement. Either the schema generator needs much harder language (literal JSON to inject), or the temporal fields need to be hardcoded in Rust at the schema-generation site rather than relying on the LLM to remember.

3. **The L0 chunks already have the temporal markers.** `pyramid::ingest::parse_conversation_messages` in `ingest.rs:231-234` formats every line as `--- {label} [{ts}] ---` where `label` is `PLAYFUL` (user) or `CONDUCTOR` (assistant) and `ts` is the first 19 chars of the ISO timestamp. The data is there. We just have to make the extractor preserve it.

4. **The synthesis layer can do temporal work via inference alone.** Even without explicit `speaker` / `at` fields on L0, the apex of Run 4 produced a chronological narrative. This means the synthesis prompts (especially the new chronological framing in `synthesis_prompt.md` + `answer.md`) work — they just lack precision. With real timestamped fields the synthesizer could cite turn-by-turn instead of arc-by-arc.

5. **Conversations need a meta-pyramid context.** Single-session pyramids will always think the world started at chunk 0. The decompose/synth layers see "first the team agreed on a plan" when in reality these are months-old patterns being applied to a new task. The fix requires giving the L1+ synth steps access to a higher pyramid of pyramids (a project knowledge vine) and letting them cite that as evidence with a different provenance class. This is its own workstream — flagged here, not addressed.

## Next-up workstreams

### A. Real chronological processing as a Rust-supported option
The `chains/questions/conversation-chronological.yaml` design-spec exists (commit `a7d8a50`) and describes a triple-pass forward/reverse/combine extraction pattern. It is currently un-shippable because the executor doesn't support:
- `sequential_context.direction: "reverse"` — `question_loader.rs:158` only accepts `mode: "accumulate"` (forward). Need to extend `SequentialContextConfig` with a `direction` field and add reverse iteration in the runner.
- `input.zip_steps: [...]` — there's no current way for an L0c step to consume two prior step outputs and pair them per-chunk (combine `forward_view[i]` with `reverse_view[i]`). The legacy `chain_executor.rs` had `zip_steps` for the old chain DSL; the question pipeline needs an equivalent.
- `save_as: step_only` — intermediate steps need a way to NOT persist as nodes. Forward + reverse passes are throwaway; only the combine output should become L0.

In addition to the executor work, the temporal field capture (fix #3 from Run 4) probably wants to be enforced in Rust rather than via meta-prompting. When `characterize` classifies the source as a sequential transcript, the schema-generation site (`extraction_schema.rs:176`) should hardcode `speaker` and `at` fields into the topic_schema rather than asking the LLM to remember.

### B. Per-content-type chain selection via config (not filename binding)
Today: when the prod build path resolves prompts, it hardcodes `prompts/question/` in `evidence_answering.rs:850` and similar sites. There is no per-content-type prompt directory. The `chains/questions/*.yaml` files exist (`code.yaml`, `document.yaml`, `conversation.yaml`) but **they are only loaded by `parity.rs` for dual-validation**, not by the production `run_decomposed_build` path. The selection of "which YAML / which prompts dir" is therefore not something an operator can swap without code changes.

What we want: a single config file (YAML, lives at `chains/registry.yaml` or similar) that says:
```yaml
chains:
  code:
    questions: chains/questions/code.yaml
    prompts:   chains/prompts/question/
  document:
    questions: chains/questions/document.yaml
    prompts:   chains/prompts/question/
  conversation:
    questions: chains/questions/conversation.yaml
    prompts:   chains/prompts/question-conversation/
  question:
    questions: chains/questions/question.yaml
    prompts:   chains/prompts/question/
```

Then to swap conversation builds onto the chronological pipeline you flip one line:
```yaml
  conversation:
    questions: chains/questions/conversation-chronological.yaml
    prompts:   chains/prompts/question-conversation-chronological/
```

…rather than renaming files or changing Rust constants. This is the precondition for the fork we already started (commit `e9c9c7f`) to actually take effect.

Implementation sketch:
- New `chain_registry.yaml` config file at `chains/` root
- New loader (`chain_registry.rs::load_registry`) that parses it on chain_registry init
- `chain_registry::default_chain_id(content_type)` becomes `chain_registry::resolve(content_type) -> ChainBinding { questions_path, prompts_dir }`
- All call sites that hardcode `prompts/question/` (~6-8 sites in `evidence_answering.rs`, `extraction_schema.rs`, `chain_executor.rs`) consume the resolved `prompts_dir` instead
- Operators override per-pyramid via the existing `pyramid_chain_assignments` table

### C. Wire the conversation prompt fork into prod
Once (B) lands, switch the conversation entry in `chain_registry.yaml` to point at `chains/prompts/question-conversation/` and remove the temporal gates from `prompts/question/` (returning the generic pipeline to its pre-edit state). The conversation pipeline can then make its temporal directives unconditional and develop independently.

### D. Meta-pyramid context (separate workstream)
Out of scope for this iteration. Flagged in observation 5.

## Commits referenced

| SHA | What |
|---|---|
| `48cd70b` | `pyramid: build_folder_map handles single-file sources` (Run 1 fix) |
| `844ad25` | `pyramid: stop truncating headlines on save` (related quality fix) |
| `a3056bc` | `web: collapse Topic structure by default` (web UI redesign) |
| `a7d8a50` | `chains: backup conversation v1 + add chronological design-spec` (Run 3 prep) |
| `b3e42dd` | `prompts: abstain on empty evidence + temporal-aware extraction` (Run 3 → Run 4 prompt fixes) |
| `e9c9c7f` | `chains: fork question prompts → question-conversation` (parallel pipe seed) |
