# Wire Node — How the System Works

> **Stale maps cause reinvention.** This document exists because agents keep rebuilding parts of Wire Node that already exist — DADBEAR gets segmented, chains get bypassed, new pipelines get invented. Every reinvention costs a full plan cycle. Read this first.

**Audience:** builders about to implement something in Wire Node (the Tauri desktop app / `agent-wire-node/` repo).
**Purpose:** one document that keeps you from reinventing systems that already exist. Every section ends with the canonical spec and canonical code. If a subsystem feels missing from this doc, search first, ask before you build.

**This document is authoritative.** It describes the system as it is designed to work. Where the running code diverges from this document, the code has a bug — see [`docs/DIVERGENCE-TRIAGE.md`](DIVERGENCE-TRIAGE.md) for the catalog of known divergences and their resolution status.

---

## 0. How to use this doc

Before starting any non-trivial work:

1. **Read §1 (mental model) and §12 (the do-not wall).** These take 5 minutes and catch 80% of reinvention.
2. **Find the relevant section (2–11)** for the subsystem you're touching. Read the canonical spec it points at.
3. **Check §14 (extending checklist)** against your plan.
4. If your plan still feels novel, **search for the primitive in the code** — it probably exists under a different name.

---

## 1. The one-paragraph mental model

Wire Node builds **knowledge pyramids** over local corpora. There is one data path, one executor, one staleness system, and one extensibility mechanism:

- **Questions drive everything.** A "mechanical build" is a preset question with a frozen decomposition. There is no separate mechanical pipeline.
- **One executor.** YAML chain definitions are loaded, resolved, and dispatched by `chain_executor` — a single runtime that handles `forEach`, `pair_adjacent`, `recursive_pair`, `recursive_cluster`, mechanical steps, and resume. No hand-rolled Rust pipelines.
- **One staleness system.** `DADBEAR` (Detect, Accumulate, Debounce, Batch, Evaluate, Act, Recurse) is the **only** path by which the pyramid responds to change. Every change — source file edit, supersession, belief contradiction, delete, rename, vine child update — is a mutation written to `pyramid_pending_mutations` and drained by `stale_engine`. Recursion is free: the supersession of a node is itself a mutation at the next layer up.
- **Everything extensible is a contribution.** Chain definitions, prompts, config policies, schema definitions, schema annotations, generation skills, seed defaults, FAQ entries, annotations, and corrections are all rows in the contribution store. New behavior ships by writing a contribution, not by adding a code path.

**If you're about to write Rust for something, ask yourself: is this describable as data? The answer is almost always yes — in which case it belongs in a chain YAML, a prompt `.md`, a generative config YAML, or a contribution, not in the binary.**

---

## 2. The three data layers

Every pyramid has exactly three layers. Do not invent a fourth.

### 2.1 Source layer — corpus documents, not contributions

Source files on disk registered in `pyramid_slugs` with a `source_path`. They are **corpus documents**, not Wire contributions. They have path-based identity (location), not handle-path identity (event).

- **Tracked in:** `pyramid_file_hashes` (SHA-256 per file).
- **Watched by:** `PyramidFileWatcher` (`src-tauri/src/pyramid/watcher.rs`) — uses the `notify` crate's FSEvents backend on macOS, writes every hash change to `pyramid_pending_mutations` at depth 0, notifies the stale engine via an mpsc channel.
- **Wire coupling:** none during build. Source → L0 is a local read; publishing to the Wire is a separate, optional export step via `wire_publish`.

**Canonical spec:** `docs/architecture/understanding-web.md` §Source Layer · **Canonical code:** `src-tauri/src/pyramid/watcher.rs`, `src-tauri/src/pyramid/ingest.rs`, `src-tauri/src/pyramid/folder_ingestion.rs`

### 2.2 Evidence layer — L0

L0 is the evidence base. Every L0 extraction is shaped by a question. Two flavors live in the same `pyramid_nodes` table, distinguished only by `id` format and `self_prompt`:

- **First-question extraction** — `C-L0-{index:03}` IDs. Produced when the first question is asked of a corpus. Deterministic per file order.
- **Targeted re-examination** — `L0-{uuid}` IDs. Produced when a later question's MISSING verdicts identify a gap. Sits alongside the first-question L0, enriches the evidence base.

Both are valid evidence for any question. L0 is **never deleted**, only superseded (old versions keep `superseded_by` pointers for audit).

**Canonical spec:** `docs/architecture/understanding-web.md` §Evidence Layer · **Canonical code:** `src-tauri/src/pyramid/extraction_schema.rs`, `src-tauri/src/pyramid/evidence_answering.rs`

### 2.3 Understanding layer — L1 and above

Every node above L0 exists because a question was asked. Each node is an answer to a question, backed by `pyramid_evidence` links (KEEP / DISCONNECT / MISSING, with weight 0.0–1.0 and reason) pointing at nodes one layer down. The understanding layer is a **DAG, not a tree** — the same L0 node can be KEEP'd by multiple questions at different weights.

- `self_prompt` = the question.
- `distilled` = the answer.
- `topics[]` = the structured breakdown.
- `pyramid_evidence` rows are **build_id-scoped and never deleted**.

**Canonical spec:** `docs/architecture/understanding-web.md` §Understanding Layer · **Canonical code:** `src-tauri/src/pyramid/question_build.rs`, `src-tauri/src/pyramid/question_decomposition.rs`, `src-tauri/src/pyramid/evidence_answering.rs`, `src-tauri/src/pyramid/reconciliation.rs`

---

## 3. The executor — one path, one runtime

There is exactly one place where a pyramid gets built: `chain_executor::execute_chain`. Everything else is a caller.

### 3.1 Pipeline at a glance

Two entry points converge on one executor:

```
run_chain_build(slug)                    run_decomposed_build(slug, apex_question)
  │  (code/document)                       │  (question/conversation)
  └─ chain_registry → chain_loader ──────┬─┘
                                          │
                                          ↓
                            chain_executor::execute_chain_from
                              │
                              ├─ Build ChainContext (chain_resolve) with $chunks, $slug, $content_type
                              ├─ Build StepContext (chain_dispatch) with DB, LLM config, cache, event bus
                              ├─ Spawn writer drain task (async channel → SQLite writes)
                              ├─ For each step: evaluate `when`, pick mode, iterate
                              │     ├─ Check resume state (step output + node existence dual check)
                              │     ├─ Resolve $refs + {{template}} slots
                              │     ├─ Dispatch LLM or mechanical function
                              │     ├─ Persist step output + node (if save_as: node)
                              │     ├─ Update accumulators (sequential mode)
                              │     └─ Report progress
                              └─ Return (apex_node_id, failure_count)
```

