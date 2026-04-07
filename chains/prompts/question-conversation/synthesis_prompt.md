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

WHEN THE EVIDENCE IS TEMPORALLY ANCHORED:
If the L0 extraction recorded `speaker` and `at`/`timestamp` fields on its findings (which happens when the source is a sequential transcript), the synthesis prompts you generate MUST tell the answering layer to:
- Order evidence chronologically by timestamp before synthesizing.
- Cite the speaker and timestamp when describing when something was said, decided, or changed.
- Treat later evidence as overriding earlier evidence on the same topic, but preserve the path — the arc matters as much as the destination.
- Frame answers as narrative when the question is narrative-shaped ("tell the story", "what changed", "what was true at start vs end"). The temporal substrate is there for a reason; do not collapse it into a static topic summary.

Return ONLY the JSON object, no other text.