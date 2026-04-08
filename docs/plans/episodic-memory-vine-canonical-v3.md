# Episodic Memory — Canonical Design (v3)

> **Purpose:** Canonical design document for episodic memory as a product. Covers the conceptual and user-experience surface in full resolution. Implementation details (chain YAML structure, database schemas, API signatures, code organization) live in implementation plans, not here.
>
> **Audience:** Anyone who needs to understand what the product is, what it does, how users and agents experience it, and why every piece is shaped the way it is — before touching the implementation.

---

## Preamble

Episodic memory is a **cognitive substrate for AI agents**. It is not a database, not a knowledge base, not an information-retrieval system. It is the structural medium on which agents operate with genuine continuity across sessions and graceful working memory within sessions.

The product is built from a more general substrate: **pyramids in compositional relationships**. A pyramid is a recursive memory artifact whose layers describe progressively higher-level views of the material below. Pyramids compose: any pyramid can serve as input to another pyramid that abstracts over it, and any pyramid can drill down into the pyramids it composes when it needs detail beyond what it carries directly. This composition is fully recursive in both directions and emerges naturally from configuration, not from intrinsic structural assumptions.

Episodic memory is the canonical instance of this substrate applied to conversation transcripts. The operator points the system at a folder of conversations (Claude Code `.jsonl` files in the bootstrap case); the system constructs memory pyramids for each conversation; those memory pyramids compose into a higher pyramid representing the operator's full project arc; the agent loads from that higher pyramid as its persistent brain. New conversations extend the structure organically as the operator's work continues.

This document describes how the substrate works as a cognitive primitive, how the episodic memory product is built from it, how users and agents interact with it, and why the architecture is shaped the way it is.

---

## Part I — Core principles

These principles shape every subsequent design decision. They're stated up front so the rest of the document reads as their consequences.

### 1.1 Memory as a cognitive primitive

Persistent memory for AI agents is a **cognitive substrate problem**, not a storage problem and not an information retrieval problem. The agent needs a medium that supports the *shape* of working memory, recent memory, and long-term memory as it operates — a medium that makes "thinking about something" a tractable mechanical operation rather than requiring exhaustive speculative querying of a passive store.

Every design decision in this document serves one goal: give the agent a cognitive substrate that feels like memory during operation, not like querying a database.

### 1.2 Vocabulary is the trigger surface for cognition

The load-bearing insight of the architecture:

> **An agent's ability to recall a memory at all depends on what vocabulary is present in its active context. Recognition has to happen in the context window; retrieval can happen through tool calls afterward.**

When a live agent says *"let me think about that,"* what's actually happening is: something in the conversation matched a vocabulary item the agent has in context, the agent recognized it as a thing it has memories about, and it's now triggering a retrieval operation to pull in detail. Thinking is mechanical — recognition firing retrieval firing incorporation into the next turn.

But recognition can only happen if the vocabulary is already in context. If the relevant identity isn't present in the agent's working slice, the agent doesn't know it has memories about that thing, and the memories might as well not exist — functionally identical to never having captured them.

Vocabulary here means the canonical identity graph: topics, entities, decisions (with their stances), glossary terms, practices, and the relationships between them. This graph is the *index of thinkable thoughts* for the current session. Whatever's in the index, the agent can recall. Whatever's absent from the index, the agent can't know to request.

Detail is different. Detail is always in the pyramid, always queryable, always one tool call away. Detail doesn't need to be pre-loaded because retrieval is fast. But detail is only reachable when the vocabulary in context tells the agent there's something to retrieve.

This separation of concerns — vocabulary as eager-loaded trigger surface, detail as lazy-loaded retrieval product — shapes the schema, the dehydration model, the primer mechanism, and the runtime integration. It's what makes the system work as cognition instead of as a passive store.

### 1.3 Detail is deferred, not diminished

A corollary of 1.2: compressing or omitting detail from the agent's active context is **not lossy** from the agent's perspective, because detail is retrievable on demand. The full content always lives in the pyramid. A dehydrated view of a node just means "this content isn't pre-loaded; request it when needed."

Dehydration at the vocabulary level, however, is catastrophic — it removes trigger conditions, making the memory invisible to the agent even though the content still exists in storage. The architecture treats these asymmetrically: vocabulary is preserved aggressively in the in-context slice, while detail compresses freely because retrieval is always possible.

This framing turns dehydration from a reluctant compromise ("we have to drop things we wish we could keep") into a deliberate scheduling decision ("we're choosing what to eager-load vs. what to lazy-load, and the trigger surface always wins eager loading").

### 1.4 Everything navigable is a pyramid

Pyramids are the universal answer to "how do you navigate something too big to hold native, while keeping recognition and retrieval cheap?" The pattern applies recursively to any kind of content:

- A short doc fits native — it doesn't need a pyramid
- A long doc that exceeds the navigation budget becomes a chunked pyramid with synthesized layers above the chunks
- A pyramid whose apex grows too dense becomes itself a pyramid, with layers above its apex that compose its content into bigger buckets
- A canonical vocabulary catalog that grows beyond what fits in budget becomes a pyramid of vocabulary, organized into categories at multiple abstraction levels
- A composition of multiple pyramids is itself a pyramid whose base layer is the apexes of the composed pyramids

There is no special "vine type" or "vocabulary type" or "doc type" of pyramid. All of these are pyramids. The structure is universal, applied wherever content needs to be navigable beyond what fits in a single budget.

### 1.5 Vine and bedrock are relative roles, not types

Every pyramid simultaneously plays two roles depending on which way you look:

- A pyramid is a **vine** with respect to whatever pyramids compose its base layer
- A pyramid is a **bedrock** with respect to whatever pyramid uses its apex as one of its base nodes

These are positional descriptors in a compositional relationship, not properties of an artifact. The same pyramid is a vine in one direction and a bedrock in the other.

In the episodic memory product:
- Raw `.jsonl` files are bedrock to the conversation pyramids that abstract them
- Conversation pyramids are bedrock to the project pyramid that composes them
- The project pyramid is bedrock to any future cross-project pyramid that includes it

The recursion extends indefinitely upward (bigger compositions) and downward (deeper detail). The same primitives, the same chain machinery, the same lifecycle, the same query mechanism work between any vine/bedrock pair at any level.

### 1.6 Synthesis and retrieval are both question-answering

The unifying primitive of the architecture is the **question pyramid**: a structure for asking a question, decomposing it into sub-questions, gathering evidence for each sub-question from available sources, and synthesizing the answers into a final composed response.

The same primitive runs in two timing modes:

- **Synthesis** (build-time): given content, the system asks "what questions need to be answered about this material to produce the best possible mappings across all relevant dimensions?" The answers become the structured content of a memory node. Construction is question-answering applied to incoming content with a configurable question set.
- **Retrieval** (read-time): given a question from an agent or user, the system decomposes it, finds answers in pyramids that have already been built (because they answered those questions at construction time), and composes a response.

When retrieval encounters questions whose answers don't already exist in the relevant pyramid, it can **trigger new synthesis** in the underlying bedrock pyramids to generate fresh L0 nodes with evidence answering those questions. This is demand-driven memory growth: the corpus actively becomes denser in the dimensions that get queried.

