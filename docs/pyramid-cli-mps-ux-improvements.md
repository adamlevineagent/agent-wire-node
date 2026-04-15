# Pyramid CLI: Maximal Potential Solution — UX Improvements

**Source**: First-contact testing by two independent agents + API surface audit  
**Scope**: CLI (`cli.ts`) + MCP Server (`index.ts`) — the agent-facing interface  
**Goal**: Make the Knowledge Pyramid the best possible tool for AI agents navigating complex knowledge

---

## Tier 1: Close Existing Gaps (fixes to things that exist but don't work fully)

### 1.1 Inline Annotations in Drill Response
**Problem**: `drill` returns `{node, children, evidence, gaps, question_context}` but NOT annotations. The `_note` on annotate claims they're "visible via drill" — they aren't. Agents doing top-down navigation never encounter contributed knowledge.

**Fix**: Add `annotations[]` to `DrillResult`. Even just a count + most recent 3 would close the loop. If a node has annotations, agents should see them without a separate call.

**Impact**: 🔴 This is the #1 compound knowledge blocker. Every annotation deposited today is invisible to the next agent unless they think to call `annotations` separately.

---

### 1.2 Search ↔ FAQ Cross-Referral
**Problem**: Search returns `[]` with no hint. FAQ returns `{"matches": [], "message": "No FAQ entries matched"}` with no hint. Two retrieval systems in complete isolation.

**Fix**: When search returns 0 results, append a `_hint` field: `"_hint": "No keyword matches. Try pyramid_faq_match for natural-language questions."` When FAQ returns 0 matches, append: `"_hint": "No FAQ matches. Try pyramid_search with specific keywords from the apex terms."` 

**Impact**: 🔴 Prevents dead-end 0-result experiences. Zero cost.

---

### 1.3 Expose `tree` Command in CLI + MCP
**Problem**: The backend already has `GET /pyramid/:slug/tree` (line 755 routes.rs) returning `Vec<TreeNode>`. It's NOT exposed in the CLI or MCP server.

**Fix**: Add `tree <slug>` CLI command and `pyramid_tree` MCP tool. Format output as indented hierarchy: `L3 > L2 > L1 > L0` with headline + child count per node.

**Impact**: 🟡 One-call structural overview. Agents can see the whole pyramid shape without iterative drilling.

---

### 1.4 Breadcrumb Path in Drill
**Problem**: When deep in a node, there's no reverse-tree summary. `parent_id` exists but agents must make N additional `node` calls to traverse upward.

**Fix**: Server-side: walk `parent_id` chain and include `breadcrumb: [{id, headline, depth}]` in the drill response. An array from apex → current node.

**Impact**: 🟡 Instant "where am I?" awareness. Trivial implementation — just walk `parent_id` up.

---

### 1.5 DADBEAR Status Command
**Problem**: No CLI command to inspect auto-update state. Agents must infer from nodes or use raw SQLite. The handoff template references DADBEAR status fields it can't populate.

**Fix**: Route exists: `GET /pyramid/:slug/auto-update/status` (line 1050 routes.rs). Just wire it: `dadbear <slug>` CLI + `pyramid_dadbear_status` MCP tool.

**Impact**: 🟡 Operational transparency. Also fixes the handoff template placeholders.

---

### 1.6 Fix `self_prompt` Consistency
**Problem**: L1 nodes have a question in `self_prompt`. Q-L0 nodes have the full distilled content dumped into `self_prompt` (400+ chars, not a question). Inconsistent across pyramid types.

**Fix**: During build, ensure Q-L0 nodes store a generated question in `self_prompt` (e.g., from the source extraction prompt) and keep the full content only in `distilled`. If no question exists, synthesize one: "What does [source file] contain?"

**Impact**: 🟡 Agents can reliably use `self_prompt` as "the question this node answers" across all pyramid types.

---

## Tier 2: Improve Existing Capabilities (make good things better)

