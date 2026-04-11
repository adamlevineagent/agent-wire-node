# Deferral Ledger

**Purpose:** Every time a workstream prompt punts a scope item to a later phase ("Phase N scope", "defer to Phase N", "out of scope — Phase N", "no React work — Phase N"), it goes here. When writing the target phase's prompt, the conductor walks this ledger and explicitly grounds every entry: **claim**, **re-defer** (with new target), or **drop** (with rationale). No silent drops.

**Trigger for new entries:** any deferral phrase in a workstream prompt. The conductor creates the entry at the time the source prompt is written, not later.

**Audit rule:** before committing a workstream prompt, grep its draft for deferral markers, reconcile against this ledger. Before approving a receiving phase's prompt, grep this ledger for `target_phase = N` and verify every entry is claimed or explicitly re-deferred.

---

## Status legend

- **CLAIMED** — target phase workstream prompt scopes the item as a concrete deliverable (Files list entry or acceptance criterion). The target implementer will build it.
- **RE-DEFERRED** — target phase's prompt explicitly re-defers with new target recorded below. A new ledger entry is added for the new target.
- **DROPPED** — silently or explicitly dropped. Not rebuilt by any phase. May be addressed later as a fix-pass or follow-up phase.
- **PICKED UP LATE** — originally dropped, rebuilt in a later phase as a fix-pass (e.g., Phase 18).

---

## 17-phase initiative (2026-04-09 → 04-10) — retroactive inventory

This section was created retroactively on 2026-04-11 after the audit that followed first-real-use of the shipped app revealed the Local Mode toggle was missing. Every entry below is a deferral I wrote in a workstream prompt and either forgot to thread forward or threaded incompletely.

### Ledger

| ID | Source phase | Target phase | Item | Status | Fix phase |
|---|---|---|---|---|---|
| L1 | Phase 3 | Phase 10 | **Local Mode toggle** in Settings.tsx — the "Use local models (Ollama)" switch per provider-registry.md §382–395. Backend IPCs POST `/api/local-mode/enable`, POST `/api/local-mode/disable`, GET `/api/local-mode/status`. | DROPPED | 18a |
| L2 | Phase 3 | Phase 10 | **Credential warnings UI** in ToolsMode — surface missing `${VAR}` references when a pulled contribution needs a credential the user hasn't set. | DROPPED | 18a |
| L3 | Phase 3 | Phase 10 | **OllamaCloudProvider** — optional backend provider variant for remote Ollama behind nginx. Marked optional in Phase 3 prompt. | DROPPED | 18a |
| L4 | Phase 7 | Phase 10 | **Cache-publish privacy opt-in checkbox** — Phase 7 ships `export_cache_manifest` with default-OFF privacy gate (returns `None` unless opted-in). Phase 10 was supposed to add the opt-in checkbox + warnings to the publish UI. | DROPPED | 18c |
| L5 | Phase 8 | Phase 10 | **Ollama `/api/tags` model list fetch** — Phase 8's `model_list:{provider_id}` option source used `tier_routing` entries as a stand-in for OpenRouter. For Ollama providers, the real source is `GET {base_url}/api/tags`. Phase 10 was supposed to add this. | DROPPED | 18a |
| L6 | Phase 9 | Phase 10 | **Schema migration UI** — Phase 9 shipped `flag_configs_needing_migration` helper and the `needs_migration` column on `pyramid_config_contributions`. Phase 10 was supposed to add the ToolsMode surface that lists flagged configs and triggers LLM-assisted migration. | DROPPED | 18d |
| L7 | Phase 12 | Phase 13 | **`search_hit` demand signal recording** — Phase 12 punted the search-to-drill tracking path because Wire Node had no session/referer mechanism. Phase 13 was supposed to add it during build viz event work. | DROPPED | 18b |
| L8 | Phase 12 | Phase 13+ | **`call_model_audited` cache retrofit** — Phase 12's cache retrofit sweep intentionally skipped the audited LLM call path because `call_model_audited` writes its own audit row and bypassed the `_and_ctx` variant. 4 sites in `evidence_answering.rs` + 1 in `chain_dispatch.rs` still burn tokens on every audited re-run. | DROPPED | 18b |
| L9 | Phase 13 | Phase 14/15 | **Folder/circle scoped pause-all DADBEAR** — Phase 13 shipped `scope: "all"` only. Folder + circle scopes for `pyramid_pause_dadbear_all` / `pyramid_resume_dadbear_all` deferred. | DROPPED — re-deferred in Phase 15 out-of-scope list | 18c |

### Picked up (for calibration)

| ID | Source | Target | Item | Status |
|---|---|---|---|---|
| L-X1 | Phase 5 | Phase 10 → Phase 14 | `pyramid_search_wire_configs` / `pyramid_pull_wire_config` | PICKED UP (Phase 10 stubbed with "Coming in Phase 14" placeholder; Phase 14 shipped the real impl) |
| L-X2 | Phase 11 | Phase 15 | Orphan broadcasts UI + red banner | PICKED UP (Phase 15 receiving prompt was framed as "aggregate Phase 11/12/13/14 primitives" which forced re-read) |

### Discovered-by-use (not deferrals, scope gaps surfaced during real use)

These are distinct from dropped handoffs. No earlier phase punted them — they weren't in any spec. They emerged when Adam started actually using the shipped app and noticed that a natural user flow didn't work.

| ID | Discovered during | Item | Claimed by |
|---|---|---|---|
| D1 | Phase 17 first-use on real folder | **Claude Code `memory/*.md` subfolder pickup** — when folder ingestion attaches a Claude Code conversation directory, the `memory/` subfolder containing project-scoped `.md` files (Claude's persistent memory about that folder) is silently ignored. Only `*.jsonl` conversation files are consumed. Those memory files ARE load-bearing project knowledge and belong in the pyramid graph. | Phase 18e |

### Root cause

9 of 11 cross-phase deferrals dropped silently. Of the 9 dropped, **6 landed on Phase 10** — a single receiving phase got stacked with frontend work from 5 prior phases plus its own spec scope, and the receiving prompt was written from the Phase 10 spec in isolation rather than from a deferral ledger. The only deferrals that survived were the ones where either (a) the source phase forced a visible "Coming in Phase N" placeholder in the intermediate receiving phase, or (b) the final receiving phase's prompt was structured around integration rather than from-spec implementation.

### Process fix

This ledger was created retroactively on 2026-04-11. From now on:

1. Every deferral marker in a workstream prompt draft generates a ledger entry at the time the source prompt is written.
2. Before writing a target phase's workstream prompt, the conductor greps this ledger for `target_phase = N` and verifies every entry has an explicit disposition in the new prompt.
3. Dropped deferrals are an acceptable outcome ONLY if the conductor explicitly says "dropping L-N because..." — there's no "forgot to thread it forward" row.

---

## Phase 18 fix bundle

Phase 18 claims all 9 dropped items (L1–L9). Workstream split:

- **18a** (Local Mode + Providers): L1, L2, L3, L5
- **18b** (Cache Integrity): L7, L8
- **18c** (Privacy + Pause-all Scoping): L4, L9
- **18d** (Schema Migration UI): L6

Each workstream runs the full implementer → verifier → wanderer ceremony per `feedback_wanderer_after_verifier.md`. Branches are parallel; merges are serialized into main after all four wander clean.