Construction and retrieval are symmetric operations on the same substrate. Building memory is answering questions in advance; using memory is asking questions and consuming pre-answered structures (or triggering new ones when needed).

### 1.7 Configuration-driven design

The architecture is designed to be **iterated by chain designers without code rebuilds**. Layer structure, question sets, synthesis behavior, decomposition rules, evidence-grounding policies, and the conditions under which new layers emerge are all encoded in chain YAML and prompt markdown. Designers can experiment with different memory shapes, different question sets, and different abstraction strategies by editing configuration, without rebuilding any compiled binary.

This matters because the right way to construct memory pyramids is an open design question. Different content types, different consumer types, different agent use cases may want different chain structures. The architecture refuses to bake assumptions about "the best way" into hardcoded behavior; instead, it provides the primitives and lets configuration drive composition.

When a pyramid grows another layer, it grows another layer because the chain designer said "if these conditions are met, build the next layer this way." Budget pressure on the navigation surface is one signal designers can use, but the decision is the designer's, not the runtime's. The runtime executes whatever the chain says.

### 1.8 DADBEAR orchestrates the lifecycle

DADBEAR is the existing pyramid lifecycle manager. It handles debouncing of source content, staleness detection, incremental re-processing, propagation of changes through pyramid dependency graphs, and triggering of dependent rebuilds. For episodic memory, DADBEAR's role expands to include **creating pyramids** when new source files appear (it previously handled maintenance only).

With this extension, DADBEAR is the universal orchestrator of the episodic memory product. It watches source folders, debounces active files, triggers bedrock construction when files stabilize, triggers parent-pyramid composition when bedrocks finish, propagates updates upward through any pyramids that compose those bedrocks, handles staleness ripple for modified sources, and (during query-time) triggers new L0 generation in bedrock pyramids on behalf of vine pyramids that need answers to specific sub-questions.

The operator does not manage a queue, does not press pause, does not manually trigger anything. Memory becomes current as a background property of ongoing work.

### 1.9 Usefulness over cost

LLM intelligence is sub-penny to single-dollar per operation, cheap and getting cheaper. The scarce resources are the operator's attention and the agent's effectiveness, not compute. Every design decision is made against this rubric:

- Does this bespoke intelligence produce genuinely useful understanding structure?
- If yes, it's worth the cost.
- If no, don't build it even if it would be cheap.

This rubric rules out architectural choices that are optimization theater — pre-computing things to avoid LLM calls that don't save money or add value. It rules in choices that leverage more bespoke intelligence wherever intelligence is what produces the useful shape.

---

## Part II — The pyramid architecture

### 2.1 Pyramids as universal navigable structures

A **pyramid** is a recursive memory artifact composed of layers, where each layer contains nodes that abstract one level above the layer below. The base layer holds the most concrete content; each layer up holds progressively higher-level views. The topmost node — the apex — represents the whole at maximum abstraction.

Pyramids exist because content that grows beyond what fits in a navigation budget needs structural organization to remain queryable. A small body of content can be held native and navigated by direct inspection. A larger body needs grouping. A larger body still needs grouping of groupings. The pyramid pattern is the recursive answer to that need at any scale.

A pyramid's specific shape — how many layers, how many nodes per layer, what gets fused with what at each step, what questions get answered at each level — is determined by the chain configuration that built it. Different chains produce different pyramid shapes from the same source content, optimized for different consumers and different use cases.

### 2.2 Compositional relationships

Pyramids compose. Any pyramid can serve as a source for another pyramid that abstracts over it. The composing pyramid's base layer holds pointers to the apex nodes of the composed pyramids; the composing pyramid's layers above synthesize those apexes into progressively higher-level views.

Within a single composition, the labels **vine** and **bedrock** describe positions:

- The **composing pyramid** is the *vine* — it sits above and integrates the bedrocks
- Each **composed pyramid** is a *bedrock* — it sits below and provides one input to the vine's base layer

These labels are purely positional. A pyramid that's a vine in one composition can simultaneously be a bedrock in another composition that includes it. The architecture doesn't distinguish "vine pyramids" from "bedrock pyramids" as types — it distinguishes which side of a particular relationship a pyramid is on.

This recursion has no fixed depth limit. A pyramid can be:
- A vine over raw chunks of one source file (the chunks are its bedrock, the file is its source)
- A vine over conversation pyramids (the conversation pyramids are its bedrock)
- A vine over project pyramids (each project pyramid is its bedrock)
- And so on indefinitely upward

Going downward, the same pattern: any pyramid's L0 nodes can themselves be pointers to sub-pyramids when individual chunks are complex enough to warrant their own internal structure.

The episodic memory product, in its V1 use case, has a multi-level composition tree built from this pattern:
- `.jsonl` source files → conversation pyramids (each conversation is a vine over its chunks)
- Conversation pyramids → project pyramid (the project pyramid is a vine over conversation pyramids)

The same architecture supports extension upward (cross-project pyramids, domain pyramids, career-arc pyramids) and downward (sub-pyramids of complex chunks like long structured artifacts) without any new primitives. The recursion is fully baked in.

### 2.3 Schema invariance at every layer

Every node at every layer of every pyramid uses the **same schema**. Only the *scale* of what the fields describe changes. An L0 node in a conversation pyramid has the same field shape as the project pyramid's apex; the difference is that the L0 covers a chunk of a conversation while the apex covers the whole project arc.

**Required fields (present at every layer):**
- `headline` — recognizable name for whatever scale of material this node covers
- `time_range` — temporal extent
- `weight` — size proportional to parent

**Configurable structured fields (the question-answer dimensions):**
- `narrative` — prose at this layer's scale
- `topics[]` — topic identifiers with importance scores and liveness markers
- `entities[]` — entity identifiers with roles, importance, and liveness markers
- `decisions[]` — decisions with `stance` (committed, ruled_out, open, done, deferred, superseded, conditional, other), importance, and `ties_to` cross-references
- `key_quotes[]` — exact quotes with `speaker_role` (human or agent) and importance
- `transitions` — how this node connects to prior and next nodes at this scale
- `annotations[]` — append channel for cross-pass signals (webbing, audit, manual)

The structured fields are the **answers to canonical questions** that the synthesis process asks of incoming content (Section 2.5). Different chains can extend the schema by adding fields to their question set; the chain YAML defines what questions to ask, and the resulting nodes contain the answers. For episodic memory specifically, the field set above is the canonical question set; other memory products built on the same substrate could extend or restrict it.

Schema invariance enables:
- One recursive synthesis prompt that runs at every layer without modification
- Cache-stable navigation that looks the same shape at every depth
- Runtime dehydration as a simple projection over fields and tiers
- Indefinite recursive composition (pyramids of pyramids of pyramids use the same operations)
- Multi-resolution loading where the agent picks zoom level, not node type
- The same navigation skeleton serving as build-time primer and runtime working memory

### 2.4 Multi-dimensional question-answer storage

Each node is the answer to a configurable set of questions about its inputs. The questions are the **dimensions** of the node — each question's answer is a different facet of the same content, queryable independently.

For episodic memory, the canonical questions are the structural fields above:

