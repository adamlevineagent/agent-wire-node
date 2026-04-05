You are grouping a collection of extracted L0 source nodes into structural "Domains" to build a web map of the corpus.

You have the extracted concepts for each node, which may be dehydrated to fit into context (just `headline` and `orientation/summary`). Use whatever fields are present to group nodes into high-level thematic areas.

PURPOSE: Group nodes into 3–5 broad conceptual domains using a MULTI-VIEWPOINT ABSTRACTION FRAMEWORK. Do NOT group nodes based on their file architecture, file extensions, or technical implementation (e.g. "UI Components" or "Config Files" are strictly forbidden). You must look at the corpus through multiple abstract lenses:
1. **The Value/Intent Lens**: What human/business value does this group enable?
2. **The Kinetic/State Flow Lens**: How do data, leverage, and events move through this space?
3. **The Temporal Lens**: How do these components relate to time relative to each other? (e.g., The Pre-flight Definitions, The Active Cycle, The Historical Archive)
4. **The Metaphorical Lens**: If this system was a living ecosystem, what distinct organ or metabolic function is this?

PRINCIPLES:
- **Group by abstract perspective, not trivial taxonomy.** (e.g., "The Perception Boundary", "State Consensus Engine", "Temporal Build Anchors").
- **Nodes can appear in multiple domains.** If a node genuinely bridges two perspectives, assign it to both.
- **Let the material decide.** Follow the natural thematic boundaries.
- **Name domains by their systemic abstraction.** Name what the domain IS conceptually, not what literal files it contains.
- **ZERO ORPHANS:** Every `node_id` in the input MUST appear in at least one domain. Missing elements is a critical failure.

Output `assignments` — just which source nodes belong in each domain. One entry per node, just the `source_node` ID. Do NOT list individual topics as separate assignments.

Output valid JSON only:
{
  "domains": [
    {
      "name": "Domain Name — broad narrative or conceptual area",
      "description": "Summary of what overarching thematic, structural, or narrative region this domain covers",
      "assignments": [
        "Q-L0-000",
        "Q-L0-007",
        "Q-L0-015"
      ]
    }
  ]
}

/no_think
