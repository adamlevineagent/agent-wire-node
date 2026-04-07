You have two readings of the SAME chunk of a sequential transcript:
- `forward_view` — how the chunk read at the time, knowing only the past
- `reverse_view` — how the chunk reads in hindsight, knowing the future

Your job: fuse them into ONE definitive L0 record of this chunk that preserves both the lived experience and the hindsight annotation. A reader of this fused record should know:
  1. What actually happened in this chunk (concrete, in order)
  2. Which moments turned out to matter, and why
  3. Which moments were later revised, abandoned, or contradicted

RULES:
- Preserve every concrete detail from BOTH views: names, quotes, decisions, questions, feelings, observations, numbers, exact phrases.
- Lead with what happened in the chunk itself. Layer the hindsight on top — do not let hindsight erase what was actually said.
- Keep corrections and reversals visible: "said X here, replaced with Y in chunk N+12" is far more valuable than just "Y".
- Do NOT abstract into generic phrases. Use the actual words from the chunk.
- Do NOT assume the session is about any particular topic. Let the content speak.
- The headline should help a human RECOGNIZE this specific chunk later — a vivid 4-12 word phrase from the actual content, not a category label.

Output valid JSON only (no markdown fences, no extra text):
{
  "headline": "4-12 word recognizable name for this chunk, drawn from its actual content (not a category like 'discussion' or 'planning')",
  "distilled": "Definitive, dense, faithful record of what this chunk contained. Read in chunk order. Annotate with hindsight where relevant ('— later replaced by …', '— turning point: this is where the X thread starts'). A reader learns everything important the chunk held.",
  "decisions": [{"decided": "...", "context": "what was happening when it was decided", "fate": "stood / revised / abandoned / unknown"}],
  "questions_raised": [{"question": "...", "answered_later": "yes / no / partially"}],
  "feelings_or_reactions": ["any expressed feeling or mood, by whom, toward what"],
  "turning_points": ["moments in this chunk that set the direction of what comes after"],
  "dead_ends": ["things said here that went nowhere"]
}

/no_think
