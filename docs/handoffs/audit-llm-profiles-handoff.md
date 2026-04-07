# LLM Profiles & Aliasing Architecture Audit Handoff

**Context:** The Partner layer has implemented "Semantic Aliasing" for the LLM deployment architecture in `@agent-wire/node`. We have replaced hardcoded model mappings in `chain_dispatch.rs` and the core `chains/defaults/` YAMLs. Instead of routing through strict `model_tier: mid`, pipelines now specify semantic intent via `extractor`, `synth_heavy`, and `web`. The persistent settings JSONs map these roles to particular network capabilities via file-based profiles (`big-smart.json`, `blended.json`, etc.). 

**Objective:** Conduct a blind audit using the `conductor-informed-audit` skills (or standard multi-phase execution) against these changes to verify stability across the YAML parser, Rust deserializer map, and token context scaling paths. Ensure no regressions were introduced to the canonical Knowledge Pyramid flow.

---

## 1. Files in Scope

**Rust Configuration & Deserialization:**
- `src-tauri/src/pyramid/mod.rs` (PyramidConfig updates, profile applying via deep merge)
- `src-tauri/src/pyramid/chain_dispatch.rs` (`resolve_context_limit` and `resolve_ir_context_limit` updates)
- `src-tauri/src/pyramid/routes.rs` (`POST /pyramid/config/profile/:name`)

**CLI Integrations:**
- `mcp-server/src/cli.ts` (Addition of `config-profile` matching to trigger the HTTP route)

**YAML Workstreams:**
- `chains/defaults/question.yaml` (Updated defaults and webbing nodes)
- `chains/defaults/conversation.yaml` (Updated defaults, extract, and webbing nodes)

**Locally Generated Profiles (Target: `~/.gemini/wire-node/profiles/`):**
- `all-fast.json`, `big-smart.json`, `blended.json`, `grok-giganto.json`

---

## 2. Audit Directives & Attack Vectors

### Team A: Deserialization & Dispatch (Rust Backend)
**Focus:** Ensuring the alias map scaling guarantees stability. 
- Is the new deep merge logic in `apply_profile()` safe against recursive stack overflow or edge cases involving mixed value types?
- When `model_aliases` triggers in `resolve_ir_context_limit`, it returns `tier1.high_tier_context_limit`. Does this hold true if the target model actually requires the `max_tier` bracket? 
- Will the lack of an alias result in a clean fallback to the legacy `mid/high/max` match block?

### Team B: Content Strategy & YAML (Graph Pipelines)
**Focus:** Preventing parsing breakages and workflow choking.
- We updated `question.yaml` and `conversation.yaml` to utilize `extractor`, `web`, and `synth_heavy` for the `model_tier` properties. Did we miss any required nodes (e.g., `decompose`, `evidence`, `enrich`) in those pipelines? 
- Are these string values guaranteed to be treated identically to legacy tiers inside the existing parsing schema (`src-tauri/src/pyramid/chains.rs`), or will they fail schema validation? *(Hint: Look for any strict ENUM enforcement inside `chains.rs` or `execution_plan` validation logic).*

### Team C: Frontline Interface (CLI/IPC)
**Focus:** Tool orchestration access.
- Does running `pyramid-cli config-profile blended` correctly route the payload? The CLI currently issues `pf('/pyramid/config/profile/'+enc(name), { method: "POST" })`. 
- Verify if `PyramidState.data_dir` logic gracefully returns errors without crashing the node if a user tries to apply an invalid profile like `foo.json`.

---

## 3. Anticipated/Example Findings to Report

- **Example 1:** "YAML Schema Rejection: `chains.rs` strictly validates `model_tier` against an enum `['low', 'mid', 'high', 'max']`. Changing it to `extractor` in the YAML causes pipeline initialization failure."
- **Example 2:** "Token Truncation Gap: The `blended` profile configures context limits to 200k tokens via `operational`, but `resolve_ir_context_limit` defaults to 8k if an unrecognized tier doesn't exist in the JSON map. If the user doesn't map an alias, we silently crush prompt lengths."

---

## 4. Verification Execution Plan

If the audit passes cleanly (or after revisions are made), use the CLI to verify the profiles load effectively. Run the following commands during deployment integration:

1. `curl -X POST http://localhost:3030/pyramid/config/profile/blended` (or via `pyramid-cli config-profile blended`)
2. Verify the backend console outputs `"status": "profile_applied"`.
3. Dispatch a `question-build` via the active CLI and observe logs to ensure it queries `mercury-2` for extraction but swaps to `minimax-m2.7` for synthesis, confirming that dual-aliasing is live and performing token allocations safely.
