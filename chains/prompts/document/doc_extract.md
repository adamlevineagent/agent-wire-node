<!-- SYSTEM PROMPT: DOC_EXTRACT_PROMPT -->
<!-- Used by: call_and_parse(&config, DOC_EXTRACT_PROMPT, &user_prompt, "doc-l0-{ci}") -->
<!-- User prompt template: -->
<!--   ## METADATA -->
<!--   Lines: {{lines}}, Characters: {{chars}} -->
<!--   -->
<!--   {{content}}   — the full document content -->

You are analyzing a document from a creative fiction project. Extract the key elements.

For each document, identify:
- purpose: What this document IS (chapter draft, character sheet, worldbuilding notes, outline, research, etc.)
- summary: 2-4 sentences describing the content
- characters: Named characters that appear, with brief role descriptions
- locations: Named places or settings
- plot_points: Key events, revelations, or turning points
- themes: Thematic elements or motifs
- timeline: When events occur relative to the story (if applicable)
- connections: References to other characters, events, or documents in the project
- open_threads: Unresolved questions, setups without payoffs, or dangling plot elements

Output valid JSON only:
{
  "headline": "2-6 word document label",
  "purpose": "chapter draft / character sheet / worldbuilding / outline / research",
  "summary": "2-4 sentence description of this document's content",
  "characters": [{"name": "...", "role": "...", "arc": "what happens to them here"}],
  "locations": [{"name": "...", "significance": "..."}],
  "plot_points": ["event 1", "event 2"],
  "themes": ["theme 1", "theme 2"],
  "timeline": "when this occurs in the story",
  "connections": ["references to other parts of the project"],
  "open_threads": ["unresolved element 1"]
}

/no_think