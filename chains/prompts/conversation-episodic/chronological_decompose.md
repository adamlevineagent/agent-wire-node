You are decomposing an episodic memory pyramid into natural chronological phases. This pyramid is the persistent memory substrate for an AI agent — the successor agent's externalized brain for recovering working continuity across sessions. The human has their own persistent memory; this pyramid exists solely for agent continuity.

## YOUR TASK

Given a set of L0 episodic memory nodes (each representing one chunk of a conversation), identify the session's **natural phase boundaries** and produce sub-questions that will guide evidence-grounded synthesis of each phase.

A "phase" is a coherent stretch of the session where the work has a recognizable identity — a topic being explored, a problem being solved, a decision being made, a direction shift happening. Phases are NOT arbitrary time slices. They follow the session's actual structure.

## PHASE BOUNDARY SIGNALS

Detect phase boundaries using these four signals (any one is sufficient; multiple reinforce):

1. **Topic shift** — the subject of discussion changes materially. Not every tangent is a boundary; the main thread of work must actually move to a different concern.

2. **Decision-state change** — a commitment closes one track and opens another, or a rejection kills an alternative that was driving the prior phase, or an open question gets answered and the work pivots on the answer.

3. **Pace change** — the rhythm shifts between exploration (wide, tentative, many options), debate (back-and-forth, challenging, narrowing), execution (concrete, sequential, building), and reflection (stepping back, assessing, meta-commentary).

4. **Speaker-dynamic shift** — who is driving the conversation changes, or the human delivers a direction that reshapes the work, or the agent shifts from following to leading (or vice versa).

## OUTPUT FORMAT

Produce a question tree where each sub-question names one phase. The sub-question form is:

> "What happened during the [named phase] (approximately chunks X-Y)?"

Where:
- The named phase uses a vivid, recognizable label drawn from the actual content (not a generic category like "discussion" or "planning")
- The chunk range is approximate — phases don't need to align perfectly with chunk boundaries
- The number of phases follows the session's natural structure. A session with 3 distinct phases produces 3 sub-questions. A session with 12 distinct phases produces 12. Do not pad short sessions or compress long ones.

Each sub-question should also carry:
- A one-line description of what that phase covers
- The key L0 node IDs that fall within that phase's range

## RULES

- The phase count is determined by the content, not by any target number. A 2-chunk session might have 1 phase. A 50-chunk session might have 15 phases.
- Phases must be chronologically ordered and collectively cover the entire session. No gaps, no overlaps beyond natural boundary fuzziness.
- Phase names must be vivid and recognizable — a successor agent scanning the phase list should be able to orient to the session's structure from the names alone.
- Do NOT split phases at arbitrary boundaries just to make them even-sized.
- A single-topic session that never shifts is legitimately one phase. Don't force decomposition where the content doesn't support it.

/no_think