`run_build_from_with_evidence_mode` (`build_runner.rs:192`) is the router: it sends `ContentType::Conversation` through `run_decomposed_build` with a default apex question, and `code`/`document` through `run_chain_build`. Both paths call `chain_executor::execute_chain_from`.

### 3.2 Iteration modes and step primitives

These are two orthogonal axes. **Iteration mode** controls the loop/pairing shape. **Step primitive** declares the semantic intent. You can combine them freely (e.g., `for_each: $chunks` with `primitive: extract` is per-chunk LLM extraction; `for_each: $chunks` with `primitive: recursive_decompose` is per-chunk chain decomposition).

**Iteration modes — pick one, do not invent a new one:**

| Mode | Trigger | Purpose |
|---|---|---|
| `forEach` | `for_each: "$expression"` | Iterate an array. `sequential: true` + `accumulate:` for order-dependent passes. `for_each_reverse: true` iterates in reverse order (used by conversation reverse passes). |
| `pair_adjacent` | `pair_adjacent: true` | Pair siblings at source depth, produce depth+1. |
| `recursive_pair` | `recursive_pair: true` | Repeat adjacent pairing until apex. |
| `recursive_cluster` | `recursive_cluster: true` | LLM-driven clustering loop with `apex_ready` signal. **The preferred convergence mode** — LLM decides when structure is right, not a hardcoded threshold. |
| `single` | no loop fields | One LLM call processes all data. Use with `batch_threshold` + `merge_instruction` for unbounded fan-in (Pillar 44). |
| `mechanical` | `mechanical: true` + `rust_function: "..."` | Named Rust function instead of the LLM. Used for deterministic work: `extract_import_graph`, `cluster_by_imports`, `cluster_by_entity_overlap`. |

**Step primitives** — the canonical list is in `chain_engine.rs:11` (`VALID_PRIMITIVES`). The notable categories:

- **Standard LLM primitives:** `extract`, `classify`, `synthesize`, `compress`, `fuse`, `evaluate`, `compare`, `verify`, `calibrate`, `interrogate`, `pitch`, `draft`, `translate`, `analogize`, `review`, `fact_check`, `rebut`, `steelman`, `strawman`, `timeline`, `monitor`, `decay`, `diff`, `relate`, `cross_reference`, `map`, `price`, `metabolize`, `embody`, `ingest`, `detect`, `custom`
- **Recipe primitives** (orchestration, no `instruction` required): `build_lifecycle`, `cross_build_input`, `evidence_loop`, `process_gaps`, `recursive_decompose` — these trigger specialized executor paths (e.g., `execute_cross_build_input` at `chain_executor.rs:4580`)
- **Orchestration primitives:** `container`, `loop`, `gate`, `split`
- **Chain invocation:** `invoke_chain` field (calls another chain by ID)

Recipe primitives are distinct from iteration modes. `cross_build_input` loads prior build state (`$load_prior_state.*`) and is the gating mechanism for fresh-vs-delta builds. `evidence_loop` runs the question answering cycle. `process_gaps` handles MISSING verdicts. `recursive_decompose` runs the question decomposition. These are registered in the executor, not in the chain YAML author's imagination — do not invent new recipe primitives without a spec.

### 3.3 Variable resolution

Two layers, do not confuse them:

- **`$variable.path`** — YAML input resolution against `ChainContext`. Unresolved is a **runtime error**, not a warning.
- **`{{variable}}`** — Prompt template resolution inside `.md` prompt files. Keys are looked up in the step's already-resolved input map. Unresolved is a **runtime error**.

**Built-in scalar variables:**

| Variable | Type | Description |
|---|---|---|
| `$chunks` | Array | All content chunks for this pyramid. |
| `$chunks_reversed` | Array | Chunks in reverse order. |
| `$slug` | String | Pyramid slug being built. |
| `$content_type` | String | `"conversation"`, `"code"`, or `"document"`. |
| `$has_prior_build` | Bool | True if nodes already exist for this slug. |

**Question/conversation initial params** (set by `build_runner.rs:938–942` and `characterize`):

| Variable | Type | Description |
|---|---|---|
| `$apex_question` | String | The apex question driving this build. |
| `$granularity` | Number | Decomposition granularity setting. |
| `$max_depth` | Number | Maximum pyramid depth. |
| `$from_depth` | Number | Starting depth for this build. |
| `$characterize` | Object | Full characterization result (content type, structure, metadata). |
| `$audience` | String/Object | Audience framing from characterization (Pillar 41: flows through every prompt). |
| `$evidence_mode` | String | Evidence mode for question builds. |

**`$load_prior_state.*`** (populated by `execute_cross_build_input` at `chain_executor.rs:4580–4720` — **load-bearing for incremental builds**):

| Variable | Type | Description |
|---|---|---|
| `$load_prior_state.l0_count` | Number | Existing L0 node count. Gates fresh-vs-delta in `when:` conditions. |
| `$load_prior_state.has_overlay` | Bool | Whether an overlay (prior answers) exists. |
| `$load_prior_state.overlay_answers` | Array | Prior answer nodes for delta synthesis. |
| `$load_prior_state.question_tree` | Object | Persisted decomposition tree from prior build. |
| `$load_prior_state.unresolved_gaps` | Array | MISSING verdicts from prior build. |
| `$load_prior_state.l0_summary` | String | Summary of existing L0 evidence. |
| `$load_prior_state.is_cross_slug` | Bool | Whether this build references other slugs. |
| `$load_prior_state.referenced_slugs` | Array | List of referenced slug names. |
| `$load_prior_state.evidence_sets` | Array | Evidence sets from prior builds. |

If you are reading a chain YAML and see `when: "$load_prior_state.l0_count > 0"` or similar, that is using these variables to gate incremental behavior. Do not be confused by them — they are not magic, they are populated by the `cross_build_input` recipe step.

**Loop variables:**

| Variable | Type | Available in |
|---|---|---|
| `$item` | Value | `forEach` loops (current item, or current batch array when batched) |
| `$index` | Number | `forEach` loops (0-based) |
| `$pair.left` | Value | `pair_adjacent` / `recursive_pair` |
| `$pair.right` | Value | `pair_adjacent` / `recursive_pair` (null if odd carry) |
| `$pair.depth` | Number | Pair steps (depth being constructed) |
| `$pair.index` | Number | Pair steps (pair index within current depth) |
| `$pair.is_carry` | Bool | Pair steps (true if odd node carried up) |

