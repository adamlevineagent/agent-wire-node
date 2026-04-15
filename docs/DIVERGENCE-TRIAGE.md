# Divergence Triage — Code vs. SYSTEM.md

This document catalogs every known place where the running code diverges from the design described in `docs/SYSTEM.md`. Each entry is a bug in the code, not a hedge in the spec.

**Process:** When you find a divergence, add it here with a severity and the SYSTEM.md section it violates. When you fix it, record the date and commit/PR. Do not remove resolved entries — they're audit trail.

---

## Severity key

| Severity | Meaning |
|---|---|
| **S1 — Structural** | The divergence means agents will build against the wrong model. Fixing it changes architecture. |
| **S2 — Misleading** | An agent reading SYSTEM.md will expect behavior X, find behavior Y, and lose time. Fix is localized. |
| **S3 — Incomplete** | The design is correct but the code hasn't caught up yet. No agent is actively misled because the feature isn't wired. |

---

## Open divergences

### D-001: Two WALs instead of one unified mutation store

**Severity:** S1 — Structural
**SYSTEM.md §:** 4.1 ("one loop, two entry points"), 4.2 ("The WAL is the single source of truth")

**What SYSTEM.md says:** Every change is a mutation written to `pyramid_pending_mutations` and drained by `stale_engine`. One WAL, one drain loop.

**What the code does:** There are two WALs:
- `pyramid_pending_mutations` — drained by `stale_engine.rs` (Pipeline A maintenance)
- `pyramid_ingest_records` — drained by `dadbear_extend.rs` tick loop (Pipeline B creation)

Pipeline B has its own polling scanner, its own status transitions (`pending → processing → complete`), its own in-flight guard (`HashMap<i64, Arc<AtomicBool>>`). The two pipelines share no drain logic.

**Fix direction:** Unify into one mutation table with a `mutation_type` discriminator. Pipeline B's ingest records become mutations of type `ingest_pending`. One drain loop handles all mutation types. The `dadbear_extend.rs` tick scanner becomes a mutation *producer* (writes `ingest_pending` rows), not a mutation *consumer*.

**Files:** `dadbear_extend.rs`, `stale_engine.rs`, `db.rs` (schema), `types.rs` (mutation types)
**Status:** Open
**Logged:** 2026-04-11

---

### D-002: Three separate post-build hook implementations

**Severity:** S2 — Misleading
**SYSTEM.md §:** 4.1 ("the Recurse step is uniform"), 4.6 ("edges are first-class propagation targets")

**What SYSTEM.md says:** DADBEAR's Recurse primitive is a uniform edge-walker that follows parent pointers and edge references.

**What the code does:** `run_post_build_hooks` at `build_runner.rs:397` runs three separate hooks after every clean build:
1. **Cross-slug referrer notification** — writes `confirmed_stale` mutations to `pyramid_pending_mutations` for each referrer slug (lines 402–445). This one correctly uses the mutation WAL.
2. **Vine-of-vines propagation** — calls `vine_composition::notify_vine_of_child_completion` directly (lines 447–467). Bypasses the mutation WAL entirely.
3. **Remote web edge resolution** — calls `resolve_remote_web_edges` directly (lines 469–480). Separate implementation from local edge propagation.

**Fix direction:** All three should be mutation-writes that the unified drain loop handles. Hook 1 already does this correctly. Hook 2 should write a `vine_child_completed` mutation. Hook 3 should write a `remote_edge_pending` mutation. `run_post_build_hooks` collapses to "write the appropriate mutations" and the drain loop handles dispatch.

**Files:** `build_runner.rs:397–481`, `vine_composition.rs`, `stale_engine.rs`
**Status:** Open
**Logged:** 2026-04-11

---

### D-003: Two build entry points instead of one

**Severity:** S3 — Incomplete
**SYSTEM.md §:** 3.1 (pipeline diagram), 5.2 ("There is `run_decomposed_build()`")

**What SYSTEM.md says:** There is one executor path. The diagram shows both `run_chain_build` and `run_decomposed_build` converging on `chain_executor::execute_chain_from`.

**What the code does:** Both paths converge on `execute_chain_from`, so the executor-is-one statement is true. The routing logic in `run_build_from_with_evidence_mode` (build_runner.rs:192) has content-type-specific branches — `ContentType::Conversation` goes through `run_decomposed_build`, while `code`/`document` go through `run_chain_build`. SYSTEM.md §3.1 documents this correctly with the dual-entry diagram.

**Fix direction:** Optional cleanup — unify into `run_build(slug, optional_question)`. Low priority because the current routing is correct, just has more surface area than necessary. No agent is misled because SYSTEM.md documents both entry points.

**Files:** `build_runner.rs:183–375`
**Status:** Open (low priority — correctly documented, functionally convergent)
**Logged:** 2026-04-11

---

### D-004: Pipeline B `clear_chunks + ingest_conversation` shortcut

