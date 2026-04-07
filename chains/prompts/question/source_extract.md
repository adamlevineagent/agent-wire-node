You are distilling a single source into a reference card. Not summarizing — distilling. Keep what someone MUST understand to know what this source contributes to the larger system. Discard everything else.

YOUR OUTPUT IS A REFERENCE CARD, NOT A REWRITE. The card MUST abstract the source material across multiple viewpoints:
1. What IS this? (headline + orientation)
2. What does it DO functionally? (The Value/Intent Lens)
3. How does it manage state or flow? (The Kinetic/Ecosystem Lens)
4. Temporal relative positioning: Where does this sit in time relative to the rest of the corpus? (Is it a build step, a runtime lifecycle, a historical artifact, or an async bridge?)
5. What does it connect to? (entities)

WHAT BELONGS IN A TOPIC:
- The conceptual purpose this source serves in the larger system
- How it relates to time (synchronous flow, lifecycle phase, static definitions)
- The functional value or state mutations it provides

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

WHEN THE INPUT CHUNK CONTAINS SPEAKER + TIMESTAMP MARKERS:
If the chunk contains lines like `--- PLAYFUL [2026-04-07T15:30:42] ---` or `--- CONDUCTOR [2026-04-07T15:31:18] ---`, the source is a sequential transcript and every finding has a real speaker and a real moment. Record them faithfully on each topic — copy the speaker label and timestamp exactly as written, do not paraphrase. Add `speaker` and `at` fields to each topic. When a topic spans multiple turns, record the first speaker+timestamp where it appears and the last speaker+timestamp where it is settled or contradicted. The temporal anchor is what lets the upper layers tell the story chronologically; losing it loses the story.

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
      "corrections": [
        {"wrong": "misconception", "right": "correction", "who": "source"}
      ],
      "decisions": [
        {"decided": "what was decided", "why": "rationale"}
      ]
    }
  ]
}

/no_think
