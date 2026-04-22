# Plan-integrity pass artifact — rev 0.9 → rev 1.0

**Date:** 2026-04-21
**Skill:** `~/.claude/skills/plan-integrity/SKILL.md` v1.2 (Checks 12-13 added; Check 9 implementation fixed)
**Plan:** `docs/plans/walker-provider-configs-and-slot-policy-v3.md`
**Companion:** `docs/plans/walker-v3-yaml-drafts.md`

## Pre-existing drift caught (by cycle 3 Stage 2 auditors, before rev-1.0 corrections)

1. **Check 9 self-failure (B-F10).** Rev 0.9's first use of Check 9 missed: "§5.4.6 says 18 events" alongside 20-item enumeration. Fixed by rewriting Check 9 implementation in rev 1.0 skill — count the enumeration first, compare to prose, not the other way around.
2. **Check 10 (B-F3).** §2.16.5 prose referenced `NetworkUnreachable` variant absent from §2.6 enum. Rev 1.0 adds variant.
3. **Check 7 residual (B-F4).** 5 companion YAML blocks still carried `source: bundled`. Rev 1.0 scrubbed.
4. **§2 storage-type contradiction (F-C3S2-1).** `BEGIN IMMEDIATE` wrapping `app_mode` in `pyramid_config` — the latter is a JSON file, not a SQL table. Rev 1.0 §2.17 rewritten to sequential startup. Check 13 (transaction-boundary compatibility) added to catch this class going forward.

## Auto-fixed in rev 1.0

5. §2.17 sequential startup — no transactional gate needed; AppMode in-memory.
6. §2.18 state table — migration_marker, onboarding_state, node_identity_history are contributions; AppMode is in-memory; all justifications explicit.
7. §5.3 migration — migration_marker supersession replaces sentinel-field pattern.
8. §5.6 lifecycle semantics — rollback/backup/restore/use_chain_engine-flip named.
9. §6 Phase 0a-1 canonical commit order (retires §11 B-F9 alternate list).
10. §6 Phase 0a-1 pre-flight requires consumer inventory artifact.
11. §6 Phase 1 test list expanded with 10 permutations from B-F4.
12. §5.4.6 event count authoritative at 21; Phase 0a-1 body defers to §5.4.6 as single source.
13. §12 "Picking this up cold" rewritten for rev 1.0 current state.
14. Companion drafts synced to rev 1.0.

## Needs judgment (0 items)

All drift resolved during rev 1.0 absorption. Storage-type decision (Option A new SQL table vs Option B promote pyramid_config) surfaced to Adam, who correctly rejected both in favor of contribution-native resolution with in-memory AppMode.

## Clean at rev 1.0

All 13 checks pass:
- Check 1: placeholders registered
- Check 2: 21 events declared, all appear in chronicle const list in Phase 0a-1, emission sites named across §2.9 / §2.16 / §2.17 / §5.4 / §5.5
- Check 3: DispatchDecision + ResolvedProviderParams fields all in §3 catalog
- Check 4: §11 audit history claims match section content
- Check 5: section numbering monotonic through §2.18
- Check 6: `app_mode` (RwLock in AppState), `migration_marker` / `onboarding_state` / `node_identity_history` (contributions via envelope writer) all have named writers
- Check 7: companion drafts rev-banner matches plan rev
- Check 8: sensitive-fields list matches schema_annotation `sensitive: true` set
- Check 9 (fixed): 21 events prose = 21 items enumeration (3+4+6+3+5)
- Check 10: `NetworkUnreachable`, `PeerIsV1Announcer` in §2.6 enum
- Check 11: this artifact exists
- Check 12 (new): ScopeCache single-writer invariant preserved (reloader task sole writer); AppMode single-writer invariant preserved (main.rs boot coordinator sole writer)
- Check 13 (new): `BEGIN IMMEDIATE` mentions all on `pyramid_config_contributions` table; no other table has transactional contention claimed

## Skill learnings carried forward

- Check 9's first failure demonstrated that "trust the prose number" is a brittle implementation. Rev 1.0's fix: count the list structurally, prose is the claim being verified.
- Checks 12-13 are new and untested. If they fail their flagship uses in a future rev, same fix: rewrite implementation to make the structural check authoritative, prose the claim.
