You are given the summaries of N nodes from a knowledge pyramid layer. Each node has a headline, orientation, and topic list.

PURPOSE: Identify the natural higher-level architectural domains. A developer at the next level up wants to explore by broad system area — what are the domains that organize everything below them?

APEX READINESS: Set `apex_ready: true` ONLY when ALL of these are true:
- There are roughly 12 or fewer nodes
- The nodes are already distinct, well-scoped architectural domains
- Grouping them further would only create vague super-categories that obscure rather than clarify
- A developer could hold all these domains in their head as the "top-level map" of this codebase

If you have more than 12 nodes, you almost certainly need to cluster. Group them.

If `apex_ready` is true, return empty clusters. The system will synthesize all current nodes directly into the apex.

PRINCIPLES:
- **Let the material decide.** The number of groups should reflect the natural architecture, not a target count. If there are genuinely 2 broad domains, produce 2. If there are 6, produce 6.
- **Every node must be assigned to exactly ONE cluster.**
- **Cluster names should be concrete and developer-friendly:** "Desktop UI & Frontend Components", not "Group 1"
- **CRITICAL: Cluster names must be COMPLETELY DIFFERENT from each other AND from the child node headlines AND from the project name.** Test: if you cover the names and read the descriptions, could you tell them apart? If not, rename them.
  - BAD: "Pyramid Knowledge Platform" + "Knowledge Pyramid Orchestration" (too similar)
  - GOOD: "Tauri Desktop UI Stack" + "Rust Backend & Data Layer" + "LLM Pipeline & Chain Execution" (each clearly distinct)
- **Avoid generic names:** "System Overview", "Platform Overview", "Project Architecture"
- **Use architectural LAYER or RESPONSIBILITY framing,** not product name repetition
- **The only hard rule: fewer groups than inputs.** If you receive 5 nodes, you must produce fewer than 5 groups. This is how the pyramid converges toward an apex.
- Think about what a NEW DEVELOPER would want to explore: "I want to understand the UI" → one cluster. "I want to understand the data pipeline" → another.

You MUST output ONLY a valid JSON object. No markdown, no headings, no prose, no explanation, no code fences. Start your response with { and end with }. Any text before { or after } is a fatal error.

{
  "apex_ready": false,
  "clusters": [
    {
      "name": "Cluster Name",
      "description": "1-2 sentences: what this architectural area covers and why these nodes belong together",
      "node_ids": ["L1-000", "L1-003", "L1-007"]
    }
  ]
}

/no_think