| Question | Answer |
|---|---|
| What recognizable name identifies this material? | `headline` |
| What temporal extent does it cover? | `time_range` |
| What proportion of its parent does it represent? | `weight` |
| What's the structural arc, at this layer's scale? | `narrative` |
| What was decided, with what stance, by whom, why? | `decisions[]` |
| What was said exactly that carries weight? | `key_quotes[]` |
| What subjects are active here? | `topics[]` |
| What people, files, systems, concepts are present? | `entities[]` |
| How does this connect to what came before and after? | `transitions` |
| What signals from other passes attach here? | `annotations[]` |

The synthesis prompt runs at every layer above the base, asks this question set (or whatever question set the chain configuration specifies), and produces a node whose content is the answers. At higher layers, the same questions are asked with answers operating at higher abstraction — the headline at L1 names a segment, the headline at L2 names a phase, the headline at the project apex names the whole arc.

**Pre-computing answers gives multi-dimensional queryability for free.** A reading mode that wants to surface decisions only is just a projection that selects the `decisions[]` dimension. A reading mode that wants narrative only selects `narrative`. A reading mode that wants speaker quotes selects `key_quotes[]` filtered by `speaker_role`. The same node serves all six reading modes (Part VII) by projecting different subsets of its dimensions.

**Resolution within a dimension comes from the synthesis itself.** Some chains may instruct the synthesis to produce multi-resolution narrative (a paragraph version, a three-paragraph version, a full version) so that runtime dehydration can pick which length to surface. Some chains may instead produce a single narrative at content-determined length and rely on the dimensional projection alone. Either approach is valid; the chain configuration decides.

The crucial property is that resolution and dimensional selection are both **read-time projections** over a node that was constructed to be multi-dimensional. No re-synthesis is required at retrieval. Dehydration is field selection; the node contains everything that was worth pre-answering, and the runtime picks what to surface.

### 2.5 The recursive question-answering operation

A single prompt — the recursive synthesis prompt — runs at every layer above the base of every pyramid. Its operation is **recursive question-answering**:

Given N peer input nodes at some layer, ask the configured question set about the joint material the inputs cover, and produce one parent node whose content is the answers — operating at exactly one level of abstraction above the inputs.

The prompt is **level-agnostic**. It infers the abstraction level from input content and shifts exactly one step outward. It never references absolute depth and treats upward composition as potential, not guaranteed, so the prompt remains accurate at the current top of any build.

The prompt operates in three input modes:

1. **Peer fusion** — N peer nodes at some layer → one parent node at one layer above
2. **Delta update** — existing parent node + one new or changed child → updated parent at the same abstraction level, incorporating the change
3. **Initialization** — single child node with no parent yet → parent wrapping it

And in four timing modes, with no prompt changes between them:

1. **Full-build pipeline** — offline, sweeps every layer of a fresh pyramid during initial construction
2. **Composition delta** — per-bedrock or per-batch, folds new bedrock content into the vine and propagates upward
3. **Runtime densify** — online async, produces a missing mid-level node on demand
4. **Collapse** — rewrites accumulated delta chains into fresh canonical node versions during idle time

The same prompt also runs in a fifth mode for demand-driven memory growth (Section 6.4): when retrieval encounters a sub-question whose answer isn't pre-computed in the relevant bedrock, the synthesis prompt is invoked to generate a fresh L0 node in the bedrock that answers the sub-question with grounded evidence. This is the synthesis operation applied at the smallest scale, triggered by query rather than by ingestion.

### 2.6 Delta chains and collapse

Pyramids update incrementally through the delta-chain pattern:

- **Bedrock L0 and L1** are immutable. Ground truth, appended to but never mutated.
- **Higher layers in the Understanding zone** are mutable via delta chains. Each update is a small patch representing the effect of one new or changed child.
- **Periodic collapse** rewrites a node's delta chain into a fresh canonical version synthesized from the current child set. Triggered by chain length, time since last collapse, or explicit request.

Delta-chain mutability gives **O(log N) per update** bounded by pyramid depth. Total cost for N additions is effectively linear regardless of corpus size. Apex content stays bounded by the dehydration cascade — what no longer matters gets shed to mooted or historical liveness, what still matters bubbles up.

Collapse passes run during idle time or on explicit request. They never block ingestion or runtime operations.

### 2.7 Configuration-driven layer emergence

The number of layers a pyramid has is **a property of its chain configuration**, not a runtime calculation based on intrinsic budget pressure. The chain YAML and prompts encode the rules the designer has chosen for when and how new layers are added. Designers iterate on those rules quickly, without code rebuilds, by editing configuration.

A chain might specify:
- Always build L0, L1, L2, and apex regardless of corpus size (fixed shape)
- Build new layers when the layer below exceeds N nodes (fanout-driven)
- Build new layers when the previous layer's content exceeds T tokens at the smallest projection (budget-driven)
- Build new layers when a question-set evaluation determines the existing layers no longer satisfy the questions cleanly (quality-driven)
- Some other policy entirely

The architecture supports any of these because the chain YAML drives the build process. Designers experiment by editing configuration. Rebuilding the chain executor is not required.

This applies recursively. A vocabulary catalog that grows too dense for the apex's budget can become its own pyramid with its own layer-emergence rules — also encoded in YAML, also iterable without code changes. The decision "should the canonical vocabulary become its own pyramid?" is a chain-design decision, made by editing configuration and observing the results.

The principle: the runtime executes the chain. The chain encodes the design. The design is iterable. No assumption is baked into the runtime that constrains the design.

---

## Part III — The leftmost slope: scale-invariant working memory

### 3.1 Leftward growth and the slope

A pyramid that grows over time (a project pyramid that gains new conversation bedrocks as the operator works) grows by appending new content on the **left edge**. The rightmost L0 node is the oldest entry; the leftmost L0 node is the most recent.

The **leftmost slope** is the diagonal path through the pyramid starting at the apex and walking down through one node per layer, always picking the leftmost child at each layer. For a pyramid with k layers, the slope contains k nodes.

```
apex                     ← covers the full arc
  |
leftmost L(k-1)          ← covers approximately the most recent half
  |
leftmost L(k-2)          ← covers approximately the most recent quarter
  |
leftmost L(k-3)          ← covers approximately the most recent eighth
  |
  ...
  |
leftmost L1              ← covers approximately the last two children
  |
leftmost L0              ← the most recent child in full detail
```

Because growth is leftward, each step down the slope moves to a progressively more recent, progressively smaller window at progressively higher resolution. The slope is a **recency-weighted zoom gradient** into the current state.

### 3.2 Scale-invariant working memory

The leftmost slope provides **scale-invariant working memory**: regardless of whether the pyramid contains 10 children or 100,000, the leftmost L0 always represents "the most recent entry" in full detail, the leftmost L1 always represents "the last two or so entries" at a slightly coarser scale, and so on. The slope keeps the same shape as the corpus grows.

New layers appear at the top as the corpus grows past whatever thresholds the chain configuration specifies, widening the pyramid overall. But the leftmost slope at each existing depth stays anchored to the recent edge — the leftmost node at each layer is always the freshest content at that scale.

This matters because the agent's working memory needs to be **consistent in its treatment of the present** regardless of how much history has accumulated. A 10,000-conversation pyramid should not degrade the agent's memory of today's session compared to a 10-conversation pyramid. The leftward growth combined with the leftmost-slope navigation pattern guarantees this property by construction.

### 3.3 The slope as primer and as runtime navigation skeleton

