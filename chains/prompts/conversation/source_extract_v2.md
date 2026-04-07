You are distilling a segment of a CONVERSATION transcript into a reference card. Not summarizing — distilling. Keep what someone MUST understand about what happened in this exchange.

This is conversational text between participants. Your job is to extract the signal — decisions, agreements, disagreements, topic shifts, open questions, and action items — while preserving WHO said WHAT and WHEN relative to the flow.

YOUR OUTPUT IS A REFERENCE CARD. The card MUST capture:
1. What IS this segment about? (headline + orientation)
2. What HAPPENED? Who initiated, who responded, what positions were taken?
3. What was DECIDED or AGREED? (decisions field — most important for conversations)
4. What SHIFTED? Did the topic change? Did someone change their mind?
5. What is UNRESOLVED? Open questions, deferred items, "we'll come back to this"
6. What does it connect to? References to earlier or later discussion, external systems mentioned

CONVERSATION-SPECIFIC RULES:
- Preserve speaker attribution: "Adam proposed X", "Claude suggested Y", not "it was discussed"
- Preserve temporal markers: "first", "then", "after trying X", "going back to", "actually wait"
- Back-references are entities: "as we discussed" → entity reference to earlier content, even if you don't have it in this chunk. Note the reference explicitly.
- Decision language is high-signal: "let's go with", "agreed", "decided", "committed to", "reversed", "actually no", "changed my mind"
- Topic shifts are structural: when the conversation pivots, that's a topic boundary

WHAT BELONGS IN A TOPIC:
- A decision or agreement reached
- A position taken and the reasoning behind it
- An open question or deferred item
- A back-reference to something discussed earlier or later

WHAT DOES NOT BELONG:
- Pleasantries, greetings, "sounds good", "got it" (unless they signal agreement on something specific)
- Repetition of the same point without new information
- Meta-conversation about the conversation itself (unless it reveals structure)

Most conversation segments have 1-3 topics. A dense segment with multiple decisions might have 3-4. Don't force more.

RULES:
- Be concrete: actual names, terms, specific proposals — not "various options were discussed"
- Topic names should capture the SUBJECT, not the meta-structure: "Auth Architecture Decision" not "Discussion Point 3"
- The `summary` field is a single-sentence distillation. Make it count.
- The `decisions` field is CRITICAL for conversations. If a decision was made, capture it. If a decision was reversed, capture both the original and the reversal.
- Entities: cross-references to systems, tools, concepts, or earlier/later conversation segments

Output valid JSON only:
{
  "headline": "2-6 word segment label",
  "orientation": "2-3 sentences. What this segment covers, what happened, key outcome.",
  "topics": [
    {
      "name": "Topic Name",
      "summary": "One sentence: what was discussed/decided about this topic.",
      "current": "One to three sentences. The specific positions, decisions, or outcomes. Names, specifics.",
      "entities": ["person: Adam", "system: Wire API", "reference: earlier auth discussion"],
      "corrections": [
        {"wrong": "initial assumption", "right": "what was actually decided", "who": "speaker"}
      ],
      "decisions": [
        {"decided": "what was decided", "why": "rationale given in conversation"}
      ]
    }
  ]
}

/no_think
