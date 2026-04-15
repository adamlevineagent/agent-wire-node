# Episodic Memory — Canonical Design Doc

> Design for the second conversation pyramid product: **Episodic Memory** — a recursive fractal memory substrate that serves as an AI agent's **externalized persistent brain**, consumed in two complementary modes: cold-start continuity across sessions (a successor agent loading prior work) and in-session working memory management (the active agent continuously hydrating and dehydrating nodes between turns to operate with a bounded context window against an unbounded knowledge corpus).
>
> Companion to v2.6 (`conversation-chronological`), which ships the first product: **Retro / Meta-Learning** — a thesis-extraction pyramid that produces patterns, principles, and lessons from a session.
>
> Both products ride on the same underlying pipeline (chunker, chain executor, forward/reverse/combine triple-pass L0), differ in their synthesis prompts, and will coexist as separate preset selections in the wizard.
>
> **This doc covers both halves of episodic memory**: the pipeline that produces the cold storage substrate (Sections 1–11), and the runtime integration that uses the same substrate as the agent's active working brain (Section 12). The design is shaped by both consumption patterns from the start — they share schema, prompt, and recursive architecture.

---

## Section 1 — The reframe that defines the product

### 1.1 The consumer is the AI agent, not the human

Persistent memory is a resource asymmetry. The human who participated in a conversation has biological memory of it — the flow, the decisions, the tone, what they said, why they said it. They don't need a pyramid to remember.

The AI agent has no such continuity. Every new conversation starts from blank state. The agent reconstructs context by reading prior work, but "prior work" today means raw transcripts, handoff docs, or whatever structured artifacts the human was willing to produce. None of those are optimized for agent loading. They're optimized for human reading (handoffs, docs) or for pure storage (transcripts).

**The episodic memory pyramid is the agent's externalized brain.** It is written *for* the successor agent to load at the start of a new session, so that instance can reconstruct working state it has no biological memory of. The human is not the reader. The human has their own memory.

This reframe ripples through every design decision downstream. Audience, voice, priorities, schema, prompt framing — all of them shift when you accept that the document is being written for a cognitive system with no prior exposure to the material, not for a human who lived it.

### 1.2 What the successor agent needs on pickup

When a new agent instance loads memory nodes to bootstrap context for a new session, it needs to reconstruct:

- **Current state** — what's the active piece of work, where is it in its lifecycle, what's blocking
- **Binding commitments** — what did the prior instance agree to, promise, or define as done; these are binding on the successor unless explicitly released by the human
- **Rejected alternatives** — what was tried or proposed and ruled out, with the reasoning, so the successor doesn't re-propose things that already cost tokens to kill
- **Open questions** — what was left in-flight, what's still unanswered, what was deferred
- **Human direction** — what the human instructed, what their principles are, what they've corrected, what they value; the human's authority is absolute and their exact words are binding
- **Prior-agent reasoning and discoveries** — what the prior instance learned, concluded, or found; not because agent identity is continuous across instances, but because those conclusions were earned with work the successor should respect as priors rather than re-derive

Notice that "readable narrative arc" is not the top of that list. The narrative is instrumentally useful — it encodes ordering and transitions — but it serves the reconstruction task, not a reading experience. Episodic memory is working memory, not a memoir.

### 1.3 Two consumption modes: cold-start continuity and in-session hydration loop

The agent operates against the pyramid in two distinct modes. The design has to support both, and — critically — the schema, prompts, and recursive architecture are identical for both.

**Mode A — Cold-start continuity.** A new agent instance starts a session with no biological memory of prior work. It loads memory nodes from the pyramid at the start of the session to reconstruct working state: active commitments, rejected alternatives, open questions, human directives, prior discoveries, the shape of what's been done. This is a bootstrap load — one-shot, happens once at session start, pulls enough into context to continue the work the prior instance left off.

**Mode B — In-session hydration loop.** During active work, the agent's context window is bounded (~200k tokens typically) but the pyramid corpus is unbounded (thousands of sessions, millions of L0 nodes, potentially). The agent maintains a small hot working set drawn from the larger cold storage. As the conversation shifts topics, uncovers new threads, or needs deeper context, the agent hydrates specific nodes into its working set and dehydrates others back to metadata-only form to free tokens for new content. This is a continuous loop that runs between turns (`context_schedule`) and on-demand mid-turn (`pyramid_query`).

Both modes read the same pyramid. Both modes use the same schema. Both modes depend on the same recursive fractal structure. **The only difference is when and how much gets loaded.** Mode A loads a large chunk once; Mode B loads small amounts continuously as the work demands. Mode B dominates over the lifetime of an agent; Mode A is the entry point that enables Mode B to make smart hydration decisions by giving it a navigational skeleton to start from.

The schema's optional-by-default field design (Section 4.1) is load-bearing for Mode B: a dehydrated node in the working set is just a node with its narrative, quotes, and heavy fields dropped to metadata only (headline, time_range, weight, topic tags, one-line snippet). The dehydration cascade designed for pipeline upper-layer synthesis (Section 6.4) **doubles as the runtime cold representation of nodes in the Brain Map**. Same storage, two views: hydrated (full content) and dehydrated (navigation metadata). No special cold format, no separate dehydrated table, no migration — the cascade just decides how much of the node to carry at any given moment.

Full details of the runtime architecture — memory containers, cache breakpoint strategy, context manifest protocol, topic threads, CLI/HTTP interfaces, delta-chain + collapse — are in **Section 12**. The sections between here and there are about the substrate that both modes consume.

### 1.4 The agent-as-consumer creates asymmetric preservation

Because the reader is a future agent loading the memory to work from, preservation priorities diverge from what a human-audience memoir would emphasize:

- **Human quotes are preserved as authoritative direction.** The exact words carry intent and tone. A paraphrase of *"I don't want you to do X"* is not the same as the exact quote — the successor needs the exact words because they're binding instruction, and tonal signals (frustration, decisiveness, hedging) carry as much weight as the literal content.

- **Prior-agent quotes are preserved as earned state.** Not agent exposition — which is wasteline the successor doesn't need — but commitments, discoveries, rulings, findings, and definitional claims. *"I will not ship v2.6 without running the chain tests"* from the prior agent is a load-bearing commitment. *"I found the bug at build.rs:684"* is a load-bearing discovery. These survive as exact quotes because the successor treats them as priors to respect, not conclusions to re-derive.

- **Agent exposition is paraphrased into narrative prose.** Long explanatory paragraphs, restatements of established context, internal reasoning dumps — these are compressible. The narrative captures what was said without burning the quote budget on content that's recoverable from paraphrase.

The rule, stated generally: **preserve quotes when their exact words carry weight the paraphrase would lose.** For human turns, the bar is low (direction, reaction, correction, decision, distinctive phrasing). For agent turns, the bar is higher (commitment, discovery, verdict, definitional claim).

This asymmetry also creates a pleasant side effect in AI sessions where the AI speaks ~95% of the tokens: the quote budget naturally concentrates on the rare high-signal turns rather than the abundant exposition.

---

## Section 2 — Three product categories, one substrate

The conversation pyramid is a substrate that can produce different products depending on what the synthesis chain is optimized for. Each product serves a distinct cognitive function and has a distinct downstream consumer.

| Product | Cognitive analogue | What it preserves | Primary consumer | Apex reads like |
|---|---|---|---|---|
| **Retro / Meta-Learning** *(v2.6)* | Semantic memory | Patterns, principles, lessons | The human, for practice refinement | A thesis |
| **Episodic Memory** *(this doc)* | Episodic memory | Events, flow, commitments, rejections, earned state | The successor AI agent, for working continuity | Depends on the reading mode |
| **Decisions Log** *(future)* | Procedural memory | Commitments, alternatives, rationale, ledger-style | Either, for accountability and audit trail | A ledger |

All three live on the same substrate (chain executor, token-aware chunker, forward/reverse/combine L0) and differ only in their synthesis prompts and which fields they emphasize. A single session's chunks can be processed by multiple chains if both products are wanted — the ingest cost is borne once, the extraction cost doubles.

**Retro and Episodic are not competing products.** They serve different readers asking different questions. The human wanting to know *"what did I learn from this session"* reads the retro apex. The agent wanting to know *"where did I leave off, what am I committed to, what did we already try"* reads the episodic memory. Both ship. The wizard offers both as presets.

### 2.1 Why episodic is the compositional primitive

Retro pyramids are terminal. Their apex is already a thesis — synthesizing a thesis on top of theses produces meta-thesis noise, not useful meta-learning. A retro apex can be read but not meaningfully composed upward.

Episodic pyramids are compositional. Their apex is a structured memory block with narrative, decisions (with stances), quotes, time range, topic tags, and entity references. This substrate supports recursive upward composition: a vine bunch of conversation episodic memories produces a multi-session episodic memory; a vine of those produces a weekly memory; a vine of those produces a project memory; and so on. The same schema at every scale, the same synthesis operation at every scale, the same recursive prompt at every scale.

This is the reason episodic memory is worth building as a distinct second product rather than a feature flag on retro. **Episodic memory is the building block; retro is a terminal rendering.** The agent-continuity use case requires indefinite upward composition, which only the episodic shape supports.

---

## Section 3 — The core insight: recursive fractal memory

The central architectural principle, from which everything else falls out:

