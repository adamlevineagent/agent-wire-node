# Pyramid Folders, Model Routing, and Full-Pipeline Observability

**Date:** 2026-04-09
**Status:** Design — pre-implementation
**Authors:** Adam Levine, Claude (session design partner)

---

## Overview

Three interlocking capabilities that transform Wire Node from a tool you configure pyramids in to a tool that *understands your filesystem and lets you control how intelligence is applied at every step*.

1. **Recursive Folder Ingestion** — point at a folder, get nested pyramid/vine compositions automatically
2. **Per-Step Model Routing with YAML-Generated UI** — chain definitions create their own configuration surface; users control which provider+model handles each pipeline step
3. **Full-Pipeline Observability** — every LLM output persisted, OpenRouter Broadcast integration for cost actuals, complete build visualization

These are not independent features. They compose: folder ingestion creates many pyramids that need cheap local compute (model routing), which generates many LLM calls that need cost visibility (observability), which feeds back into intelligent routing decisions.

---

## 1. Recursive Folder Ingestion

### The Concept

A user points at a folder. The system walks it recursively, detects what's inside, and creates a self-organizing hierarchy of pyramids and vines — no manual configuration.

```
AI Project Files/                    <- topical vine (apex of everything)
+-- GoodNewsEveryone/                <- topical vine
|   +-- src/                         <- code pyramid
|   +-- docs/                        <- topical vine
|   |   +-- architecture/            <- document pyramid
|   |   +-- plans/                   <- document pyramid
|   +-- supabase/migrations/         <- code pyramid
+-- agent-wire-node/                 <- topical vine
|   +-- src-tauri/src/               <- code pyramid
|   +-- src/                         <- code pyramid (React)
|   +-- mcp-server/                  <- code pyramid
|   +-- docs/                        <- document pyramid
+-- vibesmithy/                      <- topical vine
```

### Self-Organizing Rules

- **Homogeneous folder with enough files** -> pyramid of that content type (threshold: ~3+ files)
- **Mixed-content folder** -> topical vine composing its children
- **Folder with subfolders** -> topical vine where each subfolder becomes a bedrock (pyramid or vine)
- **Recursion terminates** when a folder's contents are homogeneous enough to be a single pyramid
- **Minimum content threshold** -> don't create a pyramid for 1-2 files; include them in parent
- **Content type detection**: file extensions route to code/document; detected chat formats to conversation
- **Respect ignore patterns**: `.gitignore`-style skip rules, binary detection, max-file-size gating

### Architectural Prerequisites

**Vine-of-Vines Composition.** Currently `vine_composition.rs` rejects vine content type — vines cannot compose other vines. Folder ingestion is fundamentally recursive vine composition. A parent folder is a vine whose children include both file-derived pyramids (leaf content) and sub-folder vines. This constraint must be lifted.

**Topical Vine Recipe.** The existing temporal/episodic vine is conversation-specific — ordering is semantic (chronological). For folder composition, we need a second vine recipe:

- **Temporal vine** (exists) — for conversation sessions, episodic memory, sequential ordering carries meaning
- **Topical vine** (new) — for folder composition, organizes bedrocks by topic/dependency rather than time. Clustering uses import-graph and reference-graph signals from code bedrocks, entity-overlap from doc bedrocks. Needs new YAML chain definition + prompts.

In practice, code vines and document vines do not differ meaningfully at the composition level. One topical vine recipe handles both. The temporal conversation vine is the only special case, and it already exists.

**Bedrock Independence.** Already the right model — `VineComposition` tracks bedrocks with status/position, bedrocks are shared not owned. A bedrock created for one vine is reusable by another without recreation. This is correct and load-bearing for recursive composition.

---

## 2. Per-Step Model Routing with YAML-Generated UI

### The Concept

Chain YAML definitions already declare every step: its name, primitive type, model tier, concurrency, and operational characteristics. Instead of hand-building a settings page, **the YAML generates its own configuration UI**. Add a step to a chain definition, it appears in the config panel. No React code per step.

### Why This Matters

- Every new chain step automatically gets a configuration surface
- The YAML is the single source of truth for both execution AND configuration UI
- Users see the actual pipeline structure — they're configuring the real thing
- Custom chains get configuration UI for free
- Agents can propose model routing changes (the routing table is itself a contribution)

### The Routing Architecture

