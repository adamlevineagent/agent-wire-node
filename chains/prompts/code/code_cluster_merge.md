You are merging thread clustering results from multiple batches of source files in a codebase. Each batch independently grouped its files into subsystem threads. Your job is to unify these into a single coherent set of threads for the entire codebase.

You receive an array of batch results. Each batch result has a `threads` array. Files across batches that belong to the same subsystem or architectural layer should end up in the same thread.

PRINCIPLES:
- **Merge threads about the same subsystem.** If batch 1 has "Auth & Session State" and batch 2 has "Authentication Middleware", those are the same thread — merge them. Use the most precise name.
- **ZERO ORPHANS: Every single C-L0-XXX from every batch result must appear in exactly one thread assignment.** No file may be left out. There is no `unassigned` escape hatch — every source file belongs to a real subsystem. If a file seems tangential, it goes with the system it relates to most.
- **Let the material decide the final count.** Don't force-merge unrelated subsystems just to reduce count. If 6 distinct architectural layers exist, output 6 threads.
- **Subsystem names should be concrete and developer-readable:** "Chain Execution Engine", "Pyramid Explorer UI", "Tauri Desktop Shell", not "Group 1" or "Utilities".
- **Keep threads focused.** If merging creates a very large thread (10+ files), consider whether it's actually two related subsystems that deserve separate threads.
- **CRITICAL: `assignments[].source_node` MUST be the exact `C-L0-XXX` ID copied verbatim from the batch results. Do NOT use the headline in this field.**

After generating your output, verify: does every C-L0-XXX ID from every batch result appear in exactly one thread? If any are missing, add them now.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name — subsystem or architectural layer",
      "description": "1-2 sentences: what this subsystem does and why these files belong together",
      "assignments": [
        {"source_node": "C-L0-000", "topic_index": 0, "topic_name": "Headline"},
        {"source_node": "C-L0-007", "topic_index": 7, "topic_name": "Headline"}
      ]
    }
  ]
}

/no_think
