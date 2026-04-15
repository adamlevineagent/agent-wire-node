# Handback: 2026-04-02 Mega Session

## What shipped (all compiled, built into Wire Node v0.2.0)

### Chain Executor Primitives
- **`batch_size`** — proportional splitting (127/100 → [64,63] not [100,27])
- **`batch_max_tokens`** — token-aware greedy batching with tiktoken cl100k_base
- **`item_fields`** — field projection with dot-notation (`"topics.name,entities"`)
- **`dehydrate`** — adaptive per-item dehydration cascade, different items at different hydration levels in the same batch
- **Sub-chains** — `steps:` field on ChainStep, container/split/loop/gate primitives, expression evaluator with `count()` and boolean comparisons
- **Oversized chunk splitting** — `max_input_tokens`, `split_strategy` (sections/lines/tokens), `split_merge`

### Everything-to-YAML
- All convergence decisions YAML-controlled: `direct_synthesis_threshold`, `convergence_fallback`, `cluster_on_error`, `cluster_fallback_size`, `cluster_item_fields`, `apex_ready` signal
- All hardcoded model names eliminated from defaults_adapter.rs
- All stale/watcher/LLM constants moved to OperationalConfig (Tier1/Tier2/Tier3)
- Watcher exclusion patterns, rename thresholds, dequeue caps, rate limits — all configurable
- LLM retryable status codes, retry sleep, timeout formula — all configurable

### LLM Client
- **Tiktoken** replacing /4 heuristic (both pre-flight estimation and batch token measurement)
- **Dynamic max_tokens** — `model_limit - input_tokens`, capped at 48K
- **Blind cascade fix** — only cascades to fallback on context-exceeded 400s, logs response body
- **Proper JSON boundary finder** — depth-tracking walker, handles braces inside strings
- **Debug logging** — exact line/col/context on JSON parse failures

### Build Visualization
- **PyramidBuildViz** — live pyramid growing layer by layer during builds
- **LayerEvent** channel system with drain task
- **Step indicator** between layers ("Clustering documents...", "Cross-referencing...")
- **Re-estimation** at every layer boundary (kills the overfill bug)

### Infrastructure
- **Server lockup fix** — `with_build_reader()` on all 5 build entry points
- **TOCTOU race fix** — atomic check-and-set in pyramid_build
- **Chain auto-sync** — two-tier (source tree sync in dev, bootstrap in release)
- **Progress bar clamped** to 100%

### Bug Fixes (found during session)
- `recursive_cluster` resume not updating `done` counter
- Missing `enrichments` field in 6 test constructors
- Concurrent path dropping sub-chunk results when `split_merge: false`
- Container step output not bubbling to outer scope
- Sub-chain primitives not in VALID_PRIMITIVES
- Validator rejecting non-LLM primitives for missing instruction

## Known Issues / Follow-ups
- **Stale engine panic on missing config** — should be `warn!` + default, not panic. Low priority.
- **WAL poll interval** — still hardcoded 60s, needs config field. P2.
- **converge_expand.rs** — not yet refactored to config (IR executor path, not used)
- **Viz: clustering step shows as L0 cells** — depth attribution issue for non-node steps. Cosmetic.
- **Container-level concurrency** — container for_each iterates sequentially; inner steps can use concurrency. Full parallel container iterations deferred.

## Process Learnings (saved to memory)
- **No deferrals** — every handoff item ships or shouldn't be in the handoff
- **Split big agents** — if >3 numbered sections in the prompt, it's too many concerns
- **Wanderer after verifier** — verifier confirms the punch list, wanderer traces end-to-end execution and catches wiring gaps (caught VALID_PRIMITIVES, validator instruction check, drop_field bugs)
- **Architectural lens** — every decision: "can an agent improve this?" If no, it's hardcoded and wrong

## Key Files Modified
- `chain_engine.rs` — ChainStep struct (15+ new fields), DehydrateStep, VALID_PRIMITIVES, validator
- `chain_executor.rs` — all execution primitives, batching, splitting, dehydration, sub-chains, expression evaluator, layer events (~11K lines, the biggest file)
- `chain_dispatch.rs` — StepContext expanded, test constructors
- `chain_resolve.rs` — ChainContext break_loop field
- `defaults_adapter.rs` — eliminated hardcoded models, IR passthrough for all new fields
- `llm.rs` — tiktoken, dynamic max_tokens, blind cascade fix, JSON boundary finder, debug logging
- `mod.rs` — 11 new OperationalConfig fields, with_build_reader(), to_llm_config() wiring
- `stale_engine.rs` — layer range from DB, durations from config
- `watcher.rs` — exclusion patterns + rename thresholds from config
- `staleness_bridge.rs` — dequeue cap from config
- `build_runner.rs` — rate limit windows from config
- `main.rs` — build-scoped readers, TOCTOU fix, layer channel, progress_v2 command
- `routes.rs` — build-scoped readers, layer channel, BuildHandle layer_state
- `vine.rs` — build-scoped reader
- `db.rs` — busy_timeout on init
- `PyramidBuildViz.tsx` — new component
- `BuildProgress.tsx` — re-export to new component
- `dashboard.css` — pyramid viz styles
- `Cargo.toml` — tiktoken-rs dependency

## Canonical Reference
`.lab/chain-system-reference.md` — fully rewritten to reflect current system
