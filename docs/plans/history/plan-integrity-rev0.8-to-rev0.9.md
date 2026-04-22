# Plan-integrity pass artifact — rev 0.8 → rev 0.9

**Date:** 2026-04-21
**Skill:** `~/.claude/skills/plan-integrity/SKILL.md` v1.1 (Checks 9-11 added)
**Plan:** `docs/plans/walker-provider-configs-and-slot-policy-v3.md`
**Companion:** `docs/plans/walker-v3-yaml-drafts.md`

## Pre-existing drift caught (during rev-0.8 review)

Cycle 3 Stage 1 auditors found these BEFORE rev 0.9 corrections were applied. Rev 0.9 fixed them as part of structural absorption; this section records what needed fixing.

1. **Check 2 — Chronicle event count drift (B-F1).** §5.4.6 listed 14 events; Phase 0a body enumerated 12; Phase 0a exit criteria said "all 10 new events." Missing from Phase 0a: `EVENT_CONFIG_RETRACTED` (required by §5.4.4) and `EVENT_SCOPE_CACHE_LISTENER_RESTARTED` (required by §2.16.2). Fixed in rev 0.9 — count authoritatively set to 18 with full enumeration. Check 9 added to skill to prevent recurrence.
2. **Check 3 — Enum variant drift (B-F3).** §2.16.5 prose referenced `NotReady { NetworkUnreachable }` but §2.6's `NotReadyReason` enum didn't list that variant. Fixed in rev 0.9 — `NetworkUnreachable { consecutive_failures, last_success_at }` added to enum. Check 10 added to skill.
3. **Check 7 — Companion doc drift (B-F4).** Companion `walker-v3-yaml-drafts.md` header explicitly said "seeds still show `source: bundled` are stale; strip on next regeneration" and then didn't strip 5 blocks. Fixed in rev 0.9 — all 5 instances replaced with a comment noting envelope-set semantics.
4. **Check 4 — Audit-history overclaim (F-C3-10).** Rev 0.8 §11 entry said "Plan-integrity skill caught in its first run: [8 drift items]" without a persisted artifact. Fixed in rev 0.9 — artifact at `docs/plans/history/plan-integrity-rev0.7-to-rev0.8.md` written retroactively. Check 11 added to skill for future runs.

## Auto-fixed in rev 0.9

5. Section numbering: §2.17, §2.18, §5.5 added in canonical order. No out-of-order issues.
6. Cross-referenced fields: `app_mode` (new in §2.17) read by build-starter code paths — write site named in §2.17.1. `onboarding_complete_at` now a contribution per §2.18 — write site is Page 4 onboarding handler per §8. `node_identity_history` contribution per §2.18 — write site is onboarding rotation flow per §2.16.7.
7. Sensitive-field parity: §2.15's sensitive-fields list unchanged; new contributions (`onboarding_state`, `node_identity_history`) are not in the resolver-parameter model, so catalog parity doesn't apply.
8. Count assertions: 18 events declared in Phase 0a enumeration, 18 named in §5.4.6 list, Phase 0a exit criteria says "all 18." Parity verified.

## Needs judgment (0 items)

All drift resolved automatically during rev 0.9 absorption. No blocked judgment calls.

## Clean

- Checks 1, 5, 6, 8, 9, 10, 11 — all pass.

## Notes for future skill iterations

- Rev 0.9 added ~2000 LOC across §2.17, §2.18, §5.5, audit history. Drift risk was high; caught via explicit pre-commit verification.
- Skill-meta-learning (A-brain-dump-6 / B-F2): the plan-integrity skill itself is single-threaded with no meta-check. If Check 9 has a bug (regex false-positive), the next rev ships with drift unnoticed. Consider a v2.0 skill-self-test pass.