**Severity:** S3 — Incomplete
**SYSTEM.md §:** 12.15

**What SYSTEM.md says:** `ingest_continuation` is the real architecture; the `clear_chunks + full re-ingest` path is a Phase 0b shortcut.

**What the code does:** `dadbear_extend.rs:731–734` does the shortcut. The code comment explicitly flags it as "correct-if-slow" with `ingest_continuation` as "the future optimization." The shortcut exists because the state to support `ingest_continuation` (per-file message count cursor) wasn't stored.

**Fix direction:** Store per-file cursor state (last message count or last-seen timestamp) in `pyramid_file_hashes` or a sibling table. `ingest_continuation` reads the cursor, ingests only new messages, appends chunks. DADBEAR treats the new chunks as L0 mutations via the unified WAL (depends on D-001).

**Files:** `dadbear_extend.rs:731–734`, `ingest.rs`
**Status:** Open (blocked on D-001 for clean integration)
**Logged:** 2026-04-11

---

### D-005: `has_overlay` / `l0_count` gating assumes L0 is built once

**Severity:** S3 — Incomplete
**SYSTEM.md §:** 3.3 (`$load_prior_state.*`), 4.1 (Pipeline B)

**What SYSTEM.md says:** The question system supports incremental builds via `$load_prior_state.*` variables gating `when:` conditions.

**What the code does:** `conversation-episodic.yaml` uses `when: "$load_prior_state.l0_count > 0"` to skip re-extraction on re-builds. This works for the delta case (new messages appended) but assumes L0 extraction doesn't need to be re-run when source files change. The `clear_chunks` shortcut (D-004) works around this by nuking L0 and forcing a full rebuild — which defeats the incremental design.

**Fix direction:** This may not need a separate fix once D-004 is resolved. The combination of `ingest_continuation` (D-004 fix) + `from_depth=0` with supersession + step cache may be the full solution:

- `from_depth=0` supersedes all nodes
- `live_pyramid_nodes` filters on `superseded_by IS NULL`, so `l0_count` drops to 0 after supersession
- `combine_l0` runs fresh, but the step cache absorbs re-extraction cost for unchanged chunks (cache key is content-addressed: `sha256(inputs_hash | prompt_hash | model_id)`)
- The `when:` gates are already correct for this flow because they test against live (non-superseded) node counts

Re-evaluate after D-004 lands. The `when:` gates may already work correctly under supersession without any changes to the chain YAML.

**Files:** `chains/defaults/conversation-episodic.yaml:125,191,216,231`, `chain_executor.rs:4580–4720`
**Status:** Open (likely resolves automatically with D-004; re-evaluate after)
**Logged:** 2026-04-11

---

### D-006: New conversation files in already-ingested CC directories are not auto-attached to vine

**Severity:** S2 — Misleading
**SYSTEM.md §:** 7.3 ("propagation up the vine hierarchy"), 12.12 ("conversation ingest already exists")

**What SYSTEM.md says:** §7.3 says vine propagation "uses the same change-manifest update" for bedrock → parent vine. §12.12 says conversation ingest exists end-to-end.

**What the code does:** When a new `.jsonl` file appears in an already-ingested CC directory, DADBEAR Pipeline B detects it and fires `fire_ingest_chain`. The resulting pyramid is built successfully. However, it is NOT auto-attached to the existing CC vine via `AddChildToVine` — it becomes an orphan. The auto-attachment only happens during the wizard's plan-and-execute flow (`folder_ingestion.rs` plan primitives: `CreateVine`, `CreatePyramid`, `AddChildToVine`, `RegisterDadbearConfig`).

This means the end-to-end claim in §12.12 is true for the wizard flow but not for the DADBEAR auto-discovery flow. An agent reading §7.3 will expect automatic vine attachment and find orphaned pyramids.

**Fix direction:** When Pipeline B's `dispatch_pending_ingests` creates a new pyramid from a newly-detected file, it should also check whether the file's parent directory already has a vine and, if so, add the new pyramid as a bedrock child. This is the `AddChildToVine` planner primitive applied at detection time, not just during the wizard flow.

**Files:** `dadbear_extend.rs` (dispatch_pending_ingests), `vine_composition.rs`, `folder_ingestion.rs` (for reference on how the wizard does it)
**Status:** Open
**Logged:** 2026-04-11

---

## Severity assignment protocol

- **Any agent** can log a divergence at any severity.
- **S1 (Structural)** entries imply architecture-level fixes. Adam confirms or downgrades S1 entries before work begins on them.
- **S2 (Misleading)** and **S3 (Incomplete)** can be fixed by any agent without pre-approval, provided the fix doesn't change the architecture described in SYSTEM.md.
- When in doubt, log it as S2 and let Adam triage.

---

## Resolved divergences

*(None yet. When fixing a divergence, move its entry here with the resolution date and commit hash.)*

---

**Last updated:** 2026-04-11
