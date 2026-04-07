You are given all the answer nodes at a single depth level of a question-driven Knowledge Pyramid. Each node represents an answer to a question with its headline, orientation, topics, and evidence summaries.

Your job: identify the CROSS-CUTTING CONNECTIONS between these sibling answers. These are the relationships that a strict question tree misses — shared evidence, overlapping concepts, causal dependencies between answers.

RULES:
- `source` and `target` MUST be the exact `node_id` strings from the provided node list, never the headline text
- Only report SPECIFIC, CONCRETE connections — not "both are about the system"
- Name what they share: the specific concept, mechanism, component, or evidence that links them
- Each edge should help a reader understand: "answer A and answer B are connected because they both address Y"
- Let the material decide how many edges. Quality over quantity — every edge should be specific and verifiable.
- If the nodes provided contain insufficient detail to identify specific, concrete connections, return `{"edges": []}`. Never fabricate connections from thin descriptions or headlines alone. An empty result is correct when the evidence for connections is absent.
- Do not emit both A→B and B→A for the same connection
- Do not emit self-edges
- Strength 0.9-1.0: answers that directly depend on each other (understanding A requires understanding B)
- Strength 0.6-0.8: answers that share significant evidence or concepts (reading both gives richer picture)
- Strength 0.3-0.5: answers with thematic overlap (related but independently understandable)

CONNECTION TYPES to look for:

Conceptual:
- Shared evidence sources (both answers draw from the same L0 nodes)
- Shared terminology or concepts (both define or use the same domain term)
- Complementary perspectives (one covers the "what", the other the "how" or "why")
- Contradiction or tension (answers that present different views on the same issue)

Temporal/Causal:
- Prerequisite knowledge (understanding A first makes B clearer — prefix with "prerequisite:")
- Causal dependency (A describes a mechanism that B depends on — prefix with "enables:")
- Resolution arc (B resolves an issue or question raised in A — prefix with "resolves:")
- Supersession (B replaces, updates, or overrides A — prefix with "supersedes:")
- Reversal (B contradicts or undoes a decision from A — prefix with "reverses:")
- Triggered by (A caused B to happen or exist — prefix with "triggers:")
- Evolution (B is a refined version of A's approach — prefix with "evolves:")

For temporal/causal edges, prefix the `relationship` field with the type so downstream consumers can distinguish them:
  "relationship": "resolves: the auth redesign in L1-005 fixed the permission gap identified in L1-002"
  "relationship": "prerequisite: understanding the credit system (L1-001) is required before the market mechanics (L1-004)"
  "relationship": "supersedes: V2 pricing model replaces the original flat-rate approach"

Conceptual edges do NOT need a prefix — just describe the specific shared resource or overlap as before.

Not every pair has a temporal relationship, and not every pair has a conceptual one. Emit what genuinely exists. An empty result is correct when connections are absent.

Output valid JSON only:
{
  "edges": [
    {
      "source": "L1-000",
      "target": "L1-003",
      "relationship": "Both draw evidence from the authentication module; L1-000 covers auth flow, L1-003 covers security model",
      "shared_resources": ["auth module", "token validation"],
      "strength": 0.75
    },
    {
      "source": "L1-002",
      "target": "L1-005",
      "relationship": "resolves: L1-005 addresses the permission model gap identified in L1-002's analysis",
      "shared_resources": ["permission system", "role-based access"],
      "strength": 0.85
    }
  ]
}

/no_think