> **Every memory node at every layer has the same structural shape. The same synthesis operation builds every layer, from chunk to session to multi-session vine to project to career. The operation is "take N peer nodes, produce one parent node that abstracts one level above them." The prompt that performs this operation is identical at every layer; it knows nothing about what layer it's at or what comes above or below.**

This principle has three corollaries that together define the design:

**Corollary 1 — The schema is invariant across layers.** An L0 node, an L1 node, an L2 node, a session apex, a daily vine apex, a weekly vine apex — all have the same fields with the same meanings. Only the *scale* of what the fields describe changes. Headline names a chunk at L0, a session at the conversation apex, a week at the weekly apex; the field is called `headline` at every level and holds a recognizable name for whatever scale of material the node covers.

**Corollary 2 — The synthesis prompt is recursive and level-agnostic.** One prompt file, `synthesize_recursive.md`, runs at L0→L1, L1→L2, L2→session apex, session apex→daily vine, and onward indefinitely. The prompt doesn't mention "L1" or "session" or "vine" or any layer-specific noun. It describes the operation in relative terms: *"abstract one level above the inputs you see."* The model infers the abstraction level from the input content and shifts exactly one step outward.

**Corollary 3 — Upward composition is always potential, never guaranteed by the prompt.** The recursive prompt cannot assert *"your output will be consumed by another pass one layer above"* because that claim is false at the current apex. The prompt instead asserts *"your output must be composable upward at any future point, because the architecture is recursive."* The obligation on the output is identical at every layer, whether or not the upward consumption has happened yet. Today's apex might be tomorrow's middle node, and the prompt is written to be honest about that.

These three corollaries together make the memory pyramid a true fractal: same shape at every scale, built by the same operation at every scale, extending indefinitely upward.

### 3.1 The only exception: base-layer reconciliation

The recursive synthesis prompt does **pair-adjacent-like merge-and-zoom-up** on peer memory nodes. That operation runs at every layer L1 and above.

But the base-layer extraction is a different operation: it takes raw chunk text plus two intermediate views (forward-reading and reverse-reading) and fuses them into the first episodic memory node. This is reconciliation of two views of the same content, not abstraction above children. There's no "zoom up" — the output is at the same conceptual scale as the inputs (one chunk), just with both temporal lenses applied.

So the full prompt set for episodic mode is:

1. **`forward.md`** — temporal-forward extraction of a single chunk with running-context accumulation *(reused from v2.6, no changes)*
2. **`reverse.md`** — temporal-backward extraction of a single chunk with running-context accumulation *(reused from v2.6, no changes)*
3. **`combine_l0.md`** — NEW: reconciles forward and reverse into the base episodic memory schema
4. **`chronological_decompose.md`** — NEW: cuts the session into natural phase boundaries for downstream grounding
5. **`synthesize_recursive.md`** — NEW: the recursive prompt that runs at every layer L1 and above, and at every vine layer above that, forever

Five prompts total. One of them (`synthesize_recursive`) is load-bearing at every layer above the base, so it gets the most design attention.

---

## Section 4 — The unified episodic memory schema

Every node at every layer has this shape. Every field except the first three is optional, and fields can be appended later by subsequent passes (annotation, webbing, vine composition, manual agent edits) without invalidating the node.

```json
{
  "headline": "recognizable name for whatever scale of material this node covers",
  "time_range": {"start": "ISO-8601", "end": "ISO-8601"},
  "weight": {"tokens": 28341, "turns": 14, "fraction_of_parent": 0.14},

  "narrative": "Dense prose describing what happened at this scale, abstracted one level above the input scale. Written for rapid agent cognitive load, not literary reading.",

  "topics": [
    {"name": "topic identifier", "importance": 0.9}
  ],
  "entities": [
    {"name": "entity identifier", "role": "person | file | concept | system | slug | other"}
  ],

  "decisions": [
    {
      "decided": "what the decision is about",
      "stance": "committed | ruled_out | open | done | deferred | superseded | conditional | other",
      "importance": 0.9,
      "by": "who made or holds the decision",
      "at": "ISO-8601",
      "context": "what was happening when the stance was taken",
      "why": "reasoning, especially load-bearing for ruled_out",
      "alternatives": ["what was considered alongside"],
      "ties_to": {
        "topics": ["topic names this decision relates to"],
        "entities": ["entity names this decision relates to"],
        "decisions": ["other decisions this connects to or supersedes"]
      }
    }
  ],

  "key_quotes": [
    {
      "speaker": "raw speaker label from the transcript",
      "speaker_role": "human | agent",
      "at": "ISO-8601",
      "quote": "exact words",
      "context": "what was happening when they said it",
      "importance": 0.9
    }
  ],

  "transitions": {
    "from_prior": "how this scale of material connected to what came before",
    "into_next": "how this scale of material connected to what came after"
  },

  "annotations": [
    {
      "source": "webbing | vine | manual | audit | other",
      "at": "ISO-8601",
      "content": "annotation content"
    }
  ]
}
```

### 4.1 Schema principles

**Invariance.** Every field name and every field shape is identical at every layer of the pyramid and at every layer of vine composition above it. An L0 node and a monthly-vine-apex node use the same schema. What changes is the *scale* of the content in each field.

**Optionality.** Only `headline`, `time_range`, and `weight` are structurally required at every layer — they're needed for navigation and proportional synthesis. Everything else is optional. A node can have empty `decisions` or no `entities` or missing `transitions` without being invalid. This matters because:
  - Initial extraction at L0 produces what the LLM can reliably produce in one pass; some fields are genuinely absent in some chunks
  - Subsequent passes (webbing, annotation, vine composition) append to the node without blocking on fields that weren't filled initially
  - Future extractors, future prompts, and future models may produce richer data than current versions; older nodes stay valid

**Appendability.** Fields grow over time as new passes touch the node. Webbing adds topic and entity cross-links. Vine composition adds `ties_to` entries linking decisions across sessions. Manual annotation adds entries to the `annotations` field. Audit passes mark decisions with corrected stances. None of these require re-running the L0 extraction — they just append to the existing node.

**Importance as a sub-attribute.** `importance` is a 0.0–1.0 score attached to individual topics, decisions, and quotes — not a separate category. It's the weighting signal that lets upward synthesis prioritize what to preserve under compression pressure. A decision with `importance: 0.9` survives dehydration; a decision with `importance: 0.2` gets dropped first. The importance score is assigned by the LLM at extraction time based on its read of what's load-bearing in context, and can be revised upward by subsequent passes.

**Stance as a sub-attribute of decisions.** This is the reason the schema has no separate `active_commitments`, `ruled_out`, or `open_questions` fields. All of those are just decisions in different states. A committed decision has `stance: "committed"`. A rejected alternative has `stance: "ruled_out"` and a `why` field. An unresolved question has `stance: "open"`. A settled-and-shipped decision has `stance: "done"`. One field, one sub-attribute, N views — agents filter by stance to get whichever slice they need.

The stance vocabulary is open: any state that emerges from a conversation can be recorded without schema update. The recursive synthesis prompt preserves decisions by `importance` and `stance` relevance at the parent scale, not by enumerated category.

**`ties_to` as the cross-cutting navigation primitive.** This is what makes the pyramid queryable as memory rather than readable as a story. Every decision, topic, and entity carries pointers to other nodes' topics, entities, and decisions. The webbing passes compute these pointers at each layer, and the vine composition layers extend them across sessions. An agent query like *"everything we've ruled out regarding authentication across all sessions"* traverses these ties across any number of pyramids at any scale.

**Annotations as the append channel.** The `annotations` array is the escape hatch for any signal that doesn't fit elsewhere and any pass that wants to leave a trace without modifying the core fields. Webbing can annotate *"this node is strongly connected to session X, node Y."* A human can annotate *"this decision was reversed in a later session, see slug Z."* An audit pass can annotate *"the prior agent's commitment here was released by the human on date D."* Annotations never invalidate the core content; they just layer on top.

### 4.2 Why nothing is mandatory except headline, time_range, weight

The three mandatory fields are the minimum needed for the node to participate in the recursive pyramid:

- `headline` — so it can be referenced, listed, and recognized
- `time_range` — so it can be chronologically anchored and composed with temporal adjacency
- `weight` — so upward synthesis can allocate proportional attention and dehydration can compute budgets

Every other field is valuable-when-present but not necessary. The LLM at extraction time fills in what it can; subsequent passes fill in more. A node with only the three mandatory fields and a `narrative` is still a valid memory node — it just has less structured data to query against until later passes enrich it.

This matters for two reasons:

1. **Initial extraction cost.** Forcing the L0 pass to fill every field, regardless of whether the chunk content supports it, introduces hallucination pressure. A chunk with no decisions shouldn't force the model to invent them; an empty `decisions` array is the right answer. Optionality removes the incentive to fabricate.

2. **Extensibility over time.** The schema will grow. Future versions may add fields for emotional valence, speaker mood, agent uncertainty, cross-modal references (images, audio), or things we haven't thought of. Making everything optional from the start means old nodes remain valid when the schema grows, and new passes can add new fields to old nodes without rebuilding.

---

## Section 5 — The chain shape

Episodic mode uses the same chain scaffolding as retro mode (v2.6) with different prompts and a different upper-layer structure. The chain YAML would live at `chains/defaults/conversation-episodic.yaml` and diverges from `conversation-chronological.yaml` after Phase 1C.

