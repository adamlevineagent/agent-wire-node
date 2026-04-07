# Plan: Config-Driven Chain Binding + Triple-Pass Conversation Pipeline

Status: planning. Carries forward workstreams A, B, C from `docs/conversation-pyramid-testing-state.md`.

## Goals

1. **Expose chain selection as config**, not as hardcoded paths or filename binding. An operator should be able to swap which question YAML and which prompts directory a content_type uses by editing one file.
2. **Document the chain authoring surface** so a developer (us, future-us, an outside contributor, or an agent) can fork a pipeline, author a new chain, and wire it in without reading Rust.
3. **Make the triple-pass chronological conversation pipeline real and testable.** The design-spec exists at `chains/questions/conversation-chronological.yaml` (commit `a7d8a50`); the executor doesn't yet support what it needs.

## Non-goals

- Meta-pyramid / cross-session grounding (workstream D in the testing-state doc, deferred).
- Rewriting the existing question pipeline. The triple-pass variant is a sibling, not a replacement.
- Migrating existing pyramids. New configuration applies to new builds; existing slugs keep their bound chain via `pyramid_chain_assignments`.

---

## Phase 1 — Config-driven chain binding (workstream B)

### What lands

A single YAML registry file that maps `content_type → { questions, prompts }`, plus a Rust loader, plus replacing the hardcoded `prompts/question/` strings throughout the codebase.

### Files

**New: `chains/registry.yaml`**

```yaml
# Chain registry: which question set and prompt directory each content type
# uses by default. Operators can override per-pyramid via the
# pyramid_chain_assignments table; this file is the system default.
#
# To swap a content type's pipeline, change the entry below — no Rust
# changes required, no file renames required. Both the questions YAML and
# the prompts directory are independent levers.
version: 1

bindings:
  code:
    questions: chains/questions/code.yaml
    prompts:   chains/prompts/question/
    description: "Code repository pyramid (default)"

  document:
    questions: chains/questions/document.yaml
    prompts:   chains/prompts/question/
    description: "Document corpus pyramid (default)"

  conversation:
    questions: chains/questions/conversation.yaml
    prompts:   chains/prompts/question-conversation/
    description: "Sequential transcript pyramid — single forward pass with
                  speaker/timestamp awareness. Set questions to
                  conversation-chronological.yaml + prompts to
                  question-conversation-chronological/ for triple-pass."

  question:
    questions: chains/questions/question.yaml
    prompts:   chains/prompts/question/
    description: "Pure question-driven pyramid (no source corpus)"
```

**New: `src-tauri/src/pyramid/chain_registry_yaml.rs`**

Loader and resolver. Public API:

```rust
pub struct ChainBinding {
    pub content_type: String,
    pub questions_path: PathBuf,   // resolved absolute
    pub prompts_dir: PathBuf,       // resolved absolute
    pub description: String,
}

pub struct ChainRegistry {
    bindings: HashMap<String, ChainBinding>,
}

impl ChainRegistry {
    pub fn load(chains_dir: &Path) -> Result<Self>;
    pub fn resolve(&self, content_type: &str) -> Result<&ChainBinding>;
    pub fn list(&self) -> Vec<&ChainBinding>;
}
```

Loader behavior:
- Read `chains/registry.yaml` if it exists.
- If missing, fall back to a hardcoded default registry that matches today's behavior (everything → `prompts/question/`, content-type-named YAMLs). This makes the change non-breaking for existing installations.
- Validate every referenced path exists at load time. Hard error on missing files. Surface the error in the Tauri startup log so operators see it immediately.
- Cache the registry in `PyramidState` so build-time lookups are O(1).

### Call sites to update

The hardcoded `prompts/question/` strings live at (verified via grep):

| File | Line | Use |
|---|---|---|
| `evidence_answering.rs` | ~850 | answer.md path |
| `evidence_answering.rs` | ~187 | pre_map.md path |
| `evidence_answering.rs` | ~417 | pre_map_stage1.md path |
| `extraction_schema.rs` | ~201 | synthesis_prompt.md path |
| `chain_executor.rs` | (search for `prompts/question`) | various |
| `characterize.rs` | (search for `prompts/question`) | characterize.md path |

