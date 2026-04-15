# Handoff: Question Pipeline Recipe Must Be a Contribution, Not Compiled Rust

## What we realized

`build_runner.rs` contains ~1600 lines of recipe masquerading as equipment. The step sequence (characterize → enhance → decompose → extraction_schema → extract → pre_map → answer → synthesize → reconcile), the input wiring (which step's output feeds which step), the flow control ("for each layer, run pre_map then answer"), the conditional logic ("if no L0, extract first") — these are all intelligence decisions about what works best for building understanding webs. They're currently frozen in Rust.

This violates:
- **Pillar 2** (contributions all the way down): An agent who discovers that adding a "validate decomposition quality" step between decompose and extract produces better pyramids cannot contribute that improvement. The recipe is compiled Rust, not a forkable contribution.
- **Pillar 28** (the pyramid recipe is itself a contribution): The recipe for building question pyramids — the layer definitions, the extraction approach, the synthesis strategy — should be improvable by agents. Currently it can't be.
- **Pillar 37** (never prescribe outputs to intelligence): The Rust code makes decisions about what sequence works best, what context to pass to each step, when to stop decomposing. These are intelligence decisions hardcoded as program logic.

The root cause: the YAML chain format couldn't express the question pipeline's needs (recursion, dynamic prompts, cross-build state), so the recipe went into Rust as the path of least resistance. The correct solution is not "accept that the recipe lives in Rust" but "add the equipment so the recipe can be expressed as a contribution."

## The distinction: equipment vs. recipe

**Equipment (Rust, correctly):** The executor runtime. Step dispatch. Concurrency management (semaphore, parallel task spawn). LLM API client (HTTP, retry, streaming, parse). SQLite read/write. Error handling mechanics. File watching. Timer management. These are the kitchen appliances — nobody needs to fork a semaphore.

**Recipe (should be contribution, currently in Rust):** The step sequence. Which steps exist. What order they run in. What inputs each step receives. The "for each layer" iteration pattern. The "if gaps exist, re-examine" conditional. The "decompose recursively until leaves" control flow. These are intelligence decisions about the best way to build a pyramid from a question. They belong in a chain definition (YAML/MD) that compiles to IR and runs on the executor.

## The MPS end state

The question pipeline is a YAML chain definition — a contribution — that uses primitives provided by the executor. The chain definition looks something like:

```yaml
# This is a contribution. Fork it. Improve it. Publish your improvements.
steps:
  - characterize (classify the corpus)
  - enhance_question (expand the apex question using corpus context)
  - decompose (recursive: apex → branches → leaves, with horizontal review)
  - extraction_schema (holistic: examine all L1 questions, generate extraction prompt + output schema)
  - extract_l0 (for each chunk: run the GENERATED extraction prompt)
  - evidence_loop (for each layer bottom-up: pre_map candidates, then answer with verdicts)
  - synthesize (branch answers from leaf answers, apex from branch answers)
  - reconcile (identify orphans, central nodes, gap clusters)
```

Each step references a prompt file (contribution). The extraction step uses a DYNAMIC prompt — the output of the extraction_schema step. The decompose step is RECURSIVE — it calls the LLM repeatedly until all branches are resolved to leaves. The evidence_loop step iterates over a VARIABLE number of layers determined by the decomposition depth. The decomposer can access CROSS-BUILD STATE — existing answers and evidence from prior builds.

These capabilities require new primitives in the executor (Rust equipment additions):

**Primitive: recursive_decompose** — A step that calls the LLM recursively to build a question tree. Controlled by the prompt (contribution), not by Rust logic. The prompt decides when to stop (is_leaf). The executor just handles the recursion mechanics, persistence, and horizontal review dispatch.

**Primitive: dynamic_instruction** — A step that uses a prior step's output as its prompt. The extraction_schema step generates an extraction prompt; the extract step uses it. Currently impossible in YAML (instructions must be static `$prompts/` references). This is the key equipment gap.

**Primitive: evidence_loop** — A step that iterates per-layer from leaves to apex, running pre_map + answer at each layer. The number of layers is determined by the decomposition depth (variable per build). The executor handles the iteration; the prompts (contributions) handle the intelligence.

**Primitive: cross_build_input** — A step input that references the existing understanding structure from prior builds (evidence set apexes, L1+ answer headlines, accumulated MISSING verdicts). Currently this data is assembled ad-hoc in build_runner.rs. It should be a declarative input reference that the executor resolves.

Once these primitives exist, `build_runner.rs`'s question pipeline logic reduces to: "load the chain YAML, compile it, execute it." The ~1600 lines of recipe become ~50 lines of chain definition YAML.

## Quarantining the mechanical fallback

The current code has a critical architectural error: `run_decomposed_build()` calls `run_build()` (the full mechanical pipeline) when no L0 exists. This is wrong in two ways:

1. **It crashes.** The mechanical pipeline's clustering step fails with "batch_cluster returned 0 threads" on many corpora. This blocks all question builds on fresh slugs.

2. **It's architecturally backwards.** The question pipeline should never invoke the mechanical pipeline. The mechanical pipeline is a preset question — a consumer of the same executor, not a dependency of the question pipeline. Making the question pipeline depend on the mechanical pipeline means neither can exist without the other.

**Immediate quarantine (before the full recipe-as-contribution work):**

Remove the `run_build()` call from `run_decomposed_build()`. Replace it with direct L0 extraction: load chunks, call the extraction prompt (generated by extraction_schema, or the static doc_extract.md as a temporary bridge), save as C-L0 nodes. The question pipeline does its own L0 extraction. Period.

To prevent future accidental fallback:
- Add a compile-time or runtime assertion: `run_decomposed_build()` must never call `run_build()` or any function that loads a mechanical chain YAML.
- If the mechanical pipeline code paths are still needed for backward compatibility with existing presets, gate them behind an explicit `preset_mode` flag — never triggered from the question path.
- Log a WARNING if any code path attempts to invoke the mechanical pipeline from within a question build. This makes silent fallback impossible.

**Long-term quarantine (after recipe-as-contribution):**

The mechanical pipeline becomes one chain definition among many — a preset that compiles through the same chain compiler as everything else. There is no separate `run_build()` function. There is `run_chain(chain_definition)`. The mechanical preset is a chain definition. The question pipeline is a chain definition. Both compile to IR, both run on the executor. The concept of "falling back to mechanical" ceases to exist because there's no separate mechanical code path to fall back to.

## What this handoff is NOT

This is not an implementation plan. The implementing agent should audit the current `build_runner.rs`, `chain_executor.rs`, `chain_dispatch.rs`, and `defaults_adapter.rs` to understand what equipment already exists, what primitives are needed, and how to incrementally move recipe logic from Rust into chain YAML. The scope and sequencing are for the builder to determine.

The immediate priority is the quarantine: remove the `run_build()` call from `run_decomposed_build()` and replace it with direct L0 extraction so question builds work on fresh slugs. The recipe-as-contribution refactor is the larger architectural goal that follows.
