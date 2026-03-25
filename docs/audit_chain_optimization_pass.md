# Audit Handoff: Pyramid Chain Optimization Full Pass

## Overview
This document represents the blind-audit instructions for the comprehensive 8-part optimization pass of the Pyramid Build Pipeline within `@agent-wire/node`. 

The goal of this pass is to integrate web-edge awareness into clustering and synthesis, enforce thread-size limits (default 12) directly in Rust, improve frontend-specific code extraction (`code_extract_frontend.md`), and surface additive `web_edges` data natively wherever a node is consumed (API, `DrillResult`).

## Instructions for Audit Swarm
We are conducting a blind review before implementation. Each team must review the proposed architecture changes in isolation and document potential failure modes, performance bottlenecks, and schema regressions.

---

### Team A: Backend & Data Structure (Blind Review)
**Focus Area:** `src-tauri/src/pyramid/types.rs`, `query.rs`, `chain_executor.rs`, and the DB model.
**Goal:** Assess the impact of surfacing `pyramid_web_edges` efficiently at read time.

**Specific Questions:**
1. **Query Performance:** The design maps canonical node -> `thread_id` -> `pyramid_web_edges` -> opposite thread's current canonical node -> returns sorted `ConnectedWebEdge` objects. Does joining these 3 tables at read time for `DrillResult` or apex queries risk N+1 performance degredation?
2. **Backward Compatibility:** Does extending `DrillResult` with an additive `web_edges` array break exact-match deserialization in existing older frontends (Tauri or web)?
3. **Delta Chain Accumulation:** If web edges are delta-chained during a build pass, what happens if an apex query happens mid-build? Do we return partial or stale web edges?

**Example Findings (to look for):**
* *"Finding: Query N+1 in query.rs: If a thread has 50 web edges, fetching the canonical headline for the opposite side might cause 50 sequential DB lookups. Fix: Require a single LEFT JOIN."*
* *"Finding: Schema mismatch: ConnectedWebEdge uses `relationship` string, but DB stores it as `reason`. Needs mapping."*

---

### Team B: Prompts & LLM Pipeline (Blind Review)
**Focus Area:** `chains/defaults/code.yaml`, `code_extract.md`, `code_cluster.md`, new `code_extract_frontend.md`.
**Goal:** Evaluate prompt bloat, token limits, and adherence robustness when scaling context.

**Specific Questions:**
1. **Token Exhaustion:** If L0 webbing data is injected into `thread_clustering` and `thread_narrative` prompts, will a 12-file thread with heavy webbing blow past the input context limit for models like `mercury-2` or `qwen`?
2. **Semantic Overflow Split:** The system enforces `max_thread_size` (default 12) by falling back to a semantic split. Is the prompt instruction robust enough to guarantee the LLM will output valid split boundaries without truncating the thread entirely?
3. **Noun Fixation:** We want to tighten architectural noun requirements in `code_extract.md`. Does this over-constrain the model, leading it to invent nouns that aren't actually in the code?

**Example Findings:**
* *"Finding: Injecting webbing into `thread_clustering` bloats the context by 30% if we include raw reasons. Fix: Only inject the strength and opposite file name into the prompt, ignore the long paragraph reason."*
* *"Finding: Semantic Split format risks hallucinating file IDs. Fix: Force the prompt to map exact `source_node` IDs in the split JSON array."*

---

### Team C: Engine & Executor Reliability
**Focus Area:** `chain_engine.rs`, `chain_executor.rs`.
**Goal:** Ensure pipeline definition (`code.yaml`) changes execute cleanly.

**Specific Questions:**
1. **Max Thread Limits:** If `max_thread_size` is missing from an older `code.yaml`, does the engine crash or default safely?
2. **Splitting Loop:** When a thread over 12 items hits the semantic overflow split, how does the engine loop handle the newly generated N sub-threads? Are they immediately scheduled for `thread_narrative`, or do they queue?
3. **State Corruption:** If a split fails halfway (e.g., API timeout), does the parent thread get stuck in a "partial" state in the DB?

**Example Findings:**
* *"Finding: If a thread split fails, the parent thread is marked 'processed' but the sub-threads aren't generated. The files become orphans. Fix: Wrap the split execution and subsequent DB inserts in a single transaction."*

---

### Implementation Review Criteria (Post-Audit)
Before the Build Agents begin executing, they must confirm:
1. All `test_` functions pass for the new `ConnectedWebEdge` mapping.
2. `pyramid_drill` correctly surfaces the new fields in the response.
3. A dry-run of depth=0 rebuilding on a small slug (< 50 files) succeeds without thread-size panics.

*Save your audit findings to `docs/audit_chain_optimization_findings.md` and notify Partner for synthesis.*
