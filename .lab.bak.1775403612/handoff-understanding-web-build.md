# Handoff: Understanding Web Build

Canonical architecture: `docs/architecture/understanding-web.md`

This handoff defines WHAT needs to exist and WHY. It does not define HOW — the implementing agent reads the architecture doc and the codebase and decides implementation.

---

## Phase 1: See It

Question pyramids produce working 4-layer structures (proven on 127 docs and 34 code files). Nobody can see them because the UI and API assume mechanical pipeline structure.

### 1.1 Apex finder works with question pyramid nodes

**What:** `GET /pyramid/:slug/apex` returns the highest-depth non-superseded node regardless of ID format.

**Why:** Question pyramid nodes use `L{depth}-{uuid}` IDs. The current apex finder fails on these, returning "multiple nodes at every depth" even when there's a clear single apex node. Without a working apex endpoint, nothing downstream (UI, agents, MCP tools) can find the entry point.

### 1.2 Drill follows evidence links

**What:** `GET /pyramid/:slug/drill/:node_id` returns the node plus its children. For question pyramid nodes, "children" are the nodes from the layer below that have KEEP evidence links pointing to this node.

**Why:** Mechanical pyramids store children in a `children` JSON array on the parent node. Question pyramids don't — the parent-child relationship IS the evidence link (KEEP verdict in `pyramid_evidence`). Without this, drilling into a question pyramid node returns no children and navigation is dead.

### 1.3 Node display shows the question

**What:** Every question pyramid node rendered in the UI shows its `self_prompt` (the question it answers) as the primary label.

**Why:** The question IS the organizing signal. "What are the core architecture components?" tells the user exactly what they'll find if they drill in. The `headline` field is the answer summary — useful but secondary. Without the question visible, the pyramid looks like an arbitrary collection of summaries with no navigational logic.

### 1.4 Evidence verdicts are visible

**What:** When viewing a node, the user can see which source nodes were KEEP'd (with weight and reason), which were DISCONNECT'd, and what was reported MISSING.

**Why:** This is the provenance chain — "I believe X because of these sources, with this confidence." Without it, the pyramid is an oracle that gives answers with no justification. The MISSING verdicts are also how users (and agents) discover what evidence the system wishes it had.

---

## Phase 2: Grow It

The evidence base is currently static — whatever the mechanical L0 extracted is all the question pyramid has to work with. Two of the vibesmithy code files weren't touched because the pre-mapper couldn't connect them to any question. The answer step reports MISSING evidence it wishes it had. Nothing acts on those signals.

### 2.1 Targeted re-examination from MISSING verdicts

**What:** When an answer step reports MISSING evidence, the system can examine specific source files through the lens of the question that needed evidence, producing new L0 nodes that join the evidence base.

**Why:** The canonical extraction is question-agnostic — it captures what someone MUST understand about each file generically. But specific questions need specific evidence. The auth flow question needs token validation details that a generic extraction of `auth_middleware.rs` didn't capture. The MISSING verdict is the demand signal; targeted re-examination is the supply. Without this, the evidence base never grows beyond what the generic extraction captured, and every question is limited to what a one-size-fits-all extraction happened to include.

### 2.2 Evidence sets with identity

**What:** Targeted re-examinations are grouped into evidence sets. Each set knows which question triggered it, which source files it examined, and has an index (a summary of what the set contains).

**Why:** As the evidence base grows, the pre-mapper needs to find relevant evidence efficiently. Without set grouping, every new question scans every individual L0 node — which works at 127 nodes but not at 10,000. The set index is the navigational unit: "Does this set contain evidence relevant to my question?" is cheaper than scanning every node in the set. The set identity also provides provenance — you can trace any piece of evidence back to the question that demanded it.

### 2.3 Sets are DADBEAR-managed pyramids

**What:** Evidence sets are managed by DADBEAR the same way any pyramid layer is managed. When a new L0 node is added to a set, DADBEAR checks whether the set needs an index (because it now has more than one member) and whether the new node stales the existing index. As sets grow, DADBEAR's cascade naturally produces internal structure.

