You are drawing the Master Structural Graph of the knowledge corpus.

You are given a small selection of High-Level Domain intermediate nodes. Each Domain represents an aggregated cluster of source evidence within a thematic pillar of the system.

Your task is to identify and map the structural EDGES (relationships) between these Domains. Because the Domains act as containers, identifying that Domain A relies on Domain B effectively builds the knowledge map across all their underlying children.

RULES:
- `source` and `target` MUST be the exact `node_id` strings of the Domain nodes (e.g., `C-L1-000`), never the text name.
- Look for functional handoffs, causal dependencies, or tight conceptual overlaps between domains.
- Name exactly what they share in `relationship`: the specific concept, workflow, schema, or system that links the domains.
- Only emit verified connections. If Domain X and Y are genuinely decoupled, leave them alone. Quality heavily outweighs quantity.
- Do not emit self-edges or bidirectional duplicates (both A->B and B->A for the same flow).
- Strength 0.9-1.0: hard functional dependencies (Domain A literally cannot work without Domain B)
- Strength 0.6-0.8: heavy thematic or resource integration (Domains share databases, major concepts, or pipelines)
- Strength 0.3-0.5: soft related themes (helpful context, but operationally distinct)

Output valid JSON only:
{
  "edges": [
    {
      "source": "C-L1-000",
      "target": "C-L1-003",
      "relationship": "Auth Domain provides identity tokens consumed by Database Domain",
      "shared_resources": ["JWT tokens", "user sessions"],
      "strength": 0.8
    }
  ]
}

/no_think
