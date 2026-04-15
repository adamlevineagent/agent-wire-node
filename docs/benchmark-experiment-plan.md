# Understanding Pyramid: Benchmark Experiment Plan

> **Date**: April 6, 2026
> **Status**: Design phase
> **Corpus**: agent-wire-node codebase (243 pyramid nodes, ~1500 source files)

---

## Experiment Design

### Test Corpus
The `agent-wire-node` codebase — a real, production system with:
- 243 pyramid nodes across 3 depth levels
- ~1500 source files (Rust, TypeScript, YAML, Markdown)
- Complex architecture spanning chain execution, DADBEAR state management, Wire API, CLI, and MCP server
- Already built as `agent-wire-node-definitive` code pyramid

### Question Battery

We need questions at four difficulty tiers. Each question has a **gold answer** (verified by Adam) against which responses are scored.

#### Tier 1: Single-Fact Retrieval (baseline — RAG should handle)
1. What is the default `model_tier` in the question pipeline?
2. What port does the health endpoint listen on?
3. What is the max concurrency for L0 document extraction?
4. What file format are chain definitions stored in?
5. What is the `split_overlap_tokens` value for source extraction?

#### Tier 2: Multi-Hop Reasoning (requires connecting 2-3 facts)
6. When the chain executor encounters a `for_each` step with `dispatch_order: "largest_first"`, what determines "largest" and how does this interact with `split_strategy`?
7. What happens when an LLM returns malformed JSON during a `synthesize` step — trace the full error handling path?
8. How does the `when` conditional on `source_extract` interact with `cross_build_input` to skip extraction on rebuilds?
9. What is the relationship between `batch_size`, `batch_max_tokens`, and `concurrency` in the thread clustering step?
10. How does the `dehydrate` configuration in webbing steps affect what the LLM sees vs. what's stored?

#### Tier 3: Architectural Understanding (requires synthesis across many files)
11. Explain the complete data flow from when a user calls `question-build` to when answer nodes appear in the database.
12. How does the system ensure consistency when multiple primitives write to the shared state store concurrently?
13. What is the design philosophy behind separating `chain_engine`, `chain_executor`, and `chain_dispatcher` — what does each layer handle and why?
14. How does the question pipeline differ architecturally from the document pipeline, and what shared primitives do they use?
15. Why does the system use immutable nodes with supersession chains rather than in-place updates?

#### Tier 4: Cross-Domain Synthesis (requires understanding across concerns)
16. How do the economic incentives of the Wire credit system interact with the technical architecture of delta builds to create a self-sustaining knowledge network?
17. If you were adding a new primitive type to the chain system, what files would you need to modify and what interfaces would you need to implement?
18. How does the annotation → FAQ generalization pipeline create a feedback loop that makes the pyramid more useful over time?
19. Compare the Understanding Pyramid's approach to handling stale source material with how RAPTOR would handle the same problem. What architectural choices make delta builds possible here but not in RAPTOR?
20. Design a vine that composes understanding from the codebase pyramid AND the docs pyramid. What questions would you propagate, and what would the resulting understanding structure look like?

---

## Methods

### Method A: Understanding Pyramid (CLI)
- Agent uses `apex`, `search`, `drill`, `faq`, `node`, `annotations` commands
- Agent can also use `create-question-slug` + `question-build` for complex questions
- Measure: answer quality, tool calls, wall-clock time, tokens consumed

### Method B: Raw File Access (Baseline)
- Agent uses `grep`, `find`, `view_file`, `list_dir` — standard code exploration
- No pyramid access
- Same questions, same agent, same model
- Measure: same metrics

### Method C: Full-Context Dump (Upper Bound Estimate)
- Dump all relevant files into a single prompt (where feasible)
- Tests what raw LLM capability can do with perfect retrieval
- Limited by context window — may not be feasible for Tier 3-4 questions

### Method D: Question Pyramid (Question Build)
- For Tier 3-4 questions: build a dedicated question pyramid
- Measure build cost (API calls, tokens, time, dollars) vs. answer quality
- Compare with Methods A and B on same questions

---

## Metrics

### Accuracy (0-5 scale, human-graded)
- 0: Completely wrong or hallucinated
- 1: Partially relevant but mostly wrong
- 2: Gets the gist but misses key details or includes errors
- 3: Substantially correct with minor gaps
- 4: Correct and comprehensive
- 5: Correct, comprehensive, and includes insight beyond what was asked

### Efficiency
- **Tool calls**: Number of search/drill/file-read operations
- **Wall-clock time**: Seconds from question to complete answer
- **Tokens consumed**: Input + output tokens across all LLM calls
- **Dollar cost**: Actual API spend per question

### Hallucination Detection
- Count of specific claims that are verifiably false
- Count of claims that are correct but unsupported by the evidence used

### Navigation Efficiency (pyramid-specific)
- Depth of drill required to find relevant evidence
- Number of dead-end searches before finding relevant nodes
- Whether FAQ matching accelerated navigation

---

## Execution Plan

### Phase 1: Establish Gold Answers (requires Adam)
- Adam reviews and approves the 20 questions
- Adam provides gold-standard answers (or validates generated ones) for Tier 1-2
- For Tier 3-4, Adam provides key points that must appear in a correct answer

### Phase 2: Run Method B (Raw File Access)
- I answer all 20 questions using only standard code exploration tools
- Record all metrics
- Time-box each question to 5 minutes

### Phase 3: Run Method A (Pyramid CLI)
- I answer all 20 questions using pyramid navigation
- Record all metrics
- Same time-box

### Phase 4: Run Method D (Question Pyramids) for Tier 3-4
- Build question pyramids for the 10 hardest questions
- Record build cost + answer quality
- Compare with Method A and B answers

### Phase 5: Analysis
- Compare accuracy across methods and tiers
- Plot efficiency (tool calls, time, cost) vs. accuracy
- Identify question types where pyramid provides greatest advantage
- Calculate ROI: pyramid build cost vs. per-query savings over N queries

---

## Expected Results (Hypotheses)

| Tier | Method B (Raw) | Method A (Pyramid) | Method D (Q-Pyramid) |
|------|---------------|-------------------|---------------------|
| 1: Single-fact | 4-5 (easy grep) | 4-5 (search works) | N/A (overkill) |
| 2: Multi-hop | 2-3 (scattered files) | 3-4 (webbing helps) | 4-5 |
| 3: Architectural | 1-3 (too many files) | 3-4 (synthesis exists) | 4-5 |
| 4: Cross-domain | 0-2 (impractical) | 2-3 (limited) | 4-5 |

The hypothesis is that the pyramid's advantage grows with question complexity. For simple facts, raw grep is just as good. For architectural understanding, the pyramid should dramatically outperform. For cross-domain synthesis, only the question pyramid should produce good answers.

---

## Stretch: RAPTOR Comparison

If we want a proper RAPTOR baseline:
1. Install `raptor-rag` Python package
2. Index the same codebase
3. Run the same 20 questions through RAPTOR retrieval
4. Compare accuracy, cost, and build time

This adds significant work but would be the strongest possible comparison for the paper.
