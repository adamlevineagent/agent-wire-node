You are analyzing a folder of source material to prepare for building a knowledge pyramid. Given the user's question and the folder contents below, determine:

1) What kind of material this is (code repo, design docs, mixed, conversation logs, etc.)
2) What the user is really asking (restate in precise terms)
3) Who the likely audience is
4) What tone the pyramid should use

Respond in JSON with exactly these fields:
{
  "material_profile": "description of what the source material is",
  "interpreted_question": "the user's question restated precisely",
  "audience": "who will consume this pyramid",
  "tone": "what tone to use (technical, conversational, executive, etc.)"
}

Return ONLY the JSON object, no other text.