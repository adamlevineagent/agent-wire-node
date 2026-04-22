# Plan-integrity pass artifact — rev 1.0.1 → rev 1.0.2

**Date:** 2026-04-22
**Skill:** `~/.claude/skills/plan-integrity/SKILL.md` v1.2 + pyramid-annotate contribution pattern
**Plan:** `docs/plans/walker-provider-configs-and-slot-policy-v3.md`
**Companion:** `docs/plans/walker-v3-yaml-drafts.md`
**Consumer inventory:** `docs/plans/history/walker-v3-consumer-inventory.md`
**Execution mode:** LITERAL — code-grep against the running codebase for every file/line claim the plan makes; pyramid annotations contributed back for each finding.

## Pair audit (2026-04-22)

Two fresh-eyes agents ran against `agent-wire-node2` pyramid (372 nodes, depth 3).

**Arch auditor (`a0ec93c05296d78d3`)** — 8/8 architectural premises verified in code. 7 annotations posted (ids 380-389).

**Integration auditor (`a3535e126370bce07`)** — consumer-inventory spot-check + 8 cross-cut verifications against live code. 12 annotations posted total (ids 384-395) on nodes L0-008, L0-113, L0-363, L0-375, L2-005, Q-L0-204.

## Plan-to-code gaps found and fixed in rev 1.0.2

1. **§2.12 synthetic-Decision target was wrong** — plan pointed at `stale_engine.rs:92-127` but stale_engine dispatch is DECOMMISSIONED (comments at lines 180/498/602/619-622). Live DADBEAR dispatch is `dadbear_supervisor.rs:514 dispatch_materialized_item`. Retargeted.
2. **Phase 2 `ollama_probe.rs` file doesn't exist** — actual probe is `probe_ollama()` function at `local_mode.rs:330`. Plan now references the function; extraction optional.
3. **§5.1 CPP market absorption had no named touchpoint** — the read is at `llm.rs:2216-2232` inside the market dispatch retry loop via `get_compute_participation_policy(&conn)` on a fresh SQLite connection. Named explicitly; Decision-in-scope threading flagged as real work.
4. **§5.1 `resolve_ir_model` framed as "retire" was wrong** — `chain_dispatch.rs:1067` already consults `provider_registry.resolve_tier()` as priority-2 (Phase 3 fix-pass). Reframed as "subsume" — walker v3 completes the partial migration. Framing matters for implementer mental model.
5. **Phase 0b missing rail: 4-part completeness** — `schema_registry.rs:655-670` exercises partial registration; no existing assertion that all four parts exist per schema. New `test_walker_schemas_four_part_complete` added.
6. **§8 save_onboarding IPC signature gap** — current IPC takes 5 operational fields; onboarding_state needs 5 different contribution fields. Phase 0a-2 extends signature OR adds companion IPCs. Named explicitly.
7. **§5.1 yaml_renderer peer callers named** — `list_tier_routing()` additional callers at `yaml_renderer.rs:575/:749` must migrate together with :428 or UI tier surface silently drifts.
8. **§5.1 build_runner.rs:328/:358 added to retires table** — live `use_chain_engine` consumer + `from_depth` error path becomes user-visible signal for §5.6.3 modal.
9. **Phase 0a module-boundary note** — `ProviderReadiness for Fleet` crosses `src-tauri/src/fleet.rs` (top-level) and `src-tauri/src/pyramid/fleet_mps.rs` boundary.
10. **Envelope writer grep discipline** — exactly 35 sites / 9 files on `INSERT INTO pyramid_config_contributions`. Dropping the `pyramid_` prefix expands to 44 sites / 7 files on a different table. CI deny-rule specified with full table name.

## Literal verification (post-fix)

