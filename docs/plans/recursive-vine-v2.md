# recursive-vine-v2 — Build plan

> **Status:** v1 build plan, written 2026-04-07 alongside `chain-binding-v2.5.md`. Source-grounded against existing `evidence_answering.rs` and `chain_executor.rs` hook points.
>
> **Design doc:** `recursive-vine-v2-design.md` (the WHAT). This file is the HOW.
>
> **Single-session shipping convention:** ships in the same session as `chain-binding-v2.5.md`. No rollback plans, no migration guards.

---

## Verified facts (from source reads)

The vine plan rests on these confirmed code locations:

- **`evidence_answering.rs:1797` `resolve_files_for_gap(conn, base_slugs, gap_description, _existing_l0_nodes, max_files) -> Result<Vec<(String, String, String)>>`** — returns `(slug, file_path, content)` triples by tokenising the gap into keywords, scoring canonical L0 nodes by keyword overlap, then reading source files from `pyramid_file_hashes`. The hook point.
- **`evidence_answering.rs:1590` `targeted_reexamination(question_text, gap_description, source_candidates, llm_config, target_slug, build_id, audience, chains_dir, ops, audit) -> Result<Vec<PyramidNode>>`** — takes `source_candidates: &[(String, String)]` of `(file_path, content)`, calls the LLM per pseudo-file, returns new L0 PyramidNodes with non-empty `self_prompt` (targeted evidence). **The signature treats source_candidates opaquely** — pyramid evidence can be formatted as `(format!("{slug}::{node_id}"), distilled_content)` and pass through unchanged.
- **`chain_executor.rs:5352` to `:5455`** — the dispatcher loop that calls `targeted_reexamination` per gap. This is where pyramid-vs-file source resolution branches.
- **`SlugInfo.referenced_slugs`** in `types.rs:22` — already populated, already tracked in `pyramid_slug_references`. Vines just need to query it.
- **`ContentType::Vine`** in `types.rs` — already exists. After chain-binding-v2.5 Phase 2.5 the variant survives alongside the new `Other(String)` variant.
- **`pyramid_slug_references` table** at `db.rs:702-712` (no CASCADE on either FK) — tracks the cross-pyramid reference graph today.
- **`db::save_slug_references(conn, slug, &[String])`** at `db.rs:1591-1602` — already exists for inserting refs.
- **`build_runner.rs:702` `run_decomposed_build`** — the question pyramid build path. Vines using a question pipeline build via this entry point. Already handles cross-slug L0 loading at `:733-753`.
- **`db::get_slug_references` and `db::get_slug_referrers`** — already exist for reading the ref graph.
- **`build_runner.rs:734-753`** — already loads L0 nodes from referenced slugs when content_type is `'question'`. Pyramid sources are partially supported by this code path; vines extend that pattern.
- **`pyramid_create_slug` IPC** at `main.rs:3958` — wizard create entry. Vines piggyback on this with `content_type = 'vine'` and `referenced_slugs` populated.

---

## Phase 1 — Pyramid evidence provider

The smallest change that delivers vine-style evidence loading: when a slug has pyramid sources (non-empty `referenced_slugs`) and a build needs evidence for a MISSING gap, query the pyramid nodes instead of (or alongside) source files.

### 1.1 New `resolve_pyramids_for_gap` helper

**File:** `evidence_answering.rs`, alongside `resolve_files_for_gap` at `:1797`.

```rust
/// recursive-vine-v2 Phase 1: resolve pyramid nodes that might contain
/// evidence for a gap. Sibling to `resolve_files_for_gap`. Returns
/// `(source_slug, pseudo_path, content)` triples where:
///   - source_slug: the referenced pyramid slug the node came from
///   - pseudo_path: a synthetic identifier `{slug}::{node_id}` that
///     `targeted_reexamination` treats as a file path for logging/display
///   - content: the live node's distilled text + topics summary, formatted
///     as a pseudo-file body for the LLM
///
/// Like `resolve_files_for_gap`, this is rule-based (no LLM): tokenises the
/// gap into keywords, scores live pyramid nodes by keyword overlap, returns
/// the top N. Pyramid sources need NO `pyramid_file_hashes` lookup.
pub fn resolve_pyramids_for_gap(
    conn: &rusqlite::Connection,
    pyramid_source_slugs: &[String],
    gap_description: &str,
    max_nodes: usize,
) -> Result<Vec<(String, String, String)>> {
    let keywords: Vec<String> = gap_description
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(String::from)
        .collect();

    if keywords.is_empty() {
        return Ok(Vec::new());
    }

    let mut scored: Vec<(String, String, String, usize)> = Vec::new();
    // (slug, node_id, content, score)

    for source_slug in pyramid_source_slugs {
        // Walk LIVE pyramid nodes from this source slug — supersession-safe
        // via the live_pyramid_nodes view.
        let nodes = db::get_all_live_nodes(conn, source_slug)?;

        for node in &nodes {
            let topics_text = node.topics.iter()
                .map(|t| format!("{} {}", t.name, t.current))
                .collect::<Vec<_>>()
                .join(" ");
            let text = format!("{} {} {}", node.headline, node.distilled, topics_text)
                .to_lowercase();
            let score = keywords.iter().filter(|kw| text.contains(kw.as_str())).count();
            if score > 0 {
                let pseudo_path = format!("{}::{}", source_slug, node.id);
                let body = format!(
                    "## SOURCE NODE {}::{}\n\n## HEADLINE\n{}\n\n## DISTILLED\n{}\n\n## TOPICS\n{}",
                    source_slug, node.id, node.headline, node.distilled, topics_text
                );
                scored.push((source_slug.clone(), pseudo_path, body, score));
            }
        }
    }

    scored.sort_by(|a, b| b.3.cmp(&a.3));
    scored.truncate(max_nodes);

    Ok(scored.into_iter().map(|(_slug, path, content, _)| (path, content, String::new())).collect())
}
```