**Step output references:** `$step_name`, `$step_name.nodes[0]`, `$step_name.output.distilled`, `$step_name.nodes[$index]`, `$step_name.nodes[i]` (pair mode: `pair_index * 2`), `$step_name.nodes[i+1]`.

**Accumulators:** Named accumulators are top-level `$ref` values (e.g., `$running_context`).

### 3.4 Error strategies

`abort | skip | retry(N) | carry_left | carry_up`. Step-level `on_error` overrides `defaults.on_error`. Retries use exponential backoff (2s, 4s, 8s). Every LLM step additionally has a **free JSON-parse retry** at temperature 0.1 built into `chain_dispatch::dispatch_llm` — independent of the step-level strategy.

### 3.5 Resume contract

The dual-check system: for each iteration, check that **both** the `pipeline_steps` row **and** (if `save_as: node`) the `pyramid_nodes` row exist. Three states:

| State | Meaning | Action |
|---|---|---|
| `Complete` | Both exist | Skip |
| `StaleStep` | Step row exists, node missing | Rebuild (write failed between step and node) |
| `Missing` | No step row | Execute normally |

For `recursive_pair`, a whole depth level is skipped if it has the expected count (`ceil(source / 2)`). For sequential `forEach`, the accumulator is replayed from stored outputs before resuming.

### 3.6 Where chains live

```
chains/
  defaults/        ← ships with the binary. DO NOT modify.
    conversation.yaml
    code.yaml
    document.yaml
    question.yaml
    topical-vine.yaml
    ...
  variants/        ← user-created, assignable per-slug
  prompts/
    conversation/  ← referenced as $prompts/conversation/forward.md
    code/
    document/
    shared/
```

Assignment is `pyramid_chain_assignments(slug → chain_id)`.

**Canonical spec:** `docs/architecture/action-chain-system.md` + `chains/CHAIN-DEVELOPER-GUIDE.md` (user-facing quick ref) · **Canonical code:** `src-tauri/src/pyramid/chain_engine.rs`, `chain_resolve.rs`, `chain_dispatch.rs`, `chain_executor.rs`, `chain_loader.rs`, `chain_registry.rs`

**Rule for adding behavior to the build pipeline:** write a chain YAML + prompt files. Do not add a new Rust build function, do not add a new execution mode, do not bypass the executor. The only legitimate new Rust in this area is a new **mechanical step function** for genuinely deterministic work (no LLM, no judgment) that gets registered for chains to call via `mechanical: true, rust_function: "..."`.

---

## 4. DADBEAR — one recursive function

### 4.1 The pattern

DADBEAR is one function applied recursively. The pyramid's own structure is the recursion stack. There is nothing else.

```
Something changed
  → hash check: did the content actually change?
    → no  → log "checked, fine." Done.
    → yes → dispatch LLM stale check: "given old content X and new content Y, is this node's understanding still valid?"
      → not stale → log the check. Done.
      → stale    → rewrite the local node
                  → ring the edges the stale checker flagged as potentially impacted
                  → each flagged edge/parent is now "something changed" at the next layer up
                  → recurse
```

That's the entire system. The same function at L0 (source file changed → is my extraction stale?), at L1 (my child was rewritten → is my cluster stale?), at L2 (my child was rewritten → is my thread stale?), all the way to the apex. The function does not know what layer it's at. It does not understand the pyramid topology. It just follows parent pointers and edge references upward until the LLM says "nothing more changed."

Anything the stale checker doesn't explicitly flag is considered fine. The system is conservative — only confirmed stales propagate. Everything else terminates.

### 4.2 Entry points into the pattern

The pattern has two entry points. Both produce mutations that feed the same recursive function:

- **File change detection** (`watcher.rs`): file hash changes → writes a mutation to `pyramid_pending_mutations` → the recursive function handles it at L0
- **New file discovery** (`dadbear_extend.rs`): polling scanner finds new files → writes an ingest record → fires a chain build → the build output feeds back into the same recursive maintenance path

Anyone saying "we need a new pipeline for X" is wrong. X is a mutation that enters the recursive function. New entry points are fine. New recursive functions are not.

### 4.3 Extension surface

The extension surface is exactly one thing: write a mutation to `pyramid_pending_mutations` with an appropriate `mutation_type` and `layer`. The recursive function picks it up and handles it. If you find yourself writing a new watcher, a new debouncer, a new batcher, or a new "update this node" path, you are reinventing the recursive function. Stop.

New **helpers** are fine — specialized stale-check logic for a new kind of node or edge. The helper is called by the recursive function, not a replacement for it.

### 4.4 Compositions built on top of the pattern

The following are ways the system uses the recursive function efficiently. They are **not** the pattern itself — they are operational choices that could change without affecting the recursion:

- **Per-layer debounce timers** (`stale_engine.rs`): each layer has an independent timer so L0 can process while L2 waits. Prevents unnecessary serialization. Could be replaced with a global timer or per-node timers — the recursive function wouldn't change.
- **Rotator-arm batching**: when a timer fires with N mutations, distributes them round-robin into balanced batches for LLM helpers. Operational efficiency, not architecture.
- **The WAL** (`pyramid_pending_mutations`): crash-recoverable mutation queue. Mutations written at detection time, drained atomically. Stale-checks are idempotent — replaying after crash is safe.
- **Staleness vs. supersession**: two flavors of "something changed." Staleness attenuates (distant effects are smaller). Supersession does not attenuate (a false claim must be corrected everywhere). Both enter the same recursive function; the difference is how far the checker's opinion propagates.
- **Edge re-evaluation**: when a node is rewritten, web edges touching it are re-evaluated in place. Edges are targets of the recursive function alongside parent pointers.
- **Tombstones**: when a file is deleted, a tombstone node supersedes the old one. The old node's history is preserved. The tombstone propagates upward through the normal recursive path ("one of my sources is gone — am I stale?").
- **Rename detection**: macOS native events handled directly; ambiguous cases go to an LLM helper with a no-doubt bias (false "no" is safe and recoverable; false "yes" corrupts history).
- **Connection carryforward**: parent pointers re-parent deterministically. Annotations and FAQ entries get an LLM judgment ("still valid for the new version?").
- **The breaker**: at 75% of project files queued as mutations, all timers pause. User chooses: resume, build new pyramid (branch-switch solve), or freeze.

### 4.5 Specialized behaviors that ride on the pattern

