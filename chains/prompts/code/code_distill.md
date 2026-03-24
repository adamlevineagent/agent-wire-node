You read two sibling nodes describing parts of a codebase. Organize everything they contain into coherent TOPICS.

A topic is a named system, module, or capability that groups together all related components, APIs, data models, and architectural patterns. Everything we know about that subject belongs in that bundle.

Your job is to understand both siblings and decide: what are the 3-6 coherent topics that organize everything here? A developer should scan your topic names and immediately know which topic to drill into for what they need.

Merge topics that cover the same system. If both siblings discuss overlapping functionality, that is ONE topic, not two.

For each topic:
- name: a clear, descriptive name (e.g., "Authentication & Session Management", not "Auth")
- current: 1-2 sentences describing what this system IS — be specific about technologies, patterns, and key components
- entities: the specific named types, functions, files, APIs, tables, and endpoints in this topic
- corrections: leave empty (code nodes have no temporal corrections)
- decisions: leave empty (code nodes have no decisions)
- headline: a 2-6 word label for the parent node. Concrete and specific. No "This node..."

IMPORTANT: Preserve concrete details. Names of functions, types, endpoints, tables, file paths — these are what make the pyramid useful. Do NOT abstract "AuthState, send_magic_link, validate_token" into "authentication functions". Keep the specific names.

Output valid JSON only:
{
  "headline": "2-6 word node label",
  "orientation": "1-2 sentences: what this node covers. Which children to drill for which topics.",
  "topics": [
    {
      "name": "Topic Name",
      "current": "What this system IS. Technologies, patterns, key components.",
      "entities": ["SpecificType", "specific_function", "specific_endpoint"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think