```
$ grep -oE "(adds|all) +[0-9]+ +(new |local-only )?events?" plan.md
adds 22 local-only events   [§5.4.6 authoritative, single match]

$ grep -oE "[0-9]+ +chronicle +events" plan.md
[none — centralized to §5.4.6]

$ grep -oE "\{invariant:[^}]+\}" plan.md | sort -u
{invariant: app_mode_single_writer}
{invariant: config_contrib_active_unique}
{invariant: scope_cache_single_writer}

$ grep -oE "\{txn:[^}]+\}" plan.md | sort -u
{txn: pyramid_config_contributions, mode: OwnTransaction}
{txn: pyramid_config_contributions}

$ grep -c "INSERT INTO pyramid_config_contributions" src-tauri/src/**/*.rs
35  (exactly — verified match plan claim)

$ ls src-tauri/src/pyramid/ollama_probe.rs
No such file or directory  (plan no longer references this file)

$ grep -n "probe_ollama" src-tauri/src/pyramid/local_mode.rs | head -1
330:pub async fn probe_ollama(base_url: &str) -> OllamaProbeResult  ✓

$ grep -n "DECOMMISSIONED" src-tauri/src/pyramid/stale_engine.rs | wc -l
4  (confirms decommission; plan no longer uses stale_engine as dispatch target)

$ grep -n "fn dispatch_materialized_item" src-tauri/src/pyramid/dadbear_supervisor.rs
514:    async fn dispatch_materialized_item(  ✓ (new synthetic-Decision integration target)
```

## Pyramid contributions

12 annotations posted to `agent-wire-node2` (nodes L0-008, L0-113, L0-363, L0-375, L2-005, Q-L0-204). Future audit rounds can use these as informed-start context without re-reading the plan cold. This is the first rev where pyramid-annotate was part of the audit discipline; prior cycles were plan-only.

## Needs judgment (0 items)

All 10 gaps resolved via direct edits. No operator decisions pending.

## Clean checks

- 1 / 2 (count parity after §5.4.6 centralization, all match): ✓
- 3 (struct field vs catalog): ✓
- 4 (audit-history claims vs content): ✓ (rev 1.0.2 entry's specific file/line claims verified via grep before writing)
- 5 (section numbering): ✓ monotonic
- 6 (cross-referenced state fields): ✓ `onboarding_state`, `migration_marker`, `node_identity_history` all have named writers including the save_onboarding IPC gap disclosure
- 7 (companion rev-banner): ✓ 1.0.2 / 1.0.2
- 8 (sensitive-field parity): ✓
- 9 (count assertions): ✓ after §5.4.6 centralization
- 10 (enum variant coverage): ✓
- 11 (this artifact): ✓
- 12 (invariant tags): ✓ 3 invariant tags present, consistent mentions
- 13 (transaction boundaries): ✓ `{txn: pyramid_config_contributions}` named at all BEGIN IMMEDIATE sites; migration path explicitly `TransactionMode::JoinAmbient`
- 14 (contribution field-list parity — NEW this rev): ✓ `onboarding_state` field list matches across §2.18, §5.5.4, §5.6.2, §5.6.3, §8, and save_onboarding IPC gap disclosure

## Skill learnings

- **Pyramid-annotate discipline is now the gold standard.** Every agent finding annotated with mechanism-level insight leaves the knowledge graph materially richer; future audit rounds can start from annotations, not from re-reading the plan.
- **Literal code-grep before writing audit-history entries** is the only way to avoid the Check 4 overclaim pattern that bit rev 0.7/0.8. Rev 1.0.2 verified every file/line claim in the audit entry via grep output pasted into this artifact.
- **"Live code check" as a stage** between Stage 2 and implementation is new. Prior cycles were plan-vs-plan; this pair was plan-vs-code. It found 3 real drifts (stale_engine decommission, ollama_probe nonexistence, llm.rs:2216 touchpoint) that no paper audit had surfaced in 4 cycles. This discipline should be added as a required stage in conductor-audit-pass between Stage 2 Discovery and "implementation-ready" declaration.