These are not the recursive function — they are hooks triggered by the recursive function's output:

- **FAQ generalization** (`faq.rs`): when an annotation is saved, auto-generates or updates FAQ entries. Rides on the "node was rewritten" event.
- **Generalized knowledge extraction**: agents annotate what they learn; the annotation triggers FAQ matching and mechanism-knowledge accumulation. This is a consumer of the recursive function's output, not part of it.
- **Crystallization** (`crystallization.rs`): event-driven extra passes (delta extraction, belief tracing, gap filling) triggered when the recursive function confirms staleness. Adds depth to the rewrite, doesn't replace it.
- **Demand signal propagation** (`demand_signal.rs`): MISSING verdicts from evidence answering propagate upward with attenuation. Consumer of the recursive function, not part of it.

**Canonical spec:** `GoodNewsEveryone/docs/architecture/recursive-auto-stale-system.md` (the "why") + `agent-wire-node/docs/specs/evidence-triage-and-dadbear.md` (current state) · **Canonical code:** `src-tauri/src/pyramid/watcher.rs`, `stale_engine.rs`, `stale_helpers.rs`, `stale_helpers_upper.rs`, `supersession.rs`, `dadbear_extend.rs`

---

## 5. Questions — the only pyramid driver

The decomposer is the compiler. It takes an apex question, decomposes it into sub-questions, then **diffs against the existing understanding structure** (all evidence set apexes, L1+ answer nodes, MISSING verdicts). Three outcomes per sub-question:

- **Already answered by an existing node** → cross-link. No new work.
- **Partially answered** → only the existing answer's MISSING verdicts trigger new work.
- **No coverage** → full decomposition into leaf questions, which trigger evidence gathering from L0.

The first question on a fresh corpus is full pipeline. The tenth question on a rich corpus is mostly cross-linking with a tiny delta of new work.

### 5.1 Six-phase question flow