Each call site needs the slug's content_type in scope so it can resolve the prompts dir from the registry. Where content_type isn't already passed, thread it through. Where it is, just swap the hardcoded path.

Pattern:

```rust
// before
let prompt_path = chains_dir.join("prompts/question/answer.md");

// after
let binding = state.chain_registry.resolve(content_type)?;
let prompt_path = binding.prompts_dir.join("answer.md");
```

### Preserve operator override

The existing `pyramid_chain_assignments` table already lets an operator pin a specific slug to a specific chain. The registry is the *system default*; per-slug overrides still beat it. Resolution order:
1. `pyramid_chain_assignments[slug]` if present
2. `chain_registry.bindings[content_type]` from `registry.yaml`
3. Hardcoded fallback (today's behavior)

### Tests

- `test_load_registry_with_defaults` — registry.yaml present, all paths resolve
- `test_load_registry_missing_file_falls_back` — no registry.yaml, default bindings used
- `test_load_registry_missing_referenced_file_errors` — registry.yaml points at a YAML that doesn't exist on disk
- `test_resolve_unknown_content_type` — returns clean error
- Integration: build a small conversation pyramid, verify the executor loaded prompts from the registry-resolved path (assert via instrumentation log)

### Rollout

- Ship Phase 1 standalone. Conversation builds will start using `prompts/question-conversation/` (the fork from commit `e9c9c7f`) automatically once the registry points there. This is the moment the fork actually takes effect.
- After Phase 1 lands and bakes for a build or two, remove the temporal-aware gates from `prompts/question/answer.md`, `decompose.md`, `extraction_schema.md`, `source_extract.md`, `synthesis_prompt.md`. The conversation copy keeps them and makes them unconditional. The generic pipeline returns to its pre-edit state.

### Estimated effort

Small. ~150-300 lines of Rust (loader + resolver + threading) plus one new yaml file. One rebuild cycle to test.

---

## Phase 2 — Chain developer documentation

### What lands

A new doc tree under `docs/chain-development/` aimed at someone (human or agent) authoring or modifying a chain without reading Rust.

### Files

```
docs/chain-development/
├── README.md                       — index / start here
├── 01-architecture.md              — content_type → registry → questions YAML → prompts dir
├── 02-question-yaml-reference.md   — schema for chains/questions/*.yaml (steps, sequential_context, constraints, prompts)
├── 03-prompt-anatomy.md            — what each prompt in chains/prompts/question*/ does and when it runs
├── 04-temporal-conventions.md      — speaker labels, timestamps, what the executor guarantees in chunks
├── 05-pillar-37-and-prompt-discipline.md — no numerical ranges, truth conditions, why
├── 06-forking-a-pipeline.md        — step-by-step: copy prompts dir, fork question YAML, register, test
├── 07-authoring-a-new-content-type.md — adding a brand-new content_type from scratch
├── 08-testing-a-chain.md           — running parity, building a test pyramid, haiku eval pattern
└── 09-troubleshooting.md           — common failure modes (meta-questions, all-disconnect, schema generator drift)
```

### Content highlights

**01-architecture.md** — diagram of:
```
slug.content_type → ChainRegistry.resolve() → ChainBinding {
                                                 questions: chains/questions/X.yaml
                                                 prompts:   chains/prompts/Y/
                                               }
                                             → run_decomposed_build loads
                                               prompts from binding.prompts_dir
```
Explains that `chains/questions/*.yaml` is the structural shape (which step runs, what it asks, what it creates) while `chains/prompts/*/` is the per-step LLM instruction set, and the two are independently swappable.

**03-prompt-anatomy.md** — exhaustive list of every prompt in `chains/prompts/question/`:
- `characterize.md` — runs once at build start, classifies the source material
- `decompose.md` — turns the apex question into a sub-question tree
- `extraction_schema.md` — generates a per-pyramid extraction prompt + topic schema
- `source_extract.md` — fallback per-source extractor (when extraction_schema doesn't generate one)
- `pre_map.md`, `pre_map_stage1.md` — maps L0 evidence to questions
- `answer.md` — synthesizes L1+ answers from evidence (with abstain rule)
- `answer_merge.md` — merges parallel answer batches
- `synthesis_prompt.md` — generates the answer/web prompts the upper layers consume
- `question_web.md`, `web_cluster.md`, `web_cluster_merge.md`, `web_master.md`, `web_domain_apex.md` — webbing pass
- `horizontal_review.md`, `enhance_question.md`, `targeted_extract.md`, `decompose_delta.md` — auxiliary

For each: when it runs, what it consumes, what it produces, what variables it can use (`{{audience_block}}`, `{{content_type_block}}`, `{{synthesis_prompt}}`, `{{depth}}`, etc.), known failure modes.

**04-temporal-conventions.md** — documents the speaker-label / timestamp contract:
- `pyramid::ingest::parse_conversation_messages` produces `--- PLAYFUL [iso] ---` and `--- CONDUCTOR [iso] ---` markers (`ingest.rs:171-238`)
- Any chain that ingests sequential transcripts MUST preserve these markers in its L0 topic schema as `speaker` and `at` fields
- Why: without temporal anchors no upper layer can write a chronological story
- How: the schema-generation site in `extraction_schema.rs` will hardcode these fields when characterize identifies the source as sequential (Phase 3).

**05-pillar-37-and-prompt-discipline.md** — short but essential. Wire Pillar 37: never prescribe outputs to intelligence. Concretely: no "at least N", no "between 3 and 7", no "minimum X". Use truth conditions ("if every verdict is DISCONNECT, abstain"), what-to-preserve directives ("record the speaker label exactly as written"), and natural-language framing ("one logical zoom-level pulled back"). Existing examples in `answer.md` and `decompose.md`.

**06-forking-a-pipeline.md** — concrete recipe using the conversation fork as the worked example:
1. `cp -r chains/prompts/question chains/prompts/question-NEWVARIANT`
2. Edit the prompts in the new directory freely; remove gates, add directives
3. (Optionally) `cp chains/questions/conversation.yaml chains/questions/conversation-NEWVARIANT.yaml` and edit its structure
4. Update `chains/registry.yaml` to point `conversation` (or whichever content_type) at the new paths
5. Restart Wire Node — the registry is loaded at startup
6. Build a test pyramid, verify the new prompts loaded via instrumentation log
7. Compare results against the previous binding

**08-testing-a-chain.md** — the test loop we developed across runs 1-4:
- Build a test pyramid via the desktop UI (or `pyramid_question_build` IPC)
- Drill into the L1 nodes manually, look for failure modes (meta-nodes, generic copy, missing temporal fields)
- Run a haiku agent against the pyramid via the MCP CLI to score it (template prompt included)
- Iterate

**09-troubleshooting.md** — symptoms and root causes from the actual test arc:
- "L1 node has all-DISCONNECT verdicts and generic copy" → meta-question from decompose, abstain rule didn't fire (check answer.md is loaded from the right prompts dir)
- "Apex is generic / not chronological" → synthesis_prompt.md isn't telling the upper layers to frame chronologically; check the temporal gate fired
- "L0 nodes missing speaker/at fields" → schema generator drifted, the meta-prompted directive didn't translate; needs the hardcoded approach in Phase 3
- "Build crashes immediately on a single-file source" → `build_folder_map` regression; verify the file-source branch is intact

### Estimated effort

Medium. ~1200-1800 lines of markdown. Mostly capturing knowledge that already lives in the code and in `docs/conversation-pyramid-testing-state.md`. Best done after Phase 1 lands so the architecture diagram matches reality.

---

## Phase 3 — Triple-pass conversation pipeline (workstream A)

### What lands

The Rust executor extensions and supporting changes that make `chains/questions/conversation-chronological.yaml` actually executable, plus the matching prompts directory, plus a registry binding to enable it.

### Sub-phase 3a — Executor support for the design-spec features

**3a.1 — `sequential_context.direction: "reverse"`**

Today: `question_loader.rs:158` only accepts `mode: "accumulate"`. The runner iterates chunks earliest→latest with a forward-accumulating context buffer.

Needed:
- Add `direction: Option<String>` to `SequentialContextConfig` in `question_yaml.rs`
- Loader accepts `direction: "forward"` (default) or `direction: "reverse"`
- Runner: when `direction == "reverse"`, iterate chunks latest→earliest, populate context buffer with `carry` of future chunks, trim from `end` instead of `start`
- Both directions still write per-chunk outputs that downstream steps can address

**3a.2 — `save_as: step_only`**

Today: every L0-shaped step persists its outputs as nodes in the pyramid. Forward + reverse passes are throwaway intermediates and shouldn't pollute the node graph.

Needed:
- Add `save_as: Option<String>` to step config (already exists in legacy chain DSL — check whether we can reuse)
- Accepted values: `node` (default, persist as L0 node), `step_only` (persist in build state only, addressable by name from later steps but not written to `pyramid_nodes`)
- Runner: respect the flag at the persist site; step_only outputs go to a per-build hashmap keyed by step name

**3a.3 — `input.zip_steps: [...]`**

Today: a step's input is a single source (`$chunks`, `$L0`, `$step_name`). The combine step needs to consume two prior step outputs and pair them per-chunk.

Needed:
- Add `zip_steps: Option<Vec<ZipStepRef>>` to `StepInput` config
- Each entry is either a step name or `{ step: name, reverse: bool }` (reverse flag flips iteration order before zipping, so latest-first reverse-pass output pairs with earliest-first forward-pass output by chunk index)
- Runner: when `zip_steps` is present, build the input for each iteration as `{forward_view: ..., reverse_view: ...}` keyed by the listed step names
- The combine prompt receives both views as named fields it can reference (`{{forward_view}}`, `{{reverse_view}}` or as JSON object fields)

### Sub-phase 3b — Make schema-field injection driven by chain config (not by Rust deciding)

This is the fix for Run 4's load-bearing failure. The principle: **the chain YAML decides what structural fields the L0 schema must contain; the Rust executor enforces what the chain says.** Rust never makes the call about whether a pyramid is "temporal" — the chain config does, by declaring it.

This keeps the entire decision composable. A new pipeline that wants `{location, mood, weather}` fields instead of `{speaker, at}` writes those into its YAML and gets them, with no Rust changes.

**Schema additions to the chain YAML DSL:**

Add an optional `enforce_topic_fields:` block to the L0-shaped step in any question YAML:

```yaml
  - ask: "..."
    creates: L0 nodes
    enforce_topic_fields:
      - name: speaker
        description: "Speaker label exactly as written in the chunk marker"
        required: true
      - name: at
        description: "ISO timestamp from the chunk marker for the moment this finding was first introduced"
        required: true
```

The list is arbitrary — any number of fields, any names, any descriptions. The chain author decides.

**Optionally**, declare the same at the registry level so all chains in a binding inherit a baseline:

```yaml
  conversation-chronological:
    questions: chains/questions/conversation-chronological.yaml
    prompts:   chains/prompts/question-conversation-chronological/
    description: "Triple-pass chronological conversation pyramid"
    enforce_topic_fields:
      - name: speaker
      - name: at
```

Step-level declarations override registry-level for the same field name.

**What Rust does (the enforcement, not the decision):**

In `extraction_schema.rs` at the schema-generation site (`generate_synthesis_prompts` or wherever the `topic_schema` is materialized):

1. Read the resolved chain binding's `enforce_topic_fields` list (registry-level + step-level merged)
2. If the list is empty, do nothing — the LLM-generated schema is used as-is, exactly like today
3. If the list is non-empty, after the LLM produces its `topic_schema`, **append (or upsert)** every field from `enforce_topic_fields` into it before writing the schema to the build state
4. The enforced fields go through to the L0 extractor LLM as required fields it must populate, alongside any LLM-generated ones

The temporal-aware directive in `prompts/question*/extraction_schema.md` stays as belt-and-suspenders prose for the LLM (it'll see why those fields exist), but the field presence no longer depends on the LLM remembering. Two layers: the chain config makes the structural guarantee, the prose makes the semantic intent clear.

**Why this is composable, not hardcoded:**

- Rust contains zero references to "speaker", "at", "timestamp", "PLAYFUL", "CONDUCTOR", "transcript", "conversation", or "temporal". It only knows how to read an `enforce_topic_fields` list and append the entries to a topic_schema.
- The conversation-chronological pipeline declares its own temporal field set in YAML.
- A future "meeting-with-emotion-tracking" pipeline can declare `{speaker, at, emotion, energy_level}` in its own YAML, and the same Rust code path enforces it.
- A code or document pipeline declares no enforced fields and behaves exactly as today.
- Removing the temporal capture from a pipeline is a YAML edit, not a Rust change.
- Promoting `conversation-chronological` to the default `conversation` binding is a one-line registry edit; the temporal field enforcement comes with it because it's declared in the chain config, not in Rust.

This is the bar set by the composability test in our prior conversation: forking `conversation-chronological` into `meeting-five-pass` requires zero Rust changes. With this design, that holds.

### Sub-phase 3c — Author the matching prompts directory

```bash
cp -r chains/prompts/question-conversation chains/prompts/question-conversation-chronological
```

Then edit the four prompts in the new directory that the triple-pass yaml references:
- `forward.md` (already drafted at `chains/prompts/conversation-chronological/forward.md` — move to new home)
- `reverse.md` (already drafted, move)
- `combine.md` (already drafted, move)
- `extraction_schema.md` — strip the meta-prompted temporal directive (now redundant with hardcoded fields in 3b)

The remaining prompts (decompose, answer, pre_map, synthesis, web_*) are inherited from the conversation copy.

### Sub-phase 3d — Wire the chronological binding

Add to `chains/registry.yaml`:

```yaml
  conversation-chronological:
    questions: chains/questions/conversation-chronological.yaml
    prompts:   chains/prompts/question-conversation-chronological/
    description: "Triple-pass chronological conversation pyramid (forward/reverse/combine)"
```

This is a *new content_type* (`conversation-chronological`) rather than a swap of the default `conversation` binding. Operators opt in by either:
- Setting the slug's content_type to `conversation-chronological` at creation, or
- Pinning an existing conversation slug via `pyramid_chain_assignments`

This way the default conversation pyramid keeps its known-good single-pass behavior, and the chronological variant is available for opt-in until it's proven stable enough to promote to default.

### Sub-phase 3e — Test the triple-pass pyramid

Same test loop as Run 4:
1. Create a new conversation pyramid pointed at the same `.jsonl`
2. Set its content_type to `conversation-chronological` (or pin via assignment)
3. Use the chronological steelman question: "Tell the story of this chat session in chronological order: what was attempted, what failed, what was learned, what was decided, and what shipped? What was true at the beginning of the sequence that was not true at the end, and vice-versa?"
4. Build it
5. Verify L0 nodes have `speaker` + `at` fields populated (the hardcoded approach should make this airtight)
6. Drill into L1s — confirm no meta-nodes, no generic copy
7. Read the apex — should be at least as chronological as Run 4's
8. Run a haiku agent for an independent score
9. Compare to Run 4's 6/10

Target: 8/10 or higher, with **temporal capture passing** as the headline win.

### Estimated effort

Larger than Phase 1. Rust changes touch the executor at multiple points:
- `question_yaml.rs` (~50 lines): new fields on `SequentialContextConfig` and `StepInput`
- `question_loader.rs` (~30 lines): validation for new fields
- `question_decomposition.rs` / runner (~150-300 lines): reverse iteration, step_only persistence, zip_steps input assembly
- `extraction_schema.rs` (~50 lines): hardcoded temporal field injection
- `chain_registry_yaml.rs` (already from Phase 1): no changes
- New prompts directory (no new prompts; just relocate the three drafted ones)
- Tests for each new feature

One rebuild cycle. One test pyramid. Possibly two iterations to tune.

---

## Sequencing

```
Phase 1 (Config-driven binding)
   │
   ├──→ Phase 2 (Chain developer docs)
   │       │ depends on Phase 1 architecture being real
   │
   └──→ Phase 3 (Triple-pass executor + chronological pipeline)
           │ depends on Phase 1 binding existing so 3d can wire it
           │ Phase 2 docs can be backfilled with Phase 3 examples
```

Phase 2 and Phase 3 can run in parallel after Phase 1 lands, if we want. Realistically we'll want Phase 1 in a single PR, then Phase 3 in a second PR, then Phase 2 written against the merged state of both.

## Risks

1. **Threading content_type to all the prompt-loading sites in Phase 1.** Some sites currently don't have it in scope. Surface area is small but tedious. Mitigation: do the threading first as a no-op refactor, then introduce the registry resolution.

2. **The reverse-iteration runner change in 3a.1 might interact with chunk-sized batching elsewhere.** The current runner assumes forward-only iteration in several places. Need to audit.

3. **`zip_steps` opens a door to combinatorial step inputs we don't want.** Limit to exactly two steps per zip in the initial implementation; revisit if a real use case needs more.

4. **Hardcoded temporal field injection in 3b might conflict with custom topic_schemas defined in YAML.** Resolution: registry-bound chains can opt out via a flag, but the default for any binding flagged as `temporal: true` is to inject the fields.

5. **Existing conversation pyramids built before Phase 1 won't have temporal fields.** They keep working as-is; new builds get the upgrade. No migration needed.

## Open questions

- ~~Should `chains/registry.yaml` support per-binding `temporal: true` flags so the schema-generation site knows when to inject speaker/at, or should that be inferred from the characterize result alone?~~ **Resolved in 3b above**: neither. The chain YAML declares its own `enforce_topic_fields` list (or inherits one from the registry binding), and Rust enforces what the chain says. The `temporal` concept never leaks into Rust at all.

- Should the `conversation-chronological` content_type be a real first-class type (added to the `ContentType` enum in Rust) or a virtual content_type that only exists in the registry? **Recommendation:** first-class. Keeps the slug-creation flow honest and lets the desktop UI offer it as a wizard option.

- Should we auto-promote `conversation-chronological` to be the default `conversation` binding once it tests at parity or better? **Recommendation:** yes, but only after at least three independent test pyramids score 8/10 or higher with the haiku evaluator. Until then it lives as opt-in.

## Done criteria

- [ ] `chains/registry.yaml` exists and is loaded at startup
- [ ] `ChainRegistry::resolve(content_type)` returns the correct binding for code, document, conversation, question
- [ ] All hardcoded `prompts/question/` paths replaced with registry-resolved paths
- [ ] Conversation builds load from `chains/prompts/question-conversation/` automatically
- [ ] `docs/chain-development/` exists with all 9 files
- [ ] Executor accepts `sequential_context.direction: "reverse"`
- [ ] Executor accepts `save_as: step_only`
- [ ] Executor accepts `input.zip_steps`
- [ ] `extraction_schema.rs` hardcodes `speaker` + `at` fields when source is sequential
- [ ] `chains/prompts/question-conversation-chronological/` exists with the three new pass prompts
- [ ] `conversation-chronological` registry binding works
- [ ] A test pyramid built with `conversation-chronological` has L0 nodes with populated `speaker` + `at` fields
- [ ] Haiku evaluator scores the chronological test pyramid at 8/10 or higher