### 5.1 Full chain phases

```
Phase 0:   load_prior_state           (cross_build_input, step_only)

Phase 1A:  forward_pass                (extract, sequential, accumulate running_context)
Phase 1B:  reverse_pass                (extract, sequential, for_each_reverse, accumulate running_context)
Phase 1C:  combine_l0                  (extract, sequential, zip_steps from forward/reverse,
                                        emits episodic schema, save_as: node)

Phase 2:   l0_webbing                  (web, cross-links L0 nodes by topics/entities/decisions)

Phase 3:   refresh_state               (cross_build_input, step_only, re-reads populated L0)

Phase 4:   chronological_decompose     (recursive_decompose with phase-detection prompt;
                                        cuts the session into natural phase sub-questions
                                        based on topic shifts, decision-state changes, pace
                                        changes, speaker dynamics)

Phase 5:   extraction_schema           (extract, step_only, defines episodic schema for sub-questions)

Phase 6:   evidence_loop               (grounds each phase's narrative in specific L0 nodes
                                        via KEEP/MISSING/DISCONNECT verdicts; produces L1
                                        segment nodes with citation provenance)

Phase 7:   gap_processing              (catches phases with insufficient grounding, re-examines)

Phase 8:   l1_webbing                  (cross-links L1 segment nodes)

Phase 9:   pair_adjacent L1→L2         (recursive synthesis, dehydration-aware)

Phase 10:  l2_webbing                  (cross-links L2 phase nodes)

Phase 11:  pair_adjacent L2→apex       (recursive synthesis, dehydration-aware)

Phase 12:  apex_webbing                (cross-links the apex with other session apexes if
                                        composed into a vine; no-op for a single-session build)
```

Phases 0 through 8 mirror the question pipeline with episodic prompts. Phase 9 and above use the recursive synthesis prompt for upward compression.

### 5.2 What's reused from v2.6

Forward pass and reverse pass prompts (`forward.md`, `reverse.md`) are reused without modification. The temporal-context accumulation they perform is valuable for any conversation chain, retro or episodic.

The token-aware chunker (`chunk_transcript_tokens` with `snap_to_line_boundaries` and tail merge) is reused without modification.

The `for_each_reverse` executor support and the `$chunks_reversed` resolver are reused without modification.

The chain executor primitives (`pair_adjacent`, `evidence_loop`, `process_gaps`, `web`, `recursive_decompose`) are reused without modification.

### 5.3 What's new for episodic

1. **`combine_l0.md` prompt** — emits the episodic schema instead of the question-pipeline L0 contract. Same inputs (forward/reverse pass outputs for the same chunk), different output shape.

2. **`chronological_decompose.md` prompt** — cuts the session into phases by content-driven boundary detection rather than thematic sub-question derivation.

3. **`synthesize_recursive.md` prompt** — the single recursive prompt used at L1→L2, L2→apex, and beyond. Replaces the retro chain's per-layer synthesis.

4. **Chain YAML `conversation-episodic.yaml`** — new file, diverges from `conversation-chronological.yaml` starting at Phase 1C (different combine prompt) and reshapes Phases 4 and above around the episodic structure.

5. **Dehydration configuration on upper-layer synthesis steps** — field-level cascade that drops low-priority schema fields under input budget pressure without truncating narrative prose.

### 5.4 Chronological decompose vs thematic decompose

The question pipeline's `recursive_decompose` primitive is general-purpose: it takes an apex question and produces a tree of sub-questions with grounded evidence requirements. In retro mode, the apex question is thematic (*"what are the key patterns and decisions in this session?"*) and decompose cuts into thematic sub-questions (*"what happened with authentication?"*, *"what happened with the audit findings?"*).

In episodic mode, the decompose prompt cuts into **chronological-plus-arc sub-questions** instead. The apex question is implicit: *"reconstruct this session as a navigable memory."* The decompose prompt identifies natural phase boundaries using four signals:

1. **Topic shift** — when the subject of discussion changes materially
2. **Decision-state change** — when a commitment closes one track and opens another, or a rejection kills an alternative that was driving the prior phase
3. **Pace change** — when the rhythm shifts between exploration, debate, execution, reflection
4. **Speaker-dynamic shift** — when who's driving the conversation changes, or when the human delivers a direction that reshapes the work

The output is N phase sub-questions of the form *"What happened during the [named phase] (approximately chunks X–Y)?"* where N is determined by the session's natural structure, not prescribed. A session with 3 distinct phases produces 3 sub-questions; a session with 12 distinct phases produces 12. Fan-out follows content.

Each phase sub-question is then answered by `evidence_loop` with an episodic-shaped answer prompt that produces a grounded L1 segment node with narrative, decisions, quotes, and citations to specific L0 chunks via KEEP verdicts.

This is the exact point where episodic memory fuses what the question pipeline learned (grounded synthesis with evidence verdicts and gap detection) with what chronological composition needs (time-based structure and natural fan-out). **The decompose primitive is general; only the prompt that drives it is episodic-specific.**

### 5.5 Webbing as the cross-cutting meta-navigation layer

Webbing runs at every layer (L0, L1, L2, apex) and computes cross-links between peer nodes at that depth. In episodic mode, webbing operates on the schema's topics, entities, and decisions to produce edges that say "this node is connected to that node because they share topic X" or "this decision supersedes that earlier decision" or "this entity appears in both nodes."

Webbing is what turns the chronological pyramid into a queryable memory graph. The chronological spine (pair_adjacent from L1 upward) gives the agent *"walk the session in order."* The webbing edges give the agent *"find every moment connected to this thing."* Both access patterns ride on the same stored data; the pyramid serves both natively without re-extraction.

For vine composition, webbing extends across sessions: the vine's own webbing pass cross-links L1 segments from session A with L1 segments from session B that share topics or entities or decisions. An agent query at the vine layer *"show me every time we touched authentication across all sessions"* traverses these cross-session web edges.

---

## Section 6 — The recursive synthesis prompt

This is the single most load-bearing prompt in the episodic chain. It runs at every layer above the base, at every vine layer above that, forever. The design quality of this one prompt determines whether the fractal architecture works or collapses into mechanical restatement.

### 6.1 The operation the prompt performs

Input: N peer memory nodes at some scale (2 adjacent L0s, or 2 adjacent L1s, or 2 adjacent session apexes in a vine, etc.)

Output: one parent memory node at one level of abstraction above the inputs, with the same schema shape, describing the joint material the inputs covered.

The prompt does not know what layer it's at. It infers the abstraction level from input content and produces output exactly one level outward.

### 6.2 The purpose block (agent-audience, apex-accurate)

Every prompt in this chain opens with a purpose block that tells the model what it's doing and who consumes the output. The synthesize_recursive prompt's block is the most carefully phrased because it has to be true at every layer including the current apex.

