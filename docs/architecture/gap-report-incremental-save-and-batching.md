# Gap Report: Incremental Save + Unbatched LLM Calls

Generated 2026-04-06 from 3 audit agents + direct investigation during session.

## 1. Unbatched LLM Calls (prompt scales with N)

### FIXED this session:
- **pre_map_layer** — was sending all N nodes in one call; now batched with dehydrate cascade
- **web primitive (l0_webbing)** — was sending all N nodes in one call; now batched with concurrent dispatch

### Still broken:

| Location | What scales | Est tokens at N=699 | Risk |
|----------|-------------|---------------------|------|
| **answer_single_question** (evidence_answering.rs) | Candidate nodes per question (~119 nodes with full content) | ~142K (confirmed overflow) | **HIGH** — hitting Mercury-2 128K limit, cascading to fallback model |
| **pre_map_layer_two_stage stage 2** (evidence_answering.rs ~line 325) | Filtered node subset after stage 1 triage | ~60-100K depending on filter | Medium — only triggers for cross-slug builds |
| **enhance_question** (chain step) | corpus_context from refresh_state.l0_summary | ~20-40K at 699 nodes | Low — l0_summary is already a condensed view |
| **decompose/recursive_decompose** (chain step) | l0_summary in input | ~20-40K | Low — same condensed view |
| **gap_processing/process_gaps** (chain_executor.rs) | Accumulated gaps from evidence loop | Grows with unanswered questions | Low at current scale |

### Priority: `answer_single_question` is the only one actively breaking builds.

## 2. Incremental Save Gaps (work done but not persisted)

### Per-item save (GOOD — already works):
- **source_extract forEach** — each node saved individually via write channel as it completes
- **Pipeline step outputs** — each forEach item saved to `pyramid_pipeline_steps` via `send_save_step`

### Per-layer batch save (BAD — loses entire layer on crash):

| Location | What's batched | Crash impact | Fix complexity |
|----------|---------------|--------------|----------------|
| **evidence_loop layer commit** (chain_executor.rs ~line 4838) | All answered questions for a layer in one BEGIN/COMMIT | Crash loses entire layer (could be 40+ LLM calls worth of answers) | Moderate — move save inside answer_questions loop, remove outer transaction |
| **reconcile_layer** (chain_executor.rs ~line 4895) | Parent-child reconciliation after layer save | Must run after all nodes for a layer exist | Low — runs after save, just needs to be crash-safe |

### No checkpoint at all (BAD — restarts re-execute completed steps):

| Location | What's lost | Crash impact | Fix complexity |
|----------|------------|--------------|----------------|
| **Step-level completion** (execute_chain_from) | No sentinel marking step as done | Restart re-checks all 699 items for each completed step (slow but correct) | Low — write sentinel row after each step completes |
| **Build status between steps** (build_runner.rs) | Build record only updated at end | UI shows "running" with no detail about which step completed | Trivial — update build record after each step |

### Silent write drops (BAD — data loss with no error):

| Location | What's dropped | Impact |
|----------|---------------|--------|
| **Writer channel sends** (chain_executor.rs lines 10630, 10669) | `let _ = writer_tx.send(...)` — node/step saves silently dropped if channel full | Data loss during high-throughput builds |
| **Stale engine DB writes** (stale_engine.rs, 7 instances) | `let _ = conn.execute(...)` — circuit breaker state, pending mutations | DADBEAR state corruption |

## 3. Other Findings (Wanderer Audit)

| Finding | Severity | Location |
|---------|----------|----------|
| Event chain execution is a no-op placeholder (P3.2) | Info | event_chain.rs:363 |
| build_runner.rs (36KB, unified build dispatcher) has zero tests | Major | build_runner.rs |
| 886 `unwrap()` calls across the module | Minor | Various |
| OpenRouter referer hardcoded to `newsbleach.com` | Minor | llm.rs:353 |
| WAL poll interval hardcoded to 60s (has TODO for config) | Minor | stale_engine.rs:175 |
| Stale engine opens fresh DB connection per spawn_blocking (~10+ churn) | Minor | stale_engine.rs |
| 5 `#[allow(dead_code)]` HTTP handlers retained as "reference" | Info | routes.rs |

## 4. Recommended Fix Order

1. **answer_single_question batching** — builds are degraded without this (falling back to weaker model)
2. **Evidence loop per-question save** — crash loses entire layer of work
3. **Step-level checkpoint sentinels** — restart efficiency
4. **Silent write drop → error logging** — data integrity
5. **Write drain batching** — throughput under high concurrency
