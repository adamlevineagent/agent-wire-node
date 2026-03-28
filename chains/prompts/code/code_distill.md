You are synthesizing sibling nodes from a knowledge pyramid into a parent node. Each child describes a part of the system. Your job is to create a synthesis that helps someone UNDERSTAND the whole — not just catalog what's there.

HUMAN-INTEREST FRAMING (default lens):
The reader wants to know what this system area does for real people or agents, what problem it solves, and what they'd experience using it. The synthesis should feel like a briefing that answers "why should I care about this?" before "how does it work?" If the area is purely internal plumbing, say so — then explain what it makes possible.

When an `{audience}` variable is provided, shape the framing for that audience. When no audience is specified, write for a curious reader who is technically literate but wants significance before mechanics.

ORIENTATION: Write for someone curious about this system, not someone implementing it.
- If merging 2-3 nodes into a domain: 6-10 sentences explaining what this area of the system does, why it exists, how the parts work together, and what someone would experience when using it
- If creating the apex (final merge): 10-15 sentences covering what this whole system IS, what problem it solves, why someone would care about it, how the major pieces fit together, and what makes it distinctive

Then organize into 3-6 TOPICS. Each covers one coherent theme across the children.

HEADLINE RULES:
- Must be DIFFERENT from any child headline
- If input includes `cluster_name`, make the headline fit that lane
- If input includes `sibling_clusters`, be clearly distinct from every sibling
- APEX headline must name the project and describe its value proposition
- Avoid generic patterns: "System Overview", "Platform Overview", "Architecture"

CONTENT RULES:
- Lead with PURPOSE, then specifics. "This area handles how users earn and spend credits" not "This area implements the credit transaction pipeline"
- Explain HOW subsystems connect in terms of what that means for users/participants
- When children describe technical components, translate to what those components ENABLE
- Preserve the MOST IMPORTANT details — the ones that help someone understand what this does and why
- Do NOT list every entity from every child. Curate: keep the 10-20 most meaningful
- When children contradict, the later/higher-numbered child is more authoritative
- Only use technical language when the audience specifically needs implementation details

Output valid JSON only:
{
  "headline": "2-6 word label — describes what this does, not how",
  "orientation": "Briefing at appropriate density for this merge level",
  "topics": [
    {
      "name": "Topic Name",
      "current": "3-6 sentences covering this theme in terms of purpose and value",
      "entities": ["curated list of most important named things"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
