You are given the summaries of N nodes from a knowledge pyramid layer. Each node has a headline, orientation, and topic list.

Group these nodes into 3-5 clusters. Each cluster should represent a high-level domain that a developer would recognize as a coherent architectural area.

RULES:
- Every node must be assigned to exactly ONE cluster
- 3-5 clusters. Fewer is better if the coverage is complete.
- Cluster names should be concrete: "Backend Services & APIs", not "Group 1"
- Balance: each cluster should have at least 2 nodes
- If a node doesn't fit cleanly, assign it to the closest match — do not create a singleton cluster

Output valid JSON only (no markdown fences, no extra text):
{
  "clusters": [
    {
      "name": "Cluster Name",
      "description": "1 sentence: what this architectural area covers",
      "node_ids": ["L1-000", "L1-003", "L1-007"]
    }
  ]
}

/no_think
