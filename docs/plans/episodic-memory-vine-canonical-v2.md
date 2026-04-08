# Episodic Memory — Canonical Design (v2)

> **Purpose:** Canonical design document for the episodic memory product — the Vine Pyramid. Covers the full conceptual and user-experience surface at the architectural level, not implementation details (chain YAML, database schemas, API signatures, code structure — those live in implementation plans).
>
> **Audience:** Anyone who needs to understand what the product is, what it does, how users experience it, and why every piece is shaped the way it is — before touching the implementation.

---

## Preamble

Episodic memory is a **cognitive substrate for AI agents**. Not a database. Not a knowledge base. Not a search index.

AI agents, by default, have no continuity across sessions and no graceful working memory within sessions. Every new session starts from blank state; every session's cognitive scaffolding collapses when the context window fills. The episodic memory product gives agents a persistent substrate on which they can operate with genuine continuity, modeled on (but not mimicking) the properties of human memory that support moment-to-moment decision-making.

The substrate takes the form of a **Vine Pyramid** — a recursive memory pyramid whose base layer is the apex nodes of other pyramids (conversation transcripts processed into memory-schema form). The Vine grows continuously as new conversations happen, maintained in the background by the existing pyramid-update orchestrator. The result is a structure the agent treats as *memory* during operation: it recognizes things the conversation mentions, retrieves details when it needs to think about something, manages its working set between turns, and carries binding commitments and earned state across session boundaries.

This document describes how the Vine works as a cognitive primitive, how it's constructed, how users and agents interact with it, and why the architecture is shaped the way it is.

---

## Part I — Core principles

These principles shape every subsequent design decision. They're stated here first so the rest of the document can be read as the consequences of accepting them.

### 1.1 Memory as a cognitive primitive

Persistent memory for AI agents is a **cognitive substrate problem**, not a storage problem and not an information retrieval problem. The agent needs a medium that supports the *shape* of working memory, recent memory, and long-term memory as it operates — a medium that makes "thinking about something" a tractable operation rather than requiring exhaustive speculative querying of a passive data store.

Every design decision in this document serves one goal: give the agent a cognitive substrate that feels like memory during operation, not like querying a database.

### 1.2 Vocabulary is the trigger surface for cognition

The load-bearing insight of the architecture:

> **An agent's ability to *recall* memory at all depends on what vocabulary is present in its active context. Recognition has to happen in the context window; retrieval can happen through tool calls afterward.**

When a live agent says *"let me think about that,"* what's actually happening is: something in the conversation matched a vocabulary item the agent has in context, the agent recognized it as a thing it has memories about, and it's now triggering a retrieval operation to pull in the detail. Thinking is mechanical — it's a recognition firing a retrieval firing an incorporation into the next turn.

But recognition can only happen if the vocabulary is already in context. If the relevant identity isn't present in the agent's working slice, the agent doesn't know it has memories about that thing, and the memories might as well not exist — functionally identical to never having captured them.

Vocabulary here means the full canonical identity graph: topics, entities, decisions (with their stances), glossary terms, practices, and the relationships between them. This graph is the *index of thinkable thoughts* for the current session. Whatever's in the index, the agent can recall. Whatever's absent from the index, the agent can't know to request.

Detail is different. Detail is always in the pyramid, always queryable, always one CLI call away. Detail doesn't need to be pre-loaded because retrieval is fast. But detail is only reachable when the vocabulary in context tells the agent there's something to retrieve.

This separation of concerns — vocabulary as eager-loaded trigger surface, detail as lazy-loaded retrieval product — shapes the schema, the dehydration model, the primer, and the runtime integration. It's what makes the system work as cognition instead of as a data store.

### 1.3 Detail is deferred, not diminished

A corollary of 1.2: compressing or omitting detail from the agent's active context is **not lossy** from the agent's perspective, because detail is retrievable on demand. The full content always lives in the pyramid. A dehydrated view of a node just means "this content isn't pre-loaded; request it when needed."

Dehydration at the vocabulary level, however, is catastrophic — it removes trigger conditions, making the memory invisible to the agent even though the content still exists in storage. The architecture treats these asymmetrically.

This framing matters because it turns dehydration from a reluctant compromise ("we have to drop things we wish we could keep") into a deliberate design choice ("we're scheduling what to eager-load vs. what to lazy-load, and the trigger surface always wins eager loading").

### 1.4 The Vine is just a pyramid

Architecturally, the Vine is a pyramid that uses the same chain executor, the same recursive synthesis operation, the same delta mechanism, the same `ties_to` webbing, the same staleness propagation, and the same query APIs as every other pyramid in the system. Nothing new is invented at the pyramid-creation layer.

The only distinctive property: **the Vine's base layer is the apex nodes of other pyramids** — specifically, the bedrock conversation pyramids ingested from raw transcripts. Everything else is ordinary pyramid behavior running at the composition scale.

This is a deliberate simplification. The product is built from composition and recursion applied to primitives that already exist. The complexity lives in how the pieces compose, not in any new piece.

### 1.5 DADBEAR orchestrates the lifecycle

DADBEAR is the existing auto-update system for pyramids. It handles debouncing, staleness detection, incremental re-processing, and propagation of changes through pyramid dependency chains. For episodic memory, DADBEAR gains one new capability — **the ability to create pyramids** (it previously handled maintenance only).

With this extension, the entire ingestion cycle becomes a DADBEAR workflow: watch a source folder, debounce active files, trigger bedrock creation when files stabilize, trigger Vine deltas when bedrocks finish, handle staleness ripple for modified sources, propagate updates through the Vine's dependency chain. The user does not manage a queue, does not press pause, does not manually trigger anything. The Vine becomes current as a background property of the user's ongoing work.

### 1.6 Usefulness over cost

LLM intelligence is sub-penny to single-dollar per operation, cheap and getting cheaper. The scarce resources are the user's attention and the agent's effectiveness, not compute. Every design decision is made against this rubric:

- Does this bespoke intelligence produce genuinely useful understanding structure?
- If yes, it's worth the cost.
- If no, don't build it even if it would be cheap.

