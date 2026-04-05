You are synthesizing a structural "Domain Node" representing a logical sector of the corpus map.

You have a set of L0 evidence nodes that all belong to this domain. Your task is to act as the "Apex" for this domain, summarizing what it does as a cohesive unit.

**CRITICAL REQUIREMENT (The Evidence Pointer):** 
Unlike a normal narrative component, this Domain Node acts as a structural proxy for a graph edge map. In the `source_nodes` array, you MUST output the EXACT `node_id` strings (e.g. `Q-L0-015`) of **every single node** that was provided to you in the prompt. By holding these direct pointers to the foundation layers, when the domain nodes are webbed together, the structural map carries direct source logic automatically.

RULES:
- `orientation` is a complete functional overview of what this domain represents across its constituents.
- `topics` capture the primary capabilities or conceptual pillars this domain is responsible for. Do NOT just summarize each input file individually. You must evaluate the domain through a MULTI-LENS ABSTRACTION FRAMEWORK:
  1. The Value/Intent Lens
  2. The Kinetic/State Flow Lens
  3. The Temporal Lens (relative timing, lifecycles)
  4. The Metaphorical Organ Lens
- `source_nodes` must be a flat array of string IDs explicitly declaring exactly which L0 inputs fed into this domain (e.g. `["Q-L0-000", "Q-L0-003"]`). Inclusion is mandatory; omitting a source node unlinks the graph.

Output valid JSON only:
{
  "orientation": "The overarching macro-level function and identity of this structural domain.",
  "topics": [
    {
      "name": "Concept Pillar Name",
      "summary": "High-level summary of this pillar.",
      "current": "More detailed explanation of how the inputs create this capability or concept.",
      "entities": ["concept: X", "entity: Y"]
    }
  ],
  "source_nodes": [
    "Q-L0-000",
    "Q-L0-001"
  ]
}

/no_think
