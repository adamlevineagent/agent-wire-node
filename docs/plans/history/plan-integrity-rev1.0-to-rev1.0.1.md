# Plan-integrity pass artifact — rev 1.0 → rev 1.0.1

**Date:** 2026-04-21
**Skill:** `~/.claude/skills/plan-integrity/SKILL.md` v1.2
**Plan:** `docs/plans/walker-provider-configs-and-slot-policy-v3.md`
**Companion:** `docs/plans/walker-v3-yaml-drafts.md`
**Execution mode:** LITERAL — lists counted via grep, tags grepped, numbers compared arithmetically. Prior artifacts were written as narrated claims without literal execution. This one is not.

## Pre-existing drift caught by cycle 4 Stage 1 auditors (before rev 1.0.1 corrections)

1. **Check 9 self-failure (both A-F1 + B-F1, CRITICAL).** Rev 1.0 shipped with 3 different event counts: §5.4.6 = 21, Phase 0a-1 body = 20, Phase 0a exit criteria = 18. Plus Phase 6 color-map ref "14 local-only events." Skill was invoked between rev 0.9 and 1.0 but the artifact's claim "Check 9: 21 events prose = 21 items enumeration" was not literally verified — the body-said-20 and exit-said-18 drift existed and wasn't caught.
2. **Check 2 failure (A-F2 / B-F2).** `dispatch_failed_policy_blocked` referenced in §3 catalog (line 460, `on_partial_failure: fail_loud` description) and Phase 1 tests (line 847) but absent from §5.4.6 registry. This is a 22nd event. Check 2 ("every event has a registry entry") missed it.
3. **Check 13 failure (A-F3).** §2.16.1 wraps supersede in `BEGIN IMMEDIATE`. §5.3 migration opens outer `BEGIN TRANSACTION`. Step 6's `migration_marker` supersession would nest → SQLite errors. Check 13 ("transaction-boundary compat") was added rev 1.0 specifically but didn't catch this because the plan had no `{txn:}` tags for the check to grep.
4. **Check 12 vaporware (B-F6).** Rev 1.0 added Check 12 (invariant-tag coverage) referencing `{invariant: X}` markers. Plan contained zero such markers. Check could not fire. Same pattern at the skill-discipline layer.

## Auto-fixed in rev 1.0.1

All fixes are literal, with post-fix literal verification:

5. **Count drift** — §5.4.6 updated to 22 (added `dispatch_failed_policy_blocked` to Decision lifecycle, 4+4+6+3+5=22). Phase 0a-1 body: "count per §5.4.6" (no number restated). Phase 0a exit criteria: "per §5.4.6." Phase 6 color-map ref: "22 local-only events." §11 rev-1.0 "21 chronicle events total" bullet replaced with "§5.4.6 authoritative." §5.4.2 "brings total to 21" struck.
6. **Nested BEGIN IMMEDIATE (A-F3)** — `TransactionMode::{OwnTransaction, JoinAmbient}` parameter added to `supersede_config_contribution`. Migration path (§5.3 step 6) uses `JoinAmbient`. §2.16.1 tagged `{txn: pyramid_config_contributions, mode: OwnTransaction}`.
7. **Phase 0a-1 commit reorder (A-F4 / B-F5)** — shim introduced in commit 4 (refactor 35 INSERT sites to pass-through writer); commit 5 activates validation + BEGIN IMMEDIATE + unique index atomically. No window with unprotected legacy INSERTs against an enforcing index.
8. **`use_chain_engine` explicit-false (A-F5)** — §5.6.3 rewritten: migration inspects operator intent; explicit `false` triggers boot modal with ACK recorded as `chain_engine_enable_ack` field on `onboarding_state` contribution.
9. **§8 onboarding residual (B-F3)** — struck "or pyramid_config.json"; `onboarding_state` contribution is the only path.
10. **Phase 0a exit criteria language (B-F4)** — updated from rev-0.9 transactional-gate language to §2.17 sequential-boot test spec.
11. **Invariant tags (B-F6)** — added `{invariant: config_contrib_active_unique}` + `{txn: pyramid_config_contributions}` to §2.16.1; `{invariant: scope_cache_single_writer}` to §2.16.2; `{invariant: app_mode_single_writer}` to §2.17.1.
12. **PUNCHLIST credits (B-F8)** — §7 collapse table adds rows for P0-1, P0-2, P1-5, P2-8.
13. **`migration_unknown_providers_ack` (A-F6 / B-F7)** — folded into `onboarding_state.migration_acks`.
14. **BreakerState rehydrate (A-F7)** — explicit note added to §2.16.4.
15. **Rollback vs migration_marker (A-F8)** — §5.6.1 re-supersedes to v2 via rollback-context bypass, not retraction.

## Literal verification (post-fix)

```
$ grep -oE "(adds|all) +[0-9]+ +(new |local-only )?events?" plan.md
adds 22 local-only events    [single match, matches §5.4.6]

$ grep -oE "[0-9]+ +chronicle +events" plan.md
[no matches — all prose count removed except §5.4.6 authoritative]

$ grep -oE "\{invariant:[^}]+\}" plan.md | sort -u
{invariant: app_mode_single_writer}
{invariant: config_contrib_active_unique}
{invariant: scope_cache_single_writer}

$ grep -oE "\{txn:[^}]+\}" plan.md | sort -u
{txn: pyramid_config_contributions, mode: OwnTransaction}
{txn: pyramid_config_contributions}

$ grep -n "^### 2\." plan.md
[monotonic 2.9 → 2.10 → ... → 2.17 → 2.18, no drift]

$ wc -l §5.4.6 enumeration = 4+4+6+3+5 = 22  [matches prose "adds 22"]
```

## Needs judgment (0 items)

All drift resolved automatically. No judgment calls.

## Skill learnings

1. **"Trust the prose number" is still a brittle failure mode.** Even rev 1.0's Check 9 rewrite (count-enumeration-first) can be bypassed if the skill invocation is narrated rather than executed. Rev 1.0.1 ran the grep commands literally. Future runs must include the shell-command output (or equivalent verification) in the artifact, not just claims.
2. **Checks 12-13 are growth-path checks.** They need tags in the plan to grep for. Rev 1.0.1 adds tags; future revs that introduce new single-writer/single-reader invariants or new transaction boundaries must tag them, or the checks silently pass without verifying anything.
3. **Meta-meta:** the recurring "promised infrastructure without wiring" pattern (Roots 13, 18, 23, 28) appeared AGAIN at the skill-discipline layer (Checks 12-13 were vaporware; Check 9's rewrite trusted prose). Every new enforcement mechanism must have an execution-verification step, not just a specification step.

## Cycle 4 Stage 2 gate

Rev 1.0.1 is ready for either Cycle 4 Stage 2 discovery audit OR the Codex fresh-eyes pass Adam requested. Both auditors in Cycle 4 Stage 1 independently recommended stopping paper audits — residuals-only trend has held through Stage 1.