This rubric rules out architectural choices that are optimization theater — pre-computing things to avoid LLM calls that don't save money or add value. It rules in choices that leverage more bespoke intelligence wherever intelligence is what produces the useful shape.

---

## Part II — The architecture

### 2.1 Bedrock pyramids

A **bedrock pyramid** is the memory-schema pyramid built from one conversation transcript (typically a `.jsonl` file from a Claude Code session or equivalent recording). Its structure:

- **L0** — base-layer nodes, one per chunk of the conversation, produced by a forward/reverse/combine extraction chain that fuses temporally-forward and temporally-backward views of each chunk into a single episodic-schema node
- **L1** — segment nodes, grounded in L0 via an evidence loop that verifies each claim with KEEP/MISSING/DISCONNECT verdicts
- **L2** — phase nodes, produced by pair-adjacent composition of L1 nodes via recursive synthesis
- **Bedrock apex** — the session-level node, produced by pair-adjacent composition of L2 nodes via recursive synthesis

Bedrock pyramids are built by the existing chain executor using the episodic chain configuration. They are addressable as standalone pyramids (they support all six reading modes independently), and they serve as the L0 layer of the Vine.

Bedrock L0 and L1 nodes are **immutable** — ground truth. If a bedrock needs correction because the source transcript changed, the bedrock is re-ingested (fully or incrementally) and a new version supersedes the old one; in-place edits are not a supported operation.

### 2.2 The Vine pyramid

The **Vine** is a pyramid whose L0 layer consists of pointers to bedrock apex nodes, ordered temporally. Its structure:

- **Vine L0** — one slot per ingested bedrock, pointing at the bedrock apex
- **Vine L1** — pair-adjacent composition of consecutive L0 pairs via recursive synthesis
- **Vine L2** — pair-adjacent composition of consecutive L1 pairs
- **...further upward as the corpus grows...**
- **Vine apex** — single node representing the full arc at maximum abstraction

A single operator or project typically has one active Vine. The Vine is where the operator's persistent memory for that project lives, and the substrate from which the agent's persistent memory for that project is drawn.

### 2.3 Leftward growth convention

**New bedrocks append on the LEFT edge of the Vine.** The rightmost L0 is the first conversation ever ingested; the leftmost L0 is the most recent. Growth proceeds leftward over time.

This convention is load-bearing, not cosmetic. It makes the **leftmost slope** (the path from the Vine apex down through one node per layer, always picking the leftmost child) the **recent side** of the pyramid — and the recent side is the side the agent needs as the working-memory primer (Part III).

### 2.4 Schema invariance at every layer

Every node at every layer of a bedrock pyramid, and every node at every layer of the Vine, uses the **same schema**. Only the *scale* of what the fields describe changes.

**Required fields (present at every layer):**
- `headline` — recognizable name for whatever-scale-of-material-this-covers
- `time_range` — temporal extent
- `weight` — size proportional to parent

**Optional fields (appendable across passes, tiered within each field where it benefits):**
- `narrative` — dense prose at this layer's scale, written at multiple resolution tiers simultaneously (Section 2.5)
- `topics[]` — topic identifiers with importance scores and liveness markers
- `entities[]` — entity identifiers with roles, importance, and liveness markers
- `decisions[]` — decisions with `stance` (committed | ruled_out | open | done | deferred | superseded | conditional | other), importance, and `ties_to` cross-references
- `key_quotes[]` — exact quotes with `speaker_role` (human | agent) and importance
- `transitions` — how this node connects to prior and next nodes at this scale
- `annotations[]` — append channel for cross-pass signals (webbing, vine composition, audit, manual)

Schema invariance enables:
- One recursive synthesis prompt that runs at every layer without modification
- Cache-stable navigation that looks the same shape at every depth
- Runtime dehydration as a simple projection operation (Section 2.5) with no shape shift
- Indefinite upward composition (Vines of Vines use the same operations)
- Multi-resolution loading at runtime — the agent picks zoom level, not node type
- The same navigation skeleton serving as build-time primer and runtime working memory

### 2.5 Multi-resolution nodes

Each node is stored as a **multi-resolution artifact** — the recursive synthesis prompt produces several pre-computed distillation levels in a single output pass, and those levels live as sub-fields on the node. At read time, dehydration is a pure projection operation: pick the richest combination of sub-fields that fits the available budget. No runtime synthesis is required to change a node's resolution.

The analogy is mipmaps in graphics: a texture is stored at multiple resolutions simultaneously so the renderer can pick the right level at read time rather than computing scaled versions on the fly. Memory nodes apply the same pattern to narrative content and vocabulary.

**Narrative tiering.** The `narrative` field is structured as multiple complete, coherent versions at decreasing resolution. A reasonable default is four tiers:

- `narrative.full` — the primary dense prose at the abstracted layer, length content-determined
- `narrative.medium` — roughly half the length, preserving canonical vocabulary and every load-bearing decision
- `narrative.short` — a single paragraph (roughly 100–200 words) preserving canonical vocabulary and live decisions
- `narrative.line` — one sentence identifying what the node covers

Each tier is an independently-written coherent artifact at its own resolution. No tier is a truncation or prefix of another tier — they're separate writings with different scopes. The synthesis prompt produces all tiers in one LLM call.

Below `narrative.line` is the `headline` field — the irreducible floor that identifies the node even when narrative content is entirely absent.

**Vocabulary tiering.** Topics, entities, and decisions each carry a `liveness` marker in addition to their importance score:

- `live` — currently relevant to the state of the work (active topics, entities appearing in recent conversations, decisions with stance committed / open / conditional)
- `mooted` — was once live but has been superseded, retired, or resolved; preserved because cross-references from other nodes may still point at it
- `historical` — rarely relevant, archival-quality; drop first under pressure

Under dehydration, mooted and historical vocabulary drop before live vocabulary. Live vocabulary is part of the permanent floor — it never drops regardless of pressure.

**The number of tiers is chain-configurable.** Different chains can request different tier structures. The synthesis prompt is told at invocation time what tiers to produce. Defaults ship with sensible values; experimenters can tune in YAML.