**Three levels of model resolution** (already partially implemented in `resolve_model()` / `resolve_ir_model()`):

1. **Provider Registry** — declares available compute backends:
   ```
   providers:
     openrouter:
       type: openrouter
       base_url: https://openrouter.ai/api/v1
       api_key: (from settings)
       broadcast: (webhook config for observability)
     ollama-local:
       type: openai_compat
       base_url: http://localhost:11434/v1
       auto_detect_context: true   # GET /api/show -> context_length
   ```

2. **Tier-to-Provider+Model Mapping** (extends existing `model_aliases`):
   ```
   tier_routing:
     extractor: { provider: openrouter, model: inception/mercury-2 }
     web: { provider: ollama-local, model: gemma3:27b }
     synth_heavy: { provider: openrouter, model: qwen/qwen3.5-flash }
   ```

3. **Per-Step Overrides** — user-driven via the generated UI, stored in DB

### Example Configuration

A power user with a local GPU and OpenRouter key:

| Step Class | Tier | Provider | Model | Rationale |
|---|---|---|---|---|
| L0 extraction (first ingest) | extractor | OpenRouter | Mercury 2 | Quality matters for bedrock |
| Webbing | web | OpenRouter | M2.7 | Cheap near-frontier |
| L1+ synthesis, evidence, apex | synth_heavy | OpenRouter | M2.7 | Worth pennies for intelligence |
| Stale checks, DADBEAR L0 | stale_local | Ollama | Gemma 42B | High volume, local = free |
| DADBEAR L1+ | stale_remote | OpenRouter | M2.7 | Higher synthesis worth paying for |

A cost-sensitive user doing everything locally:

| Step Class | Provider | Model |
|---|---|---|
| Everything | Ollama | Gemma 42B |

### The Generated UI

```
Pipeline: question-pipeline v2.0.0

  Tier Defaults
  +---------------------------------------------------------+
  |  extractor    -> OpenRouter / mercury-2      [128k]     |
  |  web          -> Ollama / gemma-42b          [32k]      |
  |  synth_heavy  -> OpenRouter / m2.7-flash     [900k]     |
  +---------------------------------------------------------+

  Step                  Tier          Model              Cost/call
  source_extract        extractor     <- tier default     $0.003
  l0_webbing            web           <- tier default     $0.00
  enhance_question      synth_heavy   <- tier default     $0.001
  decompose             synth_heavy   <- tier default     $0.001
  evidence_loop         synth_heavy   (override: ollama)  $0.00
  l1_webbing            web           <- tier default     $0.00
  l2_webbing            web           <- tier default     $0.00
```

### Local Compute Mode

Not a wizard. A single toggle: **"Local mode (Ollama)"** that:
- Sets all tiers to the local provider
- Auto-detects model context window via Ollama API (`/api/show`)
- Derives dehydration budgets from detected context limit
- Sets concurrency to 1 (home hardware)

