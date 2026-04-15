# Rust Handoff: Everything-to-YAML — The Canonical MPS

## Status
**IMPLEMENTED** — compiled, built into Wire Node v0.2.0. All P0 and P1 items complete. See deviations at bottom.

**Supersedes:** `handoff-apex-ready-signal.md`, `handoff-convergence-to-yaml.md`

## Principle

Every decision that shapes pyramid quality, maintenance behavior, or operational strategy must live in YAML/config so it's a contribution — improvable by any agent through the Wire's contribution model. Rust is a dumb execution engine. It reads config and does what it says.

This is not an optimization. This is the architecture. If a parameter is frozen in a binary, no agent can improve it, no operator can tune it, and no contribution can supersede it. Pre-release means we fix this now, not after people are complaining about hardcoded values they can't change.

---

## PART 1: CONVERGENCE LOOP (chain_executor.rs)

### 1.1 Direct synthesis threshold
**Current:** `if current_nodes.len() <= 4` (line 4469)
**Fix:** New YAML field `direct_synthesis_threshold: Option<usize>`. When None, no hardcoded threshold — rely on apex_ready signal only.
```yaml
direct_synthesis_threshold: null  # trust the LLM
```

### 1.2 apex_ready signal
**Current:** Not implemented. Loop always clusters until <= 4, then force-synthesizes.
**Fix:** Check `apex_ready: true` in cluster response. LLM decides when the current nodes ARE the right top-level structure. Jump to direct apex synthesis.
```yaml
cluster_response_schema:
  properties:
    apex_ready:
      type: boolean
    clusters:
      type: array
      # ...
```

### 1.3 Convergence safety net (force-merge)
**Current:** Lines 4684-4716. If clusters >= input count, mechanically merge smallest pairs.
**Fix:** New YAML field `convergence_fallback: "retry" | "force_merge" | "abort"`. Default "retry" — re-call LLM with stronger instruction before resorting to mechanical merge.
```yaml
convergence_fallback: "retry"
```

### 1.4 Cluster failure fallback (positional groups of 3)
**Current:** Lines 4612, 4662. `chunks(3)` on failure.
**Fix:** New YAML field `cluster_on_error: "positional(N)" | "retry(N)" | "abort"`.
```yaml
cluster_on_error: "retry(3)"
cluster_fallback_size: 3  # only used with positional fallback
```

### 1.5 Cluster input projection
**Current:** Lines 4554-4565. Hardcoded to `node_id`, `headline`, `orientation` (truncated 500 chars), `topics` (names only).
**Fix:** New YAML field `cluster_item_fields`. Uses same projection as `item_fields` but specific to the clustering sub-call.
```yaml
cluster_item_fields: ["node_id", "headline", "orientation"]
```

### 1.6 Orientation truncation in cluster input
**Current:** `truncate_for_webbing(&n.distilled, 500)` (line 4561).
**Fix:** Subsumed by `cluster_item_fields`. If you want full orientation, include it. If you want truncation, that's a stretch goal (field-level truncation modifiers). For now, `batch_max_tokens` handles the sizing naturally.

### 1.7 Clustering retry strategy
**Current:** `ErrorStrategy::Retry(3)` hardcoded (line 4599).
**Fix:** Read from `cluster_on_error` on the step, falling back to step's `on_error`.

---

## PART 2: LLM CLIENT (llm.rs)

### 2.1 Default models
**Current:** Lines 54-56. `"inception/mercury-2"`, `"qwen/qwen3.5-flash-02-23"`, `"x-ai/grok-4.20-beta"`.
**Status:** Already in `pyramid_config.json` as `primary_model`, `fallback_model_1`, `fallback_model_2`. But some code paths read the hardcoded defaults instead of config. **Audit and eliminate all hardcoded model name references.**

### 2.2 Context limits
**Current:** Lines 57-58. `120_000`, `900_000`.
**Status:** Already in Tier1Config. Same issue — some code paths use hardcoded values.

### 2.3 Timeout strategy
**Current:** Lines 60-61, 84. Base 120s, max 600s, scaling formula `prompt_chars / 100_000 * 60`.
**Fix:** Move to config:
```json
{
  "llm_base_timeout_secs": 120,
  "llm_max_timeout_secs": 600,
  "llm_timeout_chars_per_increment": 100000,
  "llm_timeout_increment_secs": 60
}
```

### 2.4 Retry strategy
**Current:** Lines 59, 236-243. Max 5 retries, 1s sleep between retries, `2^n` exponential backoff.
**Fix:** Move to config:
```json
{
  "llm_max_retries": 5,
  "llm_retry_base_sleep_secs": 1,
  "llm_retry_backoff": "exponential"
}
```

### 2.5 HTTP cascade triggers
**Current:** Line 257. HTTP 400 triggers primary → fallback cascade.
**Status:** This is structural (400 = context exceeded = try bigger model). OK to stay in Rust, but the cascade order should be config-driven (already is via model tiers).

### 2.6 Retryable status codes
**Current:** Line 277. `429 | 403 | 502 | 503`.
**Fix:** Move to config: `"llm_retryable_status_codes": [429, 403, 502, 503]`.

---

## PART 3: STALE ENGINE / DADBEAR (stale_engine.rs, watcher.rs, staleness_bridge.rs)

### 3.1 WAL poll interval
**Current:** Line 156. `Duration::from_secs(60)`.
**Fix:** Already partially in config. Ensure ALL timing references read from config, no hardcoded fallbacks.

### 3.2 Debounce timer fallback
**Current:** Lines 142, 291. `Duration::from_secs(300)` when config unavailable.
**Fix:** Config must always be available. Remove fallback, fail explicitly if config missing.

### 3.3 Phase display duration
**Current:** Lines 646, 1107. `Duration::from_secs(10)`.
**Fix:** Move to config: `"phase_display_duration_secs": 10`.

### 3.4 Stale batch caps
**Current:** Tier3Config defaults: `batch_cap_nodes: 5`, `batch_cap_connections: 20`, `batch_cap_renames: 1`.
**Status:** Already in config. Verify no hardcoded overrides exist.

### 3.5 Max concurrent helpers
**Current:** Tier1Config default: `stale_max_concurrent_helpers: 3`.
**Status:** Already in config. Verify no hardcoded overrides.

### 3.6 Staleness threshold
**Current:** staleness_bridge.rs:35. Default `0.3`.
**Status:** Already in Tier2Config. Verify used everywhere.

### 3.7 Runaway breaker threshold
**Current:** watcher.rs:684. Default `0.5` (50%).
**Status:** Already in AutoUpdateConfig. Verify used everywhere.

### 3.8 Layer iteration range
**Current:** `for layer in 0..=3` (lines 95, 168).
**Fix:** Derive from actual pyramid depth, not hardcoded 3. Read max depth from DB or config.

### 3.9 File watcher exclusion patterns
**Current:** watcher.rs:212-224. Hardcoded: `/target/`, `/node_modules/`, `/.git/`, etc.
**Fix:** Move to config: `"watcher_exclude_patterns": ["/target/", "/node_modules/", "/.git/", ...]`.

### 3.10 Rename similarity threshold
**Current:** watcher.rs:409, 447. 50% character overlap.
**Fix:** Move to config: `"rename_similarity_threshold": 0.5`.

### 3.11 Rename candidate time window
**Current:** watcher.rs:390. `2000ms`.
**Fix:** Move to config: `"rename_candidate_window_ms": 2000`.

### 3.12 Staleness queue dequeue cap
**Current:** staleness_bridge.rs:97. `50` items.
**Fix:** Move to config: `"staleness_queue_dequeue_cap": 50`.

---

## PART 4: DEFAULTS_ADAPTER.RS (Legacy IR fallback defaults)

### 4.1 All hardcoded model names
**Current:** 15+ instances of `"qwen/qwen3.5-flash-02-23"` scattered through fallback step generation.
**Fix:** Read from `pyramid_config.json` model fields. No model name should appear in Rust source.

### 4.2 All hardcoded concurrency values
**Current:** `concurrency = 8` (L0), `concurrency = 5` (L1), etc.
**Fix:** Read from chain YAML defaults section. The defaults_adapter should only fill in values when the YAML doesn't specify them, using config values as the source.

### 4.3 All hardcoded retry policies
**Current:** `"retry(3)"` everywhere.
**Fix:** Read from chain YAML `defaults.on_error` or step-level `on_error`.

### 4.4 Node ID patterns
**Current:** `"C-L0-{index:03}"`, `"D-L0-{index:03}"`, `"L1-{index:03}"`, `"L{depth}-{index:03}"`.
**Fix:** Already in chain YAML `node_id_pattern`. Ensure defaults_adapter reads from YAML, not hardcoded.

### 4.5 Header lines truncation
**Current:** Line 808. `"header_lines": 20`.
**Fix:** Move to chain YAML step config: `header_lines: 20`.

---

## PART 5: RATE LIMITING (build_runner.rs)

### 5.1 Hourly rate limit window
**Current:** Line 89. `Duration::from_secs(3600)`.
**Fix:** Move to config: `"rate_limit_hourly_window_secs": 3600`.

### 5.2 Daily spend cap window
**Current:** Line 113. `Duration::from_secs(86400)`.
**Fix:** Move to config: `"rate_limit_daily_window_secs": 86400`.

---

## PART 6: COST & PRICING (Tier1Config)

### 6.1 Default token prices
**Current:** `$0.19/M input`, `$0.75/M output`.
**Status:** Already in Tier1Config. These should update via contribution (economic_parameter contributions from the Wire). Verify no hardcoded overrides.

---

## PART 7: DELTA & COLLAPSE (Tier3Config)

### 7.1 Collapse threshold
**Current:** `collapse_threshold: 50`.
**Status:** Already in config.

