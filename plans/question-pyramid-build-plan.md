# Question Pyramid: Build Plan

## What This Is

The question pyramid system takes a user's question and a folder, and builds a knowledge pyramid that answers that question. The question defines everything — what to extract, how to organize, how deep to go, what matters. The pyramid hangs from its question, not builds up from data.

## Core Architecture

### The Seven Meta-Questions

Every pyramid is created by answering seven meta-questions in sequence. Steps 1-4 run before any source material is read. L0 extraction runs after Step 4. Steps 5-7 run after L0 results are available.

#### Step 1: "What am I looking at, given what the user wants to know?"

- **Input**: user's question + folder path
- **Action**: Read the folder map (file names, extensions, directory structure — no content). Characterize the material in context of the question.
- **Output**: material profile ("112 Rust + TypeScript files, Tauri desktop app, ~50K lines") + initial interpretation

#### Step 2: "Is this what the user means?"

- **Input**: interpreted question + material profile + audience inference
- **Action**: Present the interpretation back to the user for confirmation or correction. "Here's how I understand your question and who I think you are. Is this right?"
- **Output**: confirmed question interpretation + audience profile + tone guidance
- **Note**: This is a conversation moment. The user sees the interpretation and either confirms or corrects. This prevents the entire pyramid being built on a wrong assumption.

#### Step 3: "What claims, if proven, would fully answer this question?"

- **Input**: confirmed question + material profile
- **Action**: Decompose the question into the minimum set of sub-claims that constitute a complete, evidence-supported answer. Each claim must be:
  - **Necessary** — removing it leaves a gap in the answer
  - **Sufficient** — evidence from the source material can prove it
  - **Non-redundant** — no two claims prove the same thing
- **Recursion**: Each sub-claim is itself a question. If it can be answered directly from source files, it's a leaf. If it needs further decomposition, recurse. Stop when every leaf is answerable by reading files.
- **Output**: complete question tree with all leaves identified
- **No suggested ranges**: Don't say "3-7 sub-questions." Trust the decomposition intelligence to produce exactly as many claims as the question requires.

#### Step 4: "What should I look for in each file?"

- **Input**: all leaf questions from the question tree + material profile
- **Action**: Generate the extraction prompt and schema. The leaf questions tell the system what aspects of each file matter. If leaves include "Who uses this?" the extraction captures user-facing features. If leaves include "What are the trust boundaries?" the extraction captures auth patterns.
- **Output**:
  - Extraction prompt (fills the generic extract template)
  - Per-pyramid topic schema (what fields nodes at every level should have)
  - This schema is NOT fixed — it's generated from the question

### → RUN L0 EXTRACTION ←

With the extraction prompt generated, read every source file. Per-file, parallel. The extraction is shaped by the question — it looks for what the leaf questions need, not "everything."

#### Step 5: "How should results be grouped?"

- **Input**: question tree + actual L0 results
- **Action**: With real data in hand, design the grouping criteria for each non-leaf level. The L0 results reveal: how many topics exist, how much overlap between files, which sub-questions have rich vs sparse evidence. A sub-question with 40 supporting L0 nodes groups differently than one with 3.
- **Output**: clustering prompt per non-leaf level (fills the generic classify template)
- **Also**: Prune or flag sub-questions that have no evidence. If a leaf question found nothing relevant in any file, it should be removed from the tree rather than producing an empty node.

#### Step 6: "How should each answer read?"

- **Input**: question tree + audience profile + tone guidance + L0 results
- **Action**: Generate the synthesis prompt for each non-leaf node. The sub-question itself IS the prompt — "Answer: 'What problem does this solve?' using these inputs." The audience context shapes tone and detail level.
- **Output**: synthesis prompt per non-leaf node (fills the generic synthesize template)

#### Step 7: "What connections matter?"

- **Input**: question tree + L0 results
- **Action**: Decide what types of cross-references matter for this pyramid based on what the L0 results actually contain. For a "what is this?" pyramid: shared concepts and terminology. For a developer pyramid: shared tables, endpoints. For a security pyramid: shared trust boundaries.
- **Output**: connection type guidance (fills the generic web template)

### → RUN BUILD ←

Bottom-up: cluster → synthesize → web → up to apex. Every prompt was shaped by the question. Every step uses the generated prompts, not hardcoded files.

---

## Content-Agnostic Templates

Four generic prompt templates replace all content-type-specific prompt files.

### `templates/extract.md`
```
Read this material. You are gathering information to help answer: "{{question}}"

Focus on these aspects:
{{aspects}}

Output valid JSON:
{
  "headline": "2-6 word label",
  "orientation": "{{orientation_guidance}}",
  "topics": [
    {
      "name": "Topic Name",
      "current": "{{detail_guidance}}",
      "entities": [{{entity_types}}]
    }
  ]
}

/no_think
```

### `templates/synthesize.md`
```
Answer this question: "{{question}}"

Using the provided inputs. Be specific, use real names from the material.
{{additional_guidance}}

Output valid JSON:
{
  "headline": "{{headline_guidance}}",
  "orientation": "{{orientation_guidance}}",
  "topics": [{{topic_schema}}]
}

/no_think
```

