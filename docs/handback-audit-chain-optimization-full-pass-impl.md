# Audit Handoff — Chain Optimization Full Pass Implemented

## What Landed

- Phase 0 safety/integrity fixes:
  - Added source-path validation and normalization before slug creation and ingest.
  - Removed HTTP API-side `auth_token` replacement from config updates.
  - Replaced single-slot `active_build` with per-slug tracking for HTTP and Tauri flows.
  - Made rebuild cleanup transactional, kept FK toggling inside the transaction, cleared stale `children` arrays on the surviving lower layer, and cleaned stale thread/web/distillation/delta state during rebuild cleanup.
  - Made depth-scoped web-edge persistence transactional and moved the step marker save ahead of edge mutation.
  - Replaced webbing polling sleeps with an explicit writer flush barrier.
  - Unified LLM timeout calculation between normal and usage-tracking calls.
  - Moved semantic output validation inside retry handling so empty/invalid-but-parseable outputs consume retry budget.

- Phase 1 chain runtime/schema foundations:
  - Added `instruction_map`, `max_thread_size`, `context`, and `compact_inputs` to `ChainStep`.
  - Added runtime prompt-ref resolution for `cluster_instruction`, `merge_instruction`, and `instruction_map` values.
  - Implemented `header_lines` truncation for string payloads, chunk arrays, and chunk objects.
  - Implemented `context` resolution with indexed per-item context blocks in the system prompt.
  - Centralized prompt construction with the shared system-prompt builder and used it across single, forEach, concurrent forEach, pair, group, recursive cluster, and web steps.
  - Added frontend-aware prompt routing via `instruction_map`, including TSX/JSX and probable frontend JS/TS files outside `src-tauri`.
  - Added recursive-cluster assignment persistence and resume via `cluster_assignment` pipeline steps.

- Phase 2 query/data/API foundations:
  - Added `pyramid_web_edges` indexes for `(slug, thread_a_id)` and `(slug, thread_b_id)`.
  - Added additive `ConnectedWebEdge`, `NodeWithWebEdges`, and `DrillResult.web_edges` payloads.
  - Added connected-edge lookup as a SQL join from canonical node -> thread -> opposite thread -> opposite canonical node/headline.
  - Updated HTTP apex/node routes and Tauri apex/node commands to return additive `web_edges`.
  - Added query tests covering node/drill edge payloads.

- Phase 3 optimization pass:
  - Added `l0_webbing` to the code chain with `compact_inputs: true`.
  - Added compact webbing payload generation for L0 passes.
  - Injected capped file-level connection summaries into `thread_clustering`.
  - Injected capped cross-thread connection summaries into `thread_narrative`.
  - Injected capped cross-subsystem connection summaries into upper-layer synthesis.
  - Added `max_thread_size: 12` and schema-level `maxItems: 12` for thread clustering assignments.
  - Added semantic overflow split support with deterministic fallback and a dedicated split prompt.
  - Added `code_extract_frontend.md` and routed frontend extracts through `instruction_map`.
  - Tightened clustering/distill/recluster/thread prompts to use the new connection context and reduce generic headline overlap.
  - Enabled L0 web-edge storage by allowing depth-0 thread self-heal.

## Verification

- `cargo fmt --manifest-path /Users/adamlevine/AI Project Files/agent-wire-node/src-tauri/Cargo.toml`
- `CARGO_TARGET_DIR=/tmp/agent-wire-node-codex-target cargo test --manifest-path /Users/adamlevine/AI Project Files/agent-wire-node/src-tauri/Cargo.toml --no-run`
- `CARGO_TARGET_DIR=/tmp/agent-wire-node-codex-target cargo test --manifest-path /Users/adamlevine/AI Project Files/agent-wire-node/src-tauri/Cargo.toml chain_executor::tests -- --nocapture`
- `CARGO_TARGET_DIR=/tmp/agent-wire-node-codex-target cargo test --manifest-path /Users/adamlevine/AI Project Files/agent-wire-node/src-tauri/Cargo.toml pyramid::query::tests -- --nocapture`
- `CARGO_TARGET_DIR=/tmp/agent-wire-node-codex-target cargo test --manifest-path /Users/adamlevine/AI Project Files/agent-wire-node/src-tauri/Cargo.toml chain_engine::tests -- --nocapture`
- `CARGO_TARGET_DIR=/tmp/agent-wire-node-codex-target cargo test --manifest-path /Users/adamlevine/AI Project Files/agent-wire-node/src-tauri/Cargo.toml chain_dispatch::tests -- --nocapture`
- `CARGO_TARGET_DIR=/tmp/agent-wire-node-codex-target cargo test --manifest-path /Users/adamlevine/AI Project Files/agent-wire-node/src-tauri/Cargo.toml db::tests -- --nocapture`
- `npm run build`

All of the above passed.

## Remaining Caveat

- I could not run a live networked depth-0 pyramid rebuild from this environment, so the remaining proof point is runtime behavior against the real LLM-backed pipeline. The code, schemas, prompts, and local test coverage are in place for that run.