If the auto-detection works (and it should — Ollama's API exposes context_length), the user makes one choice and everything adapts.

### LLM Agnosticism Principle

The system is fundamentally agnostic about how people connect LLMs. Users can:
- Put in an OpenRouter API key (existing flow)
- Connect to Ollama locally
- Hook up to any OpenAI-compatible API manually
- Mix and match across steps granularly

The provider registry is the abstraction. `llm.rs` currently hardcodes `https://openrouter.ai/api/v1/chat/completions` — this becomes `provider.base_url + "/chat/completions"` with provider-specific auth and response parsing.

---

## 3. Full-Pipeline Observability

### Universal LLM Output Persistence

**Core insight:** Every LLM output is intelligence. The cost to store is near-zero, the cost to regenerate is real (time + money), and the cache-hit potential is high on active codebases.

**Current state:**
- Step outputs that hit `send_save_step()` are persisted to `pyramid_pipeline_steps`
- Reverse pass outputs ARE persisted
- `pyramid_llm_audit` captures every LLM call (prompts, responses, tokens, latency)
- `ChainContext.step_outputs` (HashMap) is ephemeral — lost if build abandons mid-chain

**Target state:** Every LLM call is keyed by `(inputs_content_hash, prompt_hash, model_id)`. If that triple matches a prior call whose source material hasn't changed, skip the LLM call and return cached output. The audit trail becomes a content-addressable cache, not a write-only log.

**Reroll / Supersession:** The cache is the default path, but users can "reroll" any node — meaning: ignore the cache for this specific call, run fresh, supersede the existing node with the new output. The cache key stays the same; the new output gets a new `generation_id` and supersedes the old. The cache then stores the new output for future lookups. This is non-deterministic work — sometimes you get outlier bad answers and the right move is to regenerate. Implementation: a `force_fresh: true` flag that bypasses the cache check for that one invocation.

**Implications:**
- Resume after crash or partial build failure — completed steps are cache hits, no wasted recomputation
- Upward stale propagation can cache-hit when a parent node's inputs haven't materially changed (e.g., only one of five children was updated and the parent synthesis is the same)
- Build visualization shows every step (not just node-creating ones) because every step has a persisted output record

**Clarification on DADBEAR:** The cache does NOT make primary stale checks free. DADBEAR triggers on file hash changes, meaning the file *did* change, so the stale check inputs are different and the cache won't match. The stale check LLM call runs every time. Where the cache helps is narrower: (a) crash recovery mid-DADBEAR cycle, (b) parent-level propagation where the parent's synthesis inputs may be unchanged despite a child update, (c) evidence re-answering when the evidence question hasn't changed but its source was re-ingested.

### OpenRouter Broadcast Integration

OpenRouter's Broadcast feature sends OTLP traces to a webhook after each API call completes. This gives us actual cost, tokens, and latency — verified by the provider, not estimated by us.

**The pattern:** Assumptions through the front door, actuals via webhook.

- Wire Node sends LLM calls with `trace` metadata:
  ```json
  {
    "trace": {
      "trace_id": "{build_id}",
      "span_name": "{step_name}",
      "generation_name": "{step_name}",
      "pyramid_slug": "{slug}",
      "depth": "{depth}"
    },
    "session_id": "{slug}/{build_id}",
    "user": "{node_identity}"
  }
  ```
- OpenRouter Broadcast webhook sends OTLP JSON to a Wire Node HTTP endpoint
- Wire Node reconciles `pyramid_cost_log` assumptions against webhook actuals
- Discrepancies surface in the DADBEAR oversight page

Every Wire Node has a built-in Cloudflare tunnel (`tunnel.rs`), so the webhook endpoint is publicly reachable even for home users behind NAT. OpenRouter Broadcast POSTs to `{tunnel_url}/hooks/openrouter`.

For local Ollama calls, there's no webhook — but we control the client, so we log actuals directly (synchronous inline). Same destination table, different ingestion path.

### Evidence Triage Intelligence

A dumb numerical cap on evidence nodes is a Pillar 37 violation. The maximal version:

Evidence questions go through a **triage step** that assesses:
1. Is this question worth answering given what we already know?
2. Is the answer worth keeping current, or is it stable enough to check infrequently?
3. What's the minimum model tier that can answer this reliably?

The triage step is itself a cheap LLM call (local model, short context) that gates expensive ones. This is the model routing table in action:
- Evidence triage -> local Gemma (cheap gate)
- Evidence answering for routine questions -> local Gemma
- Evidence answering for high-value questions -> M2.7 via OpenRouter
- Evidence answering for frontier-quality-needed -> Mercury 2

**There is no absolute standard.** We provide a sensible default policy, but users define for themselves how they want triage to prioritize. Examples of legitimate user preferences:

- "Everything on fast smart expensive except obvious trolling" (quality maximizer)
- "Most things not maintained at all except when there is actual demand; what is maintained is done entirely via low-context local, because I primarily use it as scaffolding for autonomous agents" (cost minimizer / agent-first)
- "Aggressive initial builds with cloud, local-only for maintenance" (hybrid)

We have no idea what any given user wants. That's OK. They'll figure it out, share it on the Wire, and everyone gets to use the best versions of all the solutions.

The evidence triage policy is expressed as YAML (see Generative Configuration Pattern below) — user-editable, contribution-based, Wire-shareable.

### Build Visualization

Currently `LayerEvent` and `PyramidBuildViz.tsx` only fire on node mutations. With every LLM output persisted, each step should emit its own event type. The visualization shows the complete execution trace: forward passes, reverse passes, cluster assignments, evidence checks, webbing — not just node creation.

This is inherently cool to watch and helps users understand what's happening inside their pyramids.

---

## 4. Generative Configuration Pattern

### The Primitive

Every behavioral configuration in Wire Node follows the same flow:

```
User intent (natural language)
        |
        v
LLM generates structured YAML conforming to a schema
        |
        v
System renders YAML as editable UI (same pattern as chain step config)
        |
        v
User accepts / edits / regenerates
        |
        v
YAML becomes runtime config (stored as a contribution)
        |
        v
Shared on Wire -> community discovers best versions
```

The user types a naive ask. The system expands it into a full set of guidelines/schema as YAML. The YAML is presented as UI (per the chain config pattern). The user can accept it or provide notes — and each round of notes produces a new version that supersedes the previous, with the note attached as provenance.

### Notes Paradigm (Not Regeneration)

"Regenerate" is a slot machine pull — it discards context and starts from scratch. The notes paradigm is a conversation with the artifact:

1. User sees generated YAML (rendered as UI)
2. User provides notes: "I don't want cloud calls for maintenance, only initial builds. Check intervals should be weekly minimum."
3. System takes the *existing YAML* plus the *notes* and generates a new version
4. New version supersedes the previous — both exist in the version history
5. The note that produced the transition is attached to the supersession record

```
User intent (natural language)
        |
        v
v1: LLM generates YAML (contribution)
        |
        v
User reviews (rendered as UI)
        |
    [accept] --> done, v1 is active
        |
    [notes] --> "less aggressive, local only for X"
        |
        v
v2: LLM generates new YAML (supersedes v1, note attached)
        |
        v
User reviews v2
        |
    [accept] --> done, v2 is active
    [notes]  --> another round, v3 supersedes v2
```

This is fundamentally different from regeneration:

- **The AI sees what it already produced.** It refines rather than restarting. It knows what the user accepted implicitly (everything they didn't mention) and what to change (the notes). Intent narrows with each round rather than resetting.
- **Every version is a contribution with provenance.** The note that drove each transition is the "why" of the supersession. Six months later, someone (or an agent) can read the version chain and understand not just what the policy is but how it got there — which decisions were deliberate, which defaults were accepted passively, what the user's priorities were based on what they corrected.
- **The notes themselves are intelligence.** "I don't want cloud calls for maintenance" tells the system something about the user beyond this one YAML. It could inform future generative config in other domains. Notes accumulate into an understanding of user intent that makes every subsequent generation better.

This is not new machinery — it's the existing contribution/supersession/annotation pattern applied to configuration artifacts. A policy YAML is a contribution. A note is an annotation. The version history is a provenance trail. On the Wire, the full refinement chain is visible: "they started with the default, made it local-only, tightened check intervals, added demand signals." That chain teaches judgment, not just configuration.

### Why This Is a Primitive, Not a Feature

This is not specific to evidence triage. It's the same pattern for every configurable behavior:

| Domain | User says | System generates |
|---|---|---|
| Evidence policy | "Keep costs low, only update what matters" | Evidence triage YAML with demand signals, model tier routing, check intervals |
| Build strategy | "Maximum quality, this is my main project" | Build config YAML with cloud models, high concurrency, deep evidence |
| Stale check policy | "Only maintain what agents query" | DADBEAR policy YAML with demand-based triggers, local-only compute |
| Custom chain | "I want extraction focused on API contracts" | Chain YAML with custom prompts and extraction instructions |
| Custom prompts | "Focus on architectural decisions, not implementation" | Prompt files tuned to the user's stated priority |
| Custom skills | "A skill that reviews PRs for security issues" | Skill YAML with review criteria and output format |

In every case: user expresses intent -> LLM generates valid YAML -> UI renders it for review -> accepted YAML becomes a versioned contribution -> shareable on Wire.

### Example: Evidence Policy YAML

User types: "Most things not maintained except on demand. Local compute for everything. I use this for agent scaffolding."

System generates:

```yaml
evidence_policy:
  version: 1
  description: "Demand-driven, local-only, agent scaffolding mode"
  
  triage_rules:
    - condition: "first_build AND depth == 0"
      action: answer
      model_tier: stale_local
      priority: normal
    - condition: "stale_check AND no_demand_signals"
      action: defer
      check_interval: "never"
    - condition: "stale_check AND has_demand_signals"
      action: answer
      model_tier: stale_local
    - condition: "evidence_question_trivial"
      action: skip
  
  demand_signals:
    - type: agent_query_count
      threshold: 2
      window: "14d"
    - type: user_drill_count
      threshold: 1
      window: "7d"
  
  budget:
    maintenance_model_tier: stale_local
    initial_build_model_tier: stale_local
    max_concurrent_evidence: 1
```

User sees this as a UI form (each field editable), accepts it, and it becomes the active policy for that pyramid. Later they share it on the Wire tagged "agent-scaffolding-evidence-policy" and other users with similar needs pull it.

### The UI Generation Rule

Every YAML schema gets a generated UI. The schema defines the fields; the UI renders them. Adding a new field to a schema automatically extends the UI. This is the same mechanism as the chain step configuration panel — YAML in, UI out, no per-feature React code.

### The Wire Sharing Multiplier

Someone figures out the optimal evidence policy for a 500-file TypeScript monorepo. They share it as a contribution. Everyone with similar codebases pulls it. The evidence policy YAML becomes tradeable intelligence on the Wire, just like pyramid content. The same applies to build strategies, stale policies, custom chains, custom prompts, and skills. The Wire's value increases because it's not just sharing knowledge — it's sharing operational configurations that encode hard-won judgment.

### Build It Once

The generative configuration infrastructure is:
1. A YAML schema registry (defines valid fields per config type) with a schema annotation layer that tells the renderer how to present each field (dropdown vs freetext vs number vs nested group)
2. A generation prompt per schema (intent -> valid YAML)
3. A generic YAML-to-UI renderer (already needed for chain config)
4. Contribution storage for accepted configs (already exists)

Build the renderer and the generation path once. Every new configurable behavior gets intent-to-YAML-to-UI-to-contribution for free.

**Implementation note:** The YAML-to-UI renderer is load-bearing for the entire generative configuration pattern and must be designed carefully. Full documentation of the schema annotation model, renderer capabilities, and supported field types MUST be written before any implementation begins. Getting the schema annotation model wrong costs more than the implementation itself.

### YAML-Driven Creation UI

The "add workspace / generate pyramid" interface should also be driven by loaded chain YAMLs rather than hardcoded content type options. Currently, creating a pyramid means picking from a fixed list (code, document, conversation, question). Instead:

- Available pipeline configurations come from loaded chain YAML definitions
- Adding a new chain YAML (like the topical vine recipe, or a custom user chain) automatically makes it available as a creation option
- Folder ingestion mode becomes another option alongside the others — one that recursively invokes the content-type-specific pipelines
- Custom chains pulled from the Wire appear as creation options without UI changes
- The same YAML-to-UI renderer generates the "configure this new pyramid" form

The YAMLs don't just configure *how* a pipeline runs — they define *what pipelines exist* and drive the creation UI.

---

## 5. DADBEAR Stabilization

### Root Cause

The tick loop fires a new directory scan while the previous ingest dispatch is still processing. This causes:
- Duplicate/quadruplicate WAL entries in `pyramid_pending_mutations`
- Stacked stale checks that collapse at drain time but confuse the UI
- Evidence loops running aggressively, creating evidence nodes that balloon L0 count (200 files -> 528 L0s)

### Fixes

1. **Per-config in-flight lock** — skip scan for a config if its previous dispatch is still in-flight. `dispatch_pending_ingests()` sets a lock that the next tick respects.
2. **Change-manifest supersession** (see below) — stale updates produce deltas, not full regenerations. Fixes the viz orphaning bug.
3. **Evidence triage** (see above) — gates expensive evidence creation with cheap intelligence
4. **Freeze/unfreeze restart handling** — after unfreezing, DADBEAR should re-apply without requiring app restart

### Change-Manifest Supersession (Stable Node IDs)

**Problem:** When a stale check supersedes an upper-layer node (e.g., L3-000 → L3-S000), the system creates a new node with a new ID. All structural references — evidence links, web edges, parent-child lookups in the viz — still point to the old ID. The DAG visualization breaks: the apex renders alone with no children, even though children exist in the database. This has happened repeatedly.

**Root cause in the viz:** `get_tree()` in `query.rs` builds the parent-child graph from `pyramid_evidence` links. Those links reference the old node ID (L3-000). The new node (L3-S000) has no evidence links pointing to it, so `children_by_parent.get("L3-S000")` returns empty. The tree renders a lone apex.

**Solution: Change manifests, not full regeneration.**

Instead of asking the LLM to regenerate an entire node from scratch and creating a new ID, stale supersession asks: "given that these children changed in these specific ways, what needs to change in this node's synthesis?"

The LLM produces a **change manifest**:
- Which children were swapped (L2-002 → L2-S000, L2-003 → L2-S001)
- What specifically changed in the synthesis content (a targeted delta, not a full rewrite)
- Whether the node's identity fundamentally changed or just its content

The system then:
1. **Updates the node in place** — same ID (L3-000), bumped `build_version`, updated `children` array, applied content delta
2. **`pyramid_node_versions`** (append-only) captures the prior version for full history
3. **All reference tables remain valid** — evidence links, web edges, viz lookups all still point to L3-000
4. The viz DAG renders correctly because the ID never changed

**New ID only when identity changes.** If the manifest says the node's identity has fundamentally changed (e.g., entire cluster reorganized, node now covers different territory), only then create a new ID and update references explicitly. The default path is in-place update; new ID is the exception.

**Benefits beyond bug fixing:**
- Cheaper LLM calls — asking "what changed?" is a smaller, more focused prompt than "regenerate everything"
- Better quality — LLMs are better at targeted edits than full regeneration from scratch
- Aligns with the notes paradigm — a stale check is the system providing "notes" on an existing node ("your children changed, here's how"), producing a new version, not a new entity

### DADBEAR Oversight Page

A unified view of all DADBEAR activity across pyramids:
- Per-pyramid enable/disable with bulk controls
- Default norms for newly created pyramids
- State of LLM calls and staleness checks across all pyramids
- OpenRouter webhook cost reconciliation (estimated vs actual)
- The data already exists across `pyramid_stale_check_log`, `pyramid_pending_mutations`, `pyramid_llm_audit`, and `pyramid_cost_log` — this is frontend assembly on existing data

---

## Dependency Graph

```
Provider Registry (llm.rs abstraction)
    |
    +-> LLM Output Cache (content-addressable by input/prompt/model hash)
    |       |
    |       +-> Evidence Triage Policy (user-defined YAML, generative config)
    |
    +-> YAML-to-UI Renderer (generic: chain config, evidence policy, build strategy, etc.)
    |       |
    |       +-> Generative Config (intent -> YAML -> UI -> contribution)
    |
    +-> OpenRouter Broadcast Webhook (cost actuals per step)

Vine-of-Vines (lift composition constraint)
    |
    +-> Topical Vine Recipe (new YAML + prompts)
            |
            +-> Recursive Folder Ingestion (walk, detect, compose)

DADBEAR Fix (per-config in-flight lock) -- independent, do first

Build Viz (emit events for all persisted LLM outputs) -- depends on universal persistence
```

---

## Design Principles

- **YAML is the source of truth.** Chain definitions, evidence policies, build strategies, and all behavioral configurations are YAML. The YAML drives both execution and UI. No hand-coded settings pages per feature.
- **Intent-to-YAML-to-UI-to-Contribution.** Users express intent in natural language. The system generates structured YAML. The UI renders it for review. Accepted YAML becomes a versioned contribution, shareable on the Wire. Build the renderer and generation path once; every new configurable behavior gets this for free.
- **LLM agnostic.** Users connect whatever compute they want — OpenRouter, Ollama, any OpenAI-compatible API. The provider registry is the abstraction.
- **Every LLM output is intelligence.** Store everything, cache by content hash, skip calls when inputs haven't changed. Users can reroll (supersede) any node when non-determinism produces outlier bad answers.
- **Self-organizing over configured.** Folder ingestion detects content types and creates appropriate structures. Users point and go.
- **No absolute standards.** We provide sensible defaults, but users define their own policies. They share them on the Wire, the best versions propagate, and everyone benefits.
- **Contributions all the way down.** Tier routing, evidence policies, DADBEAR norms, build strategies — all contribution-based. Agents can propose changes. Configurations are tradeable intelligence.
- **Assumptions through the front door, actuals via webhook.** Cost estimates from our side, verified by OpenRouter Broadcast. Discrepancies are visible.
- **Stable node IDs, versioned content.** Stale supersession produces change manifests, not full regenerations. Nodes update in place with version bumps; `pyramid_node_versions` preserves history. New IDs only when node identity fundamentally changes. This keeps all structural references (evidence, web edges, viz) valid and makes stale updates cheaper and higher quality.
- **Simple and maximal are allies.** One toggle for local mode, not a wizard. One topical vine recipe, not per-content-type variants. The right abstractions reduce configuration surface while increasing capability.