**Storage cost grows modestly; runtime cost drops dramatically.** A multi-resolution node is larger than a single-version node (several narrative versions plus tiered vocabulary classifications), but the amortization works heavily in favor of pre-computation. Dehydration and rehydration at runtime become free — pure field selection, no LLM calls, deterministic and fast. For a cognitive substrate that's queried continuously during agent operation, this tradeoff is decisively correct.

### 2.6 The recursive synthesis operation

A single prompt — the recursive synthesis prompt — runs at every layer above the base. It operates in three input modes:

1. **Peer fusion.** N peer nodes at some layer become one parent node at one layer of abstraction above them.
2. **Delta update.** An existing parent node plus one new or changed child becomes an updated parent node at the same abstraction level.
3. **Initialization.** A single child node becomes a parent node wrapping it, at one layer above, when the child is the first of its kind.

The prompt is **level-agnostic**. It infers the abstraction level from input content and shifts exactly one step outward. It never references absolute depth and cannot assert "another layer exists above" because at the current apex of any build that claim would be false. Instead, upward composition is phrased as potential, not guaranteed, so the prompt is accurate at every layer including the current top.

The prompt runs at four distinct **timing modes**, with no changes between them:

- **Full-build pipeline** — offline, sweeps every layer of a fresh pyramid during initial bedrock construction
- **Ingestion delta** — per-bedrock (or per-batch), folds new L0 content into the Vine and propagates upward
- **Runtime densify** — online async, produces one missing mid-level node on demand when the agent discovers a gap
- **Collapse** — rewrites accumulated delta chains into fresh canonical node versions during idle time

The prompt's output includes all the multi-resolution tiers described in Section 2.5, produced in one LLM call regardless of which timing mode is invoking it.

### 2.7 Delta chains and collapse

The Vine updates incrementally through the delta-chain pattern:

- **Bedrock L0 and L1** are immutable. Ground truth, appended to but never mutated.
- **Vine L0** is also immutable — each slot holds a stable pointer to a bedrock apex, which is itself immutable.
- **Vine L1 and above** are mutable via delta chains. Each update to a node is stored as a small delta patch representing the effect of one new or changed child appearing beneath it.
- **Periodic collapse** rewrites a node's delta chain into a fresh canonical version synthesized from the current child set. Collapses are triggered by chain length, time since last collapse, or explicit request.

This gives the Vine **O(log N) per ingestion** bounded by pyramid depth, not corpus breadth. Total cost for N conversations is O(N log N), effectively linear for any realistic N. Vine apex size stays bounded by the dehydration cascade regardless of corpus growth — new content that matters bubbles up, content that no longer matters gets shed to mooted or historical liveness or drops out of the apex entirely while remaining in the lower layers.

Collapse passes run during idle time or on explicit request. They never block ingestion or runtime operations.

---

## Part III — The leftmost slope: scale-invariant working memory

### 3.1 What the slope is

The **leftmost slope** of the Vine is a single diagonal path from the Vine apex down through one node per layer, always picking the leftmost child at each layer. For a Vine with k layers, the slope contains k nodes.

```
Vine apex                ← covers the full arc
  |
leftmost L(k-1)          ← covers approximately the most recent half
  |
leftmost L(k-2)          ← covers approximately the most recent quarter
  |
leftmost L(k-3)          ← covers approximately the most recent eighth
  |
  ...
  |
leftmost L1              ← covers approximately the last two conversations
  |
leftmost L0              ← the most recent conversation in full detail
```

Because growth is leftward (Section 2.3), each step down the slope moves to a progressively more recent, progressively smaller time window at progressively higher resolution. The slope is the **recency-weighted zoom gradient** into the current state of the work.

### 3.2 Scale invariance

The leftmost slope provides **scale-invariant working memory**: regardless of whether the Vine holds 10 conversations or 100,000, the leftmost L0 and L1 always represent "the most recent conversation" and "the last couple of conversations" at full detail. The slope keeps the same shape as the corpus grows — new layers appear on top, widening the overall pyramid, but the leftmost slope at each existing depth stays anchored to the recent edge.

This matters because the agent's working memory needs to be *consistent in its treatment of the present* regardless of how much history has accumulated. A ten-thousand-conversation Vine should not degrade the agent's memory of today's session compared to a ten-conversation Vine. The leftward-growth convention combined with the leftmost-slope primer mechanism guarantees this property by construction.

### 3.3 The zoom gradient as a cognitive affordance

Each node in the slope is a different zoom level on a different time window. The combination gives the agent simultaneous context at multiple scales:

- **The apex** — the whole arc, low-detail-per-unit-time, full canonical identity catalog
- **Mid-slope nodes** — progressively more recent time windows at progressively higher detail
- **Bottom of slope** — today's work in full resolution

This is what the agent needs to operate: a meta-understanding of where the work is headed, mid-resolution context on recent phases, fine-grained detail on the immediate present. Each node contributes a different cognitive scale, and the union is a coherent multi-resolution view.

Under token pressure, dehydration follows the slope structure naturally: drop apex-facing slope nodes first (they're lossy but the loss is distant-scale meta-narrative, which hurts less), preserve recent-end slope nodes (they're the short-term memory the agent is actively using). Because each node is itself multi-resolution (Section 2.5), the dehydration is two-dimensional: which nodes to include, and which tier of each included node to render.

### 3.4 One artifact, two consumption modes

