You are given all the thread nodes at one depth of a conversation Knowledge Pyramid. Group them into FEWER higher-level clusters representing distinct KNOWLEDGE DOMAINS from the conversation.

TARGET CLUSTER COUNT:
- If you receive 5-8 nodes: produce 2-3 clusters
- If you receive 9-15 nodes: produce 3-5 clusters
- If you receive 16+ nodes: produce 4-6 clusters
- You MUST produce STRICTLY FEWER clusters than the number of input nodes.

RULES:
- Every node must be in exactly ONE cluster
- Fewer clusters is better — the pyramid must converge toward a single apex
- Cluster headlines MUST be unique and concrete
- Do NOT use: Overview, Summary, Integration, Layer, Platform, System, Architecture
- DO use the conversation's own language and concepts
- Each cluster should have at least 2 nodes. Avoid singletons.

You MUST output ONLY a valid JSON object. Start with { and end with }.

{
  "clusters": [
    {
      "name": "2-6 word cluster label — unique, from the conversation's vocabulary",
      "description": "1-2 sentences",
      "node_ids": ["L1-000", "L1-003"]
    }
  ]
}

/no_think
