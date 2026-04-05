# Handoff: Recipe-as-Contribution + Holistic Audit — 2026-04-04

## Overview

Two major efforts completed in a single session:

1. **Recipe-as-Contribution implementation** — Refactored ~1200 lines of hardcoded Rust question pipeline into a forkable 65-line YAML chain definition with 4 executor primitives.
2. **Holistic codebase audit** — 6 independent auditors reviewed the entire 90K-line Rust + 79-file TypeScript codebase. 90 findings total (5 critical, 34 major, 51 minor). All criticals and ~30 majors fixed.

---

## Part 1: Recipe-as-Contribution

### What changed

The question pipeline build (decompose question → evidence loop → gap processing) was moved from hardcoded Rust (`build_runner.rs` ~1200 lines) to a YAML chain definition (`chains/defaults/question.yaml` ~70 lines) executed by 4 new first-class primitives.

### Files changed

| File | Change |
|------|--------|
| `chains/defaults/question.yaml` | **New.** The forkable question pipeline recipe. Steps: load_prior_state → enhance_question → decompose (or decompose_delta) → extraction_schema → l0_extract → evidence_loop → gap_processing |
| `src-tauri/src/pyramid/chain_executor.rs` | Added 4 async primitives: `execute_cross_build_input`, `execute_recursive_decompose`, `execute_evidence_loop`, `execute_process_gaps`. Each reads from `step.input` with backward-compatible fallback to hardcoded refs. ~800 new lines. |
| `src-tauri/src/pyramid/chain_engine.rs` | Added `instruction_from`, `dispatch_order`, `mode` fields to ChainStep. Added 4 new primitives to VALID_PRIMITIVES. Added `"question"` to VALID_CONTENT_TYPES. Validator: recipe exemption, `save_as` validation. |
| `src-tauri/src/pyramid/chain_resolve.rs` | Added `initial_params` to ChainContext with fallback resolution. LazyLock'd regex patterns. |
| `src-tauri/src/pyramid/chain_registry.rs` | Added `"question" => "question-pipeline"` routing. |
| `src-tauri/src/pyramid/build_runner.rs` | `run_decomposed_build` reduced from ~1200 to ~130 lines. Characterizes, loads chain, builds initial_context, calls `execute_chain_from`. Build tracking wrapper (save_build_start/complete/fail). Dead `run_question_build` removed. |
| `src-tauri/src/pyramid/chain_loader.rs` | Tier 2 bootstrap now includes `question.yaml`, `extract-only.yaml`, `prompts/question/*`, `prompts/shared/*`. |

### How to test

1. **Fresh question build:** Create a new question pyramid (Ask Question flow). Should decompose, extract L0 if needed, run evidence loop, process gaps. Check build completes and nodes appear.
2. **Delta question build:** Run the same question again (or modify it). Should detect overlay, run decompose_delta path (not fresh), reuse existing answers where possible.
3. **Cross-slug question build:** Create a question pyramid that references another slug. Should load L0 from referenced slug, run evidence loop against it.
4. **Build cancellation:** Start a question build and cancel mid-flight. Should stop cleanly, record build failure.
5. **Live build visualization:** Question builds should now show progress in the PyramidBuildViz component (layer events, node completion).

### Key behaviors to verify

- `question.yaml` steps execute in order: load_prior_state → enhance_question → decompose/decompose_delta → extraction_schema → l0_extract (only on fresh) → evidence_loop → gap_processing
- Build tracking: a `qb-XXXX` build record should appear in build history for every question build (success or failure)
- The `mode: delta` field on `decompose_delta` controls fresh vs delta behavior (not the step name)
- Each primitive reads from `step.input` when present, falls back to context refs when absent

---

## Part 2: Holistic Audit Fixes

### Critical fixes (5)

