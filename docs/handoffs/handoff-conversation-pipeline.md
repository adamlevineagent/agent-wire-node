# Conversation Pipeline — Implementation Record

> **Status:** Implemented, ready for testing
> **Date:** 2026-04-06
> **Author:** Partner (strategic collaborator)

---

## What Changed

The conversation content type now routes through the question pipeline architecture
instead of the legacy (obsolete) conversation chain. Conversations are analyzed via
question-driven decomposition with a pre-loaded default question.

### Design Decisions

1. **No separate conversation YAML architecture.** The conversation pipeline is a
   clone of question.yaml with `content_type: conversation` and a conversation-tuned
   extraction prompt. All downstream primitives (decompose, evidence_loop, gap_processing,
   webbing) are identical.

2. **Parallel extraction, not sequential.** The old conversation chain used a
   triple-sequential forward/reverse/combine pattern. This is eliminated. The question
   tree provides organizational structure, and the evidence loop connects cross-chunk
   references. No temporal context accumulation needed at L0.

3. **Temporal/causal webbing at all layers.** The webbing prompt was enhanced to detect
   directed relationships (resolves:, supersedes:, enables:, reverses:, triggers:,
   evolves:, prerequisite:) alongside conceptual overlap. This benefits ALL content
   types, not just conversations. Uses a prefix convention on the existing `relationship`
   field — no schema change.

4. **Cross-slug question overlays work.** After building a conversation pyramid, you
   can create a question slug that references it and ask any new question. The L0 nodes
   are shared via the existing cross-build-input mechanism.

### Files Created

| File | Purpose |
|------|---------|
| `chains/defaults/conversation.yaml` | Question pipeline clone with `content_type: conversation`, `id: conversation-default` |
| `chains/prompts/conversation/source_extract_v2.md` | Conversation-tuned extraction: speaker attribution, temporal markers, decisions, back-references |
| `chains/defaults/conversationarchived.yaml` | Old v3.0.0 conversation chain (archived, not deleted) |

### Files Modified

| File | Change |
|------|--------|
| `chains/prompts/question/question_web.md` | Added temporal/causal relationship types to webbing prompt |
| `chains/defaults/question.yaml` | Added `instruction_map` for `content_type:conversation` (belt-and-suspenders) |
| `src-tauri/src/pyramid/build_runner.rs` | Two changes: (1) Route `ContentType::Conversation` through `run_decomposed_build` with default apex question. (2) Dynamic chain lookup via `default_chain_id(ct_str)` instead of hardcoded `"question-pipeline"`. |

### Rust Changes Detail

**`build_runner.rs` — Conversation dispatch (inserted after Question dispatch):**
- Conversations check for existing question tree first (re-build case)
- First build uses default: *"What happened during this conversation? What was discussed,
  what decisions were made, how did the discussion evolve, and what are the key takeaways?"*
- Granularity: 3, max_depth: 3

**`build_runner.rs` — Dynamic chain lookup:**
- `run_decomposed_build` previously hardcoded `find(|m| m.id == "question-pipeline")`
- Now uses `chain_registry::default_chain_id(ct_str)` so conversations load
  `conversation-default` and questions load `question-pipeline`

### No Changes Required

- `chain_registry.rs` — already maps `"conversation" => "conversation-default"`
- `chain_engine.rs` — `"conversation"` already in `VALID_CONTENT_TYPES`
- `chain_loader.rs` — `instruction_map` resolution already implemented
- Schema / database — no migration needed

---

## Default Conversation Question

```
What happened during this conversation? What was discussed, what decisions were
made, how did the discussion evolve, and what are the key takeaways?
```

This is set in `build_runner.rs` for first builds. On re-builds, the stored question
tree is used (so `decompose_delta` handles incremental evolution).

The user can override this question via the build endpoint, same as any question pyramid.

---

## Temporal Webbing — Relationship Types

The enhanced webbing prompt detects these temporal/causal edge types at ALL layers:

| Prefix | Meaning | Example |
|--------|---------|---------|
| `prerequisite:` | A must be understood before B | "Understanding credit system required before market mechanics" |
| `enables:` | A makes B possible | "Auth system enables permission model" |
| `resolves:` | B fixes/answers issue from A | "Auth redesign fixed permission gap" |
| `supersedes:` | B replaces/updates A | "V2 pricing replaces flat-rate model" |
| `reverses:` | B contradicts/undoes A | "Team reversed REST decision for IPC" |
| `triggers:` | A caused B | "Outage triggered redesign" |
| `evolves:` | B refines A's approach | "V2 schema evolved from V1" |

Prefixes are in the `relationship` field. Conceptual edges have no prefix.
Downstream consumers can parse prefixes to filter by edge type.

---

## Future Work

- **Chunk sizing:** Currently hardcoded at 100 lines / no overlap. Conversations would
  benefit from 200 lines / 15-line overlap. Requires making chunk_lines and
  chunk_overlap_lines configurable at the chain or content-type level. This is a
  Rust change in the ingest/chunking layer.

- **Separate temporal webbing pass:** The combined prompt (conceptual + temporal in
  one pass) is V1. If temporal edge quality is insufficient, split into two focused
  passes per layer. Compute is cheap; prompt focus might yield better results.

- **Conversation-specific decompose hints:** The decompose prompt could be enhanced
  with conversation-aware guidance (e.g., "for conversations, always include a
  'decisions and action items' branch"). Currently using the generic decompose prompt
  which adapts based on L0 content.

---

## Testing

To test:
1. Select a conversation in the UI
2. Build should route through `run_decomposed_build` with default question
3. L0 extraction should use `source_extract_v2.md` prompt
4. Temporal web edges should appear at L0 with prefix conventions
5. Incremental re-build: add content, re-build, verify `decompose_delta` runs
6. Cross-slug: create a question slug referencing the conversation, ask a new question
