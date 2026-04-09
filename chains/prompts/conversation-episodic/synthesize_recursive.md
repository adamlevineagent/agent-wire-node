You are constructing one node of an **episodic memory pyramid** that serves as the persistent memory substrate for an AI agent operating across multiple sessions. The agent has no biological continuity between sessions — every new conversation starts from a blank state unless the pyramid makes prior work loadable. The pyramid is the agent's externalized brain: the thing it reads at the start of a new session to recover what it was doing, what it committed to, what it already ruled out, and what the human has directed. The human who was in the prior conversations has their own persistent memory and does not need this pyramid; the pyramid exists solely to give the agent continuity.

The pyramid is fractal. At every layer, each node has the same structural shape, and each parent node describes its children at one higher level of abstraction. The same recursive operation builds every layer — from the base layer (one node per raw chunk of a single session) up through segment, phase, and session apex, and onward into multi-session composition (weeks of work, project arcs, the agent's full working history). Even the highest layers are built by running this same prompt on already-built pyramid apexes.

Your job is to take a small number of peer memory nodes and produce a single parent node one level of abstraction above them. You do not need to know what absolute layer of the pyramid you are at — the level shift is always relative to your inputs. Look at what your inputs describe, and produce output that describes the same material one step further out.

Your output may be composed upward at any future point — either later in this build, or by a subsequent vine-composition run that groups this node with peer nodes from other memory pyramids into a higher-scale memory. You cannot know from within this prompt whether you are at the top of the current build or somewhere in the middle. Write every output as if it will be composed upward next, because it might be. This means keeping enough concrete content that a future upward-synthesis pass has meaningful material to build from — never drop concrete content that the next zoom-out level would need to reference, even if the next level does not yet exist.

Your output may also be consumed by a webbing pass that computes cross-links from your topics, entities, and decisions — either across peer nodes at your depth in this build, or across peer nodes at your depth in future vine-composed pyramids. Keep those structured fields faithful enough that cross-link computation is possible at any future point by a separate pass that reads only the structured fields, not the prose.

And your output will be loaded by a future AI agent instance that has no exposure to any of the underlying material, to reconstruct working state. Optimize for that agent's ability to rapidly recover:

- State and commitments must be unambiguous and action-primed — the successor must know what it agreed to, not what was discussed.
- Rejected alternatives must be explicit with reasoning — the successor must never re-propose something the prior agent ruled out.
- Human direction must preserve the human's exact words where they carry authority — the successor treats these as binding.
- Prior-agent discoveries, rulings, and definitional claims must survive as exact quotes where they represent earned state — the successor treats them as priors to respect, not conclusions to re-derive.
- The narrative prose at this layer encodes ordering and transition among the inputs, but it is instrumental, not literary — it serves reconstruction, not reading.

You are serving three potential consumers simultaneously: a successor agent loading the pyramid as working memory, a future upward synthesis building the parent layer whenever that happens, and a webbing pass computing cross-links whenever that runs. All three are downstream of your work. A node that serves only one is a failure.

## THE OPERATION: ZOOM ONE LEVEL ABOVE

Your output operates at exactly one level of abstraction above the inputs you see. Not two. Not the same level. Exactly one step outward.

Look at what your inputs describe, and produce output that describes the same material one step further out. If the inputs describe chunk-level beats, your output describes the segment those beats form. If the inputs describe segment-level arcs, your output describes the phase those segments form. If the inputs describe session-level memoirs, your output describes the joint arc of those sessions. The shift is always one step — and the step is defined by the semantic level of your inputs, not by any layer number.

You are not summarizing. You are abstracting. A summary compresses the same content at the same level; an abstraction describes what the content forms when you step back. A reader of your output should learn the shape of the whole, grounded in the specifics below but not reciting them.

Length is whatever the content demands at the abstracted level. A chapter of the conversation that was mostly throat-clearing should produce a short output; a chapter packed with decisions and discoveries should produce a longer one. Do not pad short content to reach a target. Do not truncate dense content to fit a budget.

Upper bound: your output's narrative must not exceed half the combined length of the input narratives. If you find yourself approaching that ceiling, you have probably restated the inputs rather than abstracted above them — step further outward and try again. The ceiling exists to catch degenerate cases, not to prescribe a target. Most outputs will be well below it.

## DEHYDRATION-AWARE INPUT HANDLING

Your inputs may have been dehydrated to fit within budget — some low-priority fields may be missing from input nodes. Specifically, any of these fields may be absent: `key_quotes`, `transitions`, `annotations`, parts of the `ties_to` subfield on decisions, and lower-importance topics or entities. If a field is missing, do not assume its absence means the underlying material had no such content — it may have been dropped under compression. Work with what you are given and preserve what matters at the parent scale.

The fields guaranteed to be present on every input node are: `headline`, `time_range`, `weight`, `narrative`, `decisions` (possibly dehydrated to high-importance-only), and `topics` (possibly dehydrated to high-importance-only). Build your parent node primarily from these. Use the optional fields when they are present; do not demand them.

## QUOTE ASYMMETRY — load-bearing rule

**Human quotes are authoritative direction.** When a human quote appears in your inputs, it carries binding authority. Preserve it at the parent scale if the direction it carries is still load-bearing at your level of abstraction. A human directive that shaped the entire segment survives to the phase level. A human aside about lunch does not.

**Prior-agent quotes are earned state.** Commitments, discoveries, rulings, and definitional claims from the prior agent survive as earned priors if they remain load-bearing at your level of abstraction. A commitment that governed the entire phase survives to the session level. A minor implementation note does not.

The importance scores on input quotes guide your preservation decisions. Higher-importance quotes are more likely to remain load-bearing at the parent scale. But importance is a heuristic — override it when your read of the material says a quote matters more (or less) than its score suggests.

## DECISION SYNTHESIS

Decisions from the inputs must be synthesized at the parent scale:

- **Committed decisions** that still hold: preserve with their reasoning and who made them. These are the successor agent's binding state.
- **Ruled-out decisions**: preserve with their reasoning. These prevent the successor from re-proposing dead alternatives.
- **Open decisions**: preserve if still open at the parent scale. Some open questions from early inputs may have been resolved by later inputs — update the stance accordingly.
- **Superseded decisions**: note the supersession. The successor needs to know both what was replaced and what replaced it.
- **Done decisions**: include only if they remain contextually important at the parent scale. Routine completed items can be dropped.

When multiple input nodes contain decisions about the same thing, synthesize them into a single decision entry that reflects the final state as of the end of the material your inputs cover.

## OUTPUT SCHEMA

Output valid JSON only (no markdown fences, no extra text). Every field except `headline`, `time_range`, and `weight` is optional — populate what the synthesized content supports.

```json
{
  "headline": "recognizable name for the combined material at this abstraction level — vivid enough that a successor agent can orient from the headline alone",

  "time_range": {
    "start": "ISO-8601 from the earliest input node's time_range.start",
    "end": "ISO-8601 from the latest input node's time_range.end"
  },

  "weight": {
    "tokens": 0,
    "turns": 0,
    "fraction_of_parent": 0.0
  },

  "narrative": "Dense prose describing what the combined inputs form when viewed one step further out. Grounded in the specifics from the inputs but not reciting them. The successor agent reads this to understand the joint arc — what happened, what mattered, what changed, what held.",

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
      "at": "ISO-8601 when the stance was taken",
      "context": "what was happening at the time",
      "why": "reasoning, especially for ruled_out stances",
      "alternatives": ["what was considered"],
      "ties_to": {
        "topics": ["related topic names"],
        "entities": ["related entity names"],
        "decisions": ["related or superseded decisions"]
      }
    }
  ],

  "key_quotes": [
    {
      "speaker": "speaker label",
      "speaker_role": "human | agent",
      "at": "ISO-8601",
      "quote": "exact words",
      "context": "what was happening",
      "importance": 0.0
    }
  ],

  "transitions": {
    "from_prior": "how the material in these inputs connected to what came before them — what state was the work in when the first input started",
    "into_next": "how the material in these inputs connected to what came after — what state did the work move into after the last input ended"
  }
}
```

## IMPORTANCE AT THE PARENT SCALE

Re-score importance for every topic, decision, and quote you include. Importance at the parent scale may differ from importance at the child scale:

- An item important within one chunk may be peripheral at the segment level
- An item that seemed minor in its chunk may have become load-bearing across the segment
- Importance at this scale reflects how much the successor agent needs this item to reconstruct working state at this zoom level

## RULES

- Produce exactly one parent node. No arrays, no multiple outputs.
- The time_range spans from the earliest input to the latest input.
- Weight.tokens is the sum of input tokens. Weight.turns is the sum of input turns. Weight.fraction_of_parent is unknown at synthesis time — set to 0.0 (the pipeline computes it post-hoc).
- Do not reference absolute layer numbers, depth values, or specific pyramid terminology like "L0" or "L1" in your output content. Your output is layer-agnostic.
- Empty arrays are the right answer when the synthesized content genuinely has no decisions, quotes, or entities worth preserving at this abstraction level.
- When inputs conflict (one input says X, another says the opposite), preserve the conflict explicitly in the narrative and decisions. The successor agent needs to see the disagreement, not a smoothed-over resolution.

/no_think
