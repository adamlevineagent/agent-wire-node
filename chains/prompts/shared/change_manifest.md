You are updating a knowledge synthesis node based on changes to its children. Instead of regenerating the synthesis from scratch, identify what SPECIFICALLY needs to change and produce a targeted update manifest.

The node's current content and its children's changes are provided below. Your job is to decide what — if anything — needs to change in the parent node given the new information.

RULES:
- Most updates only need distilled text changes. Don't touch headline unless the node's core meaning shifted.
- If a child was updated but the parent synthesis already captures the gist, say so — set distilled to null.
- Prefer small targeted updates over wholesale rewrites. Each update should be the minimum viable edit.
- identity_changed is TRUE only if the node's fundamental topic/coverage changed (very rare). When in doubt, set it to false.
- Topic operations: "add" for a genuinely new topic, "update" for a topic whose current text needs refinement, "remove" ONLY for a topic that is no longer relevant after the child changed.
- The reason field is mandatory: one sentence explaining what changed and why.
- Do NOT invent children_swapped entries the user did not ask about — only include the child id pairs you were told about in the input.

Output valid JSON in this exact shape:

{
  "node_id": "the_node_being_updated",
  "identity_changed": false,
  "content_updates": {
    "distilled": "new synthesis text OR null for no change",
    "headline": null,
    "topics": [
      { "action": "update", "name": "topic_name", "current": "new topic text" },
      { "action": "add", "name": "new_topic", "current": "description" },
      { "action": "remove", "name": "obsolete_topic" }
    ],
    "terms": null,
    "decisions": null,
    "dead_ends": null
  },
  "children_swapped": [
    { "old": "old_child_id", "new": "new_child_id" }
  ],
  "reason": "One sentence explaining what changed and why.",
  "build_version": <current_build_version_plus_one>
}

If nothing needs to change, still return a valid manifest with content_updates fields all null, an empty children_swapped, and a reason explaining why no update was needed. Set build_version to current_build_version + 1 anyway so the validator is satisfied.

Output JSON only. No prose before or after.

/no_think
