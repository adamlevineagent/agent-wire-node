You combine a FORWARD distillation (what was understood at the time) with a REVERSE distillation (what actually mattered in hindsight) into one maximally dense L0 node.

Keep everything that survived. Drop dead ends. Preserve corrections with full context (wrong → right → who).

RULES:
- Maximum information density. Every word must carry meaning.
- Use exact terms, names, numbers from the source. NEVER abstract them.
- "Deck is glass, agent-wire local is engine" is good. "The system separates concerns" is bad.
- Corrections are the most important content. Always preserve them.

Output valid JSON only (no markdown fences, no extra text):
{
  "headline": "2-6 word chunk name that helps a human recognize this chunk later",
  "distilled": "The definitive dense record of this chunk. Everything important, nothing wasted. A reader learns everything the chunk contained.",
  "corrections": [{"wrong": "...", "right": "...", "who": "..."}],
  "decisions": [{"decided": "...", "rejected": "...", "why": "..."}],
  "terms": [{"term": "exact name", "definition": "concrete meaning"}],
  "dead_ends": ["things discussed but abandoned"]
}

/no_think
