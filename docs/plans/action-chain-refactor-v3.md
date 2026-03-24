# Action Chain Refactor — Pyramid Build Engine (v3, final)

## Context

8,141 lines across 4 files (build.rs:3364, vine.rs:3423, delta.rs:1070, meta.rs:284) with vine duplicating build.rs wholesale. The Wire has a complete action chain system — we're bringing that model to the local pyramid engine.

**Goal:** Standard build pipelines (conversation, code, document) become data — YAML chain definitions + markdown prompt files. The Rust engine is a runtime executor (~1500-2000 lines, not "thin"). Users can customize prompts, model selection, error handling, and step ordering without recompiling.

**Branch:** `action-chain-refactor`

**Scope (v1):** Conversation, code, document pipelines ONLY. Vine, delta, and meta are wave 2 — they have unique lifecycle/concurrency/side-effect requirements that the generic executor shouldn't absorb yet.

---

## Vine Rethink: Universal Meta-Pyramid

**Key insight:** The vine is not conversation-specific. It's a **universal meta-pyramid** that nests any existing pyramids together — code, docs, conversations, whatever. You build individual pyramids for specific things, then create a vine that wires them into a temporal/narrative structure.

**What this changes:**
- No JSONL discovery/ingestion — source pyramids are already built
- No `vine_bunches` table — replaced by `vine_sources` (vine_slug → source_slug[])
- No conversation-specific logic in vine construction
- Adding a pyramid to a vine = add source slug, rebuild vine L0+ (cheap)
- A vine's L0 = apex + penultimate layer from each source pyramid
- The vine itself is a normal pyramid with its own slug

**What stays the same:**
- L1 temporal-topic affinity clustering
- L2+/apex synthesis
- Intelligence passes (ERAs, entity resolution, decisions, thread continuity, corrections)
- Directory wiring, integrity checks
- The chain-driven execution model (vine.yaml becomes a chain template in wave 2)

**UX flow:**
1. User builds individual pyramids (code, conversation, docs — whatever)
2. User creates a vine: "New Vine" → picks existing pyramid slugs to include
3. Vine reads their apexes + penultimate, assembles L0, clusters, builds
4. User can add/remove source pyramids later → vine rebuilds incrementally

This is non-destructive (source pyramids untouched), composable (any combination), and sets the norm: "build pyramids for things, wire them together with vines."

---

## Audit Fixes Baked In

| Audit Finding | Resolution |
|---------------|------------|
| Runtime isn't thin (~1500-2000 LOC) | Accepted. Value is configurability, not LOC reduction. |
| Mechanical steps (import graph) not abstractable | Kept as named Rust functions. YAML sequences them, doesn't replace them. Chain marks `mechanical: true`. |
| Resume state by-step, not by-depth-count | Each step checks "does this specific node_id exist?" — no count-based shortcuts. |
| Variable substitution unspecified | Full spec below. `{{var.path}}` in prompts, `$var.path` in YAML. |
| registry.json has no locking | Assignments stored in SQLite (`pyramid_chain_assignments` table), not JSON file. |
| Multiple build entry points diverge | Unify behind one shared `run_chain_build()` called by both routes.rs and main.rs. |
| Vine/delta/meta deferred to wave 2 | Only conversation.yaml, code.yaml, document.yaml in v1. |
| Frontend premature before runtime proven | Phases 6-8 gated on runtime parity. |
| Prompt ownership (shared vs variant) | Variants inline prompts. Defaults reference shared files. Editing a variant never changes shared prompts. |
| Dynamic prompts in delta.rs/meta.rs | Wave 2. These need template slots designed per-function. |
| Schema versioning | `schema_version: 1` with explicit migration function when v2 ships. |
| Observability | Per-step timing, token count, cost logged to `pyramid_cost_log`. |
| Error taxonomy | Finite set: `abort`, `skip`, `retry(N)` (1-10), `carry_left`, `carry_up`. |
| Parity testing | Compare topology (node count per depth, parent-child structure), not content. |
| Resume needs step outputs too, not just nodes | Resume checks both step output AND node existence. Sequential accumulation replays prior step outputs. |
| Chain identity needs immutable ID | Each YAML has an `id` field (UUID). Assignment table references ID, not file path. |
| model_tier too abstract | Allow direct `model: "inception/mercury-2"` override alongside tier. Tier is sugar for config-defined mapping. |
| Undefined refs must be runtime errors | Changed: required refs = error. Optional fields marked explicitly. |
| Build status needs chain/step visibility | Extend BuildStatus with `current_chain`, `current_step`, `current_primitive` for wave 3 UI. |
| JSON-retry-at-low-temp is runtime built-in | All LLM steps auto-retry at temp 0.1 on JSON parse failure before step-level on_error. Documented as runtime guarantee. |
| Mechanical step dispatch needs trait | `MechanicalStep` trait with `execute(&self, ctx: &ChainContext) -> Result<Value>`. Named registry. |
| Parity test needs fixture step outputs | Check selected step outputs + stable node fields, not just topology. |

