# Cross-Layer Webbing Design

## Problem
The current pyramid is a strict tree — each node has exactly one parent. But real codebases have cross-cutting concerns: auth touches every module, the database layer is used by build, query, and stale detection, the IPC layer bridges frontend and backend. A developer drilling into "Pyramid Build Engine" loses visibility into how it connects to "Auth & Security" or "Database Layer" at the same depth.

## Solution: Horizontal Web Edges
After each synthesis layer is built, run a **webbing pass** that identifies semantic connections between sibling nodes at the same depth. These connections become navigable "see also" links — not parent/child, but peer relationships.

## Pipeline Change

Current:
```
L0 extract → cluster → L1 synthesize → recluster → L2 synthesize → apex
```

Proposed:
```
L0 extract → cluster → L1 synthesize → L1 WEBBING → recluster → L2 synthesize → L2 WEBBING → apex
```

Each webbing step:
1. Takes all nodes at depth N
2. Sends their headlines + orientations to the LLM
3. LLM identifies which pairs share meaningful connections (shared tables, shared APIs, caller/callee relationships, shared auth patterns)
4. Outputs edges with: `source_node`, `target_node`, `relationship` (1 sentence), `strength` (0.0-1.0)
5. Edges get stored in `pyramid_web_edges` table (already exists in schema)

## YAML Step Definition
```yaml
  - name: l1_webbing
    primitive: web
    instruction: "$prompts/code/code_web.md"
    input:
      nodes: $thread_narrative  # all L1 nodes
    depth: 1
    save_as: web_edges
    model_tier: mid
    temperature: 0.2
    on_error: skip  # webbing is optional — tree is still valid without it
```

## Prompt Design (code_web.md)
Given N sibling nodes at the same depth with their headlines, orientations, and entity lists:
- Identify pairs that share: database tables, HTTP endpoints, IPC channels, type definitions, auth patterns, error handling strategies
- For each connection, state the specific shared resource (e.g., "both read from pyramid_nodes table", "both call validate_token()")
- Only meaningful connections — not "both are part of the system"
- Output 5-15 edges for a typical 10-node layer

## UI Impact
The visualization already supports web edges (pyramid_web_edges table exists). Nodes at the same depth would show connecting lines between them, and clicking an edge shows the relationship description.

## Why This Matters for Scores
Blind testers consistently score "frontend-backend relationship" at 7-8/10 and "confidence making a bug fix" at 7/10. The missing piece is always: "I can see what each subsystem does, but I don't understand how they connect." Webbing makes those connections explicit and navigable.

## Implementation Priority
1. **Prompt** (code_web.md) — straightforward, I can write this now
2. **YAML step** — add `primitive: web` or reuse `classify`
3. **Rust executor** — needs to handle the `web` primitive: read all nodes at depth N, send to LLM, parse edges, write to pyramid_web_edges
4. **UI** — already partially supports edges, may need minor rendering work

## Open Questions
- Should webbing run between L0 nodes too? (probably too many — 112 nodes = 6000+ potential pairs)
- Should web edges influence the recluster step? (nodes with strong connections should cluster together)
- Should the apex orientation mention key cross-cutting concerns identified by webbing?