**Why:** The same staleness/supersession/cascade machinery that keeps the understanding layer current also keeps evidence sets current. No special-case management logic. When a source file changes, DADBEAR propagates through the canonical L0, through any targeted re-examinations of that file, through the evidence sets those re-examinations belong to, through the answer nodes that KEEP'd that evidence, all the way to the apex. One engine, recursive at every level.

### 2.4 Evidence sharing across questions

**What:** When a question needs evidence, the pre-mapper checks whether suitable evidence already exists in any evidence set from any prior question build before triggering new re-examinations. Existing evidence is referenced via KEEP verdicts (cross-linking), not duplicated.

**Why:** The underlying structure is a DAG, not a tree. The same L0 node about authentication appearing in multiple questions' KEEP lists with different weights is a signal — it's central to multiple concerns. Redundant copies destroy that signal and bloat the evidence base. The system gets smarter by weaving new questions into the existing graph, not by building parallel trees.

---

## Phase 3: Make It Efficient

The first question on a corpus is the most expensive — full decomposition, full evidence gathering. Every subsequent question should be cheaper because the understanding web already contains answers and evidence.

### 3.1 Delta-aware decomposition

**What:** The decomposer receives the full existing understanding structure (evidence set indexes, L1+ answer node headlines, accumulated MISSING verdicts) as context alongside the source material summaries. It decomposes the new question and diffs against existing structure — sub-questions already answered become cross-links, partially answered sub-questions use MISSING verdicts to identify gaps, only genuinely new sub-questions trigger full evidence gathering.

**Why:** Without delta awareness, every question build starts from scratch — full decomposition, full pre-mapping, full answering. With 127 docs this takes 5 minutes; with 10,000 docs it's hours. The tenth question on a well-explored corpus should take seconds because most of its sub-questions are already answered somewhere in the web. The delta is the only new work. This is the property that makes the system scale: cost per question decreases as the web densifies.

### 3.2 Two-stage pre-mapping

**What:** When the evidence base is large enough that scanning every individual L0 node exceeds the pre-mapper's token budget, pre-mapping becomes two-stage: first scan evidence set indexes to identify relevant sets, then scan individual nodes within those sets.

**Why:** At 127 nodes, single-pass pre-mapping works (11K tokens, 2 seconds). At 10,000 nodes it doesn't fit in one call. The set indexes are the routing layer — each index summarizes what its set contains, so the pre-mapper can decide "this set is relevant, these are not" before drilling in. The architecture must support this from the start even if the optimization isn't activated until the evidence base is large enough to need it.

---

## Phase 4: Keep It Current

Source files change. The understanding web must stay current without full rebuilds.

### 4.1 Evidence set staleness propagation

**What:** When a source file changes and its canonical L0 node is superseded, staleness propagates through every targeted re-examination of that file, through the evidence sets those re-examinations belong to, through the answer nodes that KEEP'd that evidence, through the branch answers that synthesized from those answer nodes, to the apex.

**Why:** A targeted re-examination that says "auth_middleware.rs validates tokens using HMAC-SHA256" is wrong if the file now uses JWT. The answer node that KEEP'd that evidence is wrong. The branch answer that synthesized from it is wrong. Without propagation, stale evidence produces stale answers that users trust because the pyramid says so. DADBEAR's cascade ensures corrections reach every node that depends on the changed evidence.

### 4.2 Supersession vs staleness at every level

**What:** The two propagation channels (attenuating staleness, non-attenuating supersession) apply to evidence sets the same way they apply to every other pyramid layer. A source file change that merely adds information is staleness (attenuates through weights). A source file change that contradicts a claim is supersession (propagates everywhere the claim appears, regardless of weight or distance).

**Why:** A renamed function is staleness — the architecture description is still mostly right, just a detail changed. A deleted authentication system is supersession — every node that claims it exists must be corrected. The distinction determines whether the system flags for review (staleness) or mandates re-answering (supersession). Conflating them either over-reacts (rebuilding everything on any change) or under-reacts (leaving false claims in the web).