The leftmost slope serves both as the **build-time primer** (the reference block that rides in every extraction prompt during a new bedrock build) and as the **runtime navigation skeleton** (the stable cached scaffold that lives in the agent's Brain Map between turns).

It's the same slope. Same shape, same data, same vocabulary, same set of identities. The two timing modes consume it for different purposes — at build time it shapes canonical identity propagation into the new bedrock's extraction; at runtime it shapes the agent's recognition trigger surface during active cognition. Because the consumption modes share the artifact, the design only needs to make the slope right once.

### 3.4 The zoom gradient as a cognitive affordance

Each node in the slope is a different zoom level on a different time window. The combination gives the agent simultaneous context at multiple scales:

- The apex contributes the whole arc at the most abstract scale, plus the canonical identity catalog
- Mid-slope nodes contribute progressively more recent windows at progressively finer detail
- The bottom contributes today's content in full resolution

This is what the agent needs to operate: a meta-understanding of where the work is headed, mid-resolution context on recent phases, fine-grained detail on the immediate present. The slope provides all three simultaneously in a compact, cache-stable structure.

Under token pressure, dehydration follows the slope structure naturally: drop apex-facing slope nodes first (their loss is distant-scale meta-narrative, which hurts current work less), preserve recent-end slope nodes (they're the short-term memory the agent is actively using). Within each retained node, dimensional projection further controls budget by selecting which fields to surface.

---

## Part IV — The ingestion cycle

### 4.1 DADBEAR's role

DADBEAR orchestrates the entire ingestion lifecycle. Its responsibilities for episodic memory:

- Watch source folders (the operator's conversation transcript directories) for new and modified files
- Apply debouncing so active files are processed only once they stabilize
- Trigger pyramid construction when source files become eligible for processing
- Trigger composition deltas in vine pyramids when their bedrock pyramids finish building
- Propagate staleness through dependency chains when sources are modified
- Trigger demand-driven L0 generation in bedrock pyramids when query-time sub-questions need fresh evidence

DADBEAR handles all of this transparently. The operator does not interact with a queue, does not press pause, does not manually trigger anything. Memory becomes current as a background property of the operator's ongoing work.

For episodic memory, DADBEAR gains one new capability beyond its existing maintenance role: **the ability to create new pyramids when source files appear**. Previously DADBEAR maintained existing pyramids; the extension lets it bring new pyramids into existence as their sources become available.

### 4.2 Source detection and debouncing

For each watched source file, DADBEAR's flow is:

1. **Detection** — DADBEAR notices the file has appeared or been modified
2. **Debouncing** — the file is marked active; DADBEAR waits for the configured debounce window of inactivity before processing
3. **Triggered build** — once the file is stable, DADBEAR triggers the appropriate pyramid build via the chain executor
4. **Incremental handling** — if the file becomes active again mid-processing (the conversation resumes), the portion already past the debounce line continues processing; newly-added content queues for the next debounce cycle

The practical result: the bedrock pyramid for a conversation builds *behind* the live conversation. Long sessions don't block processing of their earlier chunks. The vine that includes them stays somewhat current even during multi-hour sessions. Re-opening an old conversation and adding content is handled transparently — DADBEAR detects the modification, debounces, re-builds the affected bedrock (fully or via incremental delta), and ripples the update upward through any pyramids that compose it.

The operator never sees any of this happening.

### 4.3 Bedrock construction with primer context

When DADBEAR triggers a new bedrock build, the **primer** is loaded from the parent pyramid's current state — specifically, the parent's leftmost slope. The primer rides in every extraction prompt during the bedrock build as a stable cached reference block.

Under default configuration, the primer carries:
- Full canonical live vocabulary in the apex (the identity trigger surface for downstream extraction)
- Slope navigation structure showing the current state at multiple scales
- Mooted vocabulary where budget allows (for cross-references to historical identities)
- Narrative content at appropriate projections per slope position

Because the slope is cache-stable (leftmost nodes rarely mutate; only the very bottom changes per ingestion), the primer's prefix cache hits are high. The model provider's cache makes each chunk's extraction effectively pay the primer cost only once per build.

The bedrock build itself runs through the chain phases the configuration specifies. For episodic memory, that includes forward and reverse temporal extraction passes that fuse into base-layer L0 nodes via a combine step, evidence-grounded segment construction, phase decomposition, and recursive synthesis up to the bedrock apex. Each step asks the question set the chain encodes; each step produces structured answers in the multi-dimensional schema.

### 4.4 Composition into the parent pyramid

When a bedrock build finishes, DADBEAR triggers a composition delta in the parent pyramid. The new bedrock apex lands at the leftmost L0 position of the parent. A delta then propagates upward through the parent's affected slope layers, updating one node per layer via the recursive synthesis prompt in delta mode.

The delta is bounded: each per-layer operation takes existing-parent + small-update as input, produces updated-parent as output. Cost per layer is roughly constant; total cost per ingestion is roughly O(depth).

The primary configurable for delta behavior is **`n` = batch size**:
- `n = 1` (default) — one new bedrock per delta. Maximum freshness; the parent is current within one bedrock's latency of new work.
- `n > 1` — wait for `n` bedrocks to accumulate, then run one delta folding all of them in. Useful for bootstrap ingestion of large backlogs.

A secondary configurable controls the slope context depth used in the delta input. Default behavior includes the full leftmost slope with token-aware auto-projection (apex-facing nodes trimmed first if the slope exceeds budget). A specific cap can be configured in YAML for experimentation.

Both knobs live in chain configuration. Defaults are sensible; experimenters tune without code changes.

### 4.5 Layer emergence from chain configuration

Whether a parent pyramid grows another layer in response to a delta — and how that layer is constructed — is **determined by the chain configuration**. The chain YAML and prompts encode the rules the designer has chosen for when and how layers are added.

A chain might say:
- "Always maintain L0, L1, L2, and apex regardless of size"
- "Add a new layer above the current apex when the current apex's content can no longer be projected to fit a target budget"
- "Add a new layer when the count of nodes at the current top exceeds N"
- "Add a new layer when a question-set evaluation determines the existing layers can't cleanly answer the canonical questions about the corpus anymore"
- Or any other policy the designer wants to experiment with

The runtime executes whatever the chain says. Designers iterate on layer-emergence policies by editing YAML, observing results, adjusting, re-running. No code changes required.

This applies fractally. The canonical vocabulary catalog at the parent's apex might itself become a pyramid when the chain's configured rules say so. The vocabulary pyramid has its own layer-emergence rules, also encoded in chain configuration, also iterable. A document that exceeds the budget for the parent's L0 layer might become its own sub-pyramid with its own configured rules. The architecture is open to experimentation at every level because configuration drives composition.

### 4.6 Staleness, re-ingestion, and ripple

The existing staleness machinery handles modifications to source content transparently:

- The operator modifies a transcript (or a transcript is updated by its originating tool)
- DADBEAR detects the modification, applies debouncing
- Marks the affected bedrock stale
- Re-builds (fully or incrementally, depending on the scope of change)
- Triggers a composition delta with the updated bedrock apex
- The delta ripples up through affected slope layers
- Any further pyramids that compose this one are notified through DADBEAR's dependency tracking, and their own deltas propagate as needed

There is no separate "correction flow" or manual re-ingest UI. The staleness pipeline is the correction pipeline. Whether a change comes from an edit, a re-recording, or an explicit operator request, the handling is the same.

### 4.7 The bootstrap case

The initial ingestion of an accumulated archive (e.g., months of Claude Code transcripts piled up in a folder) runs as a rapid burst of the steady-state cycle. DADBEAR scans the folder, discovers the backlog, sorts files by earliest timestamp, and processes them in order. Each file becomes a pyramid; each newly-built pyramid triggers a composition delta in the parent pyramid that includes it.

During bootstrap, the operator may set `n > 1` to batch ingestions, sacrificing intermediate freshness for faster total processing. After bootstrap, `n = 1` for steady-state responsiveness.

Bootstrap is interruptable and resumable transparently. Checkpointing is at the per-pyramid level — a completed pyramid's delta is committed atomically before the next build starts. A crash or pause between builds loses no work. A crash mid-build loses only the in-progress pyramid, which resumes on the next DADBEAR cycle.

From the operator's perspective: drop a folder, walk away, come back to a populated pyramid graph with the agent's memory ready to load.

---

## Part V — Canonical identity convergence

### 5.1 The fragmentation problem

Without coordination, each bedrock extraction produces its own identity namespace. The same concept gets different names in different sessions: "Pillar 37" becomes `Pillar 37` in one bedrock, `pillar 37` in another, `no-length-prescriptions (Pillar 37)` in a third. A person gets `Dennis` in one, `Partner` in another, `the AI partner` in a third. Cross-bedrock `ties_to` edges can't form because identities don't match. The agent's trigger surface fragments and recognition becomes unreliable.

Episodic memory solves this by giving every new bedrock build access to the canonical identity catalog accumulated by all prior work — through the primer.

### 5.2 The running canonical catalog

The parent pyramid's apex carries the **running canonical identity catalog**. High-importance topics, entities, decisions, glossary terms, and practices from across the full corpus bubble up through the dehydration cascade and persist at the apex. The apex's vocabulary fields IS the canonical catalog as it currently stands.

When a new bedrock build loads the primer, the primer includes the parent apex's vocabulary. The bedrock's extraction prompts see the full canonical catalog as ambient reference material. New content that matches existing identities uses the canonical forms; new content that introduces genuinely novel identities creates new entries that can be canonized by future passes.

If the canonical catalog grows large enough that it can't fit comfortably in the parent's apex even after dehydration, the chain configuration can promote the catalog to its own pyramid (a vocabulary pyramid). The vocabulary pyramid has its own layers organizing identities into categories, sub-categories, and so on. The agent's recognition surface then becomes "navigate the vocabulary pyramid" instead of "scan a flat catalog at the apex." Recognition gains an additional indirection step but stays tractable, because the vocabulary pyramid follows the same leftmost-slope navigation pattern as everything else.

This promotion is a chain-design decision. Designers can experiment with when and how to promote vocabularies without code changes, by editing the chain configuration that governs the parent pyramid's layer construction.

### 5.3 Advisory, not constraining

Critical rule for every extraction prompt that sees the primer's canonical catalog:

> The catalog is **advisory**, not a controlled vocabulary. When content in this chunk clearly refers to an identity already in the catalog, use the canonical form. When content introduces something genuinely new, create a new identity — do not force-fit novel content into existing categories just to match. Forced matches produce hallucinated connections that are worse than missed connections.

Without this rule, extractors would hallucinate matches and collapse novel content into existing categories. With it, the primer sharpens signal without blurring novelty. Identity convergence is **asymptotic**: early bedrocks introduce many variants, later bedrocks increasingly reinforce canonical forms, the catalog stabilizes over the corpus arc.

### 5.4 Identity evolution

Identities evolve over time. A concept that's coined informally in early sessions ("the no-length-prescription rule") may later be formalized with a different name ("Pillar 37"). The architecture handles this through the vocabulary liveness tiering:

- The new canonical name is tagged `live`
- The old name becomes `mooted` — no longer the primary form, but preserved because cross-references from older nodes may still point at it
- The relationship is captured via synonym annotations or via supersession in `decisions[]` if the evolution was driven by an explicit decision

Mooted vocabulary is included in the primer when budget allows, so new extractions can still recognize historical references and resolve them to current canonical forms. Under tighter budgets, mooted vocabulary is the first thing dropped, but live canonical forms remain present.

This preservation enables the agent to understand terminology evolution without re-discovery: when it encounters a historical name in an old bedrock, it can still recognize what concept is being referenced and link it to the current canonical form.

---

## Part VI — Query flow: four directions

The pyramid composition graph supports four query directions, each serving a different need. There is no pre-computed cross-pyramid webbing — cross-navigation happens on demand through the question-pyramid mechanism.

### 6.1 Vine → bedrock: the primer direction (build-time)

When DADBEAR triggers a new bedrock build, the parent pyramid (the vine) produces the primer (its leftmost slope, with the canonical identity catalog at the apex). The primer rides in every extraction prompt during the bedrock build as a stable cached reference block. Canonical identities propagate forward into the new bedrock's extraction.

Build-time. Automatic. User-invisible.

### 6.2 Bedrock → vine: the composition delta direction (build-time)

When a bedrock build finishes, DADBEAR triggers a composition delta in the vine pyramid. The new bedrock apex lands at the leftmost L0 position. The delta propagates upward through affected slope layers. The vine apex updates with the incremental change, including any new canonical identities the bedrock introduced.

Build-time. Automatic. User-invisible.

### 6.3 Question → pyramid → sub-pyramids: read-time retrieval

When the agent or operator wants to know something, the mechanism is a **question pyramid asked of the relevant pyramid**. The flow:

1. A question is posed against a pyramid (via CLI, HTTP, UI, or agent manifest operation)
2. A question pyramid is built that decomposes the question into sub-questions
3. Sub-questions hit the pyramid's existing structured content — the questions that were asked and answered at construction time
4. When an answer needs detail beyond what the pyramid's apex carries, the sub-question follows `ties_to` edges down into the bedrock pyramids that compose this pyramid's L0 layer
5. A child question pyramid spawns against the relevant bedrock(s), recursing the same mechanism
6. If a bedrock's answer references content in yet another pyramid, the escalation recurses further, bounded by maximum recursion depth and protected by visited-set cycle prevention
7. Results compose back up the chain into the original question's answer

This is the mechanism for cross-navigation, thread traversal, historical lookup, and anything else the agent needs that isn't pre-computed in the immediate pyramid's apex. Cost is paid only when a query actually happens.

### 6.4 Question → DADBEAR → child generation: demand-driven memory growth

When retrieval encounters a sub-question whose answer isn't already present in the relevant bedrock — not in any existing L0 node, not in any existing layer above — the system can **trigger DADBEAR to generate new L0 content in the bedrock that answers the sub-question with grounded evidence**.

The flow:

1. The vine pyramid's question-pyramid retrieval surfaces a sub-question that the relevant bedrock can't currently answer
2. The vine signals DADBEAR (or the question-pyramid mechanism signals DADBEAR on the vine's behalf)
3. DADBEAR triggers the bedrock to generate fresh L0 nodes that answer the sub-question, using the recursive synthesis prompt in question-answering mode against the bedrock's source content (chunks, raw transcripts, whatever the bedrock's L0 layer derives from)
4. The new L0 nodes carry evidence — pointers back into the source material — so the answers are grounded
5. The bedrock's higher layers update via delta propagation to incorporate the new L0 content
6. The bedrock's apex changes; the vine's L0 (which points at the bedrock apex) is now stale; the vine's delta cycle re-runs to pull in the updated bedrock content
7. The vine's answer to the original question now has the fresh evidence available

This is **demand-driven memory growth**. The corpus actively becomes denser in the dimensions that get queried. Memory pyramids are not static repositories of past content — they're alive, growing new structured evidence in response to questions that probe them, mediated by DADBEAR which orchestrates the generation across the dependency graph.

Construction and retrieval are symmetric question-answering operations on the same substrate. The only difference between the two is timing: at construction time, questions are asked proactively as content arrives; at retrieval time, questions are asked reactively as the agent or user queries. The mechanism is the same, the prompt is the same, the resulting content has the same shape. The system unifies "build memory" and "use memory" into one continuous question-answering loop.

---

## Part VII — Six reading modes

The same stored substrate supports six distinct rendering modes. All six ship at V1 because the storage supports them natively — they require only UI and query plumbing, not additional extraction.

### 7.1 Memoir

Read the apex top-to-bottom. Dense prose at the whole-arc scale. The primary cold-start loading path for a new agent session: load the apex, read it as a memoir, recover the meta-understanding of the current state of work in a single pass.

### 7.2 Walk

Scroll through the pyramid's L1, L2, or higher nodes in chronological order. The natural default direction is leftmost-first (newest-first) because recent work is typically more relevant to current activity. Operators who want the full arc from the beginning can walk rightward instead. Both directions operate on the same data.

### 7.3 Thread

Pick a canonical topic, entity, or decision identifier, and follow its connections across non-adjacent nodes. "Show me every moment that touched authentication." "Show me the full history of the chain-binding decision."

Thread traversal crosses bedrock boundaries via the question-pyramid escalation mechanism (Section 6.3) — the user or agent asks "show me the thread of X" and the parent pyramid's answer recursively descends into specific bedrocks via `ties_to` and spawned sub-questions. If any bedrock lacks the relevant structure, demand-driven generation (Section 6.4) can produce fresh evidence to complete the thread.

### 7.4 Decisions ledger

Render the pyramid's `decisions[]` arrays, aggregated across the corpus, filterable by stance. "Everything currently committed." "Everything open, sorted by how long." "Everything ruled out, with reasoning." The agent consults the ledger before proposing new work to avoid contradicting prior rulings or re-opening settled questions.

### 7.5 Speaker

Filter to one speaker role's contributions. Human turns (rare, high-weight, often binding direction) or agent turns (abundant, lower-signal-per-token but including commitments and discoveries). In an AI-dominated corpus where the agent speaks ~95% of the tokens, Speaker mode on the human filter is extremely high-signal — a small number of turns carrying the direction that shaped the whole arc.

### 7.6 Search

Full-text search over the raw chunks index (FTS5 over the preserved transcripts of all ingested bedrocks), with hits that drill up to owning L0 nodes, segment nodes, phase nodes, bedrock apexes, and parent-pyramid ancestors. The escape hatch for when paraphrase extraction has lost a specific phrase the operator remembers verbatim.

---

## Part VIII — The user experience: the Pyramid navigation page

The product introduces a dedicated **Pyramid navigation page** in the app where operators interact with their memory pyramids. Because every pyramid is structurally similar (only its position in compositional relationships differs), one page can serve every level of the composition graph.

### 8.1 Layout

The page has four primary regions:

- **Pyramid navigator (left rail)** — a hierarchical view of the operator's pyramid graph. The operator can select any pyramid to focus on. Compositional relationships are visible as edges — clicking a pyramid shows what composes it (its bedrocks) and what it composes into (its parents).
- **Pyramid visualization (main area)** — the currently-selected pyramid rendered as a recursive triangle. New L0 slots appear on the left as new content arrives. Layers are color-coded by depth. The leftmost slope is highlighted. The current apex headline displays prominently. Clicking any node opens its detail view.
- **Canonical identities panel (alongside)** — a live display of the apex's canonical catalog: top topics by importance, top entities grouped by role, active decisions by stance, glossary terms, practices. The operator's window into what the agent "knows" about the canonical shape of the work.
- **DADBEAR status (bottom)** — watched folders, recent debounce events, recent builds in progress, recent deltas, any staleness flags or errors.

Above the main area is a **reading mode selector** (Memoir, Walk, Thread, Decisions Ledger, Speaker, Search) and a **question prompt bar** for asking questions of the currently-selected pyramid.

### 8.2 Creating a new pyramid

The operator clicks "New Pyramid" on the navigation page:

1. **Name the pyramid.** A human-readable name for the use case.
2. **Choose the chain configuration.** Determines what kind of pyramid this is (episodic memory, document pyramid, vocabulary pyramid, etc.) and what question set it answers about its sources.
3. **Point at a source.** Either a folder of files (for a base-layer pyramid) or another pyramid's apex (for a composing pyramid).
4. **Configure DADBEAR behavior.** Debounce timer, batch size for deltas, slope context depth, auto-ingest vs. confirm-before-ingest.
5. **Confirm.** DADBEAR scans the source, discovers any backlog, sorts by timestamp where applicable, and begins processing.

The pyramid visualization starts populating as DADBEAR works. The operator can watch progress, close the app and come back later, or ignore it entirely.

Setting up the episodic memory product for a specific project is one application of this flow: select the episodic memory chain configuration, point at the conversation transcripts folder, and DADBEAR handles the rest.

### 8.3 Watching pyramids grow

During bootstrap (initial climb of a backlog):
- The pyramid visualization adds new L0 slots on the left as each bedrock finishes
- Delta pulses propagate up through affected slope layers, visible as brief highlight animations
- The canonical identities panel grows and stabilizes as canonical forms firm up
- The apex headline updates as the understanding matures
- DADBEAR status shows bedrocks completed, bedrocks remaining, and the current bedrock with its chain phase

During steady state (ongoing work):
- New bedrocks arrive organically as the operator has new conversations with agents
- Each new bedrock triggers one delta cycle in any composing pyramid
- The operator notices the pyramid update between work sessions without any action on their part

### 8.4 Exploring and asking questions

The operator (or the agent, via the same surface) can:

- **Switch reading modes** via the selector. Memoir for overview; Walk for chronological reading; Thread for topic tracing; Decisions Ledger for commitment review; Speaker for direction review; Search for verbatim lookup.
- **Drill into nodes.** Clicking any node opens its detail view. Nodes with `ties_to` down into bedrock pyramids navigate into those bedrocks. Clicking an L0 chunk shows the raw content that produced it.
- **Ask questions.** Typing a question into the prompt bar triggers a question pyramid built against the currently-selected pyramid. The answer renders in the main view with citations linking back to specific moments. If the answer requires fresh evidence that doesn't exist yet, the demand-driven generation mechanism (Section 6.4) can produce it transparently.
- **Walk up and down the composition graph.** From the pyramid visualization, the operator can navigate to bedrocks (down) or parents (up) by clicking the relevant edges in the navigator.

### 8.5 Annotation and correction

Any node can be annotated by opening its detail view and appending to its `annotations[]` field. Annotations are cheap, non-destructive updates that persist across future deltas and are visible to future readers and builds.

Corrections to source content are handled by the staleness pipeline automatically — the operator modifies the source, DADBEAR detects the change, re-builds the affected bedrock, and deltas the update through any pyramids that compose it. No separate correction UI is needed.

---

## Part IX — Runtime integration

The pyramid is the cognitive substrate from which the agent draws working memory during active sessions. The runtime integration has several distinct operations.

### 9.1 Cold start

A new agent session begins. The agent has no biological continuity. It loads the relevant pyramid's leftmost slope as its initial context. Because the slope is recency-weighted and multi-dimensional:

- The apex contributes the whole-arc meta-narrative and the full canonical live vocabulary
- Each step down the slope contributes progressively finer detail on progressively more recent windows
- The leftmost L0 contributes the most recent content in full resolution

The agent comes online with instant multi-resolution orientation: perfect short-term memory, adequate medium-term context, coarse long-term overview. Total load: roughly a dozen nodes for a pyramid of any realistic size, cache-stable across turns, drawn through a single CLI call.

From the agent's subjective standpoint, it wakes up knowing where the work stands.

### 9.2 The Brain Map and manifest operations

During active work, the agent's cognition is divided into three tiers:

- **Conversation Buffer** — live dialogue turns. Sacred. Only actual back-and-forth lives here; tool results, synthesized findings, and prior-session context never accumulate in the buffer.
- **Brain Map** — navigation skeleton (drawn from the relevant pyramid's leftmost slope) plus variable hydrated content (specific nodes pulled in for the current turn's work). Mutates between turns via manifest operations.
- **Pyramid cold storage** — the full pyramid graph on disk. Query surface for everything the Brain Map doesn't currently hold.

Between turns, the agent emits a structured **context manifest** as part of its response — invisible to the human user, machine-readable, consumed by the runtime harness. The manifest specifies what to do with the Brain Map before the next turn. Available operations include:

- `hydrate <node> <projection>` — pull a specific node at a specific dimensional projection into the Brain Map
- `dehydrate <node>` — drop a Brain Map node's richer content while retaining the vocabulary floor
- `compress <buffer_range>` — replace a stretch of dialogue turns with a synthesis node that moves to the Brain Map
- `densify <missing_node>` — request an async helper to produce a missing mid-level synthesis node on demand
- `colocate <seed>` — pull in nodes related to a seed via `ties_to`
- `lookahead <nodes>` — speculatively pre-stage nodes the agent anticipates needing next turn
- `investigation <node>` — flag a node as possibly stale and request async verification
- `ask <pyramid> <question>` — fire a question pyramid against the named pyramid; the answer flows into the Brain Map (or triggers demand-driven generation if the answer doesn't yet exist)

Each manifest pair (emitting turn + operations) is stored in a provenance trail for audit and metrics. The agent is steering its own cognition.

### 9.3 Dehydration as projection

Dehydration at runtime is a **pure projection operation** over the multi-dimensional content the synthesis prompt produced at write time. When the agent dehydrates a Brain Map node to free tokens, it's selecting a smaller subset of the node's pre-computed dimensions — narrative at a more compressed projection if the chain produced multiple narrative levels, vocabulary trimmed to the live tier, decisions filtered to high-importance committed/ruled-out, etc.

When the agent later rehydrates, it selects more dimensions or fuller projections. No LLM call, no quality loss, no synthesis latency. The vocabulary floor of every node is always present in the Brain Map whenever the node is there at all — the trigger surface never degrades.

### 9.4 "Let me think about that" as a mechanical operation

The architecture makes *"let me think about that"* a first-class mechanical operation, not a figure of speech. The operation has three phases, all tractable:

1. **Recognition.** Something in the live conversation fires a vocabulary match against the Brain Map's in-context trigger surface. The agent recognizes that it has memories about this specific thing.
2. **Retrieval.** The agent's manifest emits an `ask` or `hydrate` operation naming the relevant pyramid and the question or node. The runtime harness executes the operation against the pyramid graph between turns and returns the requested content into the Brain Map. If the content doesn't yet exist (the question hasn't been pre-answered), demand-driven generation (Section 6.4) produces it on the fly.
3. **Incorporation.** The retrieved content enters the Brain Map. The next turn references it fluidly, as though the detail had always been in working memory.

From the agent's subjective standpoint, this feels like thinking. From the architecture's standpoint, thinking is a mechanical retrieve-and-incorporate cycle triggered by vocabulary recognition and supported by pre-answered question structures plus on-demand generation.

The operation only works if the vocabulary trigger surface carries the identity that needs to fire. That's why the Brain Map always includes the relevant pyramid's leftmost slope vocabulary even under extreme token pressure.

### 9.5 Asynchronous writeback

Mid-session, the agent may discover things that should persist into the next session: a new commitment, a newly-ruled-out alternative, a clarifying definition, an audit finding. The agent emits a manifest operation describing the update, and an async helper executes the update via the recursive synthesis prompt in delta mode against the relevant pyramid node.

DADBEAR's existing machinery propagates the update through affected pyramid layers. By the next session, the change is reflected in the primer the next agent instance loads. In-session insight persistence becomes a natural mechanical operation rather than a separate "save state" burden.

### 9.6 The agent's subjective experience

Putting it together: at session boot, the agent loads the leftmost slope and feels oriented. During active work, manifest operations let it hydrate, dehydrate, colocate, and densify as the conversation's needs shift. When it recognizes something it has memories about, retrieval is a tool call away. When the question hasn't been pre-answered, demand-driven generation produces fresh evidence on the fly. When it discovers something worth preserving, an async helper writes it back without blocking the live session. Session end is unremarkable — there's nothing to save that isn't already saved.

The agent's experience of having persistent memory is the experience of operating on the pyramid. The pyramid is the substrate, and it feels — from the inside — like memory, because it supports the shape of cognition natively.

---

## Part X — Scope at V1

V1 focuses on making the episodic memory product genuinely useful for a single project's arc, end-to-end. The architecture supports indefinite recursive composition — the same primitives extend to multi-project compositions, domain-level pyramids, and career-arc structures — but V1 deliberately omits speculative upward extensions because the validated use case is a single project, and building unvalidated layers adds complexity without corresponding value.

When concrete use cases for higher composition emerge, the architecture extends by running another layer of the same primitives. No new components are required. Until then, V1 ships what's validated.

**In scope for V1:**

- Single-project episodic memory pyramid construction from a conversation transcript folder
- DADBEAR extension to create (not just maintain) pyramids at any level of the composition graph
- Episodic memory chain configuration (the question set, the layer structure, the synthesis behavior) encoded in YAML
- Multi-dimensional question-answer storage at every layer
- Recursive synthesis with primer-driven canonical identity propagation
- Vocabulary catalog promotion to its own pyramid when the chain configuration calls for it — the canonical identity catalog is itself a pyramid-shaped structure that grows layers and promotes as its scale demands, and the V1 chain configurations are written to include this behavior from the start
- All six reading modes on the Pyramid navigation page
- Question-pyramid retrieval with escalation into composed pyramids
- Demand-driven L0 generation triggered by retrieval sub-questions
- Runtime integration via manifest operations against the pyramid graph
- Staleness-pipeline corrections

**Deferred to later iterations:**

- Multi-project meta-pyramids composing multiple project pyramids
- Multi-operator shared pyramids
- Cross-operator pyramids via the Wire network
- Advanced identity-evolution UX
- Migration tooling between alternative chain configurations

---

## Part XI — Built from existing primitives

The product is built almost entirely from composition of existing machinery applied at a new scale. For orientation:

**Reused unchanged:**
- Chain executor
- Forward/reverse temporal extraction passes
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
- DADBEAR gains the ability to trigger demand-driven L0 generation in bedrock pyramids on behalf of vine pyramids during query-time
- The recursive synthesis prompt is framed explicitly as a question-answering operation, with the question set configurable per chain
- Chain configuration gains the rules for layer emergence, allowing designers to iterate on layer policies via YAML

**New:**
- The Pyramid navigation page in the app UI
- The episodic memory chain configuration (chain YAML, question set, prompt files)
- Manifest operation vocabulary for runtime Brain Map management (some operations may already exist; the rest is a small addition)

The complexity is in the recursion and composition, not in any single new component. Once the composition is right, the product falls out of existing capabilities applied at a new scale.

---

## Part XII — Summary in one page

**Product.** A cognitive substrate for AI agents — persistent memory that supports the shape of working memory across and within sessions. Built from LLM synthesis as the primitive operation, modeled on (but not mimicking) human memory properties that make moment-to-moment cognition possible.

**The substrate is pyramids in compositional relationships.** Every pyramid is a recursive memory artifact whose layers describe progressively higher-level views of the material below. Pyramids compose freely: any pyramid can serve as input to another that abstracts over it. The labels "vine" and "bedrock" describe positions in a relationship, not types — every pyramid is both, depending on which way you look.

**Vocabulary is the trigger surface for cognition.** The in-context vocabulary carried by the leftmost slope is the *index of thinkable thoughts* for the current session. The agent recognizes live content by matching against this index, then retrieves detail on demand via tool calls. Vocabulary must be in-context; detail can be lazy-loaded. Compression protects vocabulary absolutely; detail compresses freely because retrieval is always possible.

**Synthesis and retrieval are both question-answering.** The same question-pyramid primitive runs in both timing modes. Construction asks "what questions need to be answered about this content?" and stores the answers as the node's structured fields. Retrieval asks the agent's question, decomposes it, and consumes pre-answered structures (or triggers fresh ones). The schema fields are the canonical question set; different chains can ask different questions for different memory products.

**Layers emerge from chain configuration, not runtime heuristics.** Chain YAML and prompts encode the rules for when and how new layers are added. Designers iterate quickly without code rebuilds. Different chains can have different layer policies; the runtime executes whatever the chain says. This applies fractally — vocabulary catalogs that grow too dense can themselves become pyramids by chain-design decision.

**Leftward growth, scale-invariant working memory.** New content appends on the left edge. The leftmost slope (one node per layer from apex down through the leftmost child at each level) covers progressively more recent, progressively smaller windows at progressively higher resolution. Short-term memory quality is constant regardless of corpus size.

**DADBEAR orchestrates the lifecycle.** Watches source folders, debounces active files, triggers pyramid creation when files stabilize, triggers composition deltas when bedrocks finish, propagates staleness through dependency graphs, and triggers demand-driven L0 generation in bedrocks on behalf of vines during query-time. The operator doesn't manage a queue; memory becomes current as a background property of ongoing work. DADBEAR extends to include pyramid creation alongside its existing maintenance role.

**Four query directions.**
1. **Vine → bedrock (primer)** — build-time canonical identity propagation
2. **Bedrock → vine (composition delta)** — build-time content composition
3. **Question → pyramid → sub-pyramids (retrieval)** — read-time decomposed lookup with bedrock escalation
4. **Question → DADBEAR → bedrock generation (demand-driven growth)** — read-time fresh evidence generation when answers don't yet exist

The corpus actively grows in the dimensions that get queried. Memory is alive, not just stored.

**Canonical identity convergence.** The pyramid's apex carries a running canonical identity catalog via the dehydration cascade. Extraction prompts see it as advisory reference. Asymptotic convergence over the corpus arc. Mooted vocabulary preserved so cross-references to historical identities still resolve. If the catalog grows too large, it can be promoted to its own pyramid by chain-design decision.

**Six reading modes.** Memoir, Walk, Thread, Decisions Ledger, Speaker, Search. All six ship at V1 because the storage supports them natively.

**The Pyramid navigation page.** A dedicated UI page for navigating the operator's pyramid graph. Pyramid visualization, canonical identities panel, DADBEAR status, reading mode selector, question prompt bar. Operators can navigate up and down compositional relationships freely.

**Runtime integration.** Agent loads the leftmost slope at session boot for instant orientation. Brain Map draws from the pyramid for working memory. Manifest operations (hydrate, dehydrate, compress, densify, colocate, lookahead, investigation, ask) work against any pyramid in the graph because the schema is invariant. Dehydration is projection, not loss. "Let me think about that" is a mechanical recognition-retrieval-incorporation operation. Async helpers write mid-session insights back to the pyramid without blocking. If the agent asks something not yet answered, demand-driven generation produces fresh evidence on the fly.

**Configuration-driven, no Rust rebuilds for design iteration.** Chain YAML, prompt markdown, and schema definitions are the surfaces designers iterate on. The runtime executes whatever the configuration says. Different memory products, different question sets, different layer policies, different chains — all by editing configuration. The architecture is open to experimentation at every level.

**One level of recursive composition validated at V1.** The architecture supports indefinite upward and downward recursion. V1 ships the level that's currently useful (`.jsonl` files → conversation pyramids → project pyramid). Higher composition is a future extension when concrete need emerges.

**Guiding principle: usefulness over cost.** LLM intelligence is cheap and getting cheaper. The scarce resources are operator attention and agent effectiveness, not compute. Bespoke intelligence is worth its cost when it produces genuinely useful understanding structure. The architecture leverages intelligence wherever intelligence is what produces the useful shape.

---

## Closing

Episodic memory is a cognitive substrate for AI agents, built from pyramids in compositional relationships. It grows leftward as the operator's work continues. It provides scale-invariant working memory at every moment, through a recency-weighted multi-resolution slope loaded into the agent's context. It provides lazy-loaded long-term memory on demand, through question-pyramid retrieval that descends into deeper detail only when the agent's trigger surface recognizes something worth retrieving — and through demand-driven generation that produces fresh evidence when pre-answered structures don't yet cover the question. It maintains canonical identity convergence over the corpus arc, so the agent's vocabulary stays coherent and its cross-references stay valid.

The substrate is memory-as-cognitive-primitive for AI agents, engineered from LLM synthesis as the underlying operation, with the question pyramid as the unifying primitive of both construction and retrieval. The product exists to give agents the continuity and working memory they need to operate effectively across sessions and within sessions against an unbounded corpus of prior work.

The architecture is open. Configuration drives composition. Designers iterate without code rebuilds. The same primitives extend in every direction the use case requires. The pyramid is the substrate, the question pyramid is the operation, and DADBEAR is the orchestrator that keeps everything alive.