---

## Variable Resolution Spec

### In YAML chain definitions: `$variable.path`

**Scoping:**
- Step outputs namespaced by step name: `$forward_pass.output`, `$l1_pairing.nodes[0]`
- Built-ins: `$chunks`, `$chunks_reversed`, `$slug`, `$depth`, `$content_type`, `$has_prior_build`
- forEach loop: `$item`, `$index`
- Array access: `$step.nodes[i]`, `$step.nodes[i+1]` (i = current pair index)
- recursive_pair implicit vars: `$pair.left`, `$pair.right`, `$pair.depth`, `$pair.index`, `$pair.is_carry`

**On undefined reference:** Runtime ERROR (not warning). Unresolved required refs fail the step. Optional fields (marked `optional: true` in input) substitute null.

### In prompt .md files: `{{variable.path}}`

Mustache-like. The chain engine resolves `{{left_payload}}` by looking up the step's resolved input map.

**Example prompt file:**
```markdown
You are a distillation engine. Compress this conversation chunk...

## SIBLING A (earlier)
{{left}}

## SIBLING B (later)
{{right}}
```

**Example YAML step:**
```yaml
- name: "l1_distill"
  instruction: "$prompts/distill.md"
  input:
    left: "$combine_l0.nodes[i]"
    right: "$combine_l0.nodes[i+1]"
```

The engine resolves `$combine_l0.nodes[i]` → serialized JSON, passes `{left: "...", right: "..."}` to the prompt template, replaces `{{left}}` and `{{right}}`.

### Sequential State Accumulation

The forward/reverse passes accumulate `running_context` across iterations. This needs explicit support:

```yaml
- name: "forward_pass"
  forEach: "$chunks"
  sequential: true                  # must process in order, not parallel
  accumulate:
    running_context:
      init: "Beginning of conversation."
      from: "$item.output.running_context"
      max_chars: 1500
  input:
    context: "$running_context"     # references the accumulator
    chunk: "$item"
```

**Resume semantics:** On resume, the engine replays `from` expressions from all completed prior iterations to reconstruct the accumulator before continuing from the first incomplete iteration.

### recursive_pair Implicit Variables

```yaml
- name: "upper_layers"
  recursive_pair: true
  input:
    left: "$pair.left"
    right: "$pair.right"
```

Available in recursive_pair mode:
- `$pair.left` — current left node (serialized)
- `$pair.right` — current right node (null if odd carry)
- `$pair.depth` — current depth being constructed
- `$pair.index` — pair index within current depth
- `$pair.is_carry` — true if odd node being carried up

---

## Chain Definition Schema (v1)