### `templates/classify.md`
```
Group these items to answer: "{{question}}"

{{grouping_criteria}}

Each group must be a coherent answer to one aspect of the question.
Create exactly as many groups as the material demands — no more, no less.

Output valid JSON with groups.

/no_think
```

### `templates/web.md`
```
What concrete resources or concepts are shared between these items?

Look for: {{connection_types}}

Output valid JSON with edges (source, target, relationship, strength).

/no_think
```

The `{{slots}}` are filled during Steps 4-7. The templates never change. All content-type knowledge lives in the decomposition logic, not in prompt files.

---

## Schema Generation

The topic schema is generated per-pyramid during Step 4, not fixed globally.

"What is this and why should I care?" generates:
```json
{
  "name": "string",
  "current": "string — 2-4 sentences, plain language",
  "entities": ["feature names", "user-facing concepts"]
}
```

"What should a developer know?" generates:
```json
{
  "name": "string",
  "current": "string — 3-5 sentences, technical detail",
  "entities": ["function()", "StructName", "table: name(col1, col2)"],
  "corrections": [{"wrong": "", "right": "", "who": ""}],
  "decisions": [{"decided": "", "why": ""}]
}
```

"What are the security vulnerabilities?" generates:
```json
{
  "name": "string",
  "current": "string — describe the vulnerability and its impact",
  "entities": ["function()", "endpoint", "credential"],
  "risk_level": "critical|high|medium|low",
  "attack_vector": "string",
  "mitigation": "string"
}
```

The schema is derived from the question. Different questions produce different node shapes.

---

## What Exists vs What Needs to Change

### Keep As-Is
- IR executor (`execute_plan`) — correct architecture, runs any execution plan
- Defaults adapter — compiles legacy YAML to IR (backward compatibility for bottom-up pyramids)
- Converge expansion — compile-time replacement for runtime recursive clustering
- Expression engine — `$ref` resolution, wildcards, guards
- Question decomposition basics — tree generation from seed question
- Build runner dispatch — routes to correct executor
- All 481+ tests

### Delete
- Legacy executor (`execute_chain_from`, ~2,600 lines) — after parity validation
- `ChainContext` — absorb into `ExecutionState`
- `WriteOp` / `IrWriteOp` duplication — consolidate
- `use_chain_engine` flag — IR becomes the only path
- All content-type-specific prompt files (code_extract.md, doc_extract.md, etc.) — replaced by generic templates + generated prompts

### Modify
- `question_compiler.rs` — emit generated prompts from decomposition, not references to fixed prompt files
- `question_decomposition.rs` — implement the seven meta-questions, including user confirmation step and claim-based decomposition
- `chain_dispatch.rs` — consolidate `dispatch_step` and `dispatch_ir_step` into one function
- Build runner — add the L0-first flow (extract before clustering design)

### Create
- `chains/templates/extract.md` — generic extract template with slots
- `chains/templates/synthesize.md` — generic synthesize template
- `chains/templates/classify.md` — generic classify template
- `chains/templates/web.md` — generic web template
- Schema generator — Step 4 logic that produces per-pyramid topic schema
- Prompt generator — Steps 5-7 logic that fills template slots from L0 results
- User confirmation endpoint — Step 2 conversation moment

---

## Build Phases

### Phase 1: Executor Unification (prerequisite)
- Validate IR parity with legacy on vibesmithy
- Flip `use_ir_executor: true` as default
- Delete legacy executor, consolidate state types
- Bottom-up YAML pyramids continue working through defaults adapter → IR executor

### Phase 2: Generic Templates
- Create the four template files
- Modify question compiler to generate prompt content from sub-questions instead of referencing fixed files
- Test: "What is this and why should I care?" on vibesmithy should produce purpose-focused nodes, not implementation dumps

### Phase 3: Schema Generation
- Implement Step 4 — derive topic schema from leaf questions
- Implement schema injection into templates
- Test: different questions on same folder produce different node shapes

### Phase 4: L0-First Flow
- Implement the split: Steps 1-4 → L0 extraction → Steps 5-7 → build
- Implement evidence pruning (remove sub-questions with no L0 support)
- Test: grouping responds to actual L0 content, not hypothetical structure

### Phase 5: User Confirmation
- Implement Step 2 as a conversation endpoint
- The `/build/question` endpoint becomes a two-step flow: decompose → confirm → build
- Or: the Vibesmithy/Partner chat interface handles the confirmation naturally

### Phase 6: Wire Convergence
- Question compiler emits Wire action chains instead of IR execution plans
- The IR executor becomes the Wire chain executor
- Pyramid building is just one type of work the Wire processes
- Contribution types (actions, templates, skills, questions) become Wire contributions

---

## Evaluation

The rubric for a question pyramid IS the question. There is no universal 10-point checklist.

Evaluation criteria:
1. **How well does this pyramid answer the question it was asked?** (0-10)
2. **What's missing, wrong, or unnecessary?** (critique)
3. **How accurate is what it says?** (fact check)

Different questions produce different evaluations. A pyramid that scores 3/10 on "What is this and why should I care?" might score 9/10 on "How is this system built?" — and both scores would be correct.
