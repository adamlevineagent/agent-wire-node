# Conversation Pipeline — Maximal Design

## How Conversations Differ From Code and Documents

Conversations are the hardest content type to pyramidize because:

- **Meaning is emergent**: Two people saying "yes" and "no" to each other for 20 minutes produces architecture. The decisions aren't IN the words — they emerge from the interaction.
- **Temporal ordering is EVERYTHING**: Unlike code (all current) or documents (some temporal), conversations are purely sequential. Minute 45 can reverse everything from minute 10.
- **Signal-to-noise is terrible**: 80% of a conversation is filler, repetition, backtracking, thinking aloud. The 20% that matters is scattered unpredictably.
- **Corrections are the highest-value content**: "No wait, that's wrong, it should be X" — these moments are where real knowledge lives. They're easy to miss in a wall of text.
- **Context is implicit**: Conversations reference shared context that isn't in the transcript. "That thing we discussed" or "the approach from last time" — you need the whole history.
- **Multiple speakers have different authority**: The domain expert's statement overrides the generalist's guess. Speaker identity matters.

## What the Current Pipeline Gets Right

The forward/reverse/combine pattern is genuinely brilliant:
- **Forward pass**: Reads chronologically, capturing what was understood at each moment
- **Reverse pass**: Reads backward from the END, marking what actually mattered vs noise
- **Combine**: Merges both views — keeps what survived, drops dead ends, preserves corrections

This is the right architecture. Don't change it.

## What the Current Pipeline Gets Wrong

1. **L1 is blind positional pairing**: After L0 combine, adjacent nodes get paired regardless of topic. Chunks 3 and 4 get merged even if chunk 3 is about auth and chunk 4 is about UI.

2. **Thread clustering happens too late**: At L2, after information has already been lost through blind L1 pairing. By the time threads form, the specific correction at chunk 7 minute 34 has been diluted through two layers of generic distillation.

3. **Upper layers use recursive_pair**: Same 2:1 problem we solved for code. Each layer loses specificity.

4. **No speaker awareness**: Who said what matters. The person who says "no, the correct approach is X" has more authority than the person whose idea just got corrected. Speaker identity is lost at L0.

5. **No webbing**: Cross-cutting connections between threads aren't tracked.

## The Maximal Conversation Pipeline

```
PHASE 1: COMPRESSION (the existing genius — keep it)
  Step 1: Forward pass (sequential, accumulating context)
  Step 2: Reverse pass (sequential, backward from end)
  Step 3: Combine → L0 nodes (fuse forward + reverse)

PHASE 2: SEMANTIC ORGANIZATION (replace blind pairing)
  Step 4: Thread clustering (LLM groups L0 topics by subject)
  Step 5: Thread synthesis → L1 nodes (per-thread, temporally-ordered)
  Step 6: L1 webbing

PHASE 3: DISTILLATION (replace recursive_pair)
  Step 7: Recursive clustering → L2+ → apex
  Step 8: L2 webbing
```

### The Key Change: Kill L1 Blind Pairing

The current pipeline does: L0 → pair adjacent → L1 → thread cluster → L2 → pair → apex

The maximal pipeline does: L0 → thread cluster → L1 → recluster → L2 → apex

Skip the blind pairing entirely. Go straight from L0 to semantic clustering. Every L0 node's topics get clustered by subject, and thread synthesis produces rich L1 nodes directly.

This is the same pattern that worked for code (jumped from 70 to 83 when we switched from pairing to clustering). For conversations it should be even more impactful because conversation chunks are topically mixed — a single 5-minute chunk might discuss auth, then pivot to UI, then return to auth. Blind pairing merges those mixed chunks, diluting everything. Semantic clustering pulls the auth mentions together across all chunks.

### Thread Clustering for Conversations

The clustering prompt for conversations is different from code/documents:

- **Group by TOPIC, not by chunk**: A topic that spans chunks 3, 7, and 12 is ONE thread
- **Respect the correction chain**: If chunk 3 proposes X, chunk 7 corrects to Y, chunk 12 confirms Y — all three belong in the same thread
- **Speaker-aware grouping**: If two people discuss the same subject across multiple chunks, those are the same thread
- **Dead-end isolation**: Topics that went nowhere can cluster together as "explored but rejected" — or attach to the thread where they were rejected

### Thread Synthesis for Conversations

The synthesis prompt for conversations needs stronger temporal authority than code:

- **Chronological ordering is mandatory**: Topics within each thread sorted by chunk order
- **Late authority rule**: Topics from the last 30% of the conversation (not 70% like the current pipeline) should be marked as MOST AUTHORITATIVE. Conversations typically settle on final answers in the last third.
- **Correction chains are first-class**: When chunk 3 says X and chunk 7 says "no, Y", the L1 node should say "Current: Y. Previously: X, corrected at chunk 7."
- **Dead ends get a sentence, not a paragraph**: "Also discussed Z but abandoned in favor of Y (chunk 8)."

### What About the Forward/Reverse Pattern?

Keep it exactly as-is for L0. It's the best compression strategy for sequential content. The forward pass captures context as it builds; the reverse pass annotates what mattered. The combine step produces L0 nodes that are already pre-filtered for signal.

The change is what happens AFTER L0 — semantic clustering instead of blind pairing.

### Speaker Awareness

The combine prompt should preserve speaker identity in entities:
- `"speaker: Alice — proposed magic-link auth"`
- `"speaker: Bob — corrected: not magic-link, use OTP"`

This lets the thread synthesis know WHO made each decision, which matters when later authority is assessed.

### Conversation-Specific Webbing

Web edges for conversations should capture:
- **Decision dependencies**: Thread A's auth decision affects Thread B's API design
- **Speaker threads**: Same person's contributions across multiple topics
- **Contradiction flags**: Thread A says X, Thread B assumes not-X
- **Temporal bridges**: "This was discussed before X was decided" — events in one thread that happened before pivotal moments in another

## Pipeline Comparison

| Step | Current | Maximal |
|------|---------|---------|
| Forward pass | ✅ Keep | ✅ Keep |
| Reverse pass | ✅ Keep | ✅ Keep |
| Combine → L0 | ✅ Keep | ✅ Keep (+ speaker entities) |
| L1 blind pairing | ❌ Kill | — |
| Thread clustering | At L2, too late | At L1, right after L0 |
| Thread synthesis | L2 narratives | L1 narratives (richer, earlier) |
| L1 webbing | — | ✅ Add |
| Upper layers | recursive_pair | recursive_cluster |
| L2 webbing | — | ✅ Add |

## Expected Impact

The code pipeline jumped from 70 to 83 when we replaced blind pairing with semantic clustering. Conversations should see a bigger jump because:
1. Conversation chunks are more topically mixed than code files (so blind pairing loses more)
2. Corrections and temporal authority are more important in conversations (so thread-level synthesis with temporal ordering captures more value)
3. Dead ends are more prevalent in conversations (so the reverse pass's dead_end filtering gets amplified when clustering groups topics instead of positions)

## Open Questions

1. **Should we still pair L0s at all?** For very long conversations (500+ chunks), going straight from L0 to thread clustering might produce too many topics. Could do a "light pairing" step first: group adjacent L0s into windows of 3-5 purely for size reduction, then cluster.
2. **How to handle multi-day conversations?** If a conversation spans days, the temporal breaks between days might be natural thread boundaries.
3. **Should the apex include a "conversation arc" summary?** Not just topics but "the conversation started by exploring X, pivoted when Y was discovered, and concluded with Z." A narrative arc that describes the journey.