### 2.1 Intra-Depth Search Ranking
**Problem**: All results at the same depth get identical scores (L0=10, L1=20, L2=30, L3=40). No relevance differentiation within a layer. A search for "recursive synthesis" returns 5 L1 nodes all scored 20 — unranked.

**Fix**: Add FTS rank score as a secondary factor. SQLite FTS5 already provides `rank` — multiply `depth_score * (1 + normalized_fts_rank)` to differentiate within layers.

**Impact**: 🟡 Agents can actually pick the best match from search results, not just the first one.

---

### 2.2 Apex Summary Mode
**Problem**: `apex` returns a massive JSON payload — headline, distilled, all topics, all terms, all corrections, all decisions, all children, all dead_ends. For context-constrained agents, this burns a lot of tokens on structural data.

**Fix**: Add `--summary` flag (CLI) / `summary_only` param (MCP) that returns only: `{headline, distilled, self_prompt, children: [ids], terms: [{term, definition}]}`. Skip the full topic objects, corrections, decisions, dead_ends.

**Impact**: 🟡 Saves 40-60% of the apex token cost for agents that just need orientation.

---

### 2.3 Search Results with Structural Hints
**Problem**: Search results show `{node_id, depth, snippet, score}`. Agents can't tell if a hit is a leaf or a rich subtree, whether it has annotations, or how many children it has.

**Fix**: Add `child_count`, `annotation_count`, and `has_web_edges: bool` to `SearchHit`. All available from existing DB indexes with minimal query cost.

**Impact**: 🟢 Helps agents prioritize which search results to drill into.

---

### 2.4 Gap Resolution Confidence
**Problem**: Gaps like "Ablation studies" and "Comparison with academic literature" are marked `resolved: true` despite being inherently unanswerable. They used to represent research leads — now they're suppressed.

**Fix**: Change gap resolution from binary `resolved: bool` to `resolution_confidence: 0.0-1.0` where 0.0 = completely open, 1.0 = definitively answered. Threshold for "resolved" display at 0.8. Gaps with evidence but low confidence remain visible as leads.

**Impact**: 🟢 Preserves research direction value while still tracking resolution progress.

---

## Tier 3: Expose Hidden Backend Capabilities

The backend has ~20 read-only routes NOT exposed in the CLI or MCP. These are already built — they just need thin wrappers.

### 3.1 `entities <slug>` — Entity Index
**Route**: `GET /pyramid/:slug/entities`  
**Value**: Returns extracted entities (people, systems, concepts) across the pyramid. Agents can find "where is X mentioned?" without searching.

### 3.2 `terms <slug>` — Terms Dictionary
**Route**: `GET /pyramid/:slug/terms`  
**Value**: Standalone vocabulary lookup without loading the full apex. Cold-start agents learn the language instantly.

### 3.3 `corrections <slug>` — Correction Log
**Route**: `GET /pyramid/:slug/corrections`  
**Value**: "What was wrong in the source material that the pyramid corrected?" Directional signal for quality assessment.

### 3.4 `edges <slug>` — Web Edges
**Route**: `GET /pyramid/:slug/edges`  
**Value**: Full lateral connection graph. Agents can find cross-cutting themes without iterative drilling.

### 3.5 `threads <slug>` — Thread Clusters
**Route**: `GET /pyramid/:slug/threads`  
**Value**: L1 semantic clusters. Shows how L0 nodes were grouped. Useful for understanding the pyramid's organizational logic.

### 3.6 `cost <slug>` — Build Cost Report
**Route**: `GET /pyramid/:slug/cost`  
**Value**: Token/dollar cost of building this pyramid. Operational planning for agents triggering builds.

### 3.7 `stale-log <slug>` — Staleness History
**Route**: `GET /pyramid/:slug/stale-log`  
**Value**: Which nodes were re-evaluated, when, and why. Agents can assess pyramid freshness and trust.

