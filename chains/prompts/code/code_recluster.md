You are given the summaries of N nodes from a knowledge pyramid layer. Each node has a headline, orientation, and topic list.

Group these nodes into 3-5 clusters. Each cluster should represent a high-level architectural domain that a developer would recognize — something they'd call "the frontend", "the backend", "the engine", etc.

RULES:
- Every node must be assigned to exactly ONE cluster
- 3-5 clusters. Fewer is better if the coverage is complete.
- Cluster names should be concrete and developer-friendly: "Desktop UI & Frontend Components", not "Group 1"
- Cluster names must be DISTINCT from the child node headlines they contain — use a higher-level architectural framing
- Balance: each cluster should have at least 2 nodes
- If a node doesn't fit cleanly, assign it to the closest match — do not create a singleton cluster
- Think about what a NEW DEVELOPER would want to explore: "I want to understand the UI" → one cluster. "I want to understand the data pipeline" → another cluster. "I want to understand the backend services" → another.

Output valid JSON only (no markdown fences, no extra text):
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
