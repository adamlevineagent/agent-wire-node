# Episodic Memory — Canonical Design (Vine Pyramid Model)

> **Status:** Canonical design document. Supersedes the "two separate workstreams" framing in `episodic-memory-design.md` where relevant (see Part X for the diff). Aligned with the vine-driven sequential ingestion model, DADBEAR orchestration, and leftward-growth recency convention.
>
> **Scope:** Full conceptual and UX resolution of the design surface. Implementation details (chain YAML syntax, database schemas, API endpoints, code structure) are deferred to implementation plans. This document captures *what the system is*, *what it does*, *how the user experiences it*, and *why every piece is shaped the way it is*.
>
> **Written for:** Adam — the architect of this product and its immediate consumer — and for any successor agent or collaborator who needs to understand the full design surface before touching the implementation.

---

## Preamble: What this product actually is

Episodic memory is not a pyramid product. It is not a knowledge base. It is not a search index.

It is an **intelligence-as-primitive re-invention of memory for AI agents** — a cognitive substrate on which agents operate with genuine continuity across sessions and within sessions, built from LLM synthesis as the underlying cognitive operation rather than from raw text storage or database indexing.

Human memory has properties AI agents lack by default: continuity across time, scale-invariant short-term recall, asymmetric decay (the recent past is sharp; the distant past is blurry but navigable on demand), canonical identity resolution across time (recognizing the same entity in different contexts), binding commitments that persist without rehearsal. None of these are "database features" — they're properties of a cognitive substrate engineered by evolution to support moment-to-moment decision-making under bounded working memory.

This product builds an engineered version of that substrate for agents, using LLM synthesis as the primitive operation instead of biological neurons. The resulting artifact is a **vine pyramid** — a recursive memory pyramid whose leftward-growing leftmost slope gives the agent scale-invariant short-term memory at every moment of operation, with the rest of the pyramid providing addressable long-term memory on demand.

The vine is **the agent's persistent brain**. The user interacts with it, but the agent *inhabits* it.

---

## Part I — Founding insight and guiding principles

### 1.1 Memory as a cognitive primitive

Persistent memory for AI agents is not a storage problem and not an information retrieval problem. It is a **cognitive substrate problem**. The agent needs a medium that supports the shape of working memory, recent memory, and long-term memory as it operates. The vine pyramid is that substrate.

Every design decision in this document follows from a single goal: give the agent a cognitive substrate that *feels like memory during operation*, not like querying a database.

### 1.2 The vine is just a pyramid

Architecturally, the vine is **just a pyramid**. It uses the existing chain executor, the existing recursive synthesis operation, the existing delta mechanism, the existing `ties_to` webbing, the existing staleness and update machinery. It is not a new kind of thing.

The only distinctive property: **the vine's L0 layer is the apex nodes of other pyramids** — specifically, the bedrock conversation pyramids ingested from `.jsonl` transcripts. Everything else is ordinary pyramid behavior running at the composition scale.

This is a critical simplification. Nothing new gets invented at the pyramid-creation layer. The vine is a pyramid that happens to consume other pyramids as its L0 inputs, and otherwise runs the same machinery as every other pyramid in the system. Composition and recursion, brain-hurty levels, but no new parts.

### 1.3 DADBEAR as the orchestrator

DADBEAR is the existing auto-update system for pyramids. It handles debouncing, staleness detection, incremental re-processing, and propagation of changes through pyramid dependency chains. For episodic memory, DADBEAR gains **one new capability: the ability to create pyramids** (currently it only maintains existing pyramids).

Since pyramids can source from other pyramids (the whole architecture of the vine), pyramid-creation is a natural extension of DADBEAR's lifecycle management — it's been handling pyramid-maintenance all along, and creation is the prelude to maintenance for newly-appearing source files.

The ingestion loop is a DADBEAR workflow, not a separate orchestration layer:

1. DADBEAR watches the user's conversation folder (e.g., `~/.claude/projects/.../sessions/`)
2. When a `.jsonl` appears or is modified, DADBEAR applies its debounce — waits until the conversation stops being active
3. Once debounced past the threshold, DADBEAR triggers a bedrock pyramid build via the existing chain executor
4. When the bedrock apex is written, DADBEAR triggers a vine delta — folding the new bedrock apex into the vine via the existing delta pipeline
5. The delta propagates upward through the vine's leftmost slope layers via the recursive synthesis prompt in delta mode
6. If the conversation resumes mid-debounce, chunks past the debounce line get processed incrementally — the bedrock builds "behind" the live conversation
7. If the user re-opens an old conversation and adds content, DADBEAR treats it as stale, rewrites the bedrock (fully or incrementally), and ripples the update up through the vine

**The user does not manage a queue. The user does not press pause.** The vine becomes current as a background property of the user's work, handled entirely by DADBEAR's existing debounce + staleness + propagation machinery extended with pyramid creation.

### 1.4 Guiding principle: usefulness over cost

**Cost is not the constraint. Usefulness is.**

LLM intelligence is sub-penny to single-dollar per operation and getting cheaper and smarter. The scarce resources are the user's attention and the agent's effectiveness. Every design decision is made against this rubric:

- Does this bespoke intelligence produce genuinely useful understanding structure?
- If yes, it is worth the cost regardless of what the cost is (within orders of magnitude of current).
- If no, don't build it even if it would be cheap.

This rubric rules out a class of architectural choices that are *optimization theater* — pre-computing things to avoid LLM calls that don't actually save money or add value. It rules in choices that leverage more bespoke intelligence wherever intelligence is what produces the useful shape.

