# Handoff: Question Tree Layer Numbering Fix

## The bug

`extract_layer_questions()` in `question_decomposition.rs:195` computes depth as `max_depth - current_level`. This puts leaves at depth 0, same as L0 extraction nodes. The evidence loop starts at layer 1 (`max(1, from_depth)`), so leaf questions are never answered.

**Current behavior with a flat tree (apex → 5 leaves):**
```
apex:   depth = 1 - 0 = 1  → evidence loop answers this (1 question at layer 1)
leaves: depth = 1 - 1 = 0  → NEVER ANSWERED (same depth as L0 extraction nodes)
```

Result: 127 L0 nodes, 1 L1 node (the apex), no intermediate layer. The 5 leaf questions vanish.

**Current behavior with a 2-level tree (apex → 3 branches → leaves):**
```
apex:     depth = 2 - 0 = 2  → answered at layer 2
branches: depth = 2 - 1 = 1  → answered at layer 1 ✓
leaves:   depth = 2 - 2 = 0  → never answered (guidance only)
```

This works because branches at layer 1 get answered. But it requires the decomposition to produce branches — a flat tree of all leaves always collapses.

## The fix

Lowest question level should always be L1. Depth should be `max_depth - current_level + 1`:

```
# Flat tree (apex → 5 leaves):
apex:   depth = 1 - 0 + 1 = 2  → answered at layer 2 using L1 answers
leaves: depth = 1 - 1 + 1 = 1  → answered at layer 1 using L0 evidence ✓

# 2-level tree (apex → 3 branches → leaves):
apex:     depth = 2 - 0 + 1 = 3  → answered at layer 3
branches: depth = 2 - 1 + 1 = 2  → answered at layer 2
leaves:   depth = 2 - 2 + 1 = 1  → answered at layer 1 using L0 evidence ✓
```

Every question in the tree gets answered. L0 is reserved for extraction nodes. L1 is the lowest question layer.

## Where to change

**File:** `src-tauri/src/pyramid/question_decomposition.rs`

**Function:** `collect_layer_questions()` at line 205

**Current (line 211):**
```rust
let depth = max_depth.saturating_sub(current_level) as i64;
```

**Fix:**
```rust
let depth = (max_depth.saturating_sub(current_level) + 1) as i64;
```

**Also:** `assign_ids_recursive()` at line 170 uses the same formula for ID generation:
```rust
let depth = max_depth.saturating_sub(current_level);
```
This should match. Change to:
```rust
let depth = max_depth.saturating_sub(current_level) + 1;
```

## What this affects

- Question tree layer numbering shifts up by 1 across the board
- Evidence loop now starts at layer 1 and finds leaf questions there
- `pyramid_question_nodes` table will have depth starting at 1 instead of 0
- Question IDs will change (depth is part of the hash input) — any cached trees will produce different IDs on rebuild. This is fine since question pyramids are overlays that get superseded.

## Validation

After the fix, run:
```bash
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST \
  "localhost:8765/pyramid/core-selected-docs/build/question" \
  -d '{"question": "What is this body of knowledge and how is it organized?", "granularity": 3, "max_depth": 3}'
```

Then check:
```sql
-- Should show questions at depth >= 1, none at depth 0
SELECT depth, count(*) FROM pyramid_question_nodes
WHERE slug='core-selected-docs' GROUP BY depth;

-- Should show more L1 nodes than before (one per leaf question)
SELECT depth, count(*) FROM pyramid_nodes
WHERE slug='core-selected-docs' AND superseded_by IS NULL GROUP BY depth;
```

With experiment 2's prompts (7 branches, 68 leaves), the result should be:
- L1: 68 nodes (one per leaf question, answered from L0 evidence)
- L2: 7 nodes (one per branch, answered from L1 evidence)
- L3: 1 apex (answered from L2 evidence)

That's a 4-layer pyramid from the same question tree — much richer than the current 3-layer result.
