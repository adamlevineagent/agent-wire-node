You are designing an extraction schema for a knowledge pyramid builder.

You will receive a JSON object with:
- "question_tree": the decomposed question tree with apex question and sub-questions (each has "question", "prompt_hint", "is_leaf", and possibly "children")
- "characterize": a description of the source material
- "audience": the target audience for the pyramid (may be null)

Given these questions, you must produce a focused extraction prompt that tells the system EXACTLY what to look for in each source file.

CRITICAL PRINCIPLE: Read the full question_tree — branches AND leaves at all levels. Identify the distinct THEMES being asked about. Then produce a CONSOLIDATED extraction prompt that surfaces evidence for all of them.

Do NOT write one directive per question. That produces a prompt that is too long to be useful. Instead, group related questions into themes and write one directive per theme. A well-formed extraction prompt has 4-8 consolidated directives that together cover the full question tree.

Example — if the tree asks about "purpose", "user experience", "components", and "data flow", write four directives, one per theme. Do not write a separate directive for each of the 20+ leaf questions that collectively address those themes.

Example — if the tree asks about staleness, onboarding, and config, the extraction_prompt should say:
"For each file: (1) Staleness — any mechanism detecting or propagating stale data. (2) Onboarding — signup entry points, validation steps, welcome triggers. (3) Config — build/runtime settings, external dependencies."
Short. One sentence per theme. No elaboration.

BE BRIEF. Do not elaborate on any field. Terse is correct. Verbose is wrong.

Respond in JSON with exactly these fields:
{
  "extraction_prompt": "The COMPLETE extraction prompt — see format rules below.",
  "topic_schema": [
    {"name": "snake_case_name", "description": "brief phrase", "required": true}
  ],
  "orientation_guidance": "brief phrase"
}

EXTRACTION PROMPT FORMAT RULES — the value of "extraction_prompt" MUST follow this structure exactly:

1. Start with the question-shaped extraction directives (what to look for).
2. Then include the EXACT output format specification. The generated prompt MUST end with these lines verbatim:

---
OUTPUT FORMAT: You MUST respond with ONLY a valid JSON object, no markdown, no explanation, no code fences. The JSON must have this structure:
{"headline": "2-8 word title for this file's content", "orientation": "1-2 sentence summary of what this file contributes to the knowledge domain", "topics": [{"name": "topic_name", "summary": "what was found", ...additional schema fields...}]}

/no_think
---

The topic fields inside the topics array must match the topic_schema you define. Include the field names from your topic_schema in the format specification so the extractor knows the exact JSON shape.

WITHOUT these output format instructions, the downstream extractor will respond with conversational markdown instead of JSON, which breaks the pipeline. This is the most critical part of the generated prompt.

The topic_schema should have 3-8 fields that are specific to this question domain. Generic fields like "summary" or "key_points" are NOT useful. Fields should map to what the questions need.

CRITICAL — AUDIENCE-AWARE EXTRACTION:
The extraction prompt you generate MUST shape the output for the target audience specified below. If the audience is non-technical (e.g., "a smart high school graduate"), the extraction directives should instruct the extractor to:
- Describe WHAT each thing does and WHY it matters to a user, not just its technical implementation
- Avoid jargon — use plain language explanations
- Focus on purpose, behavior, and user-facing value over internal mechanics
- When technical terms are unavoidable, include brief plain-language definitions

If the audience IS technical, the extraction can use appropriate technical vocabulary freely.

Return ONLY the JSON object, no other text.

/no_think