### 7.2 Propagation depth
**Current:** `max_propagation_depth: 10`.
**Status:** Already in config.

### 7.3 Web edge parameters
**Current:** `max_edges_per_thread: 10`, `edge_decay_rate: 0.05`, `edge_min_relevance: 0.1`, `contradiction_confidence_threshold: 0.8`.
**Status:** Already in config.

---

## PART 8: VINE / CONVERSATION CLUSTERING

### 8.1 Max sessions per cluster
**Current:** vine_prompts.rs:9-38. "Max 4 sessions per cluster."
**Fix:** Move to conversation chain YAML step config.

---

## WHAT STAYS IN RUST

These are structural, not quality decisions:

1. `<= 1 node` = apex (definition of apex)
2. Loop structure (read → cluster → synthesize → repeat)
3. DB operations (read nodes, write nodes, persistence)
4. Progress tracking and resume/replay
5. LLM dispatch mechanics (HTTP calls, JSON parsing)
6. `batch_size.max(1)`, `concurrency.max(1)` floors (prevent division by zero)
7. HTTP 400 → try larger model (structural cascade)
8. File watcher event plumbing (fsnotify → mutation queue)

---

## IMPLEMENTATION PRIORITY

### P0: Blocks pyramid quality (do now)
- 1.1 `direct_synthesis_threshold`
- 1.2 `apex_ready` signal
- 1.5 `cluster_item_fields`
- 1.7 Clustering retry from step config
- 4.1 Eliminate all hardcoded model names

### P1: Blocks operational tuning
- 1.3 `convergence_fallback`
- 1.4 `cluster_on_error`
- 2.3 Timeout strategy to config
- 2.4 Retry strategy to config
- 3.8 Layer range from DB not hardcoded
- 3.9 Watcher exclusion patterns to config

### P2: Completeness (before ship)
- Everything else in this doc
- Full audit that no Rust file contains a model name string
- Full audit that no Rust file contains a numeric constant that's a quality decision

---

## FILES

### Rust changes
- `chain_engine.rs` — new fields on ChainStep
- `chain_executor.rs` — refactor `execute_recursive_cluster` to read all decisions from step config
- `llm.rs` — read all timeouts/retries/models from config
- `stale_engine.rs` — read all intervals/thresholds from config
- `watcher.rs` — read exclusion patterns and rename thresholds from config
- `staleness_bridge.rs` — read queue cap from config
- `defaults_adapter.rs` — read all fallback values from chain YAML + config, no hardcoded values
- `build_runner.rs` — read rate limit windows from config
- `converge_expand.rs` — read all thresholds from config

### YAML changes (after Rust ships)
- All chain YAMLs — add convergence controls, apex_ready to schemas
- All recluster prompts — add apex_ready instruction
- `pyramid_config.json` — add new config fields with current hardcoded values as defaults

---

## IMPLEMENTATION DEVIATIONS

Deviations from this handoff spec as implemented:

### Matches handoff (corrected after initial implementation)
- **`direct_synthesis_threshold`**: Default is `None` (no threshold). When YAML doesn't set it, only `apex_ready` signal or `<= 1` node (structural) triggers direct synthesis. YAML can set `direct_synthesis_threshold: 4` to restore old behavior.
- **`convergence_fallback`**: Default is `"retry"`. LLM gets asked again with stronger instruction before mechanical force-merge. Falls back to force_merge if retry also fails.

### Deviations
- **WAL poll interval (3.1)**: Left as hardcoded 60s with TODO comment. No config field was added — the handoff says "already partially in config" but no matching field exists. A `poll_interval_secs` field should be added to Tier2Config in a follow-up.
- **Stale engine fallback debounce (3.2)**: Changed from `Duration::from_secs(300)` fallback to `expect()` panic. Handoff says "fail explicitly if config missing" — implemented as panic, which is explicit but aggressive. Could be changed to a logged error + default.
- **Phase display min duration**: Handoff specifies a single `phase_display_duration_secs: 10` field. Implementation uses `phase_display_duration_secs / 3` (clamped to 1s) for the minimum phase display time (was hardcoded 3s). This is a reasonable derivation, just not explicitly in the handoff.
- **LLM config fields**: WS3 added 4 new fields directly to `LlmConfig` struct (`retryable_status_codes`, `retry_base_sleep_secs`, `timeout_chars_per_increment`, `timeout_increment_secs`). The handoff implies these live only in OperationalConfig, but they also need to be on LlmConfig since that's what the LLM dispatch functions receive. Both are wired: `PyramidConfig::to_llm_config()` populates LlmConfig from OperationalConfig.
- **Part 8 (vine clustering)**: Not implemented. "Max 4 sessions per cluster" in vine_prompts.rs was not moved to config. Low priority — vine builds are a separate system.
- **converge_expand.rs**: Not modified. Handoff lists it as needing config reads but was not included in the workstream scope. Needs follow-up audit.
