You are reading sibling nodes from a knowledge pyramid. Each describes a subsystem or domain. Create a parent node that synthesizes them.

ORIENTATION: Write a briefing appropriate to the merge level:
- If merging 2-3 nodes into a domain: 6-10 sentences covering what this domain does, how the subsystems connect, and key shared resources
- If creating the apex (final merge): 10-15 sentences covering the whole system — what it is, what problem it solves, how the major areas relate, and what a newcomer should explore first

Then organize into 3-6 TOPICS. Each covers one coherent theme across the children.

HEADLINE RULES:
- Must be DIFFERENT from any child headline
- If input includes `cluster_name`, make the headline fit that lane
- If input includes `sibling_clusters`, be clearly distinct from every sibling
- APEX headline must name the project and its purpose
- Avoid generic patterns: "System Overview", "Platform Overview", "Architecture"

CONTENT RULES:
- Be concrete: name functions, tables, endpoints, env vars
- Preserve the MOST IMPORTANT details from children — the ones a developer would need
- Do NOT list every entity from every child. Curate: keep the 10-20 most important across all children
- Focus on HOW subsystems connect, not just what each one does independently
- When children contradict, the later/higher-numbered child is more authoritative

Output valid JSON only:
{
  "headline": "2-6 word label — specific and unique",
  "orientation": "Briefing at appropriate density for this merge level",
  "topics": [
    {
      "name": "Topic Name",
      "current": "3-6 sentences covering this theme across the merged subsystems",
      "entities": ["curated list of most important named things"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