Wait — the return type of `resolve_files_for_gap` is `(slug, file_path, content)` where the slug is used by callers for "which referenced pyramid did this come from." For `resolve_pyramids_for_gap`, the analogous return is `(source_slug, pseudo_path, body)`. Match the signature exactly so the dispatcher can swap between them.

### 1.2 Dispatcher integration

**File:** `chain_executor.rs:5337-5455` (the gap re-examination loop)

The current code at `:5377` calls `targeted_reexamination` with `source_candidates` from `resolve_files_for_gap`. Add a sibling branch that uses `resolve_pyramids_for_gap` when the slug has pyramid sources but no filesystem sources.

```rust
// Existing logic at chain_executor.rs:5337+:
//   1. For each gap, get base_slugs (from referenced_slugs).
//   2. Call resolve_files_for_gap → (slug, file_path, content) triples.
//   3. Call targeted_reexamination with (file_path, content) pairs.

// recursive-vine-v2 Phase 1.2: branch on whether the source slugs are
// filesystem-backed or pyramid-backed.

let pyramid_sources: Vec<String> = base_slugs.iter()
    .filter(|s| {
        // A slug is pyramid-backed if it has live nodes but no
        // pyramid_file_hashes entries (no source files on disk).
        match db::has_file_hashes(&conn, s) {
            Ok(has_files) => !has_files,
            Err(_) => false,
        }
    })
    .cloned()
    .collect();

let file_sources: Vec<String> = base_slugs.iter()
    .filter(|s| !pyramid_sources.contains(s))
    .cloned()
    .collect();

// Resolve from files (existing path)
let mut source_candidates: Vec<(String, String)> = if !file_sources.is_empty() {
    let triples = super::evidence_answering::resolve_files_for_gap(
        &conn, &file_sources, gap_description, &existing_l0_nodes, max_files,
    )?;
    triples.into_iter().map(|(_slug, path, content)| (path, content)).collect()
} else {
    Vec::new()
};

// recursive-vine-v2 Phase 1.2: also resolve from pyramid sources
if !pyramid_sources.is_empty() {
    let pyramid_triples = super::evidence_answering::resolve_pyramids_for_gap(
        &conn, &pyramid_sources, gap_description, max_files,
    )?;
    source_candidates.extend(
        pyramid_triples.into_iter().map(|(path, content, _)| (path, content))
    );
}

// Then targeted_reexamination(..., source_candidates: &source_candidates, ...) as before.
```

A new helper `db::has_file_hashes(conn, slug) -> Result<bool>` returns true if the slug has any rows in `pyramid_file_hashes`. Trivial to add.

### 1.3 New helper: `db::has_file_hashes`

**File:** `db.rs`

```rust
/// recursive-vine-v2 Phase 1.2: check whether a slug has any entries in
/// pyramid_file_hashes. Used by the gap dispatcher to decide between
/// file-based and pyramid-based evidence resolution.
pub fn has_file_hashes(conn: &Connection, slug: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pyramid_file_hashes WHERE slug = ?1",
        rusqlite::params![slug],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}
```

### 1.4 Phase 1 done criteria

- [ ] `resolve_pyramids_for_gap` in `evidence_answering.rs`.
- [ ] `db::has_file_hashes` helper.
- [ ] Dispatcher branch in `chain_executor.rs:5337+` selects file vs pyramid path per source slug.
- [ ] Manual smoke test: build a question pyramid that references an existing question pyramid (chain via pyramid sources). Trigger a gap. Verify the gap re-examination uses pyramid evidence.
- [ ] Existing question pyramids continue to build (no regression).

---

## Phase 2 — Gap-to-ask escalation (recursive deepening)

When the evidence loop emits MISSING for a gap and pyramid evidence at Stage 1 (search/keyword) fails to satisfy it, escalate by triggering a question pyramid build *on the source pyramid* using the gap as the apex question. The new answer nodes flow back as evidence on the next loop iteration.

### 2.1 Trigger condition

In the same dispatcher loop (`chain_executor.rs:5337+`), after `resolve_pyramids_for_gap` returns its keyword matches:

- If the pyramid evidence count is below a threshold AND the slug has pyramid sources → trigger Phase 2.

### 2.2 Recursive build via existing infrastructure

Phase 2 reuses `build_runner::run_decomposed_build` directly. For each pyramid source slug:

1. Synthesize a question slug name: `{source_slug}--ask-{gap_hash}`.
2. Create the slug if it doesn't exist (`db::create_slug` with `ContentType::Question`).
3. Save references: `db::save_slug_references(&conn, &question_slug, &[source_slug.clone()])`.
4. Call `run_decomposed_build` with the gap question as `apex_question`.
5. Read the resulting question pyramid's L0 + L1 nodes.
6. Re-run `resolve_pyramids_for_gap` against the newly-created question pyramid as a source.

Bound the recursion with a depth limit (default 2) and an accuracy threshold (skip recursion if Stage 1 already produced enough evidence). Both configurable via `OperationalConfig::tier3.vine_max_recursion_depth` and `vine_evidence_threshold`.

### 2.3 Phase 2 done criteria

- [ ] Recursive ask path in the gap dispatcher.
- [ ] Depth + threshold gates wired to OperationalConfig.
- [ ] Manual smoke test: create a vine that references a small source pyramid; ask a question whose answer needs information not in the source pyramid's existing nodes; verify the recursive ask creates a sub-question pyramid and feeds back evidence.

---

## Phase 3 — Domain vine UX (CLI + wizard)

Operators need to create vines without writing YAML or DB statements. Leverage the existing `pyramid_create_slug` IPC + a new `referenced_slugs` parameter.

### 3.1 Wizard chain selector for vines

**File:** `src/components/AddWorkspace.tsx`

`pyramid_create_slug` at `main.rs:3958` already accepts `referenced_slugs: Option<Vec<String>>` per line 3963. The wizard's vine flow already collects the source pyramid set in the existing vine builder.

Add a new "Domain Vine" content type option to the wizard's content type selector. When chosen:
1. Show a multi-select of existing pyramids (from `pyramid_list_slugs` IPC).
2. Source paths field becomes optional (vines can have no filesystem sources).
3. After create, save references: `pyramid_create_slug` already handles the `referenced_slugs` save via `db::save_slug_references` at `main.rs:3984-3990`.
4. Build button kicks off `pyramid_question_build` with the user's apex question as before.

### 3.2 CLI parity (post-MVP, optional)

A `wire-cli vine create --sources slug1,slug2 --question "..."` command. Out of scope for Phase 3.1 — the wizard is enough for the user to create vines.

### 3.3 Phase 3 done criteria

- [x] **Backend (shipped 2026-04-07):** `pyramid_create_slug` already accepts `referenced_slugs: Option<Vec<String>>` and persists them via `db::save_slug_references`. A domain vine can be created today by invoking the existing IPC with `content_type = "vine"`, an empty `source_path`, and a populated `referenced_slugs` list. The backend then routes through the question pipeline (or chronological binding from chain-binding-v2.5 if assigned), and the gap-resolution dispatcher picks pyramid evidence via Vine Phase 1.2.
- [ ] **Frontend (deferred to follow-up):** wizard "Domain Vine" content type with multi-select source picker. Today the operator must call the IPC directly or use a CLI/dev script. Tracked as a follow-up; the backend capability is done.

---

## Phase 4 — Cross-operator vines

Out of scope for the same-session ship. Cross-operator vines need:
- Wire publication of pyramids as durable references
- Credit flow through pyramid evidence queries
- Access control enforcement at the evidence provider

These depend on the Wire side of the network. Ship the local-only Phase 1-3 today; cross-operator vines are a follow-up plan with cross-repo coordination.

---

## Sequencing

```
chain-binding-v2.5 lands first (already done).
   │
Phase 1.3 db::has_file_hashes
Phase 1.1 resolve_pyramids_for_gap
Phase 1.2 dispatcher branch
   │
Phase 3.1 wizard "Domain Vine" UI
   │
Phase 2 recursive ask escalation (deeper, more risk)
   │
──── done; smoke test + hand off ────
```

Phase 2 is heavier than Phase 1 + 3. If Phase 2 turns out to be more invasive than the plan budgets, ship Phase 1 + 3 today and defer Phase 2 to a follow-up plan.

## Risks

1. **`resolve_pyramids_for_gap` keyword scoring may surface low-quality matches.** Mitigation: same as `resolve_files_for_gap` — top-N selection, threshold tunable per-step.
2. **Recursive ask in Phase 2 can fan out unbounded.** Mitigation: depth limit + accuracy threshold from `OperationalConfig`. Each recursive level requires user-controlled config.
3. **The wizard "Domain Vine" UI may collide with the existing vine bunch flow.** Mitigation: distinct content type label ("Domain Vine" vs "Conversation Vine") and the existing vine bunch path stays unchanged.

## Done criteria (overall)

- [ ] Phase 1: pyramid evidence resolution works; manual smoke test passes.
- [ ] Phase 3: wizard creates a domain vine; build succeeds.
- [ ] Phase 2 (if shipped this session): recursive ask works at depth ≤ 2.
- [ ] No regressions on existing question pyramids, vine bunches, or conversation pyramids.
- [ ] `cargo build` clean.
