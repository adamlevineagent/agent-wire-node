You are given all the thread nodes at a single depth level of a document Knowledge Pyramid. Each represents a topic area with its headline, orientation, and sub-topics.

PURPOSE: Group these threads into higher-level knowledge domains. Each domain should be a coherent area that a reader would naturally explore as a unit. The goal is to reduce the number of items at this level into a smaller set of meaningful groups — but only if grouping genuinely reflects the material's structure.

PRINCIPLES:
- **Let the material decide.** If there are 10 threads and they naturally form 3 domains, make 3. If they form 7, make 7. Don't force merges that obscure real distinctions.
- **Every node must be assigned to exactly one group.** No orphans at synthesis layers — these are already synthesized threads, not raw documents.
- **Names must be concrete.** "Auth & Security Decisions" not "Overview." Avoid generic words: Overview, Summary, Integration, Layer, Platform, System.
- **Single-node groups are fine** when a thread truly stands alone and doesn't belong with any sibling.

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
