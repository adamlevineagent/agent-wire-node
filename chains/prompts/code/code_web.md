You are given all the nodes at a single depth level of a Knowledge Pyramid. Each node represents a subsystem or feature area with its headline, orientation, topics, and entity lists.

Your job: identify the CROSS-CUTTING CONNECTIONS between these sibling nodes. These are the relationships that a strict tree hierarchy misses — shared resources, caller/callee patterns, data flow dependencies.

RULES:
- `source` and `target` MUST be the exact `node_id` strings from the provided node list, never the headline text
- Only report SPECIFIC, CONCRETE connections — not "both are part of the system"
- Name the exact shared resource: table name, function name, endpoint, IPC channel, type definition, env var
- Each edge should help a developer understand: "if I change X in node A, I need to check node B because they share Y"
- Aim for 5-20 edges for a typical 8-12 node layer. More nodes = more edges, but quality over quantity.
- Do not emit both A→B and B→A for the same connection
- Do not emit self-edges
- Strength 0.9-1.0: direct caller/callee or shared mutable state (changing one breaks the other)
- Strength 0.6-0.8: shared read-only resource or common pattern (changing one may affect the other)
- Strength 0.3-0.5: conceptual relationship or shared dependency (good to know, not critical)

CONNECTION TYPES to look for:
- Shared database tables (both read/write the same table)
- Shared HTTP endpoints (one defines, another calls)
- Shared IPC channels (frontend invoke ↔ backend handler)
- Shared types/structs (defined in one, used in another)
- Auth dependency (one validates tokens that another issues)
- Data pipeline (output of one feeds into another)
- Shared external service (both call the same API)
- Error propagation (errors from one surface in another)

Output valid JSON only:
{
  "edges": [
    {
      "source": "L1-000",
      "target": "L1-003",
      "relationship": "Both read/write pyramid_nodes table; L1-000 ingests and L1-003 queries",
      "shared_resources": ["table: pyramid_nodes", "fn: db::save_node()"],
      "strength": 0.85
    }
  ]
}

/no_think
