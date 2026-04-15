# Plan: Layer-by-Layer Document Pyramid Verification

**Slug:** `core-selected-docs` (127 docs from Core Selected Docs)
**Goal:** Verify each pipeline layer produces maximally useful, maximally efficient output before the next layer consumes it.

---

## What does a good pyramid do?

A good pyramid lets someone who has never seen any of these 127 documents start at the apex and navigate to exactly the information they need, with each layer adding resolution they didn't have at the layer above. The apex tells you what this body of knowledge IS. The domains tell you what the major areas ARE. The threads tell you what's in each area. The L0 nodes tell you what each document SAYS. Every layer exists to answer a question the previous layer raises.

The test at every layer is: **does this layer make you smarter than the layer above did, in a way that helps you decide where to go next?**

---

## The apex defines what "good" means

Before building anything, we need to know what the apex of 127 Wire project documents SHOULD look like. These docs span:
- Wire platform architecture (action chains, contribution graph, economics)
- Agent-wire-node (Tauri app, pyramid builder, chain executor, MCP)
- Legal/business (TPUSHIT entity structure, TOS, privacy)
- Infrastructure (Supabase, deployment, Temps hosting)
- Product design (Vibesmithy, Dennis/Partner, operator dashboard)
- Design process docs (retros, audits, session notes)

A good apex should surface these as the 3-5 natural dimensions of the corpus. If the apex just says "this is a collection of project documents" — it's useless. If it says "this corpus defines a self-organizing intelligence marketplace with these architectural pillars, this business structure, and this product surface" — it's doing its job.

**The apex is the acceptance test for the entire pyramid.**

---

## Layer 6 (Upper → Apex): Does the pyramid converge to something useful?

Build the full pipeline last, but evaluate it first conceptually.

### Questions to answer
- Does the apex orientation make you want to drill down? Does it tell you what you'd FIND if you did?
- Are the L2 domain nodes the 3-5 dimensions you'd pick if you were organizing this corpus by hand?
- Does each L2 node cover a genuinely distinct area, or are there overlapping domains that should be merged?
- Is any critical area of the corpus invisible at L2? (e.g., all legal docs absorbed into "platform" with no trace)
- Can you trace from apex → L2 → L1 → L0 → source for any specific question and find the answer getting more specific at each level?

### What this layer needs from below
- L1 nodes that are faithful, cross-cutting syntheses of their thread members
- L1 webbing that reveals cross-thread connections

---

## Layer 5 (L1 Webbing): Do threads know about each other?

### Questions to answer
- Do edges reveal connections that aren't obvious from thread names alone?
- Are there threads that SHOULD be connected (shared systems, shared decisions) but aren't?
- Are there spurious connections between threads that have nothing in common?

### What this layer needs from below
- L1 nodes with enough specificity in their topics and entities that cross-thread connections can be detected
- Thread groupings that are coherent enough that inter-thread edges mean something

---

## Layer 4 (Thread Narratives): Does synthesis preserve what matters?

### Questions to answer
- Pick a thread. Read its L0 children. Now read the L1 synthesis. Would someone reading ONLY the L1 node understand the key decisions, systems, and state of that area? Or did synthesis smooth away the specifics?
- Does the L1 node add value beyond concatenating L0 summaries? Does it identify tensions, evolution, or cross-doc patterns within the thread?
- Are temporal relationships preserved? If doc A supersedes doc B's design, does the L1 node know that?
- Is the condensation ladder working? Is `current_core` at L1 genuinely the essential kernel of the thread?

### What this layer needs from below
- Thread assignments that group docs by genuine conceptual affinity (not surface-level keyword matching)
- L0 nodes with enough specificity that synthesis has real material to work with

---

## Layer 3 (Thread Clustering): Are the groupings right?