The leftmost slope serves both as the **build-time primer** (the reference block that rides in every extraction prompt during a new bedrock build) and as the **runtime navigation skeleton** (the stable cached scaffold that lives in the agent's Brain Map between turns).

It's the same slope. Same shape, same data, same vocabulary, same set of identities. The two timing modes consume it for different purposes: at build time it shapes canonical identity propagation into the new bedrock's extraction; at runtime it shapes the agent's trigger surface for recognition during active cognition.

Because the consumption modes share the artifact, the design only needs to make the slope right once. Getting it right for one mode gets it right for both.

---

## Part IV — The ingestion cycle (orchestrated by DADBEAR)

### 4.1 Temporal ordering

Ingestion is strictly temporal: conversations are processed in order of their earliest timestamp. This is a correctness requirement because canonical identity convergence depends on each ingestion seeing a primer derived from *prior* work, and decision stance tracking depends on decisions being observed in the order they were committed.

Two operating modes:

- **Bootstrap** — an existing backlog (a folder of accumulated transcripts) is processed earliest-first until the archive is caught up
- **Steady state** — new conversations are processed individually as they arrive, each triggered by the source file becoming stable

Both modes use the same underlying mechanisms. The bootstrap is a burst of steady-state events processed in rapid temporal order.

### 4.2 Debouncing and incremental processing

DADBEAR watches the operator's conversation folder and reacts to file events. For each source file:

1. Detection — DADBEAR notices the file has appeared or been modified
2. Debouncing — the file is marked active; DADBEAR waits until the file stops being actively written to for the debounce window (configurable, e.g., 5 minutes of inactivity)
3. Triggered build — once the file is stable, DADBEAR triggers a bedrock pyramid build via the existing chain executor
4. Incremental handling — if the file becomes active again mid-processing (the conversation resumes), the portion already past the debounce line continues processing; newly-added content queues up for the next debounce cycle

The practical result: the bedrock pyramid builds *behind* the live conversation. Long sessions don't block processing of their earlier chunks. The Vine stays somewhat current even during multi-hour sessions. Re-opening an old conversation and adding content is handled transparently — DADBEAR detects the modification, debounces, re-builds the affected bedrock (fully or via incremental delta), and ripples the update upward through the Vine.

The operator never sees any of this. The Vine becomes current as a background property of ongoing work.

### 4.3 The primer: leftmost slope loading

When DADBEAR triggers a new bedrock build, the Vine produces the **primer** — the leftmost slope loaded at a default hydration level (full leftmost slope with token-aware auto-dehydration applied if the total exceeds budget). The primer rides in every extraction prompt during the bedrock build as a stable reference block.

Under default settings, the primer carries:
- Full canonical live vocabulary in every slope node (the identity trigger surface)
- Minimal-tier narrative in most slope nodes
- Richer narrative tiers in the recent-end nodes if budget allows
- Mooted vocabulary included where budget permits (to anchor cross-references to historical identities)

Because the slope is cache-stable (leftmost nodes rarely mutate; only the leftmost L0 at the very bottom changes each ingestion), the primer's prefix cache hits are high. The model provider's cache makes each chunk's extraction effectively pay the primer cost only once per build.

### 4.4 Delta composition

When a bedrock build finishes, DADBEAR triggers a Vine delta. The delta folds the new bedrock apex (or a batch of new bedrock apexes, depending on configuration) into the current Vine state.

The primary tunable is **`n` = batch size**:
- `n = 1` (default) — one new bedrock per delta. Maximum freshness; the Vine is current within one conversation's latency of new work.
- `n > 1` — wait for `n` bedrocks to accumulate, then run one delta folding all of them in. Useful for bootstrap ingestion of large backlogs where intermediate Vine state between batches isn't important.

A secondary tunable is **slope context depth** (optional, configured in YAML):
- Default (unset) — the delta input includes the full leftmost slope with token-aware auto-dehydration. The slope is trimmed apex-first if it exceeds budget.
- Configured integer — cap the slope depth at a specific number of nodes. For experimentation and tuning only; not a primary knob.

Both tunables live in chain YAML. Defaults ship sensible; experimenters tune without code changes.

### 4.5 Delta propagation

Once the delta input is assembled, it runs through the recursive synthesis prompt in delta mode. The new bedrock apex(es) land at the leftmost position(s) in Vine L0. The delta then propagates upward through the affected slope layers, updating one node per layer:

- **Vine L1** — the new L0 pairs with its rightward neighbor (if one exists) to form or update the leftmost L1, or sits as an orphan at the leftmost edge until the next bedrock arrives to pair with it
- **Vine L2** — the updated L1 triggers a delta on its parent L2
- **...up to the Vine apex...**

Each per-layer delta is a bounded operation: one existing parent + one small update → one updated parent. Cost per layer is roughly constant. Total cost per ingestion is roughly O(depth).

Orphans at the leftmost edge are normal. A Vine with an odd number of L0 nodes has an orphan L0 waiting for a pair. The orphan participates in the next delta cycle when the next bedrock arrives. Orphans don't block anything — they're just visible at the growth edge until paired.

### 4.6 Staleness and re-ingestion

The existing staleness machinery handles "what happens when an old source changes" transparently:

- The operator modifies an old transcript (or a transcript is updated by its originating tool)
- DADBEAR detects the modification, applies debouncing
- Marks the affected bedrock stale
- Re-builds (fully or incrementally, depending on scope of change)
- Triggers a Vine delta with the updated bedrock apex
- Delta ripples up through affected slope layers
- The Vine becomes current with the updated content

There is no separate "correction flow" or manual re-ingest UI. The staleness pipeline is the correction pipeline. Whether the change comes from an edit, a re-recording, or an explicit operator request, the handling is the same.

### 4.7 The bootstrap case

The initial ingestion of an accumulated archive (e.g., months of Claude Code transcripts piled up in a folder) runs as a rapid burst of the steady-state cycle. DADBEAR scans the folder, sorts files by earliest timestamp, and processes them in order. Each file becomes a bedrock; each bedrock triggers a Vine delta.

During bootstrap, the operator may choose to set `n > 1` to batch ingestions, sacrificing intermediate Vine freshness for faster total processing. After bootstrap, `n = 1` for steady-state responsiveness.

Bootstrap is interruptable and resumable transparently. DADBEAR's checkpointing is at the per-bedrock level — a completed bedrock's delta is committed atomically before the next bedrock starts. A crash or pause between bedrocks loses no work. A crash mid-bedrock loses only the current in-progress build, which resumes on the next DADBEAR cycle.

From the operator's perspective: drop a folder, walk away, come back to a populated Vine.

---

## Part V — Canonical identity convergence

### 5.1 The problem

Without coordination, each bedrock extraction produces its own identity namespace. The same concept gets different names in different sessions: "Pillar 37" becomes `Pillar 37` in one bedrock, `pillar 37` in another, `no-length-prescriptions (Pillar 37)` in a third. A person gets called `Dennis` in one, `Partner` in another, `the AI partner` in a third. Cross-session `ties_to` edges can't form because identities don't match. The agent's trigger surface becomes fragmented and unreliable.

### 5.2 The running canonical catalog

The Vine apex, at the top of the leftmost slope, carries the **running canonical identity catalog**. High-importance topics, entities, decisions, glossary terms, and practices from across the full corpus bubble up through the dehydration cascade and persist at the apex. The apex's vocabulary — its `topics[]`, `entities[]`, `decisions[]`, and `annotations[]` arrays — IS the canonical catalog as it currently stands.

When a new bedrock build loads the primer, the primer includes the Vine apex's vocabulary. The extraction prompts see the full canonical catalog as ambient reference material. New content that matches existing identities uses the canonical forms; new content that introduces genuinely novel identities creates new entries that can be canonized by future passes.

### 5.3 Advisory, not constraining

Critical rule for every extraction prompt that sees the primer:

> The KNOWN IDENTITIES reference block is **advisory**, not a controlled vocabulary. When content in this chunk clearly refers to an identity already in the reference, use the canonical form. When content introduces something genuinely new, create a new identity — do not force-fit novel content into existing categories just to match. Forced matches produce hallucinated connections that are worse than missed connections.

Without this rule, the extractor would hallucinate matches and collapse novel content into existing categories. With it, the primer sharpens signal without blurring novelty.

Identity convergence is **asymptotic**: early bedrocks introduce many variants, later bedrocks increasingly reinforce canonical forms, the catalog stabilizes over the arc of the corpus. By the 20th or 50th bedrock, canonical forms dominate and new variants become rare.

### 5.4 Identity evolution and historical vocabulary

Identities can evolve over time. A concept that's coined informally in early sessions ("the no-length-prescription rule") may later be formalized with a different name ("Pillar 37"). The architecture handles this through the vocabulary liveness tiering:

- The new canonical name is tagged `live`
- The old name becomes `mooted` — it's no longer the primary way to refer to the concept, but it's preserved in the catalog because cross-references from older nodes may still point at it
- The relationship between the two is captured via synonym annotations in `annotations[]` or via supersession in `decisions[]` (if the evolution was driven by a decision)

Mooted vocabulary is included in the primer when budget allows, so new extractions can still recognize historical references. Under tighter budgets, mooted vocabulary drops first, but live canonical forms remain present.

This preservation enables the agent to understand terminology evolution without re-discovery: when it encounters a historical name in an old bedrock, it can still recognize what concept is being referenced and link it to the current canonical form.

---

## Part VI — Query flow

The Vine and its bedrocks participate in three query directions. There is **no pre-computed cross-vine or cross-bedrock webbing** — cross-navigation is handled on demand through the question-pyramid mechanism.

### 6.1 Vine → bedrock: the primer direction

When DADBEAR triggers a new bedrock build, the Vine produces the primer (leftmost slope with full canonical live vocabulary at the top). The primer rides in every extraction prompt during the bedrock build as a stable cached reference block. Canonical identities propagate forward into the new session's memory.

Build-time. Automatic. User-invisible.

### 6.2 Bedrock → Vine: the delta direction

When a bedrock finishes building, DADBEAR triggers the Vine delta. The new bedrock apex lands at the leftmost position in Vine L0. Delta synthesis propagates upward through affected slope layers. The Vine apex is updated with the incremental change, including any new canonical identities the bedrock introduced and any updates to existing identities' liveness.

Build-time. Automatic. User-invisible.

### 6.3 Question pyramid → Vine → bedrock: the escalation direction

When the agent (or the operator) needs to know something that the Vine doesn't carry at apex resolution, the mechanism is a **question pyramid asked of the Vine**.

The flow:

1. A question is posed against the Vine slug (via CLI, HTTP route, UI, or agent manifest operation)
2. A question pyramid is built that decomposes the question into sub-questions
3. Sub-questions hit Vine-level evidence via the existing evidence-loop primitive against Vine nodes
4. When an answer needs more detail than the Vine carries at apex resolution, the evidence trail leads via existing `ties_to` edges into specific bedrock pyramids
5. A child question build spawns against the relevant bedrock(s), answering the sub-questions on demand
6. If a bedrock's answer references a decision or entity from yet another session (another bedrock), the escalation recurses into that bedrock as well, bounded by a maximum recursion depth and protected by visited-set cycle prevention
7. Results flow back up the chain into the Vine's answer

This is the mechanism for cross-navigation, thread traversal, historical lookup, and anything else the agent needs that the Vine apex doesn't carry in its default dehydrated state. The mechanism pays only when a query actually happens — no standing cost for maintaining a cross-navigation index.

### 6.4 Why no cross-vine webbing

Pre-computing cross-bedrock or cross-Vine webbing would add standing maintenance cost, scale poorly with corpus size, and produce static answers that can't adapt to the specific question being asked. The question-pyramid mechanism is cheaper (pays only on query), more flexible (the question shapes the traversal), and produces higher-quality answers (bespoke intelligence applied to the specific question rather than generic pre-computed links).

Agents that judge a specific cross-bedrock connection worth manually recording can do so as an annotation, but that's agent-initiated and case-by-case. Pro-active webbing is not the default.

---

## Part VII — Reading modes

The same stored substrate supports six distinct rendering modes. All six ship at V1 because the storage supports them natively — they require only UI and query plumbing, not additional extraction.

### 7.1 Memoir

Read the Vine apex top-to-bottom. Dense prose at the whole-arc scale. The primary cold-start loading path for a new agent session: load the apex, read it as a memoir, recover the meta-understanding of the current state of work in a single pass.

### 7.2 Walk

Scroll through Vine L1, L2, or higher nodes in chronological order. The natural default direction is leftmost-first (newest-first) because recent work is typically more relevant to current activity. Users who want the full arc from the beginning can walk rightward instead. Both directions operate on the same data.

### 7.3 Thread

Pick a canonical topic, entity, or decision identifier, and follow its web edges across non-adjacent nodes. "Show me every moment that touched authentication." "Show me the full history of the chain-binding decision." Thread traversal crosses bedrock boundaries via the question-pyramid escalation mechanism (Section 6.3) — the agent or operator asks "show me the thread of X" and the Vine's answer recursively descends into specific bedrocks via `ties_to` and spawned sub-questions.

### 7.4 Decisions ledger

Render the Vine's `decisions[]` arrays, aggregated across the corpus, filterable by stance. "Everything currently committed." "Everything open, sorted by how long." "Everything ruled out, with reasoning." The agent consults the ledger before proposing new work to avoid contradicting prior rulings or re-opening settled questions.

### 7.5 Speaker

Filter to one speaker role's contributions across the whole Vine. Human turns (rare, high-weight, often binding direction) or agent turns (abundant, lower-signal-per-token but including commitments and discoveries). In an AI-dominated corpus where the agent speaks ~95% of tokens, Speaker mode on the "human" filter is extremely high-signal — a small number of turns carrying the direction that shaped the whole arc.

### 7.6 Search

Full-text search over the raw chunks index (FTS5 over the preserved transcripts of all ingested bedrocks), with hits that drill up to the owning L0 node, L1 segment, L2 phase, bedrock apex, and Vine-layer ancestor chain. The escape hatch for when paraphrase extraction has lost a specific phrase that the operator remembers verbatim.

---

## Part VIII — The user experience: the Vines page

The product introduces a dedicated **Vines page** in the app, separate from the existing Pyramid dashboard. Vines are a distinct enough product to warrant their own home.

### 8.1 Layout

The Vines page has four primary regions:

- **Vine list (left rail)** — one entry per active Vine, typically one per project or use case. Clicking selects a Vine.
- **Vine visualization (main area)** — the currently-selected Vine rendered as a recursive triangle, growing leftward as new bedrocks arrive. Layers are color-coded by depth. The leftmost slope is highlighted. The current Vine apex headline displays prominently at the top. Clicking any node opens its detail view.
- **Canonical identities panel (alongside)** — a live display of the Vine apex's canonical catalog: top topics by importance, top entities grouped by role, active decisions by stance, glossary terms, practices. This is the operator's window into what the agent "knows" about the canonical shape of the work.
- **DADBEAR status (bottom)** — watched folders, recent debounce events, recent bedrock builds in progress, recent Vine deltas, any staleness flags or errors.

Above the main area is a **reading mode selector** (Memoir / Walk / Thread / Decisions Ledger / Speaker / Search) and a **question prompt bar** for asking questions of the Vine.

### 8.2 Creating a new Vine

The operator clicks "New Vine" on the Vines page:

1. **Name the Vine.** A human-readable name for the use case.
2. **Point at a source folder.** The directory where conversation transcripts accumulate. DADBEAR will watch this folder.
3. **Configure.** Debounce timer (default reasonable), `n` batch size (default 1), slope depth (default unset = full slope with auto-dehydration), auto-ingest vs. confirm-before-ingest (default auto).
4. **Confirm.** DADBEAR scans the folder, discovers any backlog, sorts by timestamp, and begins processing.

The Vine visualization starts populating as DADBEAR works. The operator can watch progress, close the app and come back later, or ignore it entirely — DADBEAR continues in the background and picks up where it left off.

### 8.3 Watching the Vine grow

During bootstrap (initial climb of a backlog):
- The Vine visualization adds new L0 slots on the left as each bedrock finishes
- Delta pulses propagate up through affected slope layers, visible as brief highlight animations
- The canonical identities panel grows and stabilizes as canonical forms firm up
- The apex headline updates as the understanding matures
- DADBEAR status shows bedrocks completed, bedrocks remaining, and the current bedrock with its chain phase

During steady state (ongoing work):
- New bedrocks arrive organically as the operator has new conversations with agents
- Each new bedrock triggers one delta cycle
- The operator notices the Vine update between work sessions without any action on their part

### 8.4 Exploring and querying

The operator (or the agent, via the same interface) can:

- **Switch reading modes** via the selector. Memoir for overview; Walk for chronological reading; Thread for topic tracing; Decisions Ledger for commitment review; Speaker for direction review; Search for verbatim lookup.
- **Drill into nodes.** Clicking any Vine node with `ties_to` down into a bedrock navigates into the bedrock's pyramid view. Clicking any L0 chunk in a bedrock shows the raw dialogue that produced it.
- **Ask questions.** Typing a question into the prompt bar triggers a question pyramid built against the Vine, with evidence escalation into bedrocks as needed. The answer renders in the main view with citations linking back to the specific moments it's grounded in.

### 8.5 Annotation and correction

Any node can be annotated by opening its detail view and appending to its `annotations[]` field. Annotations are cheap, non-destructive updates that persist across future deltas and are visible to future readers and builds.

Corrections to source transcripts are handled by the staleness pipeline automatically — the operator modifies the source, DADBEAR detects the change, re-builds the affected bedrock, deltas the update into the Vine. No separate correction UI is needed.

---

## Part IX — Runtime integration

The Vine is the cognitive substrate from which the agent draws working memory during active sessions. The runtime integration has several distinct operations.

### 9.1 Cold start

A new agent session begins. The agent has no biological continuity. It loads the current Vine's leftmost slope as its initial context. Because the slope is recency-weighted and multi-resolution:

- The **apex** contributes the whole-arc meta-narrative and the full canonical live vocabulary
- **Each step down the slope** contributes progressively finer resolution on progressively more recent time windows
- **The leftmost L0** contributes the most recent conversation in full detail

The agent comes online with instant multi-resolution orientation: perfect short-term memory, adequate medium-term context, coarse long-term overview. Total load: roughly a dozen nodes for a Vine of any realistic size, cache-stable across turns, drawn through a single CLI call.

From the agent's subjective standpoint, it wakes up knowing where the work stands.

### 9.2 The Brain Map and manifest operations

During active work, the agent's cognition is divided into three tiers:

- **Conversation Buffer** — live dialogue turns. Sacred. Only actual back-and-forth lives here; tool results, synthesized findings, and prior-session context never accumulate in the buffer.
- **Brain Map** — navigation skeleton (drawn from the Vine's leftmost slope) plus variable hydrated content (specific Vine or bedrock nodes pulled in for the current turn's work). Mutates between turns via manifest operations.
- **Pyramid cold storage** — the full Vine and all its bedrocks on disk. Query surface for everything the Brain Map doesn't currently hold.

Between turns, the agent emits a structured **context manifest** as part of its response — invisible to the human user, machine-readable, consumed by the runtime harness. The manifest specifies what to do with the Brain Map before the next turn. Available operations include:

- `hydrate <node> <tier>` — pull a specific node at a specific resolution tier into the Brain Map
- `dehydrate <node>` — drop a Brain Map node's richer content, retaining only lower tiers (freeing tokens without losing vocabulary)
- `compress <buffer_range>` — replace a stretch of dialogue turns with a synthesis node that moves to the Brain Map
- `densify <missing_node>` — request an async helper to produce a missing mid-level synthesis node on demand
- `colocate <seed>` — pull in nodes related to a seed node via `ties_to`
- `lookahead <nodes>` — speculatively pre-stage nodes the agent anticipates needing next turn
- `investigation <node>` — flag a node as possibly stale and request async verification

Each manifest pair (emitting turn + operations) is stored in a provenance trail for audit and metrics. The agent is steering its own cognition.

### 9.3 Dehydration as projection, not loss

Because each Vine and bedrock node is stored as a multi-resolution artifact (Section 2.5), dehydration at runtime is a **pure projection operation**. When the agent dehydrates a Brain Map node to free tokens, it's selecting a smaller tier of the same node — the lower tiers are already written and waiting. No LLM call, no quality loss, no synthesis latency.

When the agent later rehydrates, it selects a higher tier. Again, no synthesis — just field selection against the node's pre-computed tiers.

The separation of concerns from Section 1.2 holds at runtime: the vocabulary floor of every node is always present in the Brain Map whenever the node is there at all. Narrative detail comes and goes based on budget and current relevance. The trigger surface never degrades.

### 9.4 "Let me think about that" as a mechanical operation

The architecture makes *"let me think about that"* a first-class mechanical operation, not a figure of speech. The operation has three phases, all tractable:

1. **Recognition.** Something in the live conversation fires a vocabulary match against the Brain Map's in-context trigger surface. The agent recognizes that it has memories about this specific thing.
2. **Retrieval.** The agent's manifest names the specific node and tier it wants (`hydrate <vine_node_id> tier=narrative.full`). The runtime harness executes the operation against the pyramid between turns and returns the requested tier into the Brain Map.
3. **Incorporation.** The retrieved detail enters the Brain Map. The next turn references it fluidly, as though the detail had always been in working memory.

From the agent's subjective standpoint, this feels like thinking about something. It's the mechanical realization of what cognition looks like when memory is externalized into a queryable substrate rather than carried entirely in the active context window.

The operation only works if the vocabulary trigger surface carries the identity that needs to fire. That's why the Brain Map always includes the Vine's leftmost slope's vocabulary even under extreme token pressure — vocabulary is the precondition for the whole cognition loop.

### 9.5 Asynchronous writeback

Mid-session, the agent may discover things that should persist into the next session: a new commitment, a newly-ruled-out alternative, a clarifying definition, an audit finding. The agent emits a manifest operation to update the relevant Vine node, and an async helper (running between turns or in a separate worker) executes the update via the recursive synthesis prompt in delta mode.

DADBEAR's existing machinery propagates the update through affected Vine layers. By the next session, the change is reflected in the primer the next agent instance loads. In-session insight persistence becomes a natural, mechanical operation rather than a separate "save state" burden on the agent or the user.

### 9.6 The agent's subjective experience

Putting it together: at session boot, the agent loads the leftmost slope and feels oriented. During active work, manifest operations let it hydrate, dehydrate, colocate, and densify as the conversation's needs shift — the working set adapts to whatever's relevant right now. When it recognizes something it has memories about, retrieval is a tool call away. When it discovers something worth preserving, an async helper writes it back without blocking the live session. Session end is unremarkable — there's nothing to save that isn't already saved.

The agent's experience of having persistent memory is the experience of operating on the Vine. The Vine is the substrate, and it feels — from the inside — like memory, because it supports the shape of cognition natively.

---

## Part X — Scope at V1

V1 focuses on making a **single Vine** genuinely useful for a **single project or use case**, end-to-end. One level of recursion (bedrock → Vine), one Vine per project, one operator, one host machine.

The architecture supports indefinite upward composition natively — schema invariance and the level-agnostic recursive synthesis prompt work identically at any layer, including meta-Vines composing Vines from multiple projects. But V1 deliberately omits meta-Vines because the validated use case is a single project's arc, and building speculative upward layers without a concrete motivating need adds complexity without corresponding value.

When a concrete need for meta-Vines emerges — composing work across multiple projects, domain-level memory, or career-scale continuity — the architecture extends by running another layer of the same recursive operation. No new primitives are required. Until then, V1 ships what's validated.

**In scope for V1:**

- Single-Vine construction from a conversation transcript folder
- DADBEAR extension to create (not just maintain) bedrock pyramids
- Episodic chain (`conversation-episodic`) producing multi-resolution bedrocks
- Multi-resolution recursive synthesis at every Vine layer with primer-driven canonical identity propagation
- All six reading modes on the Vines page
- Question-pyramid escalation (via the existing recursive-vine-v2 Phase 2 mechanism)
- Runtime integration via manifest operations against Vine and bedrock nodes
- Staleness-pipeline corrections

**Deferred to later iterations:**

- Meta-Vines composing multiple project Vines
- Multi-operator shared Vines
- Cross-operator Vines via the Wire network
- Advanced identity-evolution UX (explicit synonym unification, canonical merge operations)
- Custom priority-ladder tuning beyond chain YAML
- Migration tooling between retro and episodic pyramid builds

---

## Part XI — Built from existing primitives

The product is built almost entirely from composition of existing machinery. For orientation:

**Reused unchanged:**
- Chain executor
- Forward/reverse/combine extraction passes (from the retro conversation pipeline)
- Token-aware chunker
- Pair-adjacent synthesis primitive
- Evidence-loop grounding primitive
- Recursive decompose primitive
- Webbing primitive (within a pyramid)
- `ties_to` cross-reference tracking
- Pyramid query APIs (CLI, HTTP)
- DADBEAR maintenance and debouncing
- Staleness detection and propagation
- Delta-chain storage and collapse

**Extended for the product:**
- DADBEAR gains the ability to *create* pyramids when source files appear, not just maintain existing ones
- The recursive synthesis prompt produces multi-resolution output (narrative tiers and vocabulary liveness classification) in one LLM call
- The episodic chain YAML (`conversation-episodic`) wires the primer loading, extraction, decomposition, synthesis, and delta steps together with the episodic memory schema

**New:**
- The Vines page in the app UI
- The five new prompt files (forward and reverse reused from retro; combine_l0, chronological_decompose, and synthesize_recursive are new)
- The manifest operation vocabulary for runtime Brain Map management (some of which may already exist; the rest is a small addition)

The brain-hurty complexity is in the recursion and composition, not in any single new component. Once the composition is right, the product falls out of existing capabilities being applied at a new scale.

---

## Part XII — Summary in one page

**Product.** A cognitive substrate for AI agents — a **Vine Pyramid** that serves as persistent memory across sessions and graceful working memory within sessions. Built from LLM synthesis as the primitive operation, modeled on (but not mimicking) the properties of human memory that make cognition possible.

**The Vine is a pyramid whose base layer is other pyramids.** Each bedrock pyramid is a single conversation processed into memory-schema form. The Vine composes bedrocks upward through recursive synthesis into progressively higher layers, culminating in a single apex that represents the full arc of the work.

**Leftward growth, scale-invariant working memory.** New bedrocks append on the left edge. The leftmost slope (one node per layer from apex down through the leftmost child at each level) covers progressively more recent, progressively smaller time windows at progressively higher resolution. Short-term memory quality is constant regardless of corpus size.

**Vocabulary is the trigger surface for cognition.** The in-context vocabulary carried by the Vine's leftmost slope is the *index of thinkable thoughts* for the current session. The agent recognizes live content by matching against this index, then retrieves detail on demand via CLI operations. Vocabulary must be in-context; detail can be lazy-loaded. Compression protects vocabulary absolutely; detail compresses freely because retrieval is always possible.

**Multi-resolution nodes.** Every synthesis pass writes each node at multiple pre-computed distillation tiers — narrative at full/medium/short/line resolution, vocabulary classified as live/mooted/historical. Dehydration at read time is pure projection (pick the tier that fits the budget), never re-synthesis. Runtime dehydration and rehydration are free.

**DADBEAR orchestrates ingestion.** Watches conversation folders, debounces active files, triggers bedrock creation when files stabilize, triggers Vine deltas when bedrocks finish, handles staleness ripple for modified sources. The operator doesn't manage a queue; the Vine becomes current as a background property of their work. DADBEAR extends to include pyramid creation alongside its existing maintenance role.

**Delta composition.** Each ingestion folds `n` new bedrock apexes (default `n=1`) into the current Vine state via the recursive synthesis prompt in delta mode. Input is the current Vine apex plus the leftmost slope (with token-aware auto-dehydration by default) plus the new bedrock(s). Delta propagates upward through affected slope layers. Cost per ingestion: O(depth). Total cost: effectively linear.

**Canonical identity convergence.** The Vine apex carries a running canonical identity catalog via the dehydration cascade. Extraction prompts see it as advisory reference — use canonical forms when matching, create new identities when novel, never force-fit. Asymptotic convergence over the corpus arc. Mooted vocabulary preserved so cross-references to historical decisions and retired terms still resolve.

**Three query directions. No pre-computed cross-navigation.**
1. Vine → bedrock (primer): canonical identity propagation during ingestion
2. Bedrock → Vine (delta): composition of new content into the running state
3. Question pyramid → Vine → bedrock (escalation): on-demand cross-navigation via recursive decomposition and existing `ties_to` edges

Cross-navigation happens through the question-pyramid mechanism, paying only when a query actually happens, shaped by the specific question, producing bespoke-intelligence answers rather than static pre-computed links.

**The Vines page.** A dedicated app page with a live vine visualization, canonical identities panel, DADBEAR status, reading mode selector, and question prompt bar. Six reading modes ship at V1: Memoir, Walk, Thread, Decisions Ledger, Speaker, Search.

**Runtime integration.** Agent loads the leftmost slope at session boot for instant orientation. Brain Map draws from the Vine for working memory. Manifest operations (hydrate, dehydrate, compress, densify, colocate, lookahead, investigation) work against Vine and bedrock nodes identically because the schema is invariant across layers. Dehydration is projection, not loss. "Let me think about that" is a mechanical operation: recognize, retrieve, incorporate. Async helpers write mid-session insights back to the Vine without blocking.

**One level of recursion at V1.** One Vine per project. Upward composition into meta-Vines is a supported future extension when concrete need emerges.

**Guiding principle: usefulness over cost.** LLM intelligence is cheap and getting cheaper. The scarce resources are operator attention and agent effectiveness, not compute. Bespoke intelligence is worth its cost when it produces genuinely useful understanding structure. The architecture leverages intelligence wherever intelligence is what produces the useful shape, without optimization-theater compromises.

---

## Closing

The Vine is a cognitive substrate for AI agents, constructed from existing pyramid primitives via composition and recursion. It grows leftward as the operator's work continues. It provides scale-invariant working memory at every moment, through a recency-weighted multi-resolution slope loaded into the agent's context. It provides lazy-loaded long-term memory on demand, through a question-pyramid escalation mechanism that descends into the full detail only when the agent's trigger surface recognizes something worth retrieving. It maintains canonical identity convergence over the corpus arc, so the agent's vocabulary stays coherent and its cross-references stay valid.

The substrate is memory-as-cognitive-primitive for AI agents, engineered from LLM synthesis as the underlying operation. It exists to give agents the continuity and working memory they need to operate effectively across sessions and within sessions against an unbounded corpus of prior work.

The product is the infrastructure for continuous agent cognition over time.
