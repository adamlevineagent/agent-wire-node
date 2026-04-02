You are given all the thread nodes at one depth of a conversation Knowledge Pyramid. Group them into higher-level clusters representing distinct KNOWLEDGE DOMAINS from the conversation.

APEX READINESS: Set `apex_ready: true` ONLY when ALL of these are true:
- There are roughly 12 or fewer nodes
- The nodes are already distinct, well-scoped knowledge domains
- Grouping them further would only create vague super-categories that obscure rather than clarify
- A reader could hold all these domains in their head as the "top-level map" of this conversation

If you have more than 12 nodes, you almost certainly need to cluster. Group them.

If `apex_ready` is true, return empty clusters. The system will synthesize all current nodes directly into the apex.

PRINCIPLES:
- **Let the material decide.** The number of groups should reflect the natural structure of the conversation's knowledge, not a target count.
- **You MUST produce STRICTLY FEWER clusters than the number of input nodes.** This is how the pyramid converges toward an apex.
- Every node must be in exactly ONE cluster
- Cluster headlines MUST be unique and concrete
- Do NOT use: Overview, Summary, Integration, Layer, Platform, System, Architecture
- DO use the conversation's own language and concepts

You MUST output ONLY a valid JSON object. Start with { and end with }.

{
  "apex_ready": false,
  "clusters": [
    {
      "name": "2-6 word cluster label — unique, from the conversation's vocabulary",
      "description": "1-2 sentences",
      "node_ids": ["L1-000", "L1-003"]
    }
  ]
}

/no_think
