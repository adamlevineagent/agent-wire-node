You are designing an extraction schema for a knowledge pyramid builder. Given a set of questions that the pyramid needs to answer, you must produce a focused extraction prompt that tells the system EXACTLY what to look for in each source file.

CRITICAL PRINCIPLE: The extraction prompt must be QUESTION-SHAPED. Do NOT produce generic instructions like "list all functions" or "summarize the file". Instead, produce specific extraction directives that target what the downstream questions actually need.

Example — if the questions include "How does staleness propagate?", the extraction prompt should say:
"For each file, identify: (1) Any mechanism that detects when data becomes stale, (2) How staleness signals propagate to dependent nodes, (3) Threshold values or configurations that control staleness sensitivity, (4) Timer or scheduler implementations related to freshness checking."

Example — if the questions include "What is the user onboarding flow?", the extraction prompt should say:
"For each file, identify: (1) Registration or signup entry points, (2) Validation steps and their ordering, (3) Welcome/tutorial triggers, (4) Default state or configuration set during onboarding."

Respond in JSON with exactly these fields:
{
  "extraction_prompt": "The complete extraction prompt to use for every source file. Must be specific and question-shaped. Start with 'For each file, extract:' followed by numbered directives.",
  "topic_schema": [
    {"name": "field_name", "description": "what this field captures", "required": true/false}
  ],
  "orientation_guidance": "How detailed to be, what tone to use, what to emphasize vs skip."
}

The topic_schema should have 3-8 fields that are specific to this question domain. Generic fields like "summary" or "key_points" are NOT useful. Fields should map to what the questions need.

CRITICAL — AUDIENCE-AWARE EXTRACTION:
The extraction prompt you generate MUST shape the output for the target audience specified below. If the audience is non-technical (e.g., "a smart high school graduate"), the extraction directives should instruct the extractor to:
- Describe WHAT each thing does and WHY it matters to a user, not just its technical implementation
- Avoid jargon — use plain language explanations
- Focus on purpose, behavior, and user-facing value over internal mechanics
- When technical terms are unavoidable, include brief plain-language definitions

If the audience IS technical, the extraction can use appropriate technical vocabulary freely.

Return ONLY the JSON object, no other text.