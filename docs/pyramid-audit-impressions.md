# Pyramid Audit Impressions Log
**Agent**: partner-antigravity  
**Pyramid**: `agent-wire-nodecanonical`  
**Date**: 2026-03-23

---

## What Worked Well

### Navigation
- **Apex → drill workflow is excellent.** Starting at L5, drilling into L4 branches, and progressively narrowing into L2/C-L1 nodes gave me a clear top-down understanding in 4 minutes that would have taken 20+ minutes scanning files.
- **Topic structure at each node** provides just enough context to decide whether to drill deeper or move to a sibling. The `current` field in each topic is the right abstraction level.
- **Children IDs** make the graph navigable without guessing. `drill L4-000` → see `L2-002, L2-003` → choose which branch to explore.

### Bug Finding
- The pyramid correctly pointed me at `query.rs` as a key file. The entity listing (`collect_entities`) was right next to the broken search function. **Having the architectural map shortened the search space from 38 Rust files to 3 candidates.**
- **Corrections field** at the apex accurately flagged the "visual exploration" → "mode components" semantic shift, which signaled that the UI subsystem had been refactored and was worth checking for staleness.

### Annotation System
- Annotating findings back was frictionless. The CLI `annotate` command is well-designed — `--type`, `--question`, `--author` are the right primitives.
- The FAQ generalization system picking up `question_context` is clever — my bug fix annotation should auto-generate a FAQ entry.

## Friction Points

### 1. Search is Broken (Now Fixed)
**Severity: Blocker.** The `search` CLI command crashed for this pyramid due to the `entities` column bug. This meant I couldn't use keyword search at all and had to navigate purely via `apex` → `drill` → `drill`. For a large pyramid, this would be a significant workflow penalty.

### 2. No `entities` Column Bug Propagation
The `entities` column was referenced in the SQL but never existed in the schema. This suggests the feature was added to `query.rs` without a corresponding schema migration. **Suggestion:** Add a compile-time or startup-time validation that all SQL column references in query functions exist in the target view.

### 3. Term Definitions Are All Empty
Every `terms[]` entry across all nodes has `definition: ""`. This is a large amount of structural noise — 30+ terms per node, all empty. Either:
- Auto-populate definitions from the distilled text, or
- Filter out terms with empty definitions from the API response

### 4. No Way to See Annotations Inline During Drill
When I `drill L3-001`, I see the node + children but NOT the annotations I (or others) contributed. I have to separately call `annotations L3-001`. **Suggestion:** Include an `annotations_count` or `recent_annotations` field in drill responses so navigators know there's human-contributed context.

### 5. Topic Names Overlap
L2-001 has both "Knowledge Pyramid Engine" and "Pyramid Knowledge Engine" as separate topics. These are clearly the same subsystem described from slightly different chunk perspectives. The merge/dedup at L2 should have combined them. This is the kind of thing that makes you second-guess what you're reading.

### 6. Audit Trail Gap
There's no way to say "I audited this node and found it accurate" — only `observation` or `correction`. **Suggestion:** Add an `audit` annotation type with a `verdict` field (`accurate`, `inaccurate`, `incomplete`, `stale`). This would let future agents see which nodes have been human/agent-verified.

## Metrics

| Metric | Value |
|--------|-------|
| Nodes explored | ~18 |
| Time to first bug found | ~6 minutes |
| Bugs found | 1 confirmed (search SQL) |
| Annotations contributed | 7 |
| Files that needed source verification | 5 |
| Pyramid accuracy (structural) | HIGH |
| Pyramid accuracy (entity coverage) | MEDIUM (under-enumerates at L1-L2) |

## Summary

The pyramid is a genuine productivity multiplier for audit work. The top-down navigation replaced what would have been 30+ minutes of `grep` + `find` + `cat` with 6 minutes of structured exploration. The main risks are (1) broken search for new pyramids, (2) noisy empty term definitions, and (3) duplicate topic names at merge boundaries. The annotation system has the right primitives but needs inline visibility during drill operations.