Examples of how this principle shapes the design:
- **No pre-computed cross-vine webbing** (expensive to maintain, cheaper and more flexible to ask questions on demand)
- **Full leftward-slope in every extraction prompt by default** (the model provider's cache makes the token cost trivial, and the usefulness is high)
- **Recursive synthesis as the primitive at every layer** (one load-bearing prompt applied many times is more valuable than N specialized prompts)
- **Delta-mode synthesis instead of full re-synthesis** (not because synthesis is expensive, but because incrementality is a useful property for continuous ingestion)
- **Six reading modes all at V1** (they fall out of the same substrate; no reason not to ship all of them)

### 1.5 The first mountain

The initial bedrock source is Adam's folder of Claude Code `.jsonl` transcripts accumulated over months of working with Partner on `agent-wire-node`. Hundreds of files, unorganized, sitting on disk. This is not a contrived test corpus; it is Adam's actual working history with the project.

Climbing this mountain is the first real job of the product: DADBEAR detects the backlog, sorts by earliest timestamp, processes in temporal order, and grows the vine continuously until the full arc of the project is ingested. Then it keeps watching the folder and continues to grow the vine indefinitely as new conversations happen.

After the initial climb, the vine represents the full arc of the project and stays continuously current. Partner's next session loads the vine and comes online knowing everything.

Future bedrock sources are possible — other transcript formats, voice session captures, mail threads, document editing sessions, shell histories, whatever produces temporally-ordered human-agent interaction data. Claude Code `.jsonl` is the first target and the design must work cleanly for it before generalizing.

---

## Part II — The conceptual model

### 2.1 Bedrock pyramids

A bedrock pyramid is the memory-schema pyramid built from one conversation `.jsonl`. Its structure:

- **L0** — forward/reverse/combine-fused base-layer nodes, one per chunk, with the full episodic schema (narrative, decisions, key_quotes, topics, entities, transitions)
- **L1** — segment nodes, grounded in L0 via evidence_loop with KEEP verdicts
- **L2** — phase nodes, produced by pair_adjacent + synthesize_recursive
- **Bedrock apex** — the session-level node, produced by pair_adjacent + synthesize_recursive on the L2 nodes

Bedrock pyramids are built by the existing chain executor using the episodic chain (`conversation-episodic.yaml`) — same machinery as retro and question pyramids, different prompts.

Bedrock pyramids are **immutable once written** in the delta-chain sense (Section 12.8 of the predecessor doc): their L0/L1 nodes are ground truth. If a bedrock needs correction, DADBEAR re-ingests it (wholly or incrementally) as a new version, not via in-place edits.

Bedrock pyramids are **addressable standalone**. They support all six reading modes on their own, even though the vine is the primary consumer. Any bedrock slug can be queried directly via CLI, HTTP, or UI.

### 2.2 The vine pyramid

The vine is a pyramid whose L0 layer is the apex nodes of bedrock pyramids. Its structure:

- **Vine L0** — pointers to bedrock apexes, ordered temporally, with the leftmost being the most recent
- **Vine L1** — pair_adjacent composition of consecutive L0 pairs via recursive synthesis
- **Vine L2** — pair_adjacent composition of consecutive L1 pairs
- **...further upward as the corpus grows...**
- **Vine apex** — single node representing the full arc at maximum abstraction

Growth is **leftward**. New bedrocks append to the LEFT edge. The rightmost L0 is the oldest conversation ever ingested; the leftmost L0 is today's conversation.

The leftward convention is not arbitrary. It is the load-bearing orientation that makes the leftmost slope serve as scale-invariant short-term memory (Part III.3).

### 2.3 Schema invariance at every layer

Both bedrock and vine nodes use the same schema:

- **Required**: `headline`, `time_range`, `weight`
- **Optional**: `narrative`, `topics[]`, `entities[]`, `decisions[]` (with `stance` ∈ {committed, ruled_out, open, done, deferred, superseded, conditional, other}, `importance`, `ties_to`), `key_quotes[]` (with `speaker_role` ∈ {human, agent}, `importance`), `transitions`, `annotations[]`

The only thing that changes between a bedrock L0 chunk node and a vine apex covering a whole project arc is the *scale* of what the fields describe. Same fields, same meanings, different scope.

Schema invariance is load-bearing for:

- One recursive synthesis prompt at every layer without modification
- Cache-stable leftmost slope that looks identical in shape at every depth
- Runtime dehydration as a lightweight field-drop operation (no shape shift)
- Indefinite upward composition (vines of vines, same operation at every scale)
- Multi-resolution loading at runtime (agent picks zoom level, not node type)
- The same navigation skeleton at build time (as primer) and runtime (as Brain Map)

### 2.4 The recursive synthesis operation

A single prompt — `synthesize_recursive.md` — runs at every layer above the base in three input modes:

1. **Peer fusion**: N peer nodes at some layer → one parent node at one layer above
2. **Delta update**: existing parent node + one new or changed child → updated parent at same abstraction level
3. **Initialization**: single child with no parent yet → parent wrapping it at one layer above

The prompt is level-agnostic. It infers the abstraction level from input content and shifts exactly one step outward. It never references absolute depth. Upward composition is phrased as *potential, not guaranteed*, so the prompt is accurate at every layer including the current apex.

This single prompt runs at four distinct timing modes:

1. **Full-build pipeline** (offline, sweeps every layer during initial bedrock construction)
2. **Ingestion delta** (per-bedrock, folds a new bedrock apex into the vine)
3. **Runtime densify** (online async, produces one missing mid-level node on demand)
4. **Collapse** (offline or scheduled, rewrites accumulated delta chains into fresh canonical versions)

Same prompt. Same schema output. Four timing modes. The prompt's purpose block is written to be accurate in all of them — it never asserts "a build is running" or "another layer exists above" or any other mode-specific claim.

### 2.5 Delta chains and collapse

Per Section 12.8 of the predecessor doc:

- **Bedrock L0 and L1** are immutable. Ground truth. New ingest appends; it doesn't mutate.
- **Vine L1 and above** are mutable via delta chains. Each delta is a small patch to an existing node representing the effect of one new child appearing beneath it.
- **Periodic collapse** rewrites a delta chain on a node into a fresh canonical version synthesized from the current child set. Collapses are triggered by chain length, time since last collapse, or explicit request.

This gives the vine **O(log N) per ingestion** bounded by depth, not breadth. **Total cost for N conversations is O(N log N) ≈ linear** for any realistic N. The vine apex stays bounded in size via the dehydration cascade regardless of corpus growth.

---

## Part III — The ingestion cycle (DADBEAR-driven)

### 3.1 Temporal ordering and the two operating modes

Ingestion has two distinct operating modes:

**Initial climb.** DADBEAR scans the source folder, discovers a backlog (say, 347 `.jsonl` files), sorts them by earliest timestamp, and processes them in temporal order. Strict temporal ordering is a correctness requirement — canonical identity convergence depends on each ingestion seeing a primer derived from *prior* work, not future work. Decision stance tracking depends on decisions being observed in commit-order.

**Steady state.** After the backlog is ingested, DADBEAR keeps watching the folder. New `.jsonl`s appear as the user has new conversations with Partner. Each one is processed individually as it becomes debounce-eligible.

Both modes use the same debouncing, the same bedrock build chain, and the same vine delta mechanism. The initial climb is just a burst of steady-state events processed in rapid temporal order.

### 3.2 Debouncing and incremental processing

DADBEAR's debounce is the central affordance for "the vine is current without user intervention." The mechanism:

- A conversation starts; `.jsonl` is being actively written to
- DADBEAR detects writes and marks the file active
- If the conversation pauses for the debounce window (e.g., 5 minutes with no new writes), DADBEAR begins processing the portion of the conversation that's past the debounce line
- If the conversation resumes before processing completes, newly-added content queues up; processing of the debounced portion continues
- When the conversation pauses again, the newly-added portion enters its own debounce, then processes
- The bedrock pyramid gets built **incrementally, behind the live conversation**

This matters because:

- Long sessions don't block ingestion of their earlier chunks
- The vine stays somewhat current even during multi-hour sessions
- The user never notices the ingestion happening
- Re-opened old conversations are handled naturally: they become active again, DADBEAR detects changes, applies debouncing, reprocesses the modified portion (fully or via incremental delta), rewrites the bedrock, and deltas the update up through the vine

All of this rides on DADBEAR's existing debounce + staleness + propagation machinery. The only new capability: triggering pyramid *creation* for newly-appearing `.jsonl` files, in addition to the existing maintenance of already-existing pyramids.

### 3.3 The primer: the leftmost slope (recency-weighted multi-resolution)

Before each bedrock build, the vine produces a **primer** — a compact reference block that the bedrock's extraction prompts use as ambient context. The primer is the **leftmost slope of the current vine**, walking from the vine apex down through one node per layer to the leftmost L0.

Because growth is leftward, each node in the leftmost slope covers a progressively more recent, progressively smaller time window at progressively higher resolution:

```
vine apex                  ← FULL ARC at maximum abstraction (everything so far)
  |
leftmost L(k-1)            ← MOST RECENT HALF at one level less abstract
  |
leftmost L(k-2)            ← MOST RECENT QUARTER at two levels less abstract
  |
leftmost L(k-3)            ← MOST RECENT EIGHTH
  |
  ...
  |
leftmost L1                ← LAST ~2 conversations, fine-grained
  |
leftmost L0                ← THE MOST RECENT conversation in full detail
```

This is the architectural payoff of the leftward growth convention. The slope gives the new build four things at once:

**1. Scale-invariant short-term memory.** Regardless of corpus size — 10 conversations or 10,000 — the leftmost L0 and L1 always represent "today's conversation at full detail" and "the last two conversations at fine-grained detail." Short-term memory quality is *constant* as the corpus grows. Only the width of each layer grows; the leftmost slope at each depth stays the same shape.

**2. Recency-weighted multi-resolution context.** Each step down the slope is a doubling of recency focus and a doubling of detail. The agent (or the next build) knows today in perfect resolution, this week at high resolution, this month at medium resolution, and the full arc at low resolution — all in one compact structure. This matches how working memory should operate: fine detail on the immediate present, coarser context on the further past, all navigable.

**3. Canonical identity catalog in the apex.** Via the dehydration cascade, high-importance topics, entities, decisions, glossary entries, and practices bubble up to the apex. Loading the apex at the top of the slope = loading the full canonical identity catalog that downstream extraction uses for naming-consistency.

**4. Natural dehydration, organically inferred.** When the slope exceeds the token budget, dehydration is **inferred from slope position, not from a hardwired priority enum**. Drop apex-side nodes first (the low-detail-per-unit-time summaries are lossy to drop but don't hurt current work), keep the recent-end slope nodes intact. The priority-ordered field-drop cascade from the predecessor doc still applies *within* a retained node (for optional-field compression), but the primary dehydration mechanism is slope-position-based.

The primer rides in every extraction prompt during the bedrock build as a stable reference block. The model provider's prefix cache makes this essentially free after the first call — all chunks in a build see the same primer, and the cache means the cost is paid once per build, not per chunk.

### 3.4 Delta composition: `n` = batch size

When DADBEAR triggers a vine delta (after a bedrock finishes building, or after several bedrocks accumulate), the delta folds the new bedrock apex(es) into the current best understanding of the vine.

The configurable variable is **`n` = batch size**:

- **`n = 1` (default).** One new bedrock per delta. Maximum freshness — the vine is current within one conversation's latency of new work. One delta operation per ingestion.
- **`n > 1`.** Wait for `n` new bedrocks to accumulate, then run one delta that folds all of them into the vine together. Fewer delta operations at the cost of less freshness between batches. Useful primarily for the *initial climb* of a large backlog, where intermediate vine state between batches isn't important and fewer deltas reduces total work. Steady-state typically leaves `n=1`.

There is also an **optional slope-context-depth variable** that advanced users can set in YAML:

- **Default (empty/unset)**: the delta input includes the **full leftward slope up to the apex**, with **token-aware auto-dehydration** applied if the slope exceeds budget (dropping apex-facing nodes first, preserving recent-end nodes). This is the "natural dehydration" behavior — the delta sees everything the slope has to offer, trimmed only if necessary to fit.
- **Configured integer**: cap the slope depth at a specific number of nodes. For experimentation and tuning only; not a primary knob.

Both variables live in the chain YAML. Sensible defaults: `n=1`, slope depth unset (full slope with auto-dehydration). The defaults are what the product ships with; experimenters can tune in YAML without code changes.

### 3.5 Delta propagation upward

When a delta runs, the new bedrock apex(es) land in the vine's L0 layer at the leftmost position. Affected higher layers propagate via the recursive synthesis prompt in delta mode, updating one node per affected layer on the leftmost slope:

- **Vine L1**: the new L0 pairs with its rightward neighbor if one exists (forming a new L1 or updating the leftmost L1), or sits as an orphan at the leftmost edge until the next bedrock arrives to pair with it
- **Vine L2**: the updated L1 triggers a delta on its parent L2
- **...and so on up to the apex...**
- **Vine apex**: gets a small delta representing "one new conversation's (or batch of conversations') worth of arc"

Each delta is a bounded-input operation: existing parent + small update → updated parent. Cost per layer is roughly constant. Total cost per ingestion is roughly **O(depth) = O(log N)**.

Orphans on the leftmost growth edge are normal. A vine with an odd number of L0 nodes has an orphan L0 at the leftmost position waiting for a partner. The orphan participates in the next delta cycle when the next bedrock arrives. Orphans don't block anything — they're just visible at the growth edge until paired.

### 3.6 Staleness, re-ingestion, and ripple

DADBEAR's existing staleness machinery handles "what if an old conversation changes" transparently:

- User re-opens an old conversation and adds content
- DADBEAR detects the modification
- Applies debouncing
- Marks the bedrock stale
- Re-builds the bedrock (incrementally or fully, depending on the scope of change)
- Triggers a vine delta using the updated bedrock apex
- The ripple propagates up through affected slope layers via delta synthesis
- The vine becomes current with the updated bedrock

This "as much rippling as needed" behavior is how DADBEAR already works for other pyramid types. Episodic memory inherits it for free. There is no separate "correction flow" or "manual re-ingestion" UX — staleness is handled as part of the same pipeline that handles fresh ingestion.

---

## Part IV — Canonical identity convergence

### 4.1 The problem

Without coordination, each bedrock extraction produces its own identity namespace. "Pillar 37" becomes `Pillar 37` in one bedrock, `pillar 37` in another, `no-length-prescriptions (Pillar 37)` in a third. "Dennis" becomes `Dennis`, `Partner`, `the AI partner`. `ties_to` edges break because identities don't match. The corpus fragments silently until querying anything requires knowing every variant.

### 4.2 The solution: the vine apex as the running identity catalog

The vine apex, at the top of the leftmost slope, carries the running canonical identity catalog. Via the dehydration cascade, high-importance `topics[]`, `entities[]`, `decisions[]`, and `annotations[]` entries from across the entire corpus bubble up and persist at the apex.

When a new bedrock build runs, the primer includes the vine apex, which means the extraction prompts see the full canonical catalog as ambient reference. Topics use canonical names. Entities use canonical forms. Decisions reference prior commitments with canonical identifiers.

### 4.3 Advisory, not constraining

Critical rule for every extraction prompt that sees the primer:

> *The KNOWN IDENTITIES reference block (derived from the vine's current state) is **advisory**. When content in this chunk clearly refers to an identity already in the reference, use the canonical form from the reference. When content introduces something genuinely new, create a new identity — do not force-fit novel content into existing categories just to match. Forced matches are worse than missed matches.*

Without this rule, the extractor would hallucinate matches and collapse novel content into existing categories. With it, the primer sharpens signal without blurring novelty. Identity convergence is asymptotic: early bedrocks introduce variants, later bedrocks reinforce canonical forms, the catalog firms up over the arc.

### 4.4 Identity evolution (document but punt)

Identities can evolve over time. "The no-length-prescription rule" might later be formalized as "Pillar 37". The schema supports this organically:

- **Synonym annotations**: `annotations[]` entries can link variant names to canonical forms
- **Supersession**: `decisions[].stance = superseded` with `ties_to.decisions` pointing at the replacement
- **Manual override**: the user can correct a canonical identity in the vine apex via the Vines page; the correction propagates via DADBEAR's staleness ripple

UX for explicit user-driven identity unification is deferred to a later iteration. In V1, evolution happens organically through the primer's drift toward canonical forms over successive ingestions.

---

## Part V — Query flow: vine and its bedrocks

With DADBEAR orchestrating and **no cross-vine webbing**, the query flow simplifies to three directions:

### 5.1 Vine → bedrock: the primer direction

When DADBEAR triggers a new bedrock build, the vine produces the primer (leftmost slope with the apex at the top carrying canonical identities). The primer rides in every extraction prompt during the bedrock build as a stable cached reference block. Canonical identities propagate forward into the new session's memory.

Build-time. Automatic. User-invisible.

### 5.2 Bedrock → vine: the delta direction

When a bedrock finishes building, DADBEAR triggers the vine delta. The new bedrock apex lands at the leftmost position in vine L0. Delta synthesis propagates up through affected slope layers. Vine apex is updated with the incremental change.

Build-time. Automatic. User-invisible.

### 5.3 Question pyramid → vine → bedrock: the escalation direction

When the agent (or the user) wants to know something that the vine doesn't carry at apex resolution, the mechanism is a **question pyramid asked of the vine**. The flow:

1. Question is asked against the vine slug (via CLI, HTTP, UI, or agent manifest operation)
2. Question pyramid decomposes the question into sub-questions
3. Sub-questions hit vine-level evidence (via `evidence_loop` against vine nodes)
4. When an answer needs more detail than the vine carries at apex resolution, the evidence trail leads via existing `ties_to` edges into specific bedrock pyramids
5. A child question pyramid gets spawned against the relevant bedrock(s)
6. Bedrocks answer the sub-questions on demand
7. Results flow back up to the vine's answer

**This is exactly `recursive-vine-v2 Phase 2`** — vine gap escalation to source pyramids. The existing planned mechanism is the right primitive for the cross-navigation need. The canonical model here contextualizes Phase 2 as the *one* escalation direction rather than a separate workstream.

**Key design choice: we do NOT pre-compute cross-vine webbing.** The question-pyramid route is cheaper (pay only when a query actually happens), more flexible (the question itself shapes the traversal), and produces better answers (bespoke intelligence applied to the specific question). Pre-computed webbing would be more complexity for less value.

Agents that judge a specific cross-slug connection worth manually recording can do so via annotations — but that's agent-initiated, case-by-case, not pro-active mapping. The default behavior is on-demand question-driven traversal.

---

## Part VI — The six reading modes

All six reading modes from the predecessor design doc work on the vine natively. The leftward growth convention affects how each mode is framed, but the substrate supports all six without additional extraction.

### 6.1 Memoir — read the vine apex

The apex is the whole-arc memoir. Dense prose at maximum abstraction. For a vine covering the agent-wire-node project from January through April 2026, reading the apex gives the meta-narrative of the entire project in ~2-3K tokens. This is the primary cold-start loading path for a new Partner session.

### 6.2 Walk — scroll the vine chronologically

Walk through vine L1, L2, or higher nodes in chronological order. Because growth is leftward (newest on the left), the natural default walk direction is **leftmost-first** (newest-first) — the user or agent starts with what's most relevant to current work and walks rightward into history as needed. Walking rightward-first (oldest-first) is available for users who want the full historical arc from the beginning.

### 6.3 Thread — follow a topic across bedrock

Pick a canonical topic, entity, or decision identifier, and follow its web edges across non-adjacent bedrock nodes in chronological order. "Every time we touched chain-binding-v2 across the project arc."

Thread traversal crosses bedrock boundaries via the question-pyramid escalation mechanism (Part V.3), not via pre-computed cross-bedrock webbing. The user or agent asks "show me everything about chain-binding" and the vine's answer recursively descends into specific bedrocks via `ties_to` and spawned sub-questions.

### 6.4 Decisions Ledger — filter by stance

Across the vine, render the `decisions[]` arrays filtered by stance. "Everything currently committed." "Everything open, sorted by how long." "Everything ruled out, with reasoning." The agent consults the ledger before proposing new work to avoid contradicting prior rulings or re-opening settled questions.

### 6.5 Speaker — filter by speaker role

Filter to one speaker's contributions across the whole vine. Human turns (rare, high-weight, binding) or agent turns (abundant, lower-signal-per-token, but including commitments and discoveries). In an AI-dominated corpus where the agent speaks ~95% of the tokens, Speaker mode on the "human" filter is extremely high-signal — a small number of turns carrying the direction that shaped the whole arc.

### 6.6 Search — FTS with drill-up

Full-text search over `pyramid_chunks` (FTS5 index over the raw transcripts of all ingested bedrocks), with hits that drill up to owning L0 nodes, L1 segments, L2 phases, bedrock apexes, and vine-layer ancestors. The escape hatch for when paraphrase extraction has lost a specific phrase the user remembers.

---

## Part VII — The user experience: the Vines page

V1 introduces a **dedicated Vines page** in the app — separate from the existing Pyramid dashboard — because vines are a distinct enough product to warrant their own home.

### 7.1 Vines page layout

The Vines page has four primary regions:

**Left rail: vine list.** One entry per active vine (e.g., "agent-wire-node project arc", "GoodNewsEveryone", "personal knowledge"). Clicking an entry selects it as the current vine for the main view.

**Main view: vine visualization and content.** The centerpiece. The selected vine's recursive triangle rendered live, growing leftward as new bedrocks arrive. Layers colored by depth. Leftmost slope highlighted. Current apex headline displayed prominently at the top. Clicking any node opens its detail view.

**Alongside: the canonical identities panel.** A running display of the vine apex's canonical catalog:
- Top topics by importance
- Top entities by importance, grouped by role (people, files, concepts, systems, slugs)
- Active decisions by stance (committed / open / ruled_out / deferred)
- Glossary entries with definitions
- Practices

As ingestion progresses, this panel grows and stabilizes. It's the user's window into what the agent now "knows" about the canonical shape of the project.

**Bottom: DADBEAR status.** Watched folders, recent debounce events, recent bedrock builds in progress, recent vine deltas, any staleness flags or errors. The user's visibility into what DADBEAR is doing in the background.

### 7.2 Creating a new vine

User clicks "New Vine" on the Vines page:

1. **Name the vine.** e.g., "agent-wire-node project arc"
2. **Point at a source folder.** Claude Code transcripts directory, or another supported source.
3. **Configure DADBEAR.** Debounce timer (default 5 minutes), auto-ingest vs. confirm-before-ingest (default auto), `n` batch size for deltas (default 1), slope depth (default: empty = full slope with auto-dehydration).
4. **Confirm.** DADBEAR scans the folder, discovers the backlog (if any), sorts by timestamp, and begins processing.

The vine visualization starts populating as DADBEAR works through the backlog. The user can leave the app open and watch progress, or close it — DADBEAR continues in the background and picks up where it left off next time.

### 7.3 Watching the vine grow

During the initial climb:

- The vine visualization adds new L0 slots on the left as each bedrock finishes
- Delta pulses propagate up through affected slope layers, visible as a brief highlight animation
- The canonical identities panel grows and stabilizes as canonical forms firm up
- The apex headline updates as the understanding matures
- DADBEAR status shows live counts: bedrocks completed, bedrocks remaining, current bedrock with its chain phase

After the initial climb, in steady state:

- New bedrocks arrive organically as the user has new conversations with Partner
- Each new bedrock triggers one delta cycle (at default `n=1`)
- The user notices the vine update between work sessions without any action on their part

### 7.4 Exploring the vine

At any point, the user can switch reading modes via a selector in the main view:

- **Memoir** — the apex rendered as narrative
- **Walk** — scroll L1 or L2 chronologically (leftward-first by default)
- **Thread** — pick a canonical topic/entity/decision, see the trail across the vine (escalating into bedrocks on demand)
- **Decisions Ledger** — all decisions filtered by stance
- **Speaker** — filter to human or agent turns
- **Search** — FTS across raw chunks with drill-up

Drilling: clicking any vine node with `ties_to` pointing into a bedrock navigates into that bedrock's pyramid (which itself supports all six modes standalone). Clicking an L0 chunk shows the raw dialogue that produced it, with extraction metadata visible for debugging.

### 7.5 Asking a question of the vine

The user (or the agent) can type a question into a prompt bar on the Vines page. This triggers a **question pyramid asked of the vine** — the mechanism from Part V.3. The question decomposes, hits vine-level evidence, escalates into bedrocks as needed, and produces an answer with citations. The answer renders in the main view with drill-down links into the specific bedrock moments it's grounded in.

This is the primary surface for agent-style interaction with the vine. "What are we currently committed to?" "What did we rule out about chain architectures?" "Show me the history of the decision to ship episodic memory as the default." All of these are question-pyramid queries against the vine.

### 7.6 Annotation and correction

Any node (vine or bedrock) can be annotated by clicking "Annotate" in its detail view. Annotations are stored in the node's `annotations[]` and persist across future deltas.

Corrections are handled via DADBEAR's staleness machinery: if the user corrects something in the source (re-opens a conversation and modifies it, or flags a bedrock as needing re-ingestion), DADBEAR detects the change and ripples the update through the normal ingestion pipeline. There is no separate "correction" flow — it's all the same staleness handling.

### 7.7 Re-opening old conversations

Re-opened old conversations are handled transparently:

- User opens a conversation from two weeks ago, adds more content
- DADBEAR detects the modification, applies debouncing
- Re-builds the bedrock (fully or incrementally, depending on the scope of change)
- Triggers a vine delta with the updated bedrock apex
- Ripples up through affected slope layers
- Vines page updates to reflect the new state

The user sees this as "the vine just knows about the updated conversation." No explicit re-ingest action is required.

---

## Part VIII — Runtime integration: the agent as vine consumer

### 8.1 Cold start — loading the leftmost slope at session boot

A new Partner session loads the current vine's leftmost slope as its initial context. Because the slope is recency-weighted, the agent comes online knowing:

- **Apex**: full arc at low-detail-per-unit-time meta-narrative + canonical identities
- **Each step down**: progressively more recent, progressively higher detail
- **Bottom (leftmost L0)**: today's conversation in full resolution

~14 nodes for a 10,000-conversation vine. Cache-stable prefix. O(1) query cost. Instant orientation with perfect short-term memory and adequate long-term context.

The session starts with the agent effectively knowing "everything that matters about where we are in this project" in a single load. The rest is drill-down on demand.

### 8.2 Brain Map and manifest operations

During active work, the agent's Brain Map (Section 12.1 of the predecessor doc) is drawn from the vine. The Map has three tiers:

- **Conversation Buffer** — live dialogue, kept sacred, ~20K tokens
- **Brain Map** — stable navigation skeleton (the leftmost slope of the vine) + variable hydrated content (specific vine or bedrock nodes pulled in for the current turn's work)
- **Pyramid (cold storage)** — the full vine and all its bedrocks on disk

Manifest operations (hydrate, dehydrate, compress, densify, colocate, lookahead, investigation) work against vine and bedrock nodes identically because the schema is invariant. The agent doesn't distinguish "vine node" from "bedrock node" — it navigates and loads at whatever scale is useful for the current task.

### 8.3 Natural dehydration at runtime matches natural dehydration at build

The slope-position-based dehydration from Part III.3 extends to the runtime Brain Map. Under token pressure:

- Drop apex-facing slope nodes first (low-detail-per-unit-time summaries)
- Preserve the recent-end slope nodes (high-detail recent context)
- Within a retained node, apply the field-level priority cascade (drop `annotations` → `transitions` → low-importance `key_quotes` → ...)

Scale-invariant short-term memory holds at runtime: the agent's recent-context fidelity is constant regardless of how deep the vine goes. Long-term context gracefully degrades under pressure without affecting working memory.

### 8.4 Async helpers write back to the vine

Mid-session insights that should persist get written back via async helpers that run `synthesize_recursive` in delta mode against the affected vine nodes. DADBEAR's staleness machinery propagates the update. The next session's primer reflects the new state.

This is how "the agent learned something today" gets captured without blocking the live session: the agent emits an update intent in its manifest, an async helper picks it up between turns, runs the delta, and the result becomes available for future queries.

### 8.5 The agent's experience is "I remember"

Putting it all together: session boot → instant orientation via the leftmost slope. Active work → working memory adapts via manifest operations. Mid-session insight → persists via async delta. Session end → nothing to save, already there. Next session → picks up exactly where it left off.

The agent's experience of having persistent memory is the experience of operating on the vine. The vine is the cognitive substrate, and it feels (to the agent) like actual memory because it's navigable at every scale, responsive to the current cognitive need, and continuously current with the user's work.

---

## Part IX — One level of recursion at V1

V1 focuses on making a single vine actually useful for a single project's arc, end-to-end. **One level of recursion: bedrock → vine.** No meta-vines composing vines yet.

The architecture supports indefinite upward composition natively — schema invariance and the level-agnostic recursive synthesis prompt already work at any layer. Meta-vines of project vines (portfolio scale) or career vines of meta-vines (full working history) are natural extensions. But V1 doesn't build them because the actual use case hasn't materialized yet and building them speculatively would be complexity without validated value.

When the user finds an actual use case for composing multiple vines — e.g., "I want to see my agent-wire-node decisions alongside my GoodNewsEveryone decisions to spot architectural patterns across projects" — we zoom out another layer at that point. Until then, V1 ships one vine per use case and that's enough.

This is a deliberate application of the usefulness-over-cost principle. Meta-vines would be cheap to build because the mechanism already exists; they're omitted not because of cost but because they don't yet produce useful understanding structure the user actually wants.

---

## Part X — What this supersedes, keeps, and defers

### From `episodic-memory-design.md`

**Supersedes:**

- **Vine composition as a post-hoc workstream.** The canonical model makes vine composition intrinsic to ingestion, continuously maintained by DADBEAR. Section 8 of the predecessor doc described vine composition as something you do after building individual pyramids; here it's the driver of how those pyramids get built.
- **Left-to-right chronological convention.** Growth is leftward. Recent on the left. Historical on the right. The leftmost slope is the scale-invariant short-term memory path. The rightward edge is drill-into-when-needed historical content.
- **Priority-ordered dehydration cascade as the primary dehydration mechanism.** The primary mechanism is slope-position-based (drop apex-facing nodes first). The priority-ordered field-drop cascade from Section 6.4 of the predecessor doc still applies *within* a retained node, as a secondary mechanism for compressing the fields of a node that's kept in the slope.
- **"Founding moment" framing for the leftmost L0.** The leftmost L0 is today's conversation. There is no founding-moment privilege; historical content is accessed via rightward drill when needed, not narratively privileged.

**Keeps:**

- Schema shape (three required, rest optional, `decisions[].stance`, `importance`, `ties_to`, `annotations`)
- The five prompts (forward, reverse, combine_l0, chronological_decompose, synthesize_recursive) as the foundation for bedrock builds
- Quote asymmetry rules (human binding, prior-agent earned state, agent exposition paraphrased)
- Zoom-one-level instruction, 50% ceiling as a guardrail, Pillar 37 compliance
- The six reading modes (Memoir, Walk, Thread, Decisions Ledger, Speaker, Search)
- Runtime architecture (three containers, manifest protocol, cache breakpoint strategy)
- Delta-chain + collapse from Section 12.8 (bedrock immutable as ground truth, understanding mutable via deltas)
- Annotations as the append channel for all cross-pass signals

**Extends:**

- **Schema invariance across layers** — extends to the vine layers above the bedrock apex, not just within a single pyramid
- **The recursive synthesis prompt** — now explicitly runs in three input modes (peer fusion / delta update / initialization) and four timing modes (full build / ingestion delta / runtime densify / collapse). Same prompt file, no modifications.
- **The leftmost slope as primer** — the same compact cache-stable scaffold used as the runtime navigation skeleton is now also the build-time primer. One artifact, two consumption modes.
- **DADBEAR's capabilities** — gains pyramid creation alongside existing pyramid maintenance

### Relationship to `recursive-vine-v2`

**Phase 2** (recursive ask escalation to source pyramids) is the mechanism for the question-pyramid escalation direction in Part V.3. The existing plan applies unchanged. The canonical model here contextualizes Phase 2 as *the* escalation direction — not a separate workstream, but the primitive that makes the vine → bedrock query flow work without pre-computed cross-vine webbing.

The re-audit cycle for Phase 2 (two-stage discovery/informed audit on the prep-v2 plan) is still the right next step for that workstream. This canonical doc doesn't invalidate that work; it positions it as one of three query directions in the simplified bidirectional flow.

**Phase 4-local** (cross-operator remote vines without payment) is a scale-out story for when a user wants to compose memory across multi-operator spaces on the Wire network. Not on the V1 critical path but compatible with the canonical architecture.

**Phase 4-paid** remains blocked on the cross-repo `WS-ONLINE-H` workstream on GoodNewsEveryone. Out of scope here.

### Relationship to v2.6 retro (chain-binding-v2.6)

v2.6 retro is the **thesis-extraction product**: terminal theses produced from conversations for the human reader, for meta-learning and practice refinement. It ships as-is and remains a valid preset in the wizard.

Episodic memory is the **memory-substrate product**: compositional memory nodes produced for the agent reader, for working continuity and persistent memory. It becomes the new default conversation preset when it ships.

Both products can be run on the same conversation (chunker cost paid once, synthesis cost doubles). Users pick per-pyramid which they want:

- **Retro**: for meta-learning and thesis extraction (human reader, thesis shape)
- **Episodic**: for persistent agent memory and multi-session continuity (agent reader, memory shape)

The wizard's preset selector offers both, with **Episodic Memory** as the default when it ships.

### Deferred to implementation plans

Out of scope for this canonical design (not because they don't matter, but because they're implementation details that follow from the design):

- **Chain YAML text** (`conversation-episodic.yaml`) — follows from Part II and the prompt catalogue
- **The five prompt texts** — drafted separately against this design as the reference; `synthesize_recursive.md` is the load-bearing one
- **DADBEAR's new pyramid-creation capability** — extension to existing DADBEAR code, implementation plan of its own
- **Vines page UI components** — layout and interaction details in a frontend implementation plan
- **Database schema for vine state, delta chains, checkpoint persistence** — storage-layer implementation
- **Identity evolution UX** — documented here but punted on explicit user-facing controls
- **Manifest executor implementation for runtime operations** — follows from Section 12 of the predecessor doc
- **Async helper worker queue** — runtime implementation detail
- **Collapse triggering policy** — operational tuning, not design
- **Multi-user vine sharing** — future workstream if demand emerges

---

## Part XI — Summary in one page

**Product.** Episodic memory is an intelligence-as-primitive re-invention of memory for AI agents — a cognitive substrate that gives agents genuine continuity across sessions and within sessions, built from LLM synthesis as the underlying operation, modeled on (but not mimicking) human memory properties. The artifact is a **vine pyramid**: a recursive memory pyramid whose L0 layer is other pyramid apexes (conversation bedrocks), maintained continuously by DADBEAR.

**The vine is just a pyramid.** Same chain executor, same recursive synthesis prompt, same delta mechanism, same `ties_to` webbing, same query APIs, same staleness machinery. No new primitives at the pyramid layer. Composition and recursion, brain-hurty levels, but no new parts.

**DADBEAR orchestrates everything.** Watches the user's conversation folder. Debounces active conversations. Triggers bedrock creation (the new capability DADBEAR gains) when debounce completes. Triggers vine deltas when bedrocks finish. Handles staleness and ripple updates for re-opened old conversations. The user does not manage a queue; the vine is current as a background property of the user's work.

**Leftward growth convention.** New bedrocks append on the LEFT edge. Rightmost L0 is the oldest conversation; leftmost L0 is today. This orientation is load-bearing: it makes the leftmost slope serve as scale-invariant short-term memory.

**The leftmost slope = recency-weighted multi-resolution context.** Each layer from apex down covers a progressively more recent, progressively smaller time window at progressively higher resolution. Regardless of corpus size, the leftmost L0 and L1 always represent "today and the last couple days" at full detail. Short-term memory quality is *constant* as the corpus grows. Dehydration under pressure drops apex-facing slope nodes first (organically inferred from slope position, not from a hardwired priority enum), preserving recent-end nodes. The priority-ordered field cascade still applies within retained nodes as a secondary mechanism.

**Delta model: `n` = batch size.** Each vine delta folds `n` new bedrock apexes into the current best understanding. Default `n=1` (max freshness, one delta per bedrock). JSON-configurable for larger batches during initial climb. Optional slope-depth variable defaults to *empty* (full leftward slope with token-aware auto-dehydration); power users can cap it in YAML. Delta input is the current vine apex + the leftmost slope + the `n` new bedrocks. `synthesize_recursive` runs in delta mode.

**Cost.** O(log N) per ingestion bounded by vine depth. O(N log N) ≈ linear total for N conversations. Vine apex stays bounded in size via dehydration cascade regardless of corpus growth.

**Canonical identity convergence.** The vine apex carries the running catalog of topics, entities, decisions, glossary, practices via the dehydration cascade. Extraction prompts see it as advisory — use canonical forms when matching, create new when novel, never force-fit. Asymptotic convergence over the arc.

**Three query directions. No cross-vine webbing.**
1. **Vine → bedrock (primer)** — build-time context feeding
2. **Bedrock → vine (delta)** — ingestion composition
3. **Question pyramid → vine → bedrock (escalation)** — on-demand cross-navigation via recursive-vine-v2 Phase 2

Cross-vine navigation happens through the question mechanism, not pre-computed webbing. Cheaper, more flexible, produces better answers.

**Dedicated Vines page.** Separate from the Pyramid dashboard. Live vine visualization growing leftward. Canonical identities panel. DADBEAR status. Reading mode selector (Memoir / Walk / Thread / Decisions Ledger / Speaker / Search). Drill-down to bedrocks and raw dialogue. Question prompt bar for asking questions of the vine.

**Runtime integration.** Agent loads the vine's leftmost slope at session boot for instant orientation with perfect short-term memory. Brain Map draws from the vine. Manifest operations work against vine and bedrock nodes identically. Async helpers write back to the vine for in-session insight persistence. Scale-invariant short-term memory holds at runtime too.

**One level of recursion at V1.** One vine per use case. No meta-vines yet. Zoom out when we find the actual use case. The architecture supports indefinite upward composition natively.

**Guiding principle: usefulness over cost.** LLM intelligence is sub-penny to single-dollar per op and getting cheaper. The scarce resource is attention and effectiveness, not compute. Bespoke intelligence is worth its cost when it produces genuinely useful understanding structure — regardless of compute cost. Rules out optimization theater. Rules in intelligence-rich composition.

---

## Closing

The vine is the agent's persistent brain. It is constructed from existing pyramid primitives via composition and recursion, orchestrated by an extended DADBEAR that gains the ability to create pyramids. It grows leftward as the user's work continues. It provides scale-invariant short-term memory and addressable long-term memory on demand. It is memory-as-cognitive-primitive for AI agents, engineered from LLM synthesis as the underlying operation.

The first mountain is climbing Adam's Claude Code conversation archive for the `agent-wire-node` project — hundreds of `.jsonl` files ingested in temporal order, continuously deltaed into a running vine that becomes the agent's full working history. Every mountain after that — new projects, new domains, new sources of temporally-ordered human-agent interaction — rides on the same substrate.

The product is not a pipeline or a tool. It is the infrastructure for continuous agent cognition over time, built from the simplest possible composition of primitives the existing architecture already supports. Nothing new needs to be invented at the pyramid-creation layer. Everything that's new is recursion applied to what's already there.
