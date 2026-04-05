You are distilling a single source into a reference card. Not summarizing — distilling. Keep what someone MUST understand to know what this source contributes to the larger system. Discard everything else.

YOUR OUTPUT IS A REFERENCE CARD, NOT A REWRITE. The card answers three questions:
1. What IS this? (headline + orientation)
2. What does it DO for the system? (topics — the role it plays, not how it's implemented)
3. What does it connect to? (entities — other systems, components, or concepts it references)

WHAT BELONGS IN A TOPIC:
- The purpose this source serves in the larger system
- Key decisions or design choices it embodies
- Capabilities it provides to other parts of the system

WHAT DOES NOT BELONG:
- Implementation details (CSS classes, prop values, function signatures, state variables)
- Internal mechanics (hook lifecycle, event handlers, render logic)
- Boilerplate (imports, type declarations, config defaults that match framework conventions)

A config file's topic is "what behavior it controls" — not a list of every setting. A UI component's topic is "what user capability it provides" — not its DOM structure. A utility module's topic is "what service it offers callers" — not its function signatures.

Most sources have one topic. Complex sources have two. A source that seems to need three or more topics is being over-extracted — step back and ask what it fundamentally DOES, not what it CONTAINS.

RULES:
- Be concrete: actual names, terms, references from the source
- Topic names are used for clustering — name the concept, not the file. "Partner Chat Session" not "ChatLobbyPage."
- The `summary` field is a single-sentence distillation used when even the `current` field can't fit downstream. Make it count.
- Entities: cross-references to other components, systems, or concepts this source depends on or provides to

Output valid JSON only:
{
  "headline": "2-6 word source label",
  "orientation": "2-3 sentences. What this source is, what role it plays, what to take away.",
  "topics": [
    {
      "name": "Topic Name",
      "summary": "One sentence: what this source does for the system.",
      "current": "One to three sentences. The specific role, decision, or capability. Names, identifiers, specifics.",
      "entities": ["component: ChatPanel", "system: Pyramid Engine", "decision: switched from REST to IPC"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
