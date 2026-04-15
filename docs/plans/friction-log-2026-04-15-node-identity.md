# Friction / Implementation Log — Node Identity Phase 1a

**Date:** 2026-04-15 overnight session
**Scope:** Node handle paths, multi-machine registration, fleet roster with handle paths

---

## Session Timeline

- **00:10** — macOS release build completed (fleet-as-dispatch-provider code)
- **00:15** — Installed to /Applications, Adam tested. Discovered both machines share node ID.
- **00:20** — Diagnosed: `register-with-session` upserts by agent_id, not by machine. One agent = one node.
- **00:30** — Adam: "this is our software problem." Correct — registration conflates agent with node.
- **00:40** — Designed node identity handle paths plan. Adam pushed for maximal: handle paths, not UUIDs.
- **01:00** — Plan written. Stage 1 informed audit launched (2 agents).
- **01:30** — Audit returned: CRITICAL blast radius on agent_id rename (15+ handlers). Split into Phase 1a (additive, zero breaking) and Phase 1b (rename).
- **01:45** — Stage 1 corrections applied (10 items).
- **02:00** — Stage 2 discovery audit launched (2 agents).
- **02:30** — Discovery found: token rotation kills other machines (fleet-killer), agent_type='desktop' doesn't exist, operator_handle not in responses.
- **02:40** — Adam clarified: operator_handle = claimed_handle ?? login_email. Never null.
- **02:50** — Stage 2 corrections applied (8 items). Plan audit-clean.
- **03:00** — Wire + Node builds launched in parallel.

## Friction Points

### F1: Two machines, one node — the foundational identity bug
**What:** Registration finds node by agent_id. One operator = one agent = one node. Second machine overwrites first.
**Impact:** Fleet routing architecturally complete but impossible to test. Blocked for hours.
**Root cause:** Identity model conflates agent (Wire identity) with node (physical machine).
**Lesson:** Multi-machine fleet should have been in the original fleet routing test plan. The "happy path" test requires two distinct nodes, which requires the identity model to support it.

### F2: Rate limiter behind proxy — all traffic is 127.0.0.1
**What:** `register-with-session` IP rate limit was 30/hour. All traffic from all users shares 127.0.0.1 behind the Temps proxy.
**Impact:** After the 5090 registered, the laptop couldn't register. Rate limit exhausted by retry loop.
**Fix:** Bumped to 500/hour. Per-email limit (5/hour) is the real protection.
**Lesson:** Rate limiters behind reverse proxies need awareness of the proxy topology. IP-based limits are useless when everything is localhost.

### F3: OTP consumed but registration fails — stuck state
**What:** Supabase OTP verified successfully (200), then register-with-session returned 429. App showed error. User retried OTP verification — code already consumed (403). Stuck.
**Impact:** Adam couldn't log in for several minutes. Required server redeployment to reset rate limit state.
**Lesson:** The registration flow should save the Supabase session even if Wire registration fails. On retry, skip OTP and use the saved session. The auth state machine needs a "Supabase authenticated but Wire registration pending" state.

### F4: agent_id rename blast radius — 20+ files
**What:** Plan proposed renaming `wire_nodes.agent_id` to `active_agent_id`. Auditors found it's referenced in 15+ API handlers, SQL RPCs, merge logic, indexes.
**Impact:** Would have broken every node API endpoint on deploy. Caught by audit.
**Lesson:** Column renames on widely-referenced tables need grep-based impact analysis before planning. The rename was conceptually correct but operationally dangerous. Phased approach (add new, dual-write, migrate consumers, remove old) is mandatory for production tables.

### F5: Token rotation kills fleet peers
**What:** Registration handler revokes all previous api_client_secrets when a new machine registers. Under shared-agent model, machine B's registration invalidates machine A's token.
**Impact:** Would have made fleet routing work once then break on the next heartbeat cycle.
**Lesson:** Token management designed for single-machine-per-agent doesn't survive multi-machine. The secret rotation logic needs to be node-scoped, not agent-scoped.

### F6: Hostname detection on macOS GUI — env vars are empty
**What:** Current `hostname()` function reads COMPUTERNAME/HOSTNAME env vars. These are typically empty when launching a macOS app from Finder/dock. Falls back to "Wire Node" → every Mac gets the same handle.
**Impact:** Would have recreated the collision problem at the handle level.
**Lesson:** Use system calls (gethostname crate) for hardware identity, not environment variables. GUI apps don't inherit shell environment.

### F7: operator_handle resolution — not as simple as it sounds
**What:** The handle path `@hello/BEHEM` requires resolving the operator's handle. But wire_operators has no handle column. Handles live in wire_handles, are optional, can be in layaway, and multiple handles per operator exist.
**Impact:** Auditors flagged as critical blocker. Adam clarified: fallback to email.
**Resolution:** `operator_handle = claimed_wire_handle ?? login_email`. Never null. Simple once you know the rule, but the rule wasn't documented anywhere.
**Lesson:** Handle/identity resolution paths should be documented as a utility function, not rediscovered by each feature that needs them.

### F8: Backfill sanitization order — LOWER after REGEXP_REPLACE
**What:** The backfill SQL ran `LOWER(REGEXP_REPLACE(candidate, '[^a-z0-9-]', '-', 'g'))` — the regex matched `[^a-z0-9-]` which EXCLUDES uppercase letters, so 'W' in "Wire Node" was replaced with '-' before LOWER could save it. Result: "ire--ode" instead of "wire-node".
**Impact:** 3 nodes got garbled handles. Fixed with a manual UPDATE. Migration SQL corrected for future runs.
**Fix:** LOWER first, then REGEXP_REPLACE. Order matters.
**Lesson:** Sanitization pipelines must be ordered: normalize case → filter characters → trim. Not the reverse.

## Decisions Made

- **Phase 1a/1b split:** Don't rename agent_id. Add new columns alongside. Zero breaking changes.
- **Shared agent model:** One agent per operator, shared by all machines. Node_token is machine-scoped.
- **Handle path format:** `@{operator_handle}/{node_handle}`. Operator handle falls back to email.
- **Transition strategy:** Both old fields (node_id, operator_id UUID) and new fields (handle_path) coexist during transition. Old nodes work unchanged.
- **No secret revocation:** Registration adds secrets, doesn't revoke. Orphan cleanup on a separate schedule.

### F9: fleet_roster None on build's LlmConfig — the final fleet dispatch blocker
**What:** Everything else works — both nodes online, announces succeeding, serving_rules propagating, dispatch policy has fleet first, matched_rule_name correct. But `config.fleet_roster` is `None` on the LlmConfig used during builds.
**Impact:** Fleet dispatch never fires. All LLM calls go local despite idle fleet peer available.
**Diagnosis:** `fleet_roster` is set on `pyramid_state.config` at startup (confirmed by logs). Builds clone from the same config via `llm_config_with_cache` → `self.config.read().await.clone()`. Yet the clone has `fleet_roster: None`. No code path was found that explicitly resets it. Either a full-config replacement is happening that we haven't found, or there's a timing issue between config construction and fleet_roster wiring.
**Status:** Handoff to debugger. See `handoff-fleet-dispatch-debug.md`.

## Implementation Status

| Item | Status |
|---|---|
| Wire migration (columns + backfill + settle RPC) | Building |
| Registration handler rewrite | Building |
| Heartbeat fleet roster with handle_path | Building |
| Fleet JWT with node_handle | Building |
| NodeIdentity struct + persistence | Building |
| gethostname + token generation | Building |
| 7 register_with_session call sites | Building |
| Fleet struct handle_path fields | Building |
| Heartbeat parsing + operator_handle | Building |
| Serial verifier | Pending |
| Uninformed wanderer | Pending |
| Commit + push | Pending |
| Build + install macOS | Pending |
| Test fleet routing with two nodes | Pending |