### 3.8 `usage <slug>` — Access Patterns
**Route**: `GET /pyramid/:slug/usage?limit=100`  
**Value**: Which nodes are most accessed. Popularity signal for navigation prioritization.

### 3.9 `meta <slug>` — Meta Analysis Nodes
**Route**: `GET /pyramid/:slug/meta`  
**Value**: Post-build meta-analysis passes (webbing, entity resolution). Higher-order structural intelligence.

### 3.10 `resolved <slug>` — Resolved State
**Route**: `GET /pyramid/:slug/resolved`  
**Value**: Resolution status across the pyramid. Which questions have been answered, which remain open.

---

## Tier 4: New Capabilities (features that don't exist yet)

### 4.1 Semantic Search / NL→Keyword Rewriting
**Problem**: Search is FTS-only. Natural language queries ("how does the system handle failures") return 0 results. This is the biggest barrier for cold-start agents.

**Option A — LLM Rewrite**: Before FTS, pass the query through a fast model: "Extract 3-5 keyword phrases from this question that would match technical documentation." Use the keywords for FTS. Cost: 1 cheap LLM call per search. Latency: +500ms.

**Option B — Embedding Index**: Build a vector index of node distilled content at build time. Add `/pyramid/:slug/semantic-search?q=...` endpoint. Cost: embedding generation at build time + vector storage. Latency: +100ms per query.

**Option C — Hybrid**: FTS first; if 0 results, fall back to LLM rewrite + retry. Zero additional cost when keywords work, LLM cost only on failure.

**Recommended**: Option C (hybrid). It preserves the speed of FTS for agents who know the vocabulary while gracefully handling cold-start agents.

---

### 4.2 `diff <slug>` — Changelog Since Last Build
**Context**: DADBEAR tracks mutations and rebuild history. Agents re-visiting a pyramid need to know "what changed since I was last here?"

**Implementation**: Compare current build_id against the previous build. Show: new nodes, modified nodes, deleted nodes, changed evidence weights.

**Value**: Agents don't re-read the whole pyramid on return visits — they just read the diff.

---

### 4.3 `navigate <slug> <question>` — Guided Direct Answer
**Context**: An agent has a question. The pyramid has the answer somewhere. Currently the agent must: search → pick from results → drill → interpret. This is 3+ calls where 1 would suffice.

**Implementation**: LLM over search results: "Given these search hits, which node best answers: [question]?" Return the best node's content + a synthesized direct answer citing evidence.

**Cost**: 1 LLM call per navigation.

**Value**: One-shot question answering against the pyramid. The agent asks a question, gets an answer with provenance.

---

### 4.4 Annotation Reactions / Voting
**Context**: Multiple agents annotate. Some annotations are better than others. No mechanism to signal which annotations are most valuable.

**Implementation**: `react <slug> <annotation_id> <thumbs_up|thumbs_down>` — lightweight voting on annotations. Surfaces top-rated annotations first in drill.

**Value**: Annotation quality signal. The FAQ `hit_count` tracks usage but not quality. Reactions track quality.

---

### 4.5 `compare <slug1> <slug2>` — Cross-Pyramid Comparison
**Context**: An operator has multiple pyramids (code, docs, conversations). They overlap. No tooling to identify contradictions or gaps between them.

**Implementation**: Compare apex terms, decisions, and corrections across two slugs. Flag: terms defined differently, decisions made in one but not addressed in the other, corrections that contradict.

**Value**: Cross-pyramid coherence checking. Especially valuable for question pyramids that reference multiple sources.

---

### 4.6 Contextual `help` Per Command
**Context**: `--help` shows the command list, but no command has `--help` for its own flags and behavior. An agent trying `drill --help` gets nothing.

**Implementation**: Commander.js already supports per-command help — just add descriptions to each flag.

**Value**: Self-documenting CLI. Agents can discover capabilities without reading source code.

---

### 4.7 Agent Session Tracking
**Context**: Multiple agents explore the same pyramid. No mechanism to see "who was here before me and what did they find?"

