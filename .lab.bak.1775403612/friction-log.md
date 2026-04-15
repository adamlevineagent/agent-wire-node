# Friction Log — Question Pyramid Prompt Tuning

Items here are things that should be in the frame (Rust), not the fill (YAML/MD prompts). Prompt tuning can't fix these — they need code changes.

## Rust Changes Needed

Items 1-3 resolved 2026-04-05. Item 4 open.

## 4. Decompose runs before L0 extraction — question tree built from thin signals

**Discovered:** Experiment 6 gap analysis (2026-04-05)
**Severity:** High — root cause of persistent empty/no-evidence nodes

**What happens:** The `decompose` step runs before `l0_extract`. It sees only `$characterize` (a high-level corpus description string) and the user's question. It has no actual extracted content from the corpus. As a result, the question tree is built from general knowledge of "what this type of corpus typically contains" — which is why questions about testing, deployment, CI/CD, and specific doc files appear even when those things don't exist in the corpus.

In contrast, document.yaml extracts L0 first (generically), then clusters from the actual extracted content. Themes emerge bottom-up from real data. The question pipeline's decompose is doing the equivalent job from a position of ignorance.

All prompt mitigations ("stay within corpus", "no document-specific questions", "only ask what the summaries show") help at the margin but cannot fully compensate — the decomposer is making guesses about corpus contents that prompt instructions can't reliably prevent.

**Fix — YAML reordering only, no Rust needed:**

Current flow: `characterize → enhance → decompose → extraction_schema → l0_extract → evidence_loop`

Fixed flow: `characterize → generic_l0_extract → decompose (real content) → extraction_schema → evidence_loop`

Step 1: generic L0 extraction using a content-type-appropriate generic prompt (like document.yaml's doc_extract.md). Step 2: decompose now sees real extracted content via `$load_prior_state.l0_summary` (which will be populated from the fresh L0 nodes). Step 3: extraction_schema refines based on question tree. Step 4: evidence_loop reuses the existing generic L0 nodes — no second extraction pass needed.

**Key decision:** No second question-shaped extraction needed. document.yaml proves generic L0 nodes are sufficient for clustering and synthesis. The question-shaped extraction_schema is a refinement, not a requirement.

**What's needed:** A generic extraction prompt for code content (`code_extract.md` — adapted from doc_extract.md). The `when: "$load_prior_state.l0_count == 0"` guard on l0_extract ensures this only runs once on fresh builds.

## 1. Layer numbering inverts tree depth ✅ RESOLVED

**Resolved 2026-04-05:** Already had the `+1` offset; stale comments updated.

## 2. Question pyramid build progress reports 0/0 during setup phase ✅ RESOLVED

**Resolved 2026-04-05:** Non-node steps now count toward done/total; UI labels updated from "nodes" to "steps".

## 3. No visibility into large/failed LLM responses ✅ RESOLVED

**Resolved 2026-04-05:** `llm_debug_logging` config flag added. Logs `finish_reason` always; logs truncated/oversized response bodies when flag is enabled.