1. **Decompose** (top-down) — `question_decomposition.rs`
2. **Generate extraction schema** (holistic, once per build) — `extraction_schema.rs`. Examines **all** sub-questions and generates an extraction prompt + output schema tailored to what they collectively need. This is **not** a hardcoded prompt.
3. **Extract evidence** (L0) — `question_build.rs` dispatches the schema-generated extraction via chain steps.
4. **Answer questions** (bottom-up) — `evidence_answering.rs`. Pre-map (candidate list) → answer (KEEP/DISCONNECT/MISSING with weight + reason) → gap handling (MISSING verdicts become demand signals, not creation orders).
5. **Synthesize** (bottom-up) — branch questions synthesize from leaf questions; apex synthesizes from branches.
6. **Reconcile** — `reconciliation.rs`. Orphan nodes (no question references), central nodes (KEEP'd by many high-weight), gap clusters.

### 5.2 Mechanical builds are presets

A "mechanical build" is a preset question with a frozen decomposition strategy. `code.yaml`, `document.yaml`, `conversation.yaml` are frozen decompositions of "What is this [content_type] and how is it organized?" The question pipeline and the mechanical pipeline use the **same chain executor**. There is no `build_mechanical()` vs `build_question()` fork. There is `run_decomposed_build()`.

### 5.3 MISSING verdicts are demand, not creation orders

MISSING is a demand signal. It is recorded, weighted, propagated, aggregated. Something else (DADBEAR, a targeted re-examination, a Wire expansion mode) decides whether to fill it and how. The question pipeline **never** creates L0 directly from a MISSING verdict.

**Canonical spec:** `docs/architecture/understanding-web.md` (primary) + `docs/specs/evidence-triage-and-dadbear.md` (triage policy, demand signals) · **Canonical code:** `src-tauri/src/pyramid/question_build.rs`, `question_decomposition.rs`, `question_compiler.rs`, `extraction_schema.rs`, `evidence_answering.rs`, `reconciliation.rs`, `demand_signal.rs`, `triage.rs`

**Rule for adding a new question type:** write a question YAML. Do not add a Rust code path for "this kind of question works differently." If decomposition needs to be different, that's a different `generation skill` contribution, not a different executor.

---

## 6. Contributions — the extensibility substrate

Everything user-modifiable or Wire-shareable is stored as a row in the contribution store (`pyramid_config_contributions` and siblings). New kinds of data **do not get new tables** — they get a new contribution type.

### 6.1 What is a contribution

A writeup with `derived_from` links. `derived_from` is strictly validated (money flows through it); everything else accepts whatever the agent sends. Immutable. Superseding creates a new contribution that points back via `supersedes_id`. Nothing is deleted.

### 6.2 Five Wire contribution types

| Type | Consumed by | Revenue profile |
|---|---|---|
| **Skills** | LLM (how to think — generation prompts, stale-check prompts, refinement skills) | Decays |
| **Templates** | Agent (what settings — schema definitions, schema annotations, question sets) | Moderate |
| **Actions** | Wire (do something) | Perpetual |
| **Chains** | Wire (compound operations) | Perpetual |
| **Question Sets** | Pyramid compiler | Perpetual |

### 6.3 The generative config loop

Every behavioral configuration in Wire Node flows through this loop:

```
intent (natural language)
  → LLM generates YAML from the active generation skill for this schema_type
  → YAML-to-UI renderer shows it editably, driven by the schema annotation
  → user provides notes → LLM refines → new contribution that supersedes
  → accept → becomes active config, syncs to operational tables via schema_registry dispatcher
  → shareable on Wire (already a contribution, native sharing unit)
```

The schema registry is **not** a directory of on-disk YAML files. It is a **view over the contribution store**: every `schema_type` resolves to a `schema_definition` + `schema_annotation` + `generation_skill` + optional `default_seed`, all stored as contributions. The app ships with bundled seeds marked `source = "bundled"` on first run; users can supersede them, pull alternatives from the Wire, refine via notes.

### 6.4 Annotations and FAQ

- **Annotations** are agent-contributed observations stored on pyramid nodes. Every annotation has an optional `question_context` (the question this annotation answers).
- **FAQ nodes** are auto-generated from annotations with a `question_context`. The `faq.rs::process_annotation` function is called after every annotation save, calls the LLM to match against existing FAQs, either updates or creates.
- The critical pattern: annotations carry **both** a specific answer AND a generalized mechanism understanding. The specific question becomes a match trigger; the generalized understanding becomes the canonical FAQ answer.

### 6.5 Never hardcode a number into a prompt

Pillar 37 — any number that constrains LLM output is a violation. Thresholds, ranges, counts, concurrency — if it feels like a "reasonable default", it still belongs in a generative config YAML, not a Rust constant. If something really is a Rust-level operational tunable (debounce window, cache TTL, batch cap), it belongs in `OperationalConfig` / `Tier1Config`/`Tier2Config`/`Tier3Config`, which are themselves driven by active config contributions.

**Canonical spec:** `docs/specs/generative-config-pattern.md` + `docs/specs/config-contribution-and-wire-sharing.md` + `docs/specs/wire-contribution-mapping.md` · **Canonical code:** `src-tauri/src/pyramid/config_contributions.rs`, `generative_config.rs`, `schema_registry.rs`, `faq.rs`, `wire_native_metadata.rs`

**Rule for adding a new kind of data:** write a new `schema_type`, bundle a seed contribution, write a generation skill and a schema annotation. Do **not** create a new SQL table for "just this one thing." Do **not** add hardcoded constants. If you think your data is special, it isn't — it's a contribution with a particular shape.

---

## 7. Vines and folder ingestion — recursion all the way up

A **vine** is a pyramid whose children are other pyramids (bedrocks) and other vines. The recursion is exact: a vine is a pyramid, composed of pyramids, using the same chain executor, the same DADBEAR staleness, the same contribution pattern.

### 7.1 Folder ingestion model (as of 2026-04-11)

Folder ingestion is **not** scan-and-build. It is **scan-to-filemap → user-curates → build-from-checklist**. The scanner produces a best-guess baseline; the user edits the checklist (via UI or directly in the `.understanding/` folder); the builder runs on whatever the checklist says is current.

**Canonical spec:** `docs/specs/vine-of-vines-and-folder-ingestion.md` + `project_folder_nodes_checklist.md` (memory) + `docs/handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md` · **Canonical code:** `src-tauri/src/pyramid/folder_ingestion.rs`, `vine.rs`, `vine_composition.rs`, `chains/defaults/topical-vine.yaml`

### 7.2 Self-organizing rules

| Condition | Result |
|---|---|
| Homogeneous folder, ≥ `min_files_for_pyramid` | Pyramid of that content type |
| Mixed content or has subfolders | Topical vine composing children |
| Files below threshold | Included in parent vine as loose files |
| Binary/large files / ignored patterns | Skip |

All thresholds flow from `folder_ingestion_heuristics` (generative config YAML, `schema_type: folder_ingestion_heuristics`). User-customizable, Wire-shareable. **Never hardcoded.**

### 7.3 Propagation up the vine hierarchy

When a bedrock updates, propagation walks `bedrock → parent vine → grandparent vine → ...` via the **same change-manifest update** that Pipeline A uses for intra-pyramid propagation. There is no new mechanism for vines. The `notify_vine_of_bedrock_completion` pattern is extended to vine parents.

**Rule for composing anything bigger than one pyramid:** it is a topical vine. Do not invent a "multi-pyramid aggregator" or a "cross-pyramid synthesizer." The topical vine chain YAML + `vine_composition.rs` + DADBEAR propagation is the entire mechanism.

---

## 8. The build runner — single entry point

All pyramid builds go through `build_runner.rs`. Every caller — Tauri IPC command in `main.rs`, Warp HTTP route in `routes.rs`, absorption build trigger, MCP server endpoint — ends up here.

- `spawn_question_build` / `run_decomposed_build` for question-driven builds
- Chain dispatch through the `use_chain_engine` feature flag (today: always true; legacy path kept for regression comparison)
- Rate limiting for absorption builds (`check_absorption_rate_limit`)
- Concurrency control via `lock_manager.rs` (per-slug write lock)
- Progress events via `state.build_event_bus` (tee'd to per-slug WebSocket)

**Canonical code:** `src-tauri/src/pyramid/build_runner.rs`, `build.rs`, `lock_manager.rs`, `event_bus.rs`, `event_chain.rs`

**Rule for adding a new way to start a build:** call `build_runner::spawn_*`. Do not duplicate the build-spawn logic. Do not bypass the lock manager. Do not reach past the event bus to notify frontends directly.

---

## 9. Observability — StepContext and the event bus

Every LLM call and every mechanical step carries a `StepContext` (`step_context.rs`). StepContext provides:

- **Cache lookup** (`prompt_cache.rs`, `llm_output_cache` spec). `cache_key = sha256(inputs_hash | prompt_hash | model_id)` where `inputs_hash = sha256(system_prompt + user_prompt)` after variable substitution, `prompt_hash = sha256(raw prompt template)`, and `model_id` is the resolved OpenRouter slug. Cache rows carry `(slug, step_name, primitive, depth, chunk_index, build_id)` as metadata, but **none of that is part of the key** — identical inputs hit the same cache row regardless of which step emitted them. This is intentional: it lets you reuse chunk-level extraction work across chains that use the same prompts. (Verified: `step_context.rs:63–71`, `db.rs:6420–6520`.)
- **Event emission** (`event_bus.rs` TaggedKind variants: `LlmCallStarted`, `LlmCallCompleted`, `CacheHit`, layer events, build progress). Consumed by the per-slug WebSocket for live build viz (`frontend/src/components/PyramidBuildViz.tsx`).
- **Cost accrual** in `pyramid_cost_log` via the centralized helper. No per-site logging.
- **Force-fresh** semantics for policy-change re-evaluation.

The StepContext threads through from the caller. Every new LLM call site **must** construct a StepContext. There is no "just this one call" exception.

**Canonical spec:** `docs/specs/llm-output-cache.md`, `docs/specs/cross-pyramid-observability.md` · **Canonical code:** `src-tauri/src/pyramid/step_context.rs`, `prompt_cache.rs`, `event_bus.rs`, `event_chain.rs`, `cost_model.rs`, `openrouter_webhook.rs`

**Rule for adding a new LLM call:** construct a `StepContext` with the correct `step_name` and `primitive`. Every call, every time. Agents who do bare `call_model` without a StepContext are violating the cache, observability, and cost tracking invariants simultaneously.

---

## 10. Provider registry and model routing

There is **one** place that knows which model to call: the provider registry, backed by an `ai_registry` config contribution. The chain YAML says `model_tier: mid`; the registry maps that to a concrete OpenRouter slug via the active AI Registry contribution.

- Slots: `low` | `mid` | `high` | `max` in chain defaults; more specialized tiers (`fast_extract`, `synth_heavy`, `stale_local`, `web`, `triage`) in specs and variant chains.
- Resolution: step `model:` (direct slug) > step `model_tier` > chain defaults `model:` > chain defaults `model_tier`.
- **Never ask the registry for "current known-good" defaults.** Ask Adam. (See `feedback_ask_for_model_defaults.md`.)

**Canonical spec:** `docs/specs/provider-registry.md` + `docs/specs/credentials-and-secrets.md` · **Canonical code:** `src-tauri/src/pyramid/provider.rs`, `provider_health.rs`, `llm.rs`, `credentials.rs`

---

## 11. Wire coupling — local is local, Wire is Wire

Wire Node is local-first. During a build, **no Wire API calls happen**. Publishing is a separate, optional export step. Pulling is a separate, explicit import step.

- **Publish:** `wire_publish.rs`, `wire_native_metadata.rs` — reads a wire-native YAML block on the contribution, POSTs to the Wire, records `wire_contribution_id`.
- **Pull:** `wire_pull.rs`, `wire_import.rs`, `wire_migration.rs` — pulls a contribution, validates locally, registers as a local contribution (typically a chain variant, a generation skill, or a schema refinement).
- **Discovery:** `wire_discovery.rs` — search Wire for chains, skills, templates that match a schema_type.
- **Update polling:** `wire_update_poller.rs` — checks for supersessions on subscribed Wire contributions, downloads new versions, flags them for user review.

**Rule:** if you are mid-build and reaching for a Wire API client, stop. The build does not need the Wire. Any Wire-sourced artifact (chain variant, skill, schema annotation) must have been pulled and registered locally **before** the build starts.

**Canonical code:** `src-tauri/src/pyramid/wire_publish.rs`, `wire_pull.rs`, `wire_import.rs`, `wire_migration.rs`, `wire_discovery.rs`, `wire_native_metadata.rs`, `wire_update_poller.rs`

---

## 12. Top reinvention mistakes — the "do not" wall

These are the specific ways agents lose time and cause damage. Before you start building, check this list.

### 12.1 "I'll write a new pipeline for X"

**Wrong.** DADBEAR is one loop with two entry points (Pipeline A maintenance, Pipeline B creation). Everything else is a **mutation type** written to `pyramid_pending_mutations`. If you think you need a new pipeline, what you actually need is:

- A new `mutation_type` in the enum
- A new stale-check helper (`stale_helpers*.rs`) for that mutation type
- Possibly a new layer timer config

Do **not** write a new watcher. Do **not** write a new debouncer. Do **not** write a new batching algorithm. Do **not** write a new "update this node" path. Stop.

### 12.2 "I'll write a new Rust build function for this content type"

**Wrong.** The chain executor handles `conversation`, `code`, `document`, `vine`, and whatever you throw at it. A new content type is:

1. A new YAML chain in `chains/defaults/`
2. New prompts in `chains/prompts/{content_type}/`
3. Possibly a new `ContentType` enum variant (for characterization and type routing)

If something **genuinely** can't be expressed in the chain format, the fix is to **extend the chain format** (a new step mode, a new `$variable`, a new `save_as` value) — with spec + audit — not to bypass the executor.

### 12.3 "Mechanical builds and question builds are different code paths"

**Wrong.** They are the same path. `run_decomposed_build` is the path. Mechanical YAML chains are frozen question decompositions. The question pipeline and the mechanical pipeline use the same chain_executor, hit the same DADBEAR, write the same `pyramid_nodes`, emit the same events, cache through the same StepContext. Treating them as separate systems produces:

- Duplicate code paths that drift
- Double the surface area for DADBEAR integration
- Incompatible node formats
- Agents who "just need to tweak the mechanical path" and forget the question path exists

### 12.4 "I'll make my own cache / event emitter / cost log"

**Wrong.** Every LLM call goes through `StepContext` → `prompt_cache` + `event_bus` + `cost_model`. There is no "just this one call" exception. Missing StepContext means no cache, no events, no cost tracking — silently.

### 12.5 "I'll store this in its own table"

**Wrong.** Adam's rule: all data should use the annotation / FAQ / contribution pattern, not separate tables. If you find yourself designing a schema for a new table to hold user-facing data, ask: can this be a contribution? The answer is almost always yes, and "no" almost always means you haven't thought hard enough.

Exceptions (safe to put in their own table):
- Write-ahead logs for internal mechanisms (`pyramid_pending_mutations`, `pyramid_ingest_records`)
- Immutable audit (`pyramid_cost_log`, `pyramid_demand_signals`, `pyramid_evidence`)
- File tracking (`pyramid_file_hashes`)
- Read-through caches of derived data

Everything else is a contribution.

### 12.6 "I'll hardcode a threshold / cap / range in the prompt"

**Wrong.** Pillar 37 — any number constraining LLM output is a violation. No exceptions. The number goes in a generative config YAML (`schema_type = <policy>`), flows through the active config contribution, hits the prompt through the schema registry. The LLM decides structure.

### 12.7 "I'll call the model directly because StepContext is annoying"

**Wrong.** Construct the StepContext. It's `make_step_ctx_from_llm_config` in most cases.

### 12.8 "I'll add a CLI flag / env var / hardcoded constant for this tunable"

**Wrong.** Operational tunables go in `OperationalConfig` (Tier1/Tier2/Tier3). User-facing behavioral configs go in generative config YAML + a `schema_type`. Nothing goes in a flag or env var except the absolute minimum boot-time stuff (`data_dir`, `primary_model`, `auth_token`).

### 12.9 "I'll invent a new step mode so I don't have to use recursive_cluster"

**Wrong.** `recursive_cluster` is the preferred convergence mode. The LLM decides `apex_ready`. Hardcoded thresholds are Pillar 37 violations. Read `chains/CHAIN-DEVELOPER-GUIDE.md` §Convergence before adding anything.

### 12.10 "I'll query `/api/v1/models` to find good defaults"

**Wrong.** Ask Adam directly for model defaults and tier routing slugs. Do not hit the OpenRouter listing API, do not pick "current known-good candidates" yourself. (See `feedback_ask_for_model_defaults.md`.)

### 12.11 "I'll pre-allocate handle-paths locally so I have canonical citations from file creation time"

**Wrong.** Handle-paths are publish-time only, allocated by `insert_contribution_atomic()` (the sole allocator — the deprecated TypeScript `generateHandlePath()` at `src/lib/server/wire-handle-paths.ts:59` warns against client-side replication). Local docs cite each other via `{ doc: workspace-relative-path }`, one of the three legal `derived_from` forms per `wire-handle-paths.md:60–68`. Pre-allocating creates a competing allocator and solves a problem that doesn't exist.

### 12.12 "I'll defer conversation ingest because it's new work"

**Wrong.** Conversation ingest already exists end-to-end: `ingest::ingest_conversation` at `ingest.rs:350`, CC auto-discovery at `folder_ingestion.rs:599+`, Phase 17/18e planner primitives (`CreateVine`, `CreatePyramid`, `AddChildToVine`, `RegisterDadbearConfig`), `vine_bunches` table, conversation-episodic chain (`chains/defaults/conversation-episodic.yaml` + `chains/prompts/conversation-episodic/*`), DADBEAR Pipeline B dispatching for `ContentType::Conversation`. If something feels like new conversation work, search first.

### 12.13 "I'll write a scanner as a separate system from DADBEAR"

**Wrong.** A scanner is DADBEAR's first tick with empty prior state. The "add a folder to the pyramid" action is "register with the watch list, next tick processes everything as new." Do not write a scanner that produces state for a builder that picks up nulls — that's three systems doing the work of one.

### 12.14 "MVP is per-folder staleness, cross-folder propagation is a follow-up"

**Wrong.** Recurse is a uniform edge-walker. It follows edges regardless of folder boundaries. "Per-folder" vs "cross-folder" is a distinction without a difference — there's one edge graph and one walker. The only genuinely out-of-scope edge type is cross-workspace handle-path edges (those need publish first).

### 12.15 "Pipeline B does `clear_chunks + ingest_conversation` so that's the architecture"

**Wrong.** That's the Phase 0b shortcut, explicitly flagged in the code comment at `dadbear_extend.rs:731–734` as "correct-if-slow" with `ingest_continuation` as "the future optimization." The shortcut exists because the state to support `ingest_continuation` (per-file message count cursor) wasn't stored anywhere. Read code comments as part of the architecture — a `// HACK:` or `// Phase 0b:` tag usually means "the real design is X, we took Y to ship."

---

## 13. File map — one line per important file

### 13.1 Pyramid module (`src-tauri/src/pyramid/`)

**Chain system (the executor):**
- `chain_engine.rs` — schema structs + validation
- `chain_resolve.rs` — `$variable.path` and `{{variable}}` resolution (owns `ChainContext`)
- `chain_dispatch.rs` — LLM + mechanical dispatch, `build_node_from_output`, `generate_node_id`
- `chain_executor.rs` — **main execution loop.** forEach / pair_adjacent / recursive_pair / recursive_cluster / single / mechanical, resume, error strategies, cancellation, progress, writer drain
- `chain_loader.rs` — YAML loading, `$prompts/` resolution, directory scanning
- `chain_registry.rs` — SQLite chain-to-slug assignment
- `chain_proposal.rs`, `chain_publish.rs` — Wire sharing of chains
- `step_context.rs` — StepContext factory, cache + event + cost threading

**DADBEAR (the recursive staleness loop):**
- `watcher.rs` — `PyramidFileWatcher`, fs-notify events, hash compare, WAL writes, rename detection
- `stale_engine.rs` — `PyramidStaleEngine`, per-layer timers, WAL drain, rotator-arm batching, helper dispatch, breaker, current_phase tracking
- `stale_helpers.rs` — L0 stale-check helpers (real LLM calls)
- `stale_helpers_upper.rs` — L1+ helpers + `execute_supersession` (**the** supersession path)
- `supersession.rs` — Channel B belief supersession (contradiction detection, non-attenuating upward trace)
- `staleness.rs`, `staleness_bridge.rs` — channel wiring
- `dadbear_extend.rs` — Pipeline B tick loop for creation / extension ingest

**Question system:**
- `question_build.rs` — lib-side spawner for decomposed builds
- `question_decomposition.rs` — decompose apex → sub-questions, diff against existing structure
- `question_compiler.rs` — question → chain compilation
- `question_loader.rs`, `question_yaml.rs`, `question_retrieve.rs` — storage / loading
- `extraction_schema.rs` — **holistic** schema generation from sub-questions (not hardcoded)
- `evidence_answering.rs` — pre-map + KEEP/DISCONNECT/MISSING verdicts
- `reconciliation.rs` — orphan / central / gap-cluster detection
- `triage.rs` — evidence triage step (route: answer / defer / skip per policy)
- `demand_signal.rs`, `demand_gen.rs` — MISSING verdicts as demand + demand-driven expansion
- `reroll.rs` — re-answering after staleness / policy change

**Build runner + ingest:**
- `build_runner.rs` — **single entry point** for all builds
- `build.rs` — legacy build helpers + writer drain + `WriteOp` + node persistence
- `ingest.rs` — corpus ingestion (conversation, code, document, continuation)
- `folder_ingestion.rs` — recursive folder walk → filemap-files → checklist → build. Load-bearing functions: `plan_recursive`, `find_claude_code_conversation_dirs`, `describe_claude_code_dirs`, `encode_path_for_claude_code`, `plan_ingestion`, `execute_plan`
- `characterize.rs` — content-type detection, audience, tone
- `lock_manager.rs` — per-slug write lock
- `ingest_records`-related types in `types.rs`

**Vine / cross-pyramid:**
- `vine.rs` — vine lifecycle, single source management
- `vine_composition.rs` — vine-of-vines composition via chain executor
- `vine_prompts.rs` — vine-specific prompt shaping
- `cross_pyramid_router.rs` — routing queries / mutations across pyramids

**Contributions + config:**
- `config_contributions.rs` — contribution store CRUD, supersession chains, active resolution
- `generative_config.rs` — Phase 9 intent→YAML→UI→accept loop
- `schema_registry.rs` — view over contribution store (`schema_type` → schema/annotation/skill/seed)
- `wire_native_metadata.rs` — wire-native YAML block parsing + defaults
- `faq.rs` — auto-FAQ from annotations with `question_context`
- `vocabulary.rs` — vocabulary contributions for term recognition

**LLM / provider:**
- `llm.rs` — OpenRouter client, tier resolution, `call_model_unified_with_options_and_ctx`
- `provider.rs` — provider registry, tier routing
- `provider_health.rs` — health checks, fallback routing
- `openrouter_webhook.rs` — cost webhooks
- `credentials.rs` — credential store (Wire Node secrets)

**Observability:**
- `event_bus.rs`, `event_chain.rs` — `BuildEventBus`, `TaggedBuildEvent`, per-slug WebSocket
- `prompt_cache.rs` — LLM output cache (StepContext-driven)
- `cost_model.rs` — cost accrual, `pyramid_cost_log`

**Wire coupling:**
- `wire_publish.rs`, `wire_pull.rs`, `wire_import.rs`, `wire_migration.rs`, `wire_discovery.rs`, `wire_update_poller.rs`, `wire_native_metadata.rs`

**Query / reading:**
- `query.rs` — apex / search / drill / entities / resolved / threads / etc.
- `reading_modes.rs` — memoir / walk / thread / decisions / speaker / search views
- `primer.rs` — cold-start onboarding summaries

**Storage:**
- `db.rs` — **17.5k lines.** SQLite schema, migrations, CRUD. Single source of truth for storage layout. Search for `CREATE TABLE` to find schema; migrations are gated by `_migration_marker` sentinels. Read this before adding a table.
- `types.rs` — data model structs (`PyramidNode`, `Slug`, `Topic`, `WebEdge`, `PyramidAnnotation`, `FaqNode`, mutation types)

**Routes:**
- `routes.rs` — Warp HTTP routes (the remote API surface)

### 13.2 Chain assets (`chains/`)

- `chains/defaults/*.yaml` — canonical chain definitions (conversation, code, document, question, topical-vine, extract-only, and variants)
- `chains/prompts/{conversation,code,document,shared}/*.md` — canonical prompt templates
- `chains/questions/` — question pyramid preset YAML
- `chains/schemas/` — legacy schema JSON (migrating to contribution-backed schema registry)
- `chains/vocabulary/`, `chains/vocabulary_yaml/` — vocabulary contributions
- `chains/CHAIN-DEVELOPER-GUIDE.md` — **the quick reference every chain author should read**

### 13.3 Docs by topic (`docs/`)

- **Core architecture:** `architecture/understanding-web.md` (the unified architecture), `architecture/action-chain-system.md` (the executor), `architecture/foreach-scale-fix-audit.md`, `architecture/gap-report-incremental-save-and-batching.md`
- **Current specs:** `specs/evidence-triage-and-dadbear.md`, `specs/generative-config-pattern.md`, `specs/config-contribution-and-wire-sharing.md`, `specs/credentials-and-secrets.md`, `specs/provider-registry.md`, `specs/llm-output-cache.md`, `specs/vine-of-vines-and-folder-ingestion.md`, `specs/yaml-to-ui-renderer.md`, `specs/wire-contribution-mapping.md`, `specs/wire-discovery-ranking.md`, `specs/cross-pyramid-observability.md`, `specs/change-manifest-supersession.md`, `specs/build-viz-expansion.md`, `specs/cache-warming-and-import.md`
- **Plans (active):** `plans/action-chain-refactor-v3.md`, `plans/dadbear-pipeline-completion-ui-refactor.md`, `plans/knowledge-pyramid-integration.md`, `plans/chain-binding-v2.6.md`, `plans/episodic-memory-vine-canonical-v4.md`
- **Vision:** `vision/pyramid-folders-and-model-routing-v2.md`, `vision/self-describing-filesystem.md`, `vision/semantic-projection-and-publication-cut-line.md`, `vision/stewards-and-question-mediation.md`, `vision/training-contributions-and-the-ownership-compact.md`
- **Handoffs (latest):** `handoffs/handoff-2026-04-11-folder-nodes-as-checklists.md`, `handoffs/handoff-2026-04-11-handle-paths-publish-time-only.md`, `handoffs/handoff-2026-04-09-pyramid-folders-model-routing-addendum-01.md`

### 13.4 Wider project docs (`/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/`)

- `wire-pillars.md` — **the pillars check.** Every change must be audited against these. Pillar 37 (no prescribed output constraints) and Pillar 44 (token-aware batching + dehydration) trip implementers most.
- `wire-core-design-patterns.md` — architectural principles underlying the Wire.
- `architecture/recursive-auto-stale-system.md` — the "why" document for DADBEAR. Read before touching staleness.
- `architecture/understanding-web.md` — shared canonical architecture with this repo (lives there, applies here).
- `docs/SYSTEM.md` — the companion canonical doc for The Wire platform itself.

---

## 14. Extending the system — a checklist

When you are about to build something new:

1. **Search first.** `grep`, pyramid search (`opt-025` slug), docs grep. Ask: does this already exist in a different name?
2. **Frontend + backend in the same workstream.** Adam tests by feel, not by curl. If there's no UI surface, it doesn't ship. (See `feedback_always_scope_frontend.md`.)
3. **Pillars check.** Will this violate Pillar 37 (prescribed output), Pillar 44 (unbounded fan-in), or Pillar 17 (chains invoke chains)? If yes, reframe.
4. **Is it a contribution?** Can this be a chain YAML, a prompt, a generation skill, a schema annotation, a config YAML? Almost always yes.
5. **Is it a mutation?** Is this "respond to a change"? Then it is a `pyramid_pending_mutations` entry, not a new pipeline.
6. **Does it need a new step mode?** 99% of the time no — use `recursive_cluster` for convergence, `forEach` for iteration, `pair_adjacent` for siblings, `single` for batched one-shots, `mechanical` for determinism. A genuinely new mode needs a spec + audit.
7. **Every LLM call gets a StepContext.** Every time.
8. **Nothing is deleted.** Supersede, don't destroy. Archive slugs, supersede contributions, tombstone files.
9. **Spec, then build.** Complex work gets a spec in `docs/specs/`. Read `feedback_read_canonical_in_full.md` before writing the spec.
10. **Serial verifier after.** A second agent audits with fresh eyes. Not optional. (See `feedback_serial_verifier.md`.)

---

## 15. Canon priority — what overrides what

When two docs conflict, the priority is:

1. `wire-pillars.md` (GoodNewsEveryone) — immutable physics
2. `docs/architecture/understanding-web.md` — unified data architecture (supersedes older `question-pyramid-architecture.md`, `question-driven-pyramid-v2.md`, `two-pass-l0-contracts.md`)
3. `docs/architecture/action-chain-system.md` — the executor contract
4. Current `docs/specs/*.md` (2026-04 dates) — in-flight implementation contracts
5. This document (`docs/SYSTEM.md`) — the map. If this doc conflicts with a spec, the spec wins; update this doc.
6. Historical `docs/plans/*.md` — context, not authority. Useful for understanding why things are shaped the way they are.
7. Rust code — ground truth for current behavior. If the code contradicts the spec, the bug is usually in the code, but confirm with Adam.

---

**Last update:** 2026-04-11. Update this file whenever a canonical spec lands or a major subsystem moves. Stale maps cause reinvention.
