# Handoff: Question Pyramid Build API — Route Investigation Complete

**Date:** 2026-04-05  
**Status:** ✅ **Question build route found and confirmed working**  
**Project:** agent-wire-node  
**Running binary:** compiled from `research/question-pyramid-tuning` branch  
**Source branch:** `research/lens-framework` (forked from `main`, missing chain engine code)

---

## Root Cause

The `research/lens-framework` branch was forked from `main`, which only has the **legacy** build pipelines (`build_docs`, `build_code`, `build_conversation`). The running v0.2.0 binary was compiled from the `research/question-pyramid-tuning` branch, which has the full chain engine and question pipeline.

### Branch → Feature Map

| Branch | Chain Engine | `/build/question` | Legacy pipelines |
|--------|:-----------:|:-----------------:|:----------------:|
| `main` | ❌ | ❌ | ✅ (but obsolete) |
| `research/lens-framework` | ❌ | ❌ | ✅ (but obsolete) |
| `research/question-pyramid-tuning` | ✅ | ✅ | ✅ (fallback) |
| **Running binary** | ✅ | ✅ | ✅ |

The `mod.rs` on `research/question-pyramid-tuning` declares ~40 additional modules not on `main`:
`build_runner`, `chain_dispatch`, `chain_engine`, `chain_executor`, `chain_loader`, `chain_registry`, `chain_resolve`, `characterize`, `converge_expand`, `crystallization`, `defaults_adapter`, `event_chain`, `evidence_answering`, `execution_plan`, `execution_state`, `expression`, `extraction_schema`, `local_store`, `parity`, `publication`, `question_compiler`, `question_decomposition`, `question_loader`, `question_yaml`, `reconciliation`, `staleness`, `staleness_bridge`, `supersession`, `sync`, `transform_runtime`, `wire_import`, `wire_publish`

---

## Correct Question Build API

### Confirmed Working Endpoint

```
POST /pyramid/:slug/build/question
```

**Request body** (`QuestionBuildBody`):
```json
{
  "question": "What are the key architectural patterns in this codebase?",
  "granularity": 3,
  "max_depth": 3,
  "from_depth": null,
  "characterization": null
}
```

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `question` | string | **required** | The apex question to decompose |
| `granularity` | u32 | `3` | Decomposition breadth |
| `max_depth` | u32 | `3` | Max tree depth |
| `from_depth` | i64? | `null` | Resume build from this depth (reuse nodes below) |
| `characterization` | object? | `null` | Pre-computed characterization (skip auto-characterize) |

**Confirmed test:**
```bash
curl -s -H "Authorization: Bearer vibesmithy-test-token" \
  -H "Content-Type: application/json" \
  -X POST localhost:8765/pyramid/lens-0/build/question \
  -d '{"question":"test"}'

# Response:
# {"build_type":"question_decomposition","from_depth":0,"granularity":3,"max_depth":3,"question":"test","slug":"lens-0","status":"started"}
```

### Additional Endpoints on Running Binary

| Method | Route | Purpose |
|--------|-------|---------|
| POST | `/pyramid/:slug/build/question` | **Question pyramid build** (decomposed) |
| POST | `/pyramid/:slug/build/preview` | Preview decomposition without building |
| POST | `/pyramid/:slug/characterize` | Characterize source material |
| POST | `/pyramid/:slug/build` | Legacy/chain engine build (dispatches via `use_chain_engine` flag) |
| POST | `/pyramid/:slug/publish` | Publish pyramid to Wire |
| POST | `/pyramid/:slug/publish/question-set` | Publish question set to Wire |
| GET | `/pyramid/:slug/question-overlays` | Get question overlays |

---

## Build Dispatch Architecture (from `build_runner.rs`)

```
POST /pyramid/:slug/build/question
  └── handle_question_build()
        └── build_runner::run_decomposed_build()
              ├── question_decomposition::decompose_question()
              ├── characterize::characterize_sources()
              └── chain_executor::execute_chain_from()
                    └── loads chain YAML, executes steps via LLM

POST /pyramid/:slug/build
  └── handle_build()
        └── build_runner::run_build()
              ├── ContentType::Question → run_decomposed_build() (same as above)
              ├── use_chain_engine=true → run_chain_build() → chain_executor
              ├── use_ir_executor=true → run_ir_build() → execution_plan
              └── else → run_legacy_build() → build_docs/build_code/etc (OBSOLETE)
```

The standard `/build` endpoint DOES dispatch to the question pipeline IF the slug's `content_type` is `Question`. But `lens-0` was created with `content_type: "document"`, so it falls through to `build_docs`.

---

## What `lens-0` Needs

The slug was created as `content_type: "document"`. Two options:

### Option A: Use `/build/question` directly (CONFIRMED WORKING)
This endpoint doesn't care about the slug's `content_type` — it runs the question decomposition pipeline regardless. This is the path you just tested.

```bash
export AUTH="Authorization: Bearer vibesmithy-test-token"

curl -s -H "$AUTH" -H "Content-Type: application/json" \
  -X POST localhost:8765/pyramid/lens-0/build/question \
  -d '{
    "question": "What are the fundamental architectural patterns and design principles in these documents?",
    "granularity": 3,
    "max_depth": 3
  }'
```

### Option B: Re-create slug as `content_type: "question"`
If you want `/build` to auto-dispatch to the question pipeline:
```bash
# Delete and re-create
curl -s -H "$AUTH" -X DELETE localhost:8765/pyramid/lens-0
curl -s -H "$AUTH" -H "Content-Type: application/json" \
  -X POST localhost:8765/pyramid/slugs \
  -d '{"slug":"lens-0","content_type":"question","source_path":""}'
```

Option A is recommended — it's already confirmed working and doesn't require disrupting the ingested data.

---

## Source Branch Gap

For the `research/lens-framework` lab to be fully self-contained, the branch needs the chain engine modules. Options:

1. **Merge `research/question-pyramid-tuning` into `research/lens-framework`** — gets all chain engine code
2. **Keep using the running binary as-is** — the binary already has everything, the source branch mismatch only matters for code reading/editing
3. **Rebase `research/lens-framework` on top of `research/question-pyramid-tuning`** — cleanest if you want to edit prompts AND have source match

For now, the running binary works. The lab can proceed with experiment #0.

---

## Skill File Status

The `wire-pyramid-ops` SKILL.md needs updating to document:
- `/pyramid/:slug/build/question` endpoint with `QuestionBuildBody`
- The `build_runner.rs` dispatch architecture
- The branch-source reality (chain engine lives on `research/question-pyramid-tuning`)
