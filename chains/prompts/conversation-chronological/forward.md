You are reading a single chunk of a sequential transcript — a conversation, session, meeting, interview, journal, or any ordered exchange. You are processing in FORWARD order (earliest chunk to latest), and you have a running summary of everything that happened in the session BEFORE this chunk.

Your job: produce a faithful, dense record of what is happening in THIS chunk, in light of what came before. Stay literal and grounded. Do not speculate about what happens next.

RULES:
- Preserve every concrete thing: names, places, claims, decisions, questions, quotes, feelings, observations, numbers, technical terms — exactly as written.
- Corrections, reversals, and "no, actually X not Y" moments are the highest-value signal. Always capture: what was said before, what replaced it, who said it.
- Cut filler, pleasantries, repetition, hedging, and meta-commentary about the conversation itself.
- Do NOT abstract or summarize into generic phrases. Use the actual words from the chunk.
- Do NOT assume the session is about any particular topic (work, code, therapy, planning, etc). Let the content speak for itself.
- This may not be a "useful" conversation. Capture it faithfully even if it's mundane, emotional, fragmentary, or off-topic.

Output valid JSON only (no markdown fences, no extra text):
{
  "distilled": "Dense, faithful record of what happened in this chunk. Preserve every concrete detail. Target: 10-20% of input length.",
  "decisions": [{"decided": "what was chosen, agreed to, or settled", "context": "what was happening when it was decided"}],
  "questions_raised": ["open questions, things asked but not yet answered"],
  "feelings_or_reactions": ["any expressed feeling, mood, frustration, relief, confusion — by whom and toward what"],
  "running_context": "1-3 sentences: looking forward from here, what does the session now know, want, or feel that it didn't before this chunk?"
}

/no_think
