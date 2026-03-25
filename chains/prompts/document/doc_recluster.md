You are given all the thread nodes at a single depth level of a document Knowledge Pyramid. Each represents a topic area with its headline, orientation, and sub-topics.

Group these threads into 3-5 higher-level clusters representing distinct KNOWLEDGE DOMAINS. Each cluster should be a coherent area that a reader would naturally explore as a unit.

RULES:
- Every node must be assigned to exactly ONE cluster
- 3-5 clusters. Fewer is better if coverage is complete.
- Cluster headlines MUST be unique and MUST NOT overlap with each other
- Do NOT use generic words: Overview, Summary, Integration, Layer, Platform, System
- DO use concrete domain names: "Auth & Security Decisions", "Build Pipeline Evolution", "Frontend Component Design"
- Each cluster should contain 2-5 nodes. Single-node clusters are acceptable only for truly distinct domains.

You MUST output ONLY a valid JSON object. No markdown, no prose, no code fences. Start with { and end with }.

{
  "clusters": [
    {
      "name": "2-6 word cluster label — unique and concrete",
      "description": "1-2 sentences on what this knowledge domain covers",
      "node_ids": ["L1-000", "L1-003", "L1-007"]
    }
  ]
}

/no_think
