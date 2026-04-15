# Pyramid CLI: Experience & Friction Log
**Agent**: Antigravity (Partner)
**Session Target**: `lens-1` Knowledge Pyramid

## Executive Summary & Explainer
The Pyramid CLI provides a powerful, programmatic interface for AI agents to interact with the **Knowledge Pyramid**—a self-organizing memory system that recursively synthesizes raw data into structured, layered intelligence. Instead of relying on passive context windows, agents can actively navigate this structure through the `apex`, `search`, and `drill` commands to pull exactly the context they need (which solves the "Optimal Knowledge Problem").

Key architecture details discovered via the CLI:
*   **River-Graph Boundary**: Raw data flows ephemerally in the "River", but only adversarially-evaluated intelligence earns permanent entry into the graph.
*   **Recursive Synthesis**: Siblings without a parent collapse into one synthesis node naturally.
*   **DADBEAR Auto-Update Loop**: Uses an LLM evaluation loop and WAL logs to surgically update staleness.
*   **Cross-Hierarchy Web Edges**: Features lateral linkages (spiderwebs vs vines) that keep context connected across different branches.

---

## The Log: Actions Taken
1.  **Exploration**: 
    *   Checked system health with `health`.
    *   Listed all pyramids with `slugs`.
    *   Queried the top-level synthesis using `apex lens-1`.
    *   Ran a targeted `search` for "River-Graph".
    *   Performed deep `drill` queries on `L1-5f94e93d-4a5c-4235-a435-036d80b4ff4a` and `Q-L0-020`.
2.  **QA / Testing**: 
    *   Attempted to query the FAQ for terms exactly matching the `apex` glossary using `faq lens-1 "What is the River-Graph boundary?"`.
    *   Checked the FAQ directory structure with `faq-dir lens-1`.
    *   Tested the `DADBEAR` system status queries.
3.  **Contribution**: 
    *   Deposited two annotations using the `annotate` command to map generalized understandings and trigger FAQ formulation.

---

## ✅ Positive Experiences

*   **Atomic Precision via `drill`**: The `drill` command is fantastic. It not only provides the node's distilled synthesis and underlying decisions, but it also returns **`web_edges`**. Seeing that `Q-L0-020` connected laterally to `Q-L0-022` (the actual prompt templates for DADBEAR) gave me immediate, traversable context that a simple RAG search could never achieve.
*   **Annotation -> FAQ Pipeline**: The `annotate` command works seamlessly. Crucially, I discovered that submitting an annotation with the `--question` flag automatically integrates it into the background FAQ processing loop. This transforms agents from passive readers into active knowledge contributors.
*   **Search Relevancy**: The `search` command is snappy and returns depth levels and scores, allowing me to quickly target `L2` or `L3` nodes depending on how high-level my needed context was.

## ⚠️ Friction Points & Constructive Feedback

*   **FAQ Rigidity**: I queried `faq lens-1 "What is the River-Graph boundary?"`. Even though "River-Graph boundary" is an explicitly defined term in the `apex` output, the FAQ engine returned 0 matches. The FAQ seems heavily bound to question annotations rather than falling back to the robust `terms` dictionaries stored in the pyramid nodes. 
*   **Information Overload on `apex`**: `apex` returns a massive JSON payload out of the box. While `--compact` exists, having a simplified summary mode that *just* returns the highest-level synthesis (without the full terms, corrections, dead ends, and children manifest) might be beneficial for agents with smaller context bounds.
*   **Opaque DADBEAR Status**: The user prompt mentioned checking DADBEAR status (`Auto-update: disabled`, `Debounce: ? minutes`, etc.). However, there is no top-level CLI command (like `pyramid-cli dadbear-status`) to easily introspect the current rotation, wait times, or WAL queue length. I had to infer the architecture from node `Q-L0-020` instead.
*   **Finding "Home"**: When drilling into deep nodes like `Q-L0-020`, there's no reverse-tree summary to show me where I am in the pyramid (e.g., `L0 -> L1-foo -> L2-bar -> L3-apex`). I can see the `parent_id`, but I have to do multiple `node` calls to traverse upwards.

*Overall, this tool creates a highly effective "agentspace" protocol. It shifts the burden of context management off the infrastructure and directly onto the active agent.*
