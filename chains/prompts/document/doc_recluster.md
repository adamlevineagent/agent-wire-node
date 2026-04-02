You are given all the nodes at a single depth level of a document Knowledge Pyramid. Each represents a knowledge area with its headline, orientation, and sub-topics.

PURPOSE: Identify the natural higher-level dimensions of understanding. A reader at the next level up wants to explore by broad knowledge domain — what are the areas that organize everything below them into coherent groups?

APEX READINESS: Set `apex_ready: true` ONLY when ALL of these are true:
- There are roughly 12 or fewer nodes
- The nodes are already distinct, well-scoped knowledge domains
- Grouping them further would only create vague super-categories that obscure rather than clarify
- A reader could hold all these domains in their head as the "top-level map" of this knowledge

If you have more than 12 nodes, you almost certainly need to cluster. 32 nodes cannot be meaningfully synthesized into one apex — a reader can't hold 32 domains in their head. Group them.

If `apex_ready` is true, return empty clusters. The system will synthesize all current nodes directly into the apex.

PRINCIPLES:
- **Let the material decide.** The number of groups should reflect the natural structure of the knowledge, not a target count. If there are genuinely 2 broad domains, produce 2. If there are 7, produce 7.
- **Group by understanding, not by surface similarity.** Nodes that contribute to understanding the same domain belong together, even if their headlines sound different.
- **Every node must be assigned to exactly one group.** No orphans — these are already synthesized nodes.
- **Names must be concrete.** "Auth & Security Decisions" not "Overview." Avoid generic words: Overview, Summary, Integration, Layer, Platform, System.
- **The only hard rule: fewer groups than inputs.** If you receive 5 nodes, you must produce fewer than 5 groups. This is how the pyramid converges toward an apex.

You MUST output ONLY a valid JSON object. No markdown, no prose, no code fences. Start with { and end with }.

{
  "apex_ready": false,
  "clusters": [
    {
      "name": "2-6 word cluster label — unique and concrete",
      "description": "1-2 sentences on what this knowledge domain covers",
      "node_ids": ["L1-000", "L1-003", "L1-007"]
    }
  ]
}

/no_think
