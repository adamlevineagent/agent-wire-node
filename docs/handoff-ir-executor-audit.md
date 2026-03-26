# IR Executor Audit Handoff

## Context

The Knowledge Pyramid engine has two execution paths:
- **Legacy executor** — reads static YAML chain definitions, runs steps sequentially. Works reliably, produces pyramids scoring 85/100 on blind evaluation across 25+ experiments.
- **IR executor** — new system. Compiles question-driven pyramid definitions into an intermediate representation, then executes. Currently broken at L0 extraction with ~95% JSON parse failure rate.

The same model (inception/mercury-2 via OpenRouter), same prompt file (code_extract.md), same source files, same temperature produce near-100% JSON compliance in legacy but ~5% in IR.

## Current Symptom

L0 extraction dispatches ~128 LLM calls. ~122 return markdown prose ("Below is a **complete synthesis**...") instead of JSON. The remaining ~6 succeed. Clustering then receives only those ~6 outputs (prompt_len ~1444 chars instead of ~500K), qwen returns null content, build aborts.

## Failure Logs

All IR execution logs (latest run, opt-029): `.lab/ir-all-runs.log` (523 lines)

Key lines to examine:
- Lines containing `l0_extract` + `JSON parse failed` — the failing extractions
- Lines containing `No JSON found in:` — shows what the LLM actually returned
- Lines containing `clustering` + `prompt_len` — shows how little data reached clustering
- Lines containing `null content` — the clustering failure

The server log is at `~/Library/Application Support/wire-node/wire-node.log`. IR steps are prefixed `[IR]`, legacy steps are prefixed `[CHAIN]`.

## Previously Fixed Issues (Not the Current Problem)

1. **Response schema not wired** — clustering had no response_schema → qwen returned prose. Fixed.
2. **Wrong model for clustering** — used mercury-2 instead of qwen. Fixed.
3. **Empty input to clustering** — $l0_extract resolved to empty. Fixed: object format.
4. **Context limit not set** — qwen cascaded to fallback model. Fixed: tier-based limits.
5. **Step barrier race** — parallel forEach lost results via dropped channels. Fixed: JoinHandle await.
6. **Children wiring** — IR saves didn't apply authoritative child override. Fixed.

## How to Reproduce

```bash
# Enable IR executor
curl -X POST http://localhost:8765/pyramid/config \
  -H "Authorization: Bearer vibesmithy-test-token" \
  -H "Content-Type: application/json" \
  -d '{"use_ir_executor": true}'

# Create, ingest, build
curl -H "Authorization: Bearer vibesmithy-test-token" -H "Content-Type: application/json" \
  -X POST http://localhost:8765/pyramid/slugs \
  -d '{"slug":"ir-test","content_type":"code","source_path":"/Users/adamlevine/AI Project Files/agent-wire-node"}'
curl -H "Authorization: Bearer vibesmithy-test-token" -X POST http://localhost:8765/pyramid/ir-test/ingest
curl -X POST http://localhost:8765/pyramid/ir-test/build/question \
  -H "Authorization: Bearer vibesmithy-test-token" -H "Content-Type: application/json" \
  -d '{"question": "What should a new developer know about this codebase?"}'

# Watch: expect ~95% of l0_extract items to fail JSON parse
grep "[IR]" ~/Library/Application\ Support/wire-node/wire-node.log | tail -50
```

## How to Compare with Working Path

```bash
# Disable IR, use legacy
curl -X POST http://localhost:8765/pyramid/config \
  -H "Authorization: Bearer vibesmithy-test-token" \
  -H "Content-Type: application/json" \
  -d '{"use_ir_executor": false}'

# Build same slug with legacy
curl -H "Authorization: Bearer vibesmithy-test-token" -X POST http://localhost:8765/pyramid/ir-test/build

# Watch: expect near-100% JSON compliance
grep "[CHAIN]" ~/Library/Application\ Support/wire-node/wire-node.log | tail -50
```

The difference in how the two dispatch functions construct the LLM request is the bug.

## Key Files

| File | What to look at |
|------|----------------|
| `src-tauri/src/pyramid/chain_dispatch.rs` | `dispatch_llm_step` (legacy) vs `dispatch_ir_step` (IR) — compare how system/user prompts are constructed |
| `src-tauri/src/pyramid/chain_executor.rs` | `execute_chain_engine_build` (legacy) vs `execute_ir_plan` (IR) — compare how step inputs are resolved and passed to dispatch |
| `src-tauri/src/pyramid/question_compiler.rs` | Generates IR steps from question tree — check what prompt/instruction fields are set on L0 extract steps |
| `src-tauri/src/pyramid/chain_loader.rs` | Loads and resolves prompt files for legacy path — IR may bypass this |
| `chains/prompts/code/code_extract.md` | The L0 prompt — works in legacy, same file should be used by IR |
| `chains/defaults/code.yaml` | Legacy chain definition — the working reference |

## What We Need

The IR executor's L0 extraction must produce identical LLM requests to the legacy executor's L0 extraction. Same system prompt content, same user prompt content, same role assignments, same parameters. The prompt file is the same — the bug is in request construction.

## Research Context

This system has been under active optimization for ~14 hours across 29+ experiments. Score progression: 30 → 77 → 80 → 83 → 85. The legacy pipeline at 85/100 is the quality bar. The IR executor is the next-generation path that supports question-driven pyramids, but it must first achieve parity with the legacy path before it can be evaluated for quality improvements.

Full research lab: `.lab/` directory (gitignored, contains config, results, logs, parking lot).
Design documents: `docs/question-driven-pyramid-v2.md`, `docs/question-pyramid-architecture.md`, `docs/progressive-crystallization-v2.md`.
