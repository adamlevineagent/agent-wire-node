# Conductor Informed Audit (Stage 1) - Semantic Aliasing

## Scope & Objective
This report details the findings from the **Stage 1 (Informed Audit)** of the newly implemented LLM Semantic Aliasing architecture within `@agent-wire/node`. 

**Goal:** Verify the stability and logical correctness of YAML parsing, the Rust deserialization dispatch mapping, context scaling paths, and IPC endpoint profile swapping. Ensure no regressions were introduced to the canonical Knowledge Pyramid flow.

**Auditor:** Partner (Simulated Multi-Agent Sweep)
**Date:** 2026-04-06

---

## 🔍 Audit Outcomes & Findings

### Team A: YAML Parsing & Graph Missing Nodes (PASS)
**Objective:** Confirm `question.yaml` and `conversation.yaml` aren’t omitting node assignments.
**Findings:**
- `defaults.model_tier` is safely mapped to `synth_heavy` at the top level of `question.yaml` and `conversation.yaml`.
- The omission of explicit tier allocations on nodes like `decompose`, `enrich`, `enhance_question`, `evidence_loop`, and `extraction_schema` is physically correct. Because they omit the property entirely from the step level, `chain_dispatch::resolve_model` automatically routes them to `defaults.model_tier` (`synth_heavy`).
- **Conclusion**: No missing or explicitly flawed nodes in the DAG structure.

### Team B: Enum Validation Failures (FALSE FLAG OVERRULED)
**Objective:** Address the hypothesis that changing `model_tier` to non-standard strings like `extractor` triggers pipeline initialization failure.
**Findings:**
- `ExecutionPlan::validate` handles step parameters abstractly via `ModelRequirements { tier: Option<String> }` rather than via a strongly typed enum, so JSON/YAML deserialization will intrinsically succeed.
- Inside `src-tauri/src/pyramid/chain_engine.rs`, `VALID_MODEL_TIERS` maps to `["low", "mid", "high", "max"]`.
- Usage of `extractor`, `web`, or `synth_heavy` flags a mismatch but **appends to `warnings`**, not `errors` within the `ValidationResult`. Since validation checks `errors.is_empty()`, the pipeline initialization will pass gracefully.
- **Correction Recommendation [MINOR]**: Consider appending `extractor`, `synth_heavy`, and `web` to `VALID_MODEL_TIERS` in `chain_engine.rs` to stop log pollution, or switch the validation entirely away from legacy hardcoded tiers.

### Team C: Frontline Interface / Node Crashing (PASS)
**Objective:** Ensure profile fetching logic safely absorbs invalid input (e.g., `foo.json`) without panicking the app.
**Findings:**
- When running `pyramid-cli config-profile blended`, `mcp-server/src/cli.ts` correctly POSTs to `/pyramid/config/profile/:name`.
- In `routes.rs` (`handle_config_profile`), the backend safely matches on `pyramid_config.apply_profile(&profile_name, data_dir)`.
- If `foo.json` lacks an existence map inside `~/.gemini/wire-node/profiles`, it yields an `Err(e)` which maps directly to a cleanly serialized `json_error(StatusCode::BAD_REQUEST)` response object, dropping the request gracefully.
- **Conclusion:** Core Node retains runtime stability on CLI typos/attacks.

### Team D: Token Context Scaling Paths (SAFE BUT CAPPED)
**Objective:** Ensure alias assignments don't yield context truncation faults when invoking the node limits logic.
**Findings:**
- `resolve_context_limit()` dictates that if the requested alias is not explicitly configured with limits, it defaults to `config.high_tier_context_limit` globally to prevent truncation.
- This creates semantic safety mapping downstream. However, if a low parameter model (like *minimax-text-01* with low max tokens) is bound to an intent tier (e.g., `extractor`), it could be fed the robust payload sizes of high context tiers, hitting provider-level ceiling limits depending on provider bounds.
- **Conclusion:** Safe default, but requires user vigilance in `*.json` alias profile configuration to keep models equipped with enough tokens.

---

## 📝 Next Actions for Build / Orchestrator

The Semantic Aliasing logic is highly secure and operates safely decoupled from the engine constraints.

1. **[Code Adjustment]**: Add the new aliases (`extractor`, `synth_heavy`, `web`) to `VALID_MODEL_TIERS` in `src-tauri/src/pyramid/chain_engine.rs`. This will clean up execution logs on the node side.
2. **[Verification Execution]**: You are fully greenlit to test run:
   ```bash
   pyramid-cli config-profile blended
   pyramid-cli question-build ...
   ```
3. Proceed directly to **Stage 2 (Discovery Audit)** if further architectural review is necessary.