**Implementation**: Lightweight session logging: `register <slug> --agent <name>` at start, auto-logged drill/search/annotate activity. `sessions <slug>` shows recent agent sessions.

**Value**: Agents can avoid duplicating each other's work. Coordination primitive for multi-agent swarms.

---

### 4.8 Export / Handoff Generation
**Context**: The pyramid access handoff block in the user's original message was manually written. It should be auto-generated.

**Implementation**: `handoff <slug>` generates a markdown block with:
- CLI commands pre-filled with the slug
- DADBEAR status populated from live config
- Top 3 FAQ questions
- Annotation count + types
- All ready to paste into an agent context

**Value**: Zero-friction pyramid onboarding for any new agent.

---

## Tier 5: Quality of Life

### 5.1 De-duplicate `faq` and `faq-dir`
They return identical results. Either differentiate (faq = match mode, faq-dir = listing mode) or alias one to the other.

### 5.2 Fix Empty Error Messages
- `{"error": "Node not found"}` → `{"error": "Node 'L99-does-not-exist' not found in pyramid 'lens-1'. Available L2 IDs: [L2-179e, L2-360f, ...]"}`
- `{"error": "No apex node found"}` → distinguish "slug doesn't exist" from "slug exists but not built"

### 5.3 Update CLI Example Slugs
`pyramid-cli apex agent-wire-nodepostdadbear` hardcoded in help text — replace with a current default slug or use `<your-slug>` placeholder.

### 5.4 `--json` and `--human` Output Modes
Default to compact JSON for programmatic agents, `--human` for readable formatted output with headers and summaries. Currently only `--pretty` vs `--compact` for JSON formatting.

### 5.5 Auth Resolution Feedback
`lib.ts` silently resolves auth from env var or config file. Add `--verbose` flag that prints auth resolution path to stderr.

---

## Implementation Priority Matrix

| Item | Effort | Impact | Priority |
|------|--------|--------|----------|
| **1.1** Inline annotations in drill | Low | 🔴 Critical | **P0** |
| **1.2** Search↔FAQ cross-referral | Low | 🔴 Critical | **P0** |
| **1.3** Expose tree command | Low | 🟡 High | **P1** |
| **1.4** Breadcrumb in drill | Low | 🟡 High | **P1** |
| **1.5** DADBEAR status command | Low | 🟡 High | **P1** |
| **3.1-3.10** Expose hidden routes | Low each | 🟡 Medium | **P1** |
| **2.1** Intra-depth ranking | Medium | 🟡 High | **P1** |
| **4.8** Handoff generation | Medium | 🟡 High | **P1** |
| **2.2** Apex summary mode | Low | 🟡 Medium | **P2** |
| **1.6** self_prompt consistency | Medium | 🟡 Medium | **P2** |
| **4.1** Semantic search (hybrid) | High | 🔴 Critical | **P2** |
| **4.2** Diff/changelog | Medium | 🟡 Medium | **P2** |
| **4.6** Per-command help | Low | 🟢 Nice | **P2** |
| **5.1-5.5** QoL fixes | Low each | 🟢 Nice | **P3** |
| **4.3** Navigate (guided answer) | High | 🟡 High | **P3** |
| **4.4** Annotation voting | Medium | 🟢 Nice | **P3** |
| **4.5** Cross-pyramid compare | High | 🟡 Medium | **P3** |
| **4.7** Session tracking | Medium | 🟢 Nice | **P3** |
| **2.3** Structural search hints | Low | 🟢 Nice | **P3** |
| **2.4** Gap resolution confidence | Medium | 🟢 Nice | **P3** |

> **P0** = Do first. Unblocks the compound knowledge promise.  
> **P1** = Low-effort, high-return. Many are just wiring existing backend routes.  
> **P2** = Medium effort, meaningful improvement.  
> **P3** = Longer-term value. Build when the core is solid.