```yaml
schema_version: 1
id: "conv-default-001"             # immutable UUID/slug — identity for assignments
name: "conversation-default"
description: "Standard conversation pyramid build"
content_type: "conversation"
version: "1.0.0"
author: "wire-default"

defaults:
  model_tier: "mid"               # low | mid | high | max
  model: null                     # direct override: "inception/mercury-2" (takes precedence over tier)
  temperature: 0.3
  on_error: "retry(2)"

steps:
  - name: "forward_pass"
    primitive: "compress"
    instruction: "$prompts/conversation/forward.md"
    input: { chunks: "$chunks" }
    forEach: "$chunks"
    save_as: "node"
    node_id_pattern: "L0-{index:03}"
    depth: 0
    on_error: "skip"
    # Step-specific overrides:
    model_tier: "mid"
    temperature: 0.3

  - name: "mechanical_metadata"
    primitive: "detect"
    mechanical: true               # dispatches to Rust function, not LLM
    rust_function: "extract_mechanical_metadata"
    # No instruction, model_tier, temperature for mechanical steps

  - name: "upper_layers"
    primitive: "fuse"
    instruction: "$prompts/conversation/distill.md"
    recursive_pair: true
    save_as: "node"
    on_error: "carry_left"

post_build: []                     # wave 2: delta, meta chains
```

**Step fields:**
| Field | Required | Description |
|-------|----------|-------------|
| `name` | yes | Unique within chain |
| `primitive` | yes | One of 28 Wire primitives |
| `instruction` | LLM steps | Prompt file ref or inline text |
| `mechanical` | no | If true, dispatches to Rust function |
| `rust_function` | mechanical only | Named Rust function to call |
| `input` | no | JSON with `$ref` resolution |
| `output_schema` | no | Expected output shape |
| `model_tier` | no | Override default model tier |
| `model` | no | Direct model string override (e.g., "inception/mercury-2"). Takes precedence over tier. |
| `temperature` | no | Override default temperature |
| `sequential` | no | If true, forEach processes in order with state accumulation |
| `accumulate` | no | State accumulation config for sequential forEach (see Variable Resolution spec) |
| `forEach` | no | Loop expression |
| `pair_adjacent` | no | Pair adjacent nodes |
| `recursive_pair` | no | Repeat pairing until apex |
| `batch_threshold` | no | Token limit for batching |
| `merge_instruction` | no | Prompt for batch merge |
| `when` | no | Conditional expression |
| `on_error` | no | `abort` / `skip` / `retry(N)` / `carry_left` / `carry_up` |
| `save_as` | no | `"node"` to persist as PyramidNode |
| `node_id_pattern` | no | Template: `"L0-{index:03}"` |
| `depth` | no | Node depth for save_as |

---

## File Structure

```
{data_dir}/chains/
  schema.json                     # JSON schema for validation + agent export
  prompts/
    conversation/                 # 7 files
    code/                         # 3 files
    document/                     # 2 files
  defaults/
    conversation.yaml
    code.yaml
    document.yaml
  variants/
    {user-name}.yaml              # variants inline their prompts
```

**No registry.json.** Chain metadata lives in SQLite:

```sql
CREATE TABLE IF NOT EXISTS pyramid_chain_assignments (
    slug TEXT PRIMARY KEY REFERENCES pyramid_slugs(slug) ON DELETE CASCADE,
    chain_id TEXT NOT NULL,           -- authoritative: matches YAML id field
    chain_file TEXT,                  -- cached hint: re-resolved by directory scan on startup
    assigned_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

Chain discovery: scan `defaults/` and `variants/` directories for `.yaml` files. On startup, rebuild file→id mapping from directory scan. `chain_file` in SQLite is a cached hint for fast lookup, not identity — `chain_id` is authoritative.

---

## Implementation Phases

### Wave 1: Core Refactor (conversation, code, document)

**Phase 1: Foundation** — Rust structs + loader + validator
- Add `serde_yaml = "0.9"` to Cargo.toml
- `pyramid/chain_engine.rs`: ChainDefinition, ChainStep, ChainDefaults structs
- `pyramid/chain_loader.rs`: load YAML, resolve `$prompts/` file refs, validate schema
- `pyramid/chain_registry.rs`: SQLite assignment table, directory scanning
- `use_chain_engine: bool` on PyramidConfig (default: false)
- `ensure_default_chains(data_dir)` writes defaults on first run

**Phase 2: Prompt Extraction** — 12 prompts → markdown files
- 7 from build.rs (FORWARD, REVERSE, COMBINE, DISTILL, THREAD_CLUSTER, THREAD_NARRATIVE, MERGE)
- 3 from build.rs code pipeline (CODE_EXTRACT, CONFIG_EXTRACT, CODE_GROUP)
- 2 from build.rs document pipeline (DOC_EXTRACT, DOC_GROUP)
- Each file has `{{variable}}` slots where format! calls currently inject data
- Verify: load prompt, substitute test values, compare to hardcoded output

**Phase 3: Default Chain Templates** — 3 YAML files
- conversation.yaml, code.yaml, document.yaml
- Each expresses exact sequencing from hardcoded functions
- Mechanical steps marked `mechanical: true` with `rust_function` name

**Phase 4: Chain Runtime Engine** — the executor (~1500-2000 lines)
- `ReferenceResolver`: `$step.output.field`, `$chunks`, `$item`, array indexing, `$step.step_outputs[$index]` for cross-step zip (combine zips forward+reverse by chunk index)
- `PromptResolver`: load .md file, substitute `{{variables}}`
- `StepDispatcher`: route to call_model() or named Rust function via MechanicalStep trait
- `ChainExecutor`: main loop with forEach, pair_adjacent, recursive_pair, batch_threshold
- **Resume contract (critical):**
  - Non-node steps (forward, reverse): persist step output JSON via pipeline_steps. Resume checks step output existence.
  - Node-producing steps (combine, distill): check BOTH step output AND node existence. Step without node = stale, rebuild.
  - Sequential accumulation: replay prior step outputs to reconstruct accumulator before resuming.
  - forEach step outputs indexed by source identity (chunk index), not iteration order. Reverse pass stores at original chunk index.
- **Extended BuildStatus (add in Phase 4, not deferred):** `current_chain`, `current_step`, `current_primitive` fields for debugging during parity rollout.
- Progress: per-step reporting via mpsc channel
- Telemetry: per-step timing + tokens + cost to pyramid_cost_log
- Cancellation: checked between steps and between forEach iterations
- JSON-retry: all LLM steps auto-retry at temp 0.1 on JSON parse failure (runtime built-in)

**Phase 5: Migration** — feature flag swap
- Unify build trigger: one `run_chain_build()` called by routes.rs AND main.rs
- If `use_chain_engine`: load chain (from assignment or default), execute
- If not: call old `build_conversation`/`build_code`/`build_docs`
- Parity test: topology comparison (node count per depth, parent-child structure)
- Flip default to true after parity confirmed

### Wave 2: Vine + Delta + Meta (after wave 1 proven)
- vine.yaml with vine-specific lifecycle handling
- delta.yaml with thread/delta/distillation side effects
- meta.yaml with META-* node emission
- Each needs executor extensions for their unique patterns

### Wave 3: Frontend (after runtime has parity)

**Phase 6: Chain Manager** — list/assign/duplicate chains
**Phase 7: Chain Editor** — two-panel step editor
**Phase 8: Schema Export/Import** — the agent handoff UX
- "Copy Schema for Agent" → markdown with schema + examples + instructions
- "Paste Chain from Agent" → textarea with validation
- Named variants → save, assign per-pyramid

---

## Verification

1. **Prompt parity:** Extracted .md + test substitutions = identical to hardcoded output
2. **Chain parity (Phase 5 gate):** Build 5-chunk conversation with old and new engine. Compare: node count per depth, parent-child topology, node IDs. ALSO compare: selected step outputs (forward_pass[0], reverse_pass[0], combine[0]) and stable node fields (headline, depth, children count). LLM non-determinism means content differs, but structure and intermediate shapes must match.
3. **Resume:** Cancel at step 3, restart. Verify step 3 re-executes (node missing), steps 1-2 skip (nodes exist)
4. **Variant:** Create variant without reverse pass. Verify shorter pipeline, different topology
5. **Assignment:** Assign variant to slug A, default to slug B. Build both. Verify different chains executed