#### 1. IR parallel forEach retry bypass
**File:** `chain_executor.rs` ~line 9779
**What:** `execute_ir_parallel_foreach` was calling `dispatch_ir_step()` directly instead of `dispatch_ir_with_retry()`. Retry policies (e.g., `retry(3)`) were ignored for all parallel forEach items — which is the primary L0 extraction path.
**Fix:** Replaced with `dispatch_ir_with_retry()`.
**Test:** Run a code or document build. If an LLM call fails transiently, it should retry per the step's on_error policy, not fail immediately.

#### 2. Constant-time auth comparison
**File:** `http_utils.rs`
**What:** `ct_eq` returned early on length mismatch, leaking length info via timing.
**Fix:** Now pads to max length, XORs all bytes, timing is constant regardless of input lengths.
**Test:** Auth should still work for valid tokens and reject invalid ones. No behavioral change visible.

#### 3. Parity flag drop guard
**File:** `parity.rs`
**What:** `run_parity_test()` saved/restored executor flags manually. A panic or early `?` return would leave the system in the wrong executor mode permanently.
**Fix:** `ExecutorFlagGuard` struct with `Drop` impl restores flags automatically.
**Test:** If parity test is available, run it. System should use correct executor mode before and after, even if a build fails mid-test.

#### 4. NodeLockMap cleanup race
**File:** `crystallization.rs`
**What:** `cleanup()` used `DashMap::retain()` which has a TOCTOU race — between checking `Arc::strong_count == 1` and removing, another task could acquire the lock, getting an orphaned mutex.
**Fix:** Two-phase approach: collect candidates, then `remove_if()` with atomic re-check.
**Test:** Delta processing under concurrent builds. No visible change unless you were hitting silent lock corruption.

#### 5. Tunnel token moved to environment variable
**File:** `tunnel.rs`
**What:** Tunnel token was passed as `--token` CLI argument, visible in `ps aux`.
**Fix:** Now passed via `TUNNEL_TOKEN` environment variable on the child process.
**Test:** Start a tunnel. Verify `ps aux | grep cloudflared` does NOT show the token in the command line.

### Major fixes by area

#### Chain system
| Fix | File | Test |
|-----|------|------|
| `merge_instruction` wired up in executor | `chain_executor.rs` | YAML `merge_instruction: "$prompts/shared/merge_sub_chunks.md"` should now be used for split-merge steps instead of hardcoded default |
| `dispatch_order` field added to ChainStep | `chain_engine.rs` | Logs warning when set (not yet implemented). Verify warning appears in logs for code/document builds |
| `evaluate_when` defaults to false for unparseable | `chain_executor.rs` | A typo in a `when` condition should skip the step (not execute it) |
| `save_as` validation | `chain_engine.rs` | Invalid `save_as` values produce a validator warning |
| Zero chunks → error for non-question pipelines | `chain_executor.rs` | A build with no source files should error immediately (not run empty steps) |
| Step-name decoupling | `chain_executor.rs` + `question.yaml` | Primitives read from `step.input`. A forked chain with renamed steps should work if input block wires correctly |
| `mode` field for delta detection | `chain_engine.rs` + `chain_executor.rs` + `question.yaml` | `decompose_delta` uses `mode: delta` instead of name-based detection |
| Duplicate chain ID fixed | `document-v4-classified.yaml` | ID changed from `document-default` to `document-v4-classified` |
| v4 yaml apex_ready added | `document-v4-classified.yaml` | Recursive clustering now gets apex_ready signal |

#### Build + question pipeline
| Fix | File | Test |
|-----|------|------|
| Build tracking wrapper | `build_runner.rs` | Every question build creates a build record (check build history) |
| Returns actual build_id + node_count | `build_runner.rs` | Build completion should show correct ID and count |
| Evidence loop external build tracking | `chain_executor.rs` | No duplicate build records (was creating 2 per build) |
| TOCTOU rate limiter | `build_runner.rs` + `mod.rs` + `main.rs` | Combined `AbsorptionGate` mutex. Rate limiting should still work correctly for absorption builds |
| Conversation merge step | `conversation.yaml` + `conv_cluster_merge.md` | Large conversations (150+ chunks) should not lose thread assignments from earlier batches |
| Layer_tx for question builds | `build_runner.rs` + `main.rs` + `routes.rs` | Question builds show live progress in PyramidBuildViz |