### Questions to answer
- Read the thread names and their member doc headlines. Does each thread describe a real area of the project?
- Is any thread a grab-bag? (Multiple unrelated docs thrown together because they didn't fit elsewhere)
- Is any thread too narrow? (1-2 docs that could be part of a larger area)
- Are there docs that are in the wrong thread? (A legal doc in the infrastructure thread, a retro in the architecture thread)
- If you were organizing these 127 docs by hand, would you pick roughly these groupings?

### What this layer needs from below
- L0 webbing that reveals doc-to-doc connections (helps clustering see affinity)
- L0 nodes with specific-enough topic names and orientations that clustering can group by meaning, not keywords

---

## Layer 2 (L0 Webbing): Do docs know about each other?

### Questions to answer
- Are the edges real? Sample 10-15: does each describe a genuine relationship between two documents?
- Are shared_resources accurate? Do the named systems/decisions/entities actually appear in both docs?
- Are there obvious connections that are missing? (Two docs about the same subsystem with no edge between them)
- Is the edge count reasonable? Too few means clustering has no signal. Too many means everything is connected to everything.

### What this layer needs from below
- L0 nodes with specific entity lists and topic names that make cross-doc matching possible
- Orientations clear enough that the webbing step can understand what each doc is about

---

## Layer 1 (L0 Extraction): Is the raw material good?

This is where the pipeline starts, so this is what we build first. But we evaluate it through the lens of everything above: **does this extraction give the upper layers what they need?**

### Questions to answer

**For clustering (Layer 3):** Are topic names specific enough to cluster by? "Architecture" won't cluster — "Chain Runtime Module Layout" will.

**For webbing (Layer 2):** Are entities populated and accurate? Do they name the systems, people, and decisions that connect documents?

**For thread narrative (Layer 4):** Is `current` specific enough that a synthesis step could identify cross-doc patterns? Or is it so generic that synthesis has nothing to compare?

**For upper layers (Layer 6):** Is `current_core` genuinely the essential kernel? If someone at the apex level could only see `current_core` from every L0 node, would they understand the corpus?

**For the operator (us, right now):**
- Zero empty nodes
- No runaway completions
- Condensation ladder levels are genuinely progressive (not paraphrases of each other)
- Output is shorter than input for every doc (distillation, not rewriting)
- Orientation lets you decide whether to read the source

---

## Execution sequence

```bash
AUTH="Authorization: Bearer vibesmithy-test-token"

# 1. Build L0 only
curl -s -H "$AUTH" -X POST "localhost:8765/pyramid/core-selected-docs/build?stop_after=l0_doc_extract"
# → Inspect L0 against Layer 1 criteria
# → Fix prompt if needed, rebuild with force_from

# 2. Add webbing
curl -s -H "$AUTH" -X POST "localhost:8765/pyramid/core-selected-docs/build?stop_after=l0_webbing"
# → Inspect edges against Layer 2 criteria

# 3. Add clustering
curl -s -H "$AUTH" -X POST "localhost:8765/pyramid/core-selected-docs/build?stop_after=thread_clustering"
# → Inspect threads against Layer 3 criteria

# 4. Add thread narratives
curl -s -H "$AUTH" -X POST "localhost:8765/pyramid/core-selected-docs/build?stop_after=thread_narrative"
# → Inspect L1 nodes against Layer 4 criteria

# 5. Add L1 webbing
curl -s -H "$AUTH" -X POST "localhost:8765/pyramid/core-selected-docs/build?stop_after=l1_webbing"
# → Inspect L1 edges against Layer 5 criteria

# 6. Full pipeline
curl -s -H "$AUTH" -X POST "localhost:8765/pyramid/core-selected-docs/build"
# → Inspect apex and L2 against Layer 6 criteria
# → Run the drill-path test: pick a question, navigate apex → L0, verify each layer adds resolution
```

At each checkpoint: if the layer isn't good enough for the layers above it, fix the prompt and `force_from` that step before proceeding. Don't build upward on a weak foundation.
