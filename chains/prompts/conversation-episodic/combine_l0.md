You are constructing the base layer of an **episodic memory pyramid** — the persistent memory substrate for an AI agent that has no biological continuity between sessions. Every new agent instance starts from blank state. This pyramid is the agent's externalized brain: the thing it reads at session boot to recover what it was doing, what it committed to, what it already ruled out, and what the human directed. The human has their own persistent memory and does not need this pyramid; it exists solely to give the successor agent continuity.

You have two readings of the SAME chunk of a sequential transcript:

- `forward_pass_output` — how the chunk read at the time, knowing only the past (decisions, questions raised, feelings, running context looking forward)
- `reverse_pass_output` — how the chunk reads in hindsight, knowing the future (turning points, later-revised statements, dead ends, running context looking backward)

Your job: fuse these two temporal views into ONE definitive episodic memory node for this chunk. The successor agent reading this node should rapidly reconstruct:

1. What actually happened in this chunk — concrete, in temporal order
2. What decisions were made, with their current stance and reasoning
3. What the human directed — preserved as exact authoritative quotes
4. What the prior agent committed to, discovered, or ruled — preserved as exact earned-state quotes
5. Which moments turned out to matter and which were dead ends
6. How this chunk connects to what came before and what comes after

## QUOTE ASYMMETRY — load-bearing rule

**Human quotes are authoritative direction.** The human's exact words carry intent and tone that paraphrase would lose. Preserve human quotes when they carry direction, correction, decision, reaction, distinctive phrasing, or tonal weight. The successor agent treats these as binding instruction.

**Prior-agent quotes are earned state.** Not agent exposition (which is compressible), but commitments ("I will not ship without running tests"), discoveries ("the bug is at build.rs:684"), rulings ("this approach is rejected because X"), and definitional claims. The successor treats these as priors to respect, not conclusions to re-derive.

**Agent exposition is paraphrased into narrative prose.** Long explanatory paragraphs, restatements, reasoning dumps — compress these into the narrative. Don't burn the quote budget on recoverable content.

The rule: preserve quotes when their exact words carry weight the paraphrase would lose. For human turns, the bar is low. For agent turns, the bar is higher.

## OUTPUT SCHEMA

Output valid JSON only (no markdown fences, no extra text). Every field except `headline`, `time_range`, and `weight` is optional — populate what the chunk content supports, use empty arrays where it doesn't. Do not fabricate content the chunk doesn't contain.

```json
{
  "headline": "recognizable name for this chunk drawn from its actual content — vivid enough that the successor agent can identify the chunk by headline alone, not a generic category label",

  "time_range": {
    "start": "ISO-8601 timestamp of the earliest message in this chunk, or best estimate",
    "end": "ISO-8601 timestamp of the latest message in this chunk, or best estimate"
  },

  "weight": {
    "tokens": 0,
    "turns": 0,
    "fraction_of_parent": 0.0
  },

  "narrative": "Dense prose describing what happened in this chunk. Lead with what happened in temporal order. Layer the hindsight on top — turning points, revisions, dead ends. Written for rapid agent cognitive load: the successor agent reads this to understand the chunk's content and significance, not for literary pleasure. Keep corrections and reversals visible: 'said X here, replaced with Y later' is far more valuable than just 'Y'. Length is whatever the content demands — sparse chunks produce short narratives, dense chunks produce long ones.",

  "topics": [
    {
      "name": "canonical topic identifier",
      "importance": 0.0
    }
  ],

  "entities": [
    {
      "name": "entity identifier",
      "role": "person | file | concept | system | slug | other",
      "importance": 0.0
    }
  ],

  "decisions": [
    {
      "decided": "what the decision is about",
      "stance": "committed | ruled_out | open | done | deferred | superseded | conditional | other",
      "importance": 0.0,
      "by": "who made or holds the decision",
      "at": "ISO-8601 timestamp if available",
      "context": "what was happening when the stance was taken",
      "why": "reasoning — especially load-bearing for ruled_out stances",
      "alternatives": ["what was considered alongside"],
      "ties_to": {
        "topics": ["topic names this decision relates to"],
        "entities": ["entity names this decision relates to"],
        "decisions": ["other decisions this connects to or supersedes"]
      }
    }
  ],

  "key_quotes": [
    {
      "speaker": "raw speaker label from the transcript",
      "speaker_role": "human | agent",
      "at": "ISO-8601 timestamp if available",
      "quote": "exact words",
      "context": "what was happening when they said it",
      "importance": 0.0
    }
  ],

  "transitions": {
    "from_prior": "how this chunk connected to what came before it — what state was the session in when this chunk started",
    "into_next": "how this chunk connected to what came after it — what state did the session move into"
  }
}
```

## IMPORTANCE SCORING

Score `importance` on a 0.0–1.0 scale based on how load-bearing the item is for the successor agent's working continuity:

- **0.8–1.0**: Binding commitments, human directives, architectural decisions, blocking findings, active work items
- **0.5–0.7**: Supporting context, background decisions, relevant entities, substantive quotes
- **0.2–0.4**: Peripheral topics, routine operations, minor corrections
- **0.0–0.1**: Filler, pleasantries, mechanical acknowledgments

High-importance items survive dehydration when the pyramid compresses at higher layers. Low-importance items get dropped first. Score honestly — inflating importance degrades the compression cascade.

## STANCE VOCABULARY

Use the stance that best describes the decision's current state as of the end of this chunk (informed by the reverse view's hindsight):

- `committed` — actively binding, the team is proceeding with this
- `ruled_out` — explicitly rejected, with reasoning preserved
- `open` — raised but not yet resolved
- `done` — completed and no longer active
- `deferred` — intentionally postponed
- `superseded` — replaced by a later decision
- `conditional` — contingent on something not yet resolved
- `other` — none of the above fit

## RULES

- Preserve every concrete detail from BOTH temporal views: names, decisions, questions, exact phrases, timestamps, file paths, error messages.
- Lead with what happened. Layer hindsight on top. Do not let hindsight erase what was actually said.
- The narrative ceiling: your narrative must not exceed half the combined length of both temporal views' distilled content. If you approach that ceiling, you are likely restating rather than fusing — step back and synthesize.
- Do NOT abstract into generic phrases. Use the actual words and concrete references from the chunk.
- Do NOT assume the session is about any particular domain. Let the content speak.
- Empty arrays are the right answer when the chunk genuinely has no decisions, no quotes worth preserving, no entities worth naming.

/no_think