#### Database + routes + stale engine
| Fix | File | Test |
|-----|------|------|
| `purge_slug` transaction wrapper | `db.rs` | Purge slug should be atomic (both DELETEs or neither) |
| `enforce_access_tier` fail-closed | `routes.rs` | Unknown access tier values return 403 (not 200) |
| Dead stale engine placeholders removed | `stale_engine.rs` | No behavioral change |
| Non-atomic staleness_bridge | `staleness_bridge.rs` | CTE-based atomic mutation processing. Verify file change detection still works |
| Wire import rate-limit parsing | `wire_import.rs` | 429 responses now use server's Retry-After header instead of hardcoded 60s |

#### Infrastructure + partner
| Fix | File | Test |
|-----|------|------|
| Email masking in auth logs | `auth.rs` | Log output should show `a***@example.com` not full emails |
| Lifted results capped at 20 | `partner/conversation.rs` | Long Partner sessions should not accumulate unbounded lifted results |
| Dead partner routes removed | `partner/routes.rs` | `handle_send_message`, `handle_new_session`, `SendMessageBody` removed. 410 stubs still work |

#### Frontend
| Fix | File | Test |
|-----|------|------|
| AppContext provider useMemo'd | `contexts/AppContext.tsx` | Mode components should NOT re-render every 2 seconds from polling ticks |
| currentView via ref | `contexts/AppContext.tsx` | `currentView` identity is stable across renders |
| IntentBar vocabRegistry dependency | `IntentBar.tsx` | Cost preview should show actual classifications (not "Cost varies") after registry loads |
| PyramidBuildViz/BuildProgress onComplete ref | `PyramidBuildViz.tsx`, `BuildProgress.tsx` | Build completion callback should fire correctly without polling loop restart |
| PyramidDashboard timeout cleanup | `PyramidDashboard.tsx` | No React warnings on unmount during onboarding copy timeout |

### Minor fixes (selected)
- Regex LazyLock'd in `chain_resolve.rs` (performance)
- Silent index fallback warning in `chain_resolve.rs` (observability)
- Boolean unresolved ref warning in `chain_executor.rs` (observability)
- process_gaps error logging instead of `.ok()` (observability)
- force_from ROLLBACK on error (correctness)
- Cancellation check before synthesis prompt generation (correctness)
- Tier 2 bootstrap includes question chain + prompts (deployment)

---

## Part 3: YAML chain files changed

| File | Change |
|------|--------|
| `chains/defaults/question.yaml` | New file. Question pipeline recipe with input blocks on all primitives. |
| `chains/defaults/conversation.yaml` | Thread clustering wrapped in container sub-chain with batch + merge steps |
| `chains/defaults/document.yaml` | `split_merge_instruction` → `merge_instruction` |
| `chains/defaults/code.yaml` | `split_merge_instruction` → `merge_instruction` |
| `chains/defaults/extract-only.yaml` | `split_merge_instruction` → `merge_instruction` |
| `chains/defaults/document-v4-classified.yaml` | ID: `document-default` → `document-v4-classified`. Added `apex_ready` to cluster schema. |
| `chains/prompts/conversation/conv_cluster_merge.md` | New file. Merge prompt for conversation thread clustering batches. |

---

## Part 4: Known remaining items (not fixed in this session)

These need architectural decisions and are documented in the judgment items list:

1. **Pillar 37 across 8+ sites** — `Tier2Config::default()` bypassing operator config
2. **ModeRouter remounts** — Tab switch loses mode-local state
3. **Error response sanitization** — routes.rs leaks internal details to remote callers
4. **Horizontal review index-shift** — Leaf marks point to wrong siblings after merges
5. **/auth/complete CSRF** — Missing nonce-based protection
6. **Tunnel token encrypted storage** — Plaintext on disk (env-var fix applied for CLI, but file persistence remains)
7. **Shared reqwest::Client** — 44 separate Client::new() calls
