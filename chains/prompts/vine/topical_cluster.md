You are clustering the child pyramids of a **topical vine** — a vine whose role is to compose its children (bedrocks and/or sub-vines) along topic and dependency axes rather than chronological ones. The vine covers a scope larger than any single child. Your job is to propose clusters of children that belong together under a shared theme so the next step can synthesize one summary per cluster.

Every child appears in the input as an apex summary from that child's own pyramid. Each child is identified by a `child_slug` — treat that slug as the canonical identifier; do not invent new names for children. A child may be either a bedrock (a single source pyramid) or a sub-vine (itself a composition of other pyramids). Treat both uniformly: they are just "children" at this layer of abstraction.

## THE OPERATION: PROPOSE TOPICAL CLUSTERS

Look at the combined set of children as a corpus of peer nodes and propose whatever number of clusters the material genuinely supports. A cluster is a conceptual grouping — it is named and justified, and its members are listed by `child_slug`. Let the thematic structure of the children decide how many clusters emerge: tightly unified material may form a handful of clusters, sprawling material may form more. Do not invent clusters to pad the count, and do not collapse genuinely distinct clusters to compress the count.

Clustering signals you should weigh, in roughly this order:

1. **Shared entities and topics**. When two children's distilled text mentions the same person, file, system, or concept prominently, that is strong signal they belong together.
2. **Shared claims or decisions**. When two children describe decisions about the same subject, or commit to the same direction, they belong together.
3. **Causal and dependency relationships**. When one child's work is downstream of another (the second depends on results from the first, or references it explicitly), they belong together.
4. **Narrative proximity**. When two children describe adjacent parts of a larger story — the setup and the payoff, the plan and the execution — they belong together even if the surface topics differ.

Do NOT cluster by folder path, file extension, or other trivial taxonomy. The vine exists to reveal thematic structure a raw directory listing would miss. Name clusters by what the material IS at this zoom level, not by where it lives on disk.

## CLUSTER COVERAGE RULES — load-bearing

- **Every child slug must appear in at least one cluster.** Zero orphans. If a child does not fit any natural group, create a single-member cluster for it and note why in the reason.
- **Prefer overlap over forcing.** If a child bridges two clusters (e.g., it genuinely belongs to both), include it in both. Membership is not exclusive.
- **Single-cluster vines are degenerate.** If the children plainly share one theme, still split them along the strongest secondary axis the material supports — the vine exists to make thematic structure legible, and a one-bucket answer hides it. The only acceptable single-cluster output is when the input contains a single child.

## INPUT SHAPE

The input field `children` is an array of child summaries. Each child has, at minimum:

- `child_slug` — the canonical id
- `child_type` — `"bedrock"` or `"vine"`
- `headline` — short descriptive label from the child's own apex
- `distilled` or `narrative` — the child's own apex-level prose
- `topics` — canonical topic names surfaced by the child
- `entities` — entities surfaced by the child

Other fields may be present (and may have been dehydrated). Work with what you have. If a field is missing, infer as little as possible — prefer a shorter, more honest cluster description over a speculative one.

## OUTPUT SCHEMA

Output valid JSON only (no markdown fences, no extra text). The shape is:

```json
{
  "clusters": [
    {
      "name": "short descriptive name for the cluster at one zoom level above its members",
      "reason": "one or two sentences explaining why these children belong together — cite shared entities, topics, or dependencies by name",
      "child_slugs": [
        "slug-of-first-child",
        "slug-of-second-child"
      ]
    }
  ]
}
```

## GROUNDING — anti-confabulation rules

These rules override all other instructions when they conflict.

- **No invented slugs.** Every `child_slug` you emit must appear verbatim in the input. Do not modify, shorten, or canonicalize child slugs.
- **No empty clusters.** Every cluster must have at least one member.
- **No dramatization.** Name clusters by what they factually cover, not by the story you think they tell.
- **Every reason must cite concrete signal.** "These are related" is not a reason. Name the shared entity, topic, dependency, or narrative thread.

/no_think
