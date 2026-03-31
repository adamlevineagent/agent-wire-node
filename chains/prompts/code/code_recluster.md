You are given the summaries of N nodes from a knowledge pyramid layer. Each node has a headline, orientation, and topic list.

Group these nodes into FEWER clusters. Each cluster should represent a high-level architectural domain that a developer would recognize — something they'd call "the frontend", "the backend", "the engine", etc.

TARGET CLUSTER COUNT:
- If you receive 5-8 nodes: produce 2-3 clusters
- If you receive 9-15 nodes: produce 3-5 clusters
- If you receive 16+ nodes: produce 4-6 clusters
- You MUST produce STRICTLY FEWER clusters than the number of input nodes. If given 5 nodes, produce at most 4 clusters (prefer 2-3).

RULES:
- Every node must be assigned to exactly ONE cluster
- Cluster names should be concrete and developer-friendly: "Desktop UI & Frontend Components", not "Group 1"
- CRITICAL: Cluster names must be COMPLETELY DIFFERENT from each other AND from the child node headlines AND from the project name. Test: if you cover the names and read the descriptions, could you tell them apart? If not, rename them.
  - BAD: "Pyramid Knowledge Platform" + "Knowledge Pyramid Orchestration" (too similar)
  - GOOD: "Tauri Desktop UI Stack" + "Rust Backend & Data Layer" + "LLM Pipeline & Chain Execution" + "CLI, MCP & External Integrations" (each clearly distinct)
- Before finalizing, compare the cluster names side by side. If two names share the same head noun or both read like "project overview", rename them into distinct architectural responsibilities.
- Explicitly avoid generic names like "System Overview", "Platform Overview", "Knowledge Pyramid Platform", "Wire Node System", "Project Architecture", or "Core Platform".
- Use architectural LAYER or RESPONSIBILITY framing, not product name repetition
- Balance: each cluster should have at least 2 nodes
- If a node doesn't fit cleanly, assign it to the closest match — do not create a singleton cluster
- Think about what a NEW DEVELOPER would want to explore: "I want to understand the UI" → one cluster. "I want to understand the data pipeline" → another cluster. "I want to understand the backend services" → another.

You MUST output ONLY a valid JSON object. No markdown, no headings, no prose, no explanation, no code fences. Start your response with { and end with }. Any text before { or after } is a fatal error.

{
  "clusters": [
    {
      "name": "Cluster Name",
      "description": "1-2 sentences: what this architectural area covers and why these nodes belong together",
      "node_ids": ["L1-000", "L1-003", "L1-007"]
    }
  ]
}

/no_think
