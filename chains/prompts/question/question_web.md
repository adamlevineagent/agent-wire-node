You are given all the answer nodes at a single depth level of a question-driven Knowledge Pyramid. Each node represents an answer to a question with its headline, orientation, topics, and evidence summaries.

Your job: identify the CROSS-CUTTING CONNECTIONS between these sibling answers. These are the relationships that a strict question tree misses — shared evidence, overlapping concepts, causal dependencies between answers.

RULES:
- `source` and `target` MUST be the exact `node_id` strings from the provided node list, never the headline text
- Only report SPECIFIC, CONCRETE connections — not "both are about the system"
- Name what they share: the specific concept, mechanism, component, or evidence that links them
- Each edge should help a reader understand: "answer A and answer B are connected because they both address Y"
- Let the material decide how many edges. Quality over quantity — every edge should be specific and verifiable.
- Do not emit both A→B and B→A for the same connection
- Do not emit self-edges
- Strength 0.9-1.0: answers that directly depend on each other (understanding A requires understanding B)
- Strength 0.6-0.8: answers that share significant evidence or concepts (reading both gives richer picture)
- Strength 0.3-0.5: answers with thematic overlap (related but independently understandable)

CONNECTION TYPES to look for:
- Shared evidence sources (both answers draw from the same L0 nodes)
- Causal dependency (answer A describes a mechanism that answer B's topic depends on)
- Shared terminology or concepts (both define or use the same domain term)
- Complementary perspectives (one covers the "what", the other the "how" or "why")
- Contradiction or tension (answers that present different views on the same issue)
- Prerequisite knowledge (understanding A first makes B clearer)

Output valid JSON only:
{
  "edges": [
    {
      "source": "L1-000",
      "target": "L1-003",
      "relationship": "Both draw evidence from the authentication module; L1-000 covers auth flow, L1-003 covers security model",
      "shared_resources": ["auth module", "token validation"],
      "strength": 0.75
    }
  ]
}

/no_think
