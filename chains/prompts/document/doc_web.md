You are given all the nodes at a single depth level of a document Knowledge Pyramid. Each node represents a topic area with its headline, orientation, topics, and entity lists.

Your job: identify CROSS-CUTTING CONNECTIONS between these sibling nodes — shared references, people, systems, decisions, or dependencies that span multiple topic areas.

RULES:
- `source` and `target` MUST be exact node_id strings, never headline text
- Only report SPECIFIC connections — not "both are about the project"
- Name the exact shared element: person, system, document, decision, bug, metric
- Each edge should help a reader understand: "these two topic areas are connected because they both involve X"
- 5-20 edges for a typical 6-12 node layer
- Do not emit both A→B and B→A
- Do not emit self-edges
- Strength 0.9-1.0: direct dependency (one's conclusions affect the other's decisions)
- Strength 0.6-0.8: shared context (same people, systems, or timeline)
- Strength 0.3-0.5: thematic relationship (similar concerns, related domains)

CONNECTION TYPES to look for:
- Shared people (same person mentioned in both — author, reviewer, stakeholder)
- Shared systems/components (both discuss the same technical system)
- Decision chains (one thread's decision is referenced or built upon in another)
- Bug/fix relationships (bug found in one area, fix applied in another)
- Temporal dependencies (one happened before/after and influenced the other)
- Contradictions (two threads have conflicting conclusions — flag these!)

Output valid JSON only:
{
  "edges": [
    {
      "source": "L1-000",
      "target": "L1-003",
      "relationship": "Both discuss the pyramid_nodes schema; L1-000 found the FK bug, L1-003 documents the fix",
      "shared_resources": ["system: pyramid_nodes", "bug: FK constraint #142"],
      "strength": 0.9
    }
  ]
}

/no_think