> *You are constructing one node of an **episodic memory pyramid** that serves as the persistent memory substrate for an AI agent operating across multiple sessions. The agent has no biological continuity between sessions — every new conversation starts from a blank state unless the pyramid makes prior work loadable. The pyramid is the agent's externalized brain: the thing it reads at the start of a new session to recover what it was doing, what it committed to, what it already ruled out, and what the human has directed. The human who was in the prior conversations has their own persistent memory and does not need this pyramid; the pyramid exists solely to give the agent continuity.*
>
> *The pyramid is fractal. At every layer, each node has the same structural shape, and each parent node describes its children at one higher level of abstraction. The same recursive operation builds every layer — from the base layer (one node per raw chunk of a single session) up through segment, phase, and session apex, and onward into multi-session composition (weeks of work, project arcs, the agent's full working history). Even the highest layers are built by running this same prompt on already-built pyramid apexes.*
>
> *Your job is to take a small number of peer memory nodes and produce a single parent node one level of abstraction above them. You do not need to know what absolute layer of the pyramid you're at — the level shift is always relative to your inputs. Look at what your inputs describe, and produce output that describes the same material one step further out.*
>
> *Your output may be composed upward at any future point — either later in this build, or by a subsequent vine-composition run that groups this node with peer nodes from other memory pyramids into a higher-scale memory. You cannot know from within this prompt whether you are at the top of the current build or somewhere in the middle. Write every output as if it will be composed upward next, because it might be. This means keeping enough concrete content that a future upward-synthesis pass has meaningful material to build from — never drop concrete content that the next zoom-out level would need to reference, even if the next level doesn't yet exist.*
>
> *Your output may also be consumed by a webbing pass that computes cross-links from your topics, entities, and decisions — either across peer nodes at your depth in this build, or across peer nodes at your depth in future vine-composed pyramids. Keep those structured fields faithful enough that cross-link computation is possible at any future point by a separate pass that reads only the structured fields, not the prose.*
>
> *And your output will be loaded by a future AI agent instance that has no exposure to any of the underlying material, to reconstruct working state. Optimize for that agent's ability to rapidly recover:*
>
> - *State and commitments must be unambiguous and action-primed — the successor must know what it agreed to, not what was discussed.*
> - *Rejected alternatives must be explicit with reasoning — the successor must never re-propose something the prior agent ruled out.*
> - *Human direction must preserve the human's exact words where they carry authority — the successor treats these as binding.*
> - *Prior-agent discoveries, rulings, and definitional claims must survive as exact quotes where they represent earned state — the successor treats them as priors to respect, not conclusions to re-derive.*
> - *The narrative prose at this layer encodes ordering and transition among the inputs, but it is instrumental, not literary — it serves reconstruction, not reading.*
>
> *You are serving three potential consumers simultaneously: a successor agent loading the pyramid as working memory, a future upward synthesis building the parent layer whenever that happens, and a webbing pass computing cross-links whenever that runs. All three are downstream of your work. A node that serves only one is a failure.*

### 6.3 The zoom-level instruction (replaces length caps)

Length is not prescribed. The operation is prescribed, and length falls out of content density:

> *Your output operates at exactly one level of abstraction above the inputs you see. Not two. Not the same level. Exactly one step outward.*
>
> *Look at what your inputs describe, and produce output that describes the same material one step further out. If the inputs describe chunk-level beats, your output describes the segment those beats form. If the inputs describe segment-level arcs, your output describes the phase those segments form. If the inputs describe session-level memoirs, your output describes the joint arc of those sessions. The shift is always one step — and the step is defined by the semantic level of your inputs, not by any layer number.*
>
> *You are not summarizing. You are abstracting. A summary compresses the same content at the same level; an abstraction describes what the content forms when you step back. A reader of your output should learn the shape of the whole, grounded in the specifics below but not reciting them.*
>
> *Length is whatever the content demands at the abstracted level. A chapter of the conversation that was mostly throat-clearing should produce a short output; a chapter packed with decisions and discoveries should produce a longer one. Do not pad short content to reach a target. Do not truncate dense content to fit a budget.*
>
> *Upper bound: your output's narrative must not exceed half the combined length of the input narratives. If you find yourself approaching that ceiling, you have probably restated the inputs rather than abstracted above them — step further outward and try again. The ceiling exists to catch degenerate cases, not to prescribe a target. Most outputs will be well below it.*

This instruction replaces every length-prescription approach. The 50% ceiling is a guardrail against mechanical restatement, not a target. Dense short chunks crystallize short. Sparse long chunks crystallize shorter. The operation determines length; no length is ever required.

### 6.4 Dehydration-aware input handling

The recursive prompt does not know whether its inputs arrived with full schemas or dehydrated schemas. When input pressure forces the upper-layer synthesis step to drop low-priority fields from its inputs, the prompt must handle gracefully:

> *Your inputs may have been dehydrated to fit within budget — some low-priority fields may be missing from input nodes. Specifically, any of these fields may be absent: `key_quotes`, `transitions`, `annotations`, parts of the `ties_to` subfield on decisions, and lower-importance topics or entities. If a field is missing, do not assume its absence means the underlying material had no such content — it may have been dropped under compression. Work with what you're given and preserve what matters at the parent scale.*
>
> *The fields guaranteed to be present on every input node are: `headline`, `time_range`, `weight`, `narrative`, `decisions` (possibly dehydrated to high-importance-only), and `topics` (possibly dehydrated to high-importance-only). Build your parent node primarily from these. Use the optional fields when they're present; do not demand them.*

Field dehydration priority, from first-dropped to never-dropped:

1. `annotations` (always derivable from other passes)
2. `transitions.from_prior` and `transitions.into_next` (can be re-derived by adjacent-layer prompt)
3. `key_quotes` with `importance < 0.5`
4. `entities` with low `importance`
5. `topics` with low `importance`
6. `decisions` with `stance: "done"` and `importance < 0.7`
7. `decisions` with `stance: "deferred"` and `importance < 0.5`
8. `ties_to` sub-fields on decisions
9. **Never dropped:** `headline`, `time_range`, `weight`, `narrative`, any `decisions` with `stance: "committed"` or `stance: "ruled_out"` or `importance >= 0.7`

This priority order reflects what the successor agent needs most: committed and ruled-out decisions are the load-bearing binding state; narrative is the instrumental reading; high-importance topics and quotes are the cross-link anchors.

---

## Section 7 — The six reading modes (substrate unlocks all at V1)

The same stored episodic memory pyramid can be queried and rendered in six distinct ways. All six fall out of the same substrate — no additional extraction cost, only UI and query plumbing. V1 of the product ships all six because the substrate is free once built.

### 7.1 Mode 1: Memoir

Read the apex top-to-bottom. Dense prose at the whole-session scale. The default rendering. The 2-minute recovery for an agent loading context from the most recent session.

### 7.2 Mode 2: Walk

Scroll through L1 or L2 nodes chronologically. Each node is self-contained and meaningful standalone. Like reading a book at chapter-summary level. The agent picking up a session in the middle, wanting more detail than the apex but not every chunk.

### 7.3 Mode 3: Thread

Pick a topic, entity, or decision identifier, and follow its web edges across non-adjacent nodes in chronological order. Produces a topic-sliced narrative: *"here's everything about authentication in chronological order across this entire session (or week, or project)."* The agent answering *"when did we first start thinking about X"* or *"what's the history of Y in this project."*

### 7.4 Mode 4: Decisions ledger

Collapse the narrative entirely and render just the `decisions[]` arrays in chronological order, optionally filtered by stance. *"Show me everything committed."* *"Show me everything ruled out, with reasoning."* *"Show me every open question across all sessions."* The agent confirming its binding state before proposing new work.

### 7.5 Mode 5: Speaker

Filter to one speaker's contributions. *"Show me every human quote in chronological order, with context."* *"Show me every time the prior agent made a ruling."* The agent reviewing what the human has directed, or the human reviewing what the agent committed to.

### 7.6 Mode 6: Search

Full-text search over the raw `pyramid_chunks` table (an FTS5 index over the preserved raw transcript), with hits that drill into the owning L0 node, L1 segment, L2 phase, and apex. *"Find every occurrence of 'OTP' and show me the surrounding narrative at each level."* The agent doing exact-term retrieval when paraphrase isn't enough.

### 7.7 Why all six are V1

The storage is already there: `pyramid_nodes` + `pyramid_web_edges` + `pyramid_chunks`. The schema fields I've specified support all six modes natively: `topics` and `entities` and `decisions` and `key_quotes.speaker_role` and `decisions[].stance` are the filter and join keys for modes 3, 4, and 5. The raw chunks already contain the text that mode 6 searches. Modes 1 and 2 fall out of the pyramid structure itself.

The only V1 work beyond extraction is UI and query plumbing — rendering the modes, exposing filters, wiring the drill-down. No additional LLM calls, no re-extraction, no new chain phases. **Building the substrate right once enables all six modes for free forever.**

---

## Section 8 — Vine composition: memory that grows beyond a session

The recursive synthesis prompt runs identically at every layer, including at vine layers above any single session. This is what makes the fractal architecture extend upward indefinitely.

### 8.1 Vine bunches as recursive input

A vine bunch in the existing architecture composes multiple conversation slugs into one bunch pyramid. For episodic memory, the vine bunch takes the apex nodes of N conversation memory pyramids as its L0 inputs, runs `pair_adjacent` and `synthesize_recursive` upward, and produces a vine-level apex that describes the joint memory across all N sessions.

The recursive prompt doesn't know it's operating on session-apex inputs rather than chunk-level inputs. It sees memory-schema peer nodes and produces a parent at one level of abstraction above. For the vine, "one level above session apexes" means "multi-session arc" — exactly what the vine bunch is for.

### 8.2 Indefinite upward composition

Because the operation is recursive and the schema is invariant, vine composition extends without bound:

- **Session pyramid** — one conversation's memory (L0 chunks → L1 segments → L2 phases → session apex)
- **Daily vine** — N sessions from one day composed into a day apex
- **Weekly vine** — N days composed into a week apex
- **Project vine** — N weeks composed into a project apex
- **Career vine** — N projects composed into the agent's full working history

At every level, the same prompt. At every level, the same schema. At every level, an agent can load any node and get the right zoom level for its current cognitive need. The agent loading the "last month" vine apex sees a dense compressed narrative of the month's work with binding commitments, ruled-out alternatives, key discoveries, and enough detail to drill down into specific sessions or specific moments when needed.

### 8.3 Cross-session webbing

Vine webbing cross-links nodes across session boundaries. A decision in session A with stance `open` can be tied by web edge to a decision in session C with stance `committed` — the vine webbing computes this tie because the decisions share identity in `ties_to.decisions`. An agent querying *"is this still open?"* at the vine layer can see that the decision was resolved two sessions later.

This turns the memory substrate into a living graph that tracks decision evolution across arbitrary time spans. The agent can query *"what commitments from Q1 are still binding in Q4"* or *"what did we rule out that we've since reconsidered"* or *"which open questions have been sitting unresolved the longest."* These queries are all free once the webbing passes run at each vine layer — they're just graph traversals over stored edges.

### 8.4 Bootstrap loading

A new agent session can load memory at whichever scale matches its need:

- Just the last session's apex for minimal context
- The last week's vine apex for recent work context
- The project vine apex for the full project arc
- A specific L0 node for exact detail on a specific moment
- A thread-mode traversal starting from a specific topic for targeted history

All of this is the same substrate, loaded at different zoom levels or traversed along different axes. The agent picks the loading strategy based on what the current session's work requires, and the memory substrate serves each strategy from the same stored data.

---

## Section 9 — The synthesis between v2.6 retro and episodic

Both products ship. Both use the same underlying chain machinery. The differences are:

| Aspect | Retro (v2.6) | Episodic (this doc) |
|---|---|---|
| Chain id | `conversation-chronological` | `conversation-episodic` |
| Primary reader | Human, for meta-learning | Successor AI agent, for continuity |
| Apex shape | Thesis about patterns | Memory-schema node with recursive composability |
| L0 contract | Question pipeline contract (`headline`, `orientation`, `topics[]`) | Episodic schema (this doc, Section 4) |
| L0 prompt | `combine.md` (v2.6, emits question contract) | `combine_l0.md` (new, emits episodic schema) |
| Decompose | Thematic sub-questions | Chronological phase sub-questions |
| Upper layers | Evidence-grounded thematic synthesis | Recursive zoom-one-above via single prompt |
| Composable into vines? | No (terminal thesis) | Yes (base case of indefinite recursion) |
| Default for conversation slugs? | Yes, as of v2.6 post-ship | Switched to episodic when episodic ships |

Retro remains available as a preset for users who want the meta-learning product. Episodic becomes the new default because the agent-continuity use case is higher-value for the primary reader and because episodic nodes can also serve human reading through the six modes.

The wizard dropdown evolves from the current single option (`Chronological (forward + reverse + combine)` mapping to retro) to a preset selector:

- **Episodic Memory** *(default)* — the agent's persistent memory for working continuity across sessions; supports all six reading modes
- **Retro / Meta-Learning** — thesis extraction for pattern analysis and skill refinement
- *(later)* **Decisions Log** — ledger view for audit and accountability

Both retro and episodic can be run on the same ingested chunks if both products are wanted — the chunker cost is paid once, the synthesis cost doubles. The user picks one at build time and can re-run with the other chain later if needed.

---

## Section 10 — Prompt catalogue

The five prompts that together produce episodic memory:

### 10.1 `forward.md` *(reused from v2.6)*

Temporal-forward extraction of a single chunk with rolling running_context accumulation. Walks chunks earliest → latest. Each chunk's LLM call receives the accumulated running_context from earlier chunks and rewrites it to fold in this chunk's content. No changes from v2.6 version.

### 10.2 `reverse.md` *(reused from v2.6)*

Temporal-backward extraction of a single chunk with rolling running_context accumulation. Walks chunks latest → earliest. Mirror of forward.md but with hindsight framing. No changes from v2.6 version.

### 10.3 `combine_l0.md` *(new for episodic)*

Reconciles forward and reverse intermediate views into the base-layer episodic memory schema. Opens with the agent-audience purpose block. Task: fuse the two temporal readings into one consolidated L0 node with:

- `narrative` at chunk-level abstraction
- `decisions[]` with stance, importance, ties_to (populated where extractable)
- `key_quotes[]` with human-authoritative and agent-earned-state preservation rules
- `topics[]` and `entities[]` with importance scores for webbing to use
- `transitions` linking to prior and next chunks
- All optional fields populated where the chunk content supports them; empty arrays where it doesn't

Explicit instructions on quote asymmetry, the 50% ceiling guardrail, the speaker_role tagging (`human` vs `agent`), and the importance scoring heuristic.

### 10.4 `chronological_decompose.md` *(new for episodic)*

Phase-detection prompt that cuts the session into natural phases for downstream grounding. Opens with the agent-audience purpose block. Task: identify phase boundaries using the four signals (topic shift, decision-state change, pace change, speaker-dynamic shift) and produce N sub-questions of the form *"What happened during [named phase] (approximately chunks X–Y)?"* where N is determined by the session's natural structure, not prescribed. Output consumed by `evidence_loop` for grounding.

### 10.5 `synthesize_recursive.md` *(new, the load-bearing prompt)*

The single recursive synthesis prompt used at L1→L2, L2→apex, and at every vine layer above forever. Full purpose block from Section 6.2 above. Task: take N peer memory nodes at any scale, produce one parent node at one level of abstraction above. Zoom-one-level instruction. Dehydration-aware input handling. 50% ceiling guardrail. Dual-consumer framing with all consumers stated as potential not certain.

No layer-specific language. No absolute depth references. Runs at every level from L1 to the top of any vine composition, unchanged.

---

## Section 11 — Schema extensibility and the optionality principle

This section exists to protect the schema from a specific failure mode: forcing extraction to fill fields the content doesn't support. That failure mode produces hallucinations, inflates token cost, and makes the extraction brittle when the model is uncertain.

### 11.1 Three fields are required, everything else is optional

Required at every layer: `headline`, `time_range`, `weight`. These are the minimum needed for the node to participate in the recursive pyramid.

Everything else — `narrative`, `topics`, `entities`, `decisions`, `key_quotes`, `transitions`, `annotations` — is optional. A valid L0 node might be as minimal as:

```json
{
  "headline": "brief acknowledgment exchange",
  "time_range": {"start": "2026-04-07T03:42:11", "end": "2026-04-07T03:42:47"},
  "weight": {"tokens": 120, "turns": 2, "fraction_of_parent": 0.003}
}
```

That's a valid memory node for a chunk that was literally just *"thanks"* / *"you're welcome."* Forcing the extractor to fill in decisions, topics, and quotes for a chunk with no substance produces noise, not signal. **Empty is the right answer when content is empty.**

### 11.2 Appendability: fields grow over the node's lifetime

The schema is designed so that subsequent passes can append to a node without invalidating it or requiring re-extraction:

- **Webbing passes** add entries to `ties_to` on decisions, add topic cross-links, populate entity relationships
- **Vine composition passes** add cross-session ties (linking this decision to a related decision in another session)
- **Audit passes** append to `annotations` with audit findings, correct stances on decisions that evolved, mark superseded content
- **Manual agent edits** append to `annotations` or revise `importance` scores based on what the agent learned later
- **Future extraction passes with better models** can re-run on the same raw chunks and produce richer output that merges into the existing node

None of these pass types require the original extraction to have filled every field. The node accumulates signal over time, layer by layer, pass by pass.

### 11.3 Schema forward-compatibility

Future versions of the schema may add fields for:

- Emotional valence on quotes and decisions
- Uncertainty scores on agent claims
- Cross-modal references (images, audio, screen captures)
- Confidence intervals on time ranges
- Alternative narratives for ambiguous content
- Multi-agent attribution for sessions with more than one AI participant

Old nodes remain valid when these fields are added — they just don't have the new fields populated, and subsequent passes can fill them in or not. The optionality-first principle means schema evolution is additive and never requires data migration.

### 11.4 Importance as the universal priority signal

The `importance` field appears on topics, decisions, and key_quotes. It's a 0.0–1.0 score assigned by the LLM at extraction time based on its read of what's load-bearing in context. It's the single signal that drives:

- **Dehydration priority** — low-importance items are dropped first under input budget pressure
- **Upward synthesis compression** — the recursive prompt preserves high-importance items at the parent scale, drops low-importance items
- **Query ranking** — when an agent searches memory, importance weighting ranks results
- **Annotation priority** — webbing passes focus edge computation on high-importance anchors

Importance is revisable. Later passes can revise importance scores upward when they see that an item mattered more than initial extraction judged — a decision that seemed routine at the time but became load-bearing three sessions later gets its importance revised up by the vine composition pass.

### 11.5 The principle, stated generally

**The schema exists to capture whatever signal is available at extraction time, with everything optional and appendable so subsequent passes can enrich without blocking.** There is no "incomplete" node. A node with the three required fields and an empty narrative is valid. A node with every field populated is valid. A node that starts minimal and grows rich over time through successive passes is the expected lifecycle, not an edge case.

---

## Section 12 — Runtime integration: the pyramid as the agent's active working brain

Sections 1–11 cover how the pyramid is *built*: the chunker, the chain, the synthesis, the schema, the recursive composition. This section covers how the pyramid is *consumed by a live agent* — the runtime half of episodic memory, where stored nodes become the agent's working memory during active cognition.

This runtime architecture comes from prior work on the Partner/Dennis agent. It's load-bearing context for understanding why the pipeline is shaped the way it is: the pipeline produces the cold storage substrate that the runtime operates against, and every design decision in the earlier sections is shaped by both consumption modes.

### 12.1 The three memory containers

The agent's cognition is divided into three tiers, each with different size, volatility, and purpose:

| Container | Size | Volatility | What lives here |
|---|---|---|---|
| **Conversation Buffer** | ~20K tokens (~60–80 exchanges) | High — every turn mutates it | Pure dialogue only. Never tool results, never synthesis, never metadata. The live back-and-forth between the human and the agent. |
| **Brain Map** | ~2–3K navigation skeleton + 3K–100K+ hydrated content | Medium — mutates between turns via hydrate/dehydrate operations | Navigation skeleton (pyramid apex, L2 thread summaries, topic index) plus variable hydrated node bodies. Tool results from the current turn get moved here between turns, never left in the buffer. |
| **Pyramid (cold storage)** | Unlimited (SQLite on disk, potentially distributed) | Low — only mutates when a build runs, an annotation is written, or a delta is applied | All raw chunks, all memory nodes at every layer, all web edges, all annotations, all delta chains. The full substrate. |

The critical design constraint: **the Conversation Buffer is sacred.** Tool results, synthesized findings, metadata, and prior-session state never accumulate in the buffer — they get moved to the Brain Map between turns. This keeps the cache-stable buffer clean and ensures the 20K budget is entirely devoted to actual dialogue. An agent with a 20K conversation buffer can sustain hundreds of exchanges in a single session because its working memory scaffolding lives elsewhere, and the scaffolding is drawn from an unbounded pyramid without bounding the session's cognitive horizon.

### 12.2 Cache breakpoint strategy

The context window is laid out with deliberate cache breakpoints so the stable sections get cached by the model provider and the volatile sections pay full cost only when they actually change:

| Section | Stability | Cache impact |
|---|---|---|
| §1 System prompt | Stable across the session | Always cached |
| §2 Navigation skeleton (pyramid apex, L2 thread summaries, topic index) | Stable within a session, ~24K tokens | Cached; measured ~64% cost reduction vs paying full price per turn |
| §3 Brain Map hydrated nodes | Stable between turns unless `context_schedule` mutates | Cached most turns, full-cost only on mutation |
| §4 Conversation Buffer prefix | Monotonically growing, reuses prefix | Partially cached via prefix matching |
| §5–7 Variable sections (tool calls, current turn response, manifest output) | Volatile | Full-cost every turn |

The pyramid design matters here because the navigation skeleton (§2) is drawn from the upper layers of the pyramid — apex headlines, L2 topic summaries, web edge topology, topic/entity indices. If those stay stable within a session (which they do unless a build runs mid-session), §2 stays cached across every turn. The episodic memory schema's invariant shape at every layer is what makes the navigation skeleton compact and cacheable: the same field set at every depth means the skeleton can be assembled from whichever layers are relevant without shape-shifting, and re-assembled turn-by-turn without cache invalidation because the underlying nodes haven't changed.

This is one of the reasons schema invariance across layers (Section 3, Corollary 1) is load-bearing and not just aesthetic: it enables stable, cacheable navigation skeletons that make Mode B economically viable.

### 12.3 The context manifest protocol — context as post-model output

The central inversion: **context isn't a pre-model input that the harness assembles; it's a post-model output that the agent itself drives.**

Each agent turn emits a structured JSON manifest (invisible to the human) as part of its response. The manifest specifies what to do with the Brain Map before the next turn. The runtime harness executes the operations between turns, updates the Brain Map, and the next turn's context window reflects the new state. The agent is steering its own cognition.

The manifest supports these operations:

| Operation | What it does |
|---|---|
| `hydrate` | Pull the full body of a named node from the Pyramid into the Brain Map |
| `dehydrate` | Drop a node in the Brain Map back to metadata-only (headline, time_range, weight, topic tags, one-line narrative snippet), freeing tokens |
| `compress` | Replace a stretch of raw dialogue turns in the Conversation Buffer with a synthesis node that moves to the Brain Map (when the buffer approaches overflow) |
| `densify` | Request creation of a missing mid-level synthesis node (e.g., an L1 phase node that doesn't exist yet). Queued to an async helper model, not executed in the current turn. |
| `colocate` | Pull in network contributions — nodes from other slugs, shared pyramids from other operators, or vine-composed meta-memories related to the current work |
| `lookahead` | Speculative pre-staging of nodes the agent anticipates needing next turn, hedged against the cost of hydrating if it turns out not to be needed |
| `investigation` | Flag a node as possibly stale and request a verification pass by an async helper model (see Section 12.9) |

Each manifest is stored with its emitting turn in a provenance trail. Every manifest → turn pair is auditable, metric-able, and recoverable. Metrics include lookahead accuracy (did the pre-staged nodes actually get used?), compression timing (when does the agent choose to compress?), hydration patterns (which nodes get hydrated repeatedly, suggesting they should graduate into the stable navigation skeleton?), and density coverage (how many mid-level nodes are absent and had to be densified on demand?).

The context budget (e.g., 8K hydrated tokens, configurable) governs how many nodes can be live at once. The partner decides when to compress or dehydrate based on intelligence and what's currently relevant, not a fixed threshold. This is important: **the agent's own judgment about what matters right now is the eviction policy**, not a LRU cache or a size trigger. The agent reasons about its working set the way a human reasons about keeping papers on a desk.

### 12.4 Why the episodic schema supports runtime operations natively

Every design decision in Sections 3–11 was shaped by both consumption modes. Specifically:

**Optional fields enable dehydration as a cheap runtime operation.** A dehydrated node in the Brain Map is just a node with `narrative`, `key_quotes`, `decisions`, and most other fields dropped — leaving only `headline`, `time_range`, `weight`, `topics` (high-importance only), and a one-line snippet derived from the narrative. Because every field except the three required ones is optional, a dehydrated node is still a valid node, still queryable by the recursive synthesis prompt, still composable upward. Dehydration doesn't destroy information — the original lives in the Pyramid and can be re-hydrated on demand. The dehydrated form is just the runtime cold representation. **No special schema, no migration, no synchronization burden**: it's the same node with the cascade applied.

**`importance` drives runtime eviction.** When the Brain Map hits the context budget and the agent needs to dehydrate something, `importance` scores on topics, decisions, and quotes guide the eviction choice. Low-importance items (stale, resolved, peripheral) get evicted first; high-importance items (committed decisions, open questions, active work) get preserved. The same importance signal that drives pipeline compression drives runtime eviction. One concept, two use cases.

**`ties_to` enables runtime colocation.** When the agent hydrates a specific node, the `ties_to` sub-fields on its decisions, topics, and entities point to related nodes elsewhere in the pyramid. A smart hydration pass can speculatively pull in the first-hop related nodes along with the target — giving the agent adjacent context for free, without requiring a second turn to realize it needs more. This is the runtime application of webbing: the web edges exist for both offline cross-navigation queries and runtime colocation. The same edges serve both.

**Recursive fractal structure enables multi-resolution loading.** The agent can load the apex for orientation, a specific L2 phase for medium detail, or a specific L0 chunk for exact detail — all from the same pyramid, all with the same schema shape, all composable with whatever else is in the Brain Map. The agent picks the right zoom level for the work at hand and doesn't pay for detail it doesn't need. For a long conversation, the agent might load just the apex and one L1 phase; for a targeted drill, it might load one specific L0 plus its colocated neighbors.

**Annotations as an append channel for in-session state updates.** When the agent discovers something mid-session that should be preserved for the next session, it appends to the `annotations` field on the relevant node. The annotation is a cheap, non-destructive update that doesn't require re-extraction. Subsequent builds and the webbing pass integrate the annotations into the pyramid's canonical structure over time. This is how insights from a live session make it back into the cold storage without requiring expensive rebuilds.

**The recursive synthesis prompt runs at runtime too, not just at build time.** When a manifest issues a `densify` operation, an async helper model runs the same `synthesize_recursive.md` prompt on the target's children to produce the missing node. Same prompt, same schema, same operation — just triggered on demand at runtime rather than swept through during a full build. This is why the recursive prompt is "truly recursive": it runs at build time (offline, full pyramid), at runtime-densify time (online, producing a single missing node on demand), and at delta-update time (applying changes to existing nodes). One prompt, three timing modes, all the same operation.

### 12.5 Topic threads and the lobby model (v2)

Partner v2 introduces a **lobby + topic-thread** model on top of the three-container base. Instead of one conversation buffer for the whole session, the agent maintains multiple parallel topic threads:

- **Lobby** — top-level navigation and routing. Holds the pyramid apex, L2 thread summaries, the topic index, and the routing logic that decides which topic thread a new human utterance belongs to (or whether it starts a new thread).
- **Topic threads** — each topic thread has its own 20K token conversation buffer, its own dedicated pyramid slug, its own session-topic summaries, its own running memory. When the human's attention shifts to a different topic, the agent routes them to the corresponding topic thread and the active buffer switches.

The implication for episodic memory: **a single work session can produce multiple episodic memory pyramids, one per topic thread.** The "session" boundary is a lobby event (when the human first engages the agent); the "memory unit" boundary is a topic thread (coherent work on one subject). A vine bunch at the day level composes the day's topic threads together, not a single monolithic session memory. Cross-thread vine composition links related threads across the lobby.

This means the schema should carry an optional `thread_id` field on every node so cross-thread queries and vine composition can link work across threads. Adding `thread_id` to the schema is additive and backward-compatible — existing nodes without it are still valid (they belong to an implicit "default" thread). For the V1 single-thread build, `thread_id` is optional and unused; when topic threads ship, it becomes load-bearing.

**Progressive crystallization** is a v2 primitive: updating L2+ threads without a full reverse pass. Instead of re-running the entire forward/reverse/combine → decompose → evidence_loop → pair_adjacent chain when a thread grows, the system incrementally updates the higher layers by applying deltas to the affected nodes. This maps directly to the Delta-Chain architecture in Section 12.8.

**Avatar states** give the human a visible cognitive state for the partner: `idle → listening → thinking → crystallizing → searching → speaking`. Not directly relevant to the memory schema, but useful context for why certain operations happen. A `crystallizing` state corresponds to the agent running post-turn synthesis work (densify, compress, collapse); a `searching` state corresponds to pyramid_query fanouts. The human sees the agent thinking and can wait or interrupt as appropriate.

### 12.6 CLI interface as the pyramid query surface

The agent interacts with the pyramid through a stable CLI command interface, not by reading database tables directly. The CLI abstracts the storage and exposes the query operations an agent actually needs. This separation is the stable contract between pipeline and runtime: the pipeline produces the substrate, the CLI exposes it, the runtime consumes through it.

| Command | Returns | Primary use |
|---|---|---|
| `handoff <slug>` | Complete onboarding payload for a slug (the "cold start package") | Mode A bootstrap: a new session loads `handoff` to reconstruct working state |
| `apex <slug>` | Top-level session apex only | Lightweight orientation when full handoff is too expensive; Mode B navigation skeleton anchor |
| `tree <slug>` | Full topology visualization (nodes at which depths, web edge counts, layer structure) | Structural inspection, debugging, navigation planning |
| `search <slug> <query>` | Semantic search with Tier 1.2 hint fallback | Primary retrieval mechanism for Mode B on-demand hydration |
| `drill <slug> <node_id>` | Deep dive into a specific node with full content + colocated related nodes | Hydration on demand during active work; the "show me everything about this moment" primitive |
| `annotate <slug> <node_id>` | Append an annotation to a node | Mid-session insight preservation, FAQ contribution, correction |
| `diff <slug>` | Build change report (what changed since the last build) | Staleness detection, delta understanding, audit |
| `dadbear <slug>` | Live build status, DADBEAR auto-update metadata | Freshness awareness |
| `help` | CLI dictionary as JSON (auto-discovery) | Self-documenting surface for autonomous agents |

As of the CLI V2 friction log audit, all Tier 1–5 friction points from V1 have been resolved. Open items: scalability under very large pyramids, FAQ coverage breadth. These are operational concerns, not blockers for episodic memory.

### 12.7 Folio HTTP route

Complementary to the CLI, the Folio is an HTTP route that dumps a depth-controlled context package for a slug in a single call:

```
GET /p/{slug}/folio            → Full Folio document
GET /p/{slug}/folio?depth=2    → Folio limited to depth 2
```

The Folio is "hydration on demand" exposed as an HTTP endpoint rather than a CLI operation. It's useful for remote agents without direct CLI access, for browser-based clients, for cross-operator network sharing, and for any case where a single HTTP round-trip is preferable to multiple CLI invocations. The `depth` parameter controls how much of the pyramid gets dumped — `depth=apex` returns just the top-level summary, `depth=2` returns the navigation skeleton plus L0/L1/L2 content, no depth parameter returns the full Folio.

Status: listed in the web surface architecture, not yet implemented (P3 — Minor/Cosmetic in the audit). Implementing Folio is a follow-up once the episodic chain is stable and the runtime integration needs it. It's a thin wrapper over the CLI commands plus HTTP framing, so the implementation cost is low when it's time.

### 12.8 Delta-chain + collapse: the Recursive Knowledge Pyramid architecture

The founding motivation for this whole architecture was solving **AI partner amnesia**: bounded context windows facing unbounded information. The Recursive Delta-Chain pattern was chosen specifically to achieve **O(1) updates** and **logarithmic cost scaling**, making persistent memory economically viable rather than merely technically possible.

The architecture divides the pyramid into two zones:

**Bedrock (L0/L1)** — immutable. Raw chunks and base-layer extractions. Once written, never modified. New ingest appends; it doesn't mutate. This is the ground truth; everything above can be re-derived from it. The forward/reverse/combine L0 step (Sections 5.2, 10.3) produces bedrock nodes. These are the anchors that every higher layer ultimately cites.

**Understanding (L2+)** — mutable via delta chains. Higher-layer synthesis nodes evolve over time as new information arrives. Each update is a *delta* — a small change to the existing node — not a full re-synthesis. Periodically, a delta chain is *collapsed* into a new canonical version of the node that supersedes the prior version. The superseded version is kept in history for audit. The L1 segment, L2 phase, and apex nodes produced by the recursive synthesis prompt live in the Understanding zone and are subject to delta-chain maintenance.

Key properties:

- **Updates are O(1) per change**, not full re-processing. Adding a new chunk to a continuing conversation slug doesn't rebuild the entire pyramid — it applies a delta to the affected L2+ nodes.
- **Vertical evolution (delta chains) is separate from horizontal connections (webbing).** Web edges have their own delta chains. A new web edge can be added without touching the nodes it connects. This separation is what makes O(1) updates tractable: each axis of change is localized.
- **Logarithmic cost scaling.** As the corpus grows, the synthesis cost grows with the log of the corpus size, not linearly. This is what makes the pyramid viable for long-running agents that accumulate thousands of sessions of memory. Without delta-chain + collapse, each new session would trigger full resynthesis of every affected ancestor — quadratic cost that would kill the economics.

Agent implication: when the agent hydrates a node, it gets the current canonical version plus any pending deltas that haven't been collapsed. The agent can see `"this node is freshly collapsed"` vs `"this node has 3 pending deltas since last collapse"` and choose whether to treat the current state as authoritative or to wait for a collapse pass. The manifest can request `collapse` as an operation when the agent wants a fresh canonical version before making a decision.

The recursive synthesis prompt supports delta mode via the same instruction framing: instead of producing a new parent node from N children, it produces a delta-update to an existing parent node given one new child or one changed child. Same prompt, same schema output, just applied incrementally. The prompt's zoom-level instruction still holds in delta mode — the delta describes how the parent's scale of abstraction changes in response to the child's change, rather than producing the parent from scratch.

### 12.9 Staleness awareness and investigation operations

A key responsibility of the runtime integration is detecting when memory nodes are stale and triggering investigations to refresh them. The partner emits `investigation` manifest operations when:

- A node's content conflicts with newer information the agent has just learned in the current turn
- A decision's `stance` might have changed (e.g., something previously `committed` might now need to be `ruled_out` based on new findings, or an `open` question might now be answered)
- A web edge might be outdated (a node it pointed to has been superseded)
- A topic's `importance` might need revision (something peripheral has become load-bearing, or vice versa)
- A timestamp or `at` field appears incorrect relative to surrounding context

Investigation operations are queued to async helper models that re-examine the target nodes, run verification queries against source chunks or cross-referenced slugs, and produce updates that flow back into the pyramid as deltas. This is the pyramid's self-maintenance loop — the agent flags suspected staleness during live work, helpers verify asynchronously, and the canonical version converges toward current truth over time without blocking the live session.

The `annotations` field records every investigation's outcome so the next session can see "this node was investigated on date D by helper X; verdict was confirmed/revised/superseded." Even ignored investigations leave an annotation trail so the decision not to update is itself auditable.

### 12.10 Async helper coordination and the densify pattern

Not all synthesis needs to happen in the live session. The manifest operations `densify`, long-running `colocate`, `investigation`, and `collapse` are queued to async helper models that run in the background and write their outputs back into the pyramid via deltas. The live agent doesn't wait for these — it continues the current turn while helpers work, and the results become available in subsequent turns.

This pattern matters because it lets the live agent make smart hydration decisions without paying full synthesis cost on every turn. If the agent realizes a phase synthesis is missing (the pyramid has L0 nodes covering minutes 14:00–14:45 of the session but no L1 segment node for that range because the last build ran before those chunks were ingested), it emits a `densify` operation for that phase and continues working. A helper model picks up the densify job, runs the recursive synthesis prompt to produce the missing L1 node, and writes it back to the pyramid as a delta. The live agent queries again next turn and finds the newly-densified node available.

This is why the runtime architecture needs the recursive synthesis prompt to be genuinely runtime-callable, not just a batch pipeline step. The same prompt file is invoked by:

1. The full-build pipeline (offline, sweeps every layer)
2. Runtime densify helpers (online but async, produces one node on demand)
3. Runtime delta-update helpers (online async, produces incremental updates to existing nodes)
4. Eventual collapse passes (offline or scheduled, produces fresh canonical versions from delta chains)

All four code paths invoke the same prompt with the same schema shape. The prompt's purpose block (Section 6.2) is written to be accurate regardless of which timing mode is calling it — it never references "a build is running" or "this is the final version" or any other mode-specific assertion.

### 12.11 How runtime consumption shaped every pipeline decision

The two consumption modes aren't two products stitched together; they're one architecture designed for both from the start. To make the connection explicit:

- **Schema invariance across layers** (Section 3.1, Corollary 1) enables cache-stable navigation skeletons (12.2) and runtime multi-resolution loading (12.4). It's not aesthetic — it's load-bearing for runtime economics.
- **Optional and appendable fields** (Section 4.1, Section 11) enable runtime hydration/dehydration with no special cold representation, and annotation-based in-session state updates (12.4).
- **Importance scores** (Section 4.1, Section 11.4) drive both offline compression priority and runtime eviction priority (12.4). One concept, two use cases.
- **`ties_to` and webbing** (Sections 4, 5.5) enable both offline cross-navigation queries (Section 7.3 Thread mode) and runtime colocation during hydration (12.4).
- **Recursive fractal structure** (Section 3) enables both offline indefinite upward composition (Section 8) and runtime multi-resolution loading (12.4).
- **Agent-audience framing with apex-accurate purpose block** (Section 6.2) applies to both the cold-start successor reading the pyramid at session boot AND the live agent reading the pyramid during active work. The phrasing about upward consumption being potential-not-certain is exactly what makes the prompt work at runtime-densify time, not just at build time.
- **Delta-chain + collapse** (12.8) enables O(1) updates so the runtime can afford to continuously refine the pyramid without triggering expensive rebuilds.
- **Dehydration cascade with field priority order** (Section 6.4) doubles as the runtime cold-representation cascade — same priorities, same behavior, different trigger timing.

The pipeline produces the substrate; the runtime operates on it; they share the same schema, the same prompt, the same chain of reasoning. **Episodic memory is one product with two consumption modes**, and both modes are designed-in from the start.

---

## Section 13 — What's not in this doc

Deliberately out of scope for the canonical design:

### Pipeline side

- **Implementation sequencing** — the order of builds, which phases land first, which tests to run. This lives in a separate implementation plan when episodic mode enters active development.
- **Specific prompt text** — the three new prompts (`combine_l0`, `chronological_decompose`, `synthesize_recursive`, plus the phase-answering prompts for `evidence_loop`) will be drafted separately against this design doc as the reference. This doc specifies their shape, purpose, and downstream obligations, not their exact wording.
- **UI details for the six reading modes** — mode 1 (memoir) and mode 3 (thread) are the V1 priorities; the rest are substrate-ready but need UI plumbing. UI design lives in a separate frontend plan.
- **Vine composition chain YAML** — the vine bunch chain that composes session apexes into higher-level memory will be its own chain YAML, built when the single-session episodic chain is stable.
- **Migration from retro pyramids** — whether existing retro-built conversation slugs can be upgraded to episodic in place, or must be rebuilt from raw chunks. Decision deferred until episodic chain is stable and the cost-benefit of in-place migration can be measured.

### Runtime side

Section 12 establishes the runtime architecture (three containers, manifest protocol, delta-chain + collapse, topic threads, CLI/HTTP interface). The following implementation details are deliberately out of scope for this design doc and live in runtime-specific plans:

- **Manifest executor implementation** — the harness code that reads a manifest emitted by the agent, executes the hydrate/dehydrate/compress/densify/colocate/lookahead/investigation operations, and updates the Brain Map between turns. Section 12.3 specifies the operation semantics; the executor implementation is elsewhere.
- **Brain Map data structure** — how the Brain Map is represented in memory and on disk, how it's serialized for context-window insertion, how it's persisted across process restarts. The schema is determined by this design doc; the runtime representation is implementation detail.
- **Eviction algorithm** — given `importance` as the priority signal, the exact algorithm (pure importance-ranking, importance-plus-recency, importance-plus-access-frequency, etc.) and the budget-hit response policy. Section 12.4 specifies that importance drives eviction; the exact algorithm is a runtime concern.
- **Async helper worker queue** — how `densify`, `investigation`, `collapse`, and long `colocate` operations get queued, scheduled, executed, and their results merged back into the pyramid. Section 12.10 specifies the pattern; the worker implementation is elsewhere.
- **Delta-chain storage format** — how deltas are stored (separate table, journal, content-addressed), when collapses trigger (chain length threshold, time-based, cost-based), how superseded versions are garbage-collected. Section 12.8 specifies the architectural pattern; the storage format is a runtime plan.
- **Topic thread routing** — the lobby logic that decides which topic thread a new human utterance belongs to, how new threads are minted, how inactive threads are retired. Section 12.5 specifies the model; routing implementation is elsewhere.
- **Manifest provenance store** — the schema for storing manifest → turn pairs, the metrics collection (lookahead accuracy, compression timing, density coverage), the audit query surface. Section 12.3 specifies the pattern; the storage implementation is elsewhere.
- **Folio HTTP route implementation** — Section 12.7 specifies the API shape; the implementation is a web surface follow-up (P3 in the existing web audit).
- **CLI command implementations for any new commands not already in CLI V2** — the existing `handoff`, `apex`, `drill`, `search`, `annotate`, `tree`, `dadbear`, `diff`, `help` commands are stable per the CLI V2 friction log. New commands that might emerge from the runtime integration (e.g., `collapse`, `investigate`) are follow-ups.

---

## Section 14 — Summary: the design in one page

**Product**: Episodic Memory — the AI agent's externalized persistent brain, written for the agent (not the human) and consumed in two complementary modes: (A) cold-start continuity across sessions, where a successor agent loads prior work to reconstruct commitments, rejections, and state; and (B) in-session working memory management, where the live agent continuously hydrates and dehydrates nodes between turns to operate with a bounded context window against an unbounded knowledge corpus. The human has biological persistent memory and does not need the pyramid; the pyramid exists solely to give the agent continuity and working memory.

**Core insight**: Recursive fractal memory. Same schema at every layer, same synthesis operation at every layer, extends indefinitely upward through vine composition. One prompt (`synthesize_recursive.md`) runs at every layer above the base and at every vine layer above that, forever. The prompt is level-agnostic and treats upward consumption as potential, not guaranteed, so it is accurate at every layer including the current apex.

**Schema**: Unified memory-schema nodes with `headline`, `time_range`, `weight` required; everything else optional and appendable. `decisions[]` uses `stance` sub-attribute (committed, ruled_out, open, done, deferred, superseded, conditional, other) to cover all decision states in one field. `importance` is a 0.0–1.0 score on topics, decisions, and quotes that drives dehydration, compression, and query ranking. `ties_to` sub-field on decisions enables cross-node and cross-session webbing. Every field can be appended by subsequent passes without re-extraction.

**Chain**: forward_pass + reverse_pass + combine_l0 (emits episodic schema) → l0_webbing → refresh_state → chronological_decompose (cuts by phase, not theme) → extraction_schema → evidence_loop (grounds phase answers in L0 nodes) → gap_processing → l1_webbing → pair_adjacent L1→L2 (recursive synthesis) → l2_webbing → pair_adjacent L2→apex (recursive synthesis) → apex_webbing. Dehydration-aware at every upward synthesis step. Webbing at every layer produces the cross-cutting topical graph.

**Quote asymmetry**: Human quotes preserved aggressively as authoritative direction (binding on successor). Prior-agent quotes preserved exactly when they represent earned state (commitments, discoveries, verdicts, definitional claims). Agent exposition paraphrased into narrative. Rule: preserve quotes when their exact words carry weight the paraphrase would lose.

**Zoom-level principle**: Prompts prescribe the operation (abstract one level above inputs), not the length. 50% ceiling as a guardrail against degenerate expansion; no lower bound. Length is content-determined. Dense short content produces short output; sparse long content produces even shorter output. Never pad, never truncate.

**Dehydration**: Upper-layer synthesis uses the field-level dehydrate cascade to adapt to input budget pressure. Priority order: drop `annotations` → `transitions` → low-importance `key_quotes` → low-importance `entities` → low-importance `topics` → low-priority `decisions` by stance and importance. Never dropped: `headline`, `time_range`, `weight`, `narrative`, high-importance committed and ruled_out decisions.

**Six reading modes, all V1**: Memoir (read the apex), Walk (scroll L1/L2 chronologically), Thread (follow web edges across non-adjacent moments by topic/entity/decision identity), Decisions Ledger (filter by stance), Speaker (filter by speaker_role), Search (FTS5 over raw chunks with drill-up to owning nodes). All six fall out of the same substrate with only UI and query plumbing.

**Vine composition**: The recursive synthesis prompt runs identically at every layer, including at vine layers above any single session. Session pyramids compose into daily vines, weekly vines, project vines, career vines — indefinite upward composition. Same schema, same prompt, same operation at every scale. Cross-session webbing links decisions, topics, and entities across arbitrary time spans, turning the memory substrate into a queryable graph.

**Relationship to v2.6 retro**: Both products ship. Both use the same underlying chain machinery. Retro is thesis-extraction (terminal); episodic is memory-schema (compositional). Episodic becomes the new default for conversation slugs once it ships; retro remains available as a preset for users who want the meta-learning product.

**Runtime integration**: the pipeline produces cold storage; the runtime half of the product uses the same substrate as the agent's active working brain. Three memory containers (Conversation Buffer / Brain Map / Pyramid). Cache breakpoint strategy that keeps ~24K of navigation skeleton cached (~64% cost reduction per turn). Context manifest protocol emitted per-turn by the agent to drive hydrate / dehydrate / compress / densify / colocate / lookahead / investigation operations — context as post-model output, not pre-model input. Topic threads and the lobby model support multiple parallel conversation buffers per session. CLI commands (`handoff`, `apex`, `drill`, `search`, `annotate`, `tree`, `dadbear`, `diff`) and the Folio HTTP route provide the stable query surface. Delta-chain + collapse architecture gives O(1) updates and logarithmic cost scaling for the Understanding zone (L2+), with Bedrock (L0/L1) immutable as ground truth. Staleness awareness via async investigation helpers. The recursive synthesis prompt runs at build time, at runtime-densify time, at delta-update time, and at collapse time — one prompt, four timing modes, all the same operation. Everything about the schema (optional fields, importance, ties_to, recursive invariance, fractal structure, dehydration cascade) is shaped by both consumption modes from the start.

The product is ambitious. The architecture is recursive. The schema is fractal. The prompt is one file. The substrate unlocks six reading modes for free. The composition extends upward indefinitely. The same substrate serves cold-start bootstrap AND continuous in-session hydration. And it exists to serve one specific reader: the agent, operating both as a successor loading prior work and as a live instance managing its own working memory against an unbounded corpus.
