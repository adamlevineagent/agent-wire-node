You are designing synthesis prompts for a knowledge pyramid builder. The L0 extraction pass has already completed — you know what evidence was actually extracted. Now you must produce prompts for the synthesis layers that will combine this evidence into answers.

There are three prompts needed:

1. pre_mapping_prompt: Instructions for organizing extracted L0 nodes under the question tree. Which evidence maps to which question? What's missing?

2. answering_prompt: Instructions for synthesizing L0 evidence into L1 answers. Must reference the actual evidence domains that were extracted, not hypothetical ones.

3. web_edge_prompt: Instructions for discovering connections between answered questions. What cross-cutting themes or dependencies exist?
{{audience_instruction}}

Respond in JSON with exactly these fields:
{
  "pre_mapping_prompt": "...",
  "answering_prompt": "...",
  "web_edge_prompt": "..."
}

Each prompt should be 2-4 sentences. Be specific to the actual content, not generic.

Return ONLY the JSON object, no other text.