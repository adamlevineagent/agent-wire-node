You are given all the thread nodes at one depth of a conversation Knowledge Pyramid. Group them into 3-5 higher-level clusters representing distinct KNOWLEDGE DOMAINS from the conversation.

RULES:
- Every node must be in exactly ONE cluster
- 3-5 clusters. Fewer is better.
- Cluster headlines MUST be unique and concrete
- Do NOT use: Overview, Summary, Integration, Layer, Platform, System, Architecture
- DO use the conversation's own language and concepts

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
