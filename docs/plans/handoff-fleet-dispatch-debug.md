# Handoff: Fleet Dispatch Debug

**Date:** 2026-04-15
**Problem:** Fleet dispatch never fires despite all prerequisites being met. The `config.fleet_roster` is None on the LlmConfig used during builds.
**Status:** Diagnostic logging confirms the exact failure point. Root cause narrowed but not resolved.

---

## What Works

- Both nodes online: `@playful/mac-lan` (laptop) and `@playful/behem` (5090)
- Same operator_id ✓
- Fleet JWT issuing and receiving ✓ (403 fixed — JWT public key now persists in session.json and self-heals via heartbeat)
- Fleet announces succeeding ✓ (logs show "Fleet announce to ... succeeded")
- Fleet peer cards show serving_rules ✓ (`ollama-catchall` on both peers)
- Queue depth visible ✓ (BEHEM sees mac-lan at queue load 4)
- Dispatch policy has `provider_id: fleet` first ✓
- `matched_rule_name` = "ollama-catchall" ✓
- `has_fleet` = true ✓

## What Fails

```
Fleet Phase A: entry check has_fleet=true rule=ollama-catchall fleet_roster_present=false
```

**`config.fleet_roster` is None on the LlmConfig used during pyramid builds.**

## What We Know

1. `fleet_roster` is an `Arc<RwLock<FleetRoster>>` created at main.rs:11569
2. It IS set on `pyramid_state.config` at main.rs:11587 (DB hydration path) and 11608 (fallback path)
3. The log at 18:20:48 confirms "Dispatch policy loaded... compute queue wired" (line 11588) — so the fleet_roster set at 11587 executed
4. Builds clone LlmConfig via `llm_config_with_cache` (mod.rs:924-925) which reads from `self.config.read().await.clone()` — this is the SAME `Arc<RwLock<LlmConfig>>` that startup wrote fleet_roster to
5. PyramidState at lines 6293, 6350 clone the `config: Arc` from `state.pyramid.config` — same Arc, same underlying RwLock
6. ConfigSynced at line 11727 writes dispatch_policy + provider_pools but does NOT reset fleet_roster
7. Profile apply at line 5936 replaces the whole config but preserves fleet_roster (lines 5935, 5952-5953)
8. No code path was found that explicitly sets fleet_roster to None after startup
9. `to_llm_config()` (mod.rs:688) constructs with `fleet_roster: None` — but this is only called for the initial config, before startup code sets it

## The Mystery

The fleet_roster Arc is set on the config at startup. No code path resets it to None. The build clones from the same config. Yet the build's config has `fleet_roster: None`.

## Update: with_runtime_overlays_from fix applied but not sufficient

The debugger found and fixed the routes.rs profile-apply path (missing fleet_roster preservation). A canonical `with_runtime_overlays_from` helper now handles both profile-apply paths. **But fleet_roster is STILL None on the build's LlmConfig.**

Confirmed:
- The fix IS in the running binary (installed 11:45, launched 11:46)
- Startup sets fleet_roster at line 11587 (log confirms "Dispatch policy loaded... compute queue wired")
- Both profile-apply paths now use `with_runtime_overlays_from`
- Only 2 full-config replacement sites found in main.rs and routes.rs — both fixed
- Yet `fleet_roster_present=false` persists in Phase A logs

**The config is losing fleet_roster through a path that is NOT a full replacement.** Something else is clearing it between startup wiring and the build's config clone. Candidates:
- ConfigSynced listener at main.rs:11727 — sets dispatch_policy + provider_pools but doesn't touch fleet_roster (should be safe)
- A field-level `cfg.fleet_roster = None` somewhere we haven't found
- The config being cloned BEFORE the startup wiring (timing race)
- The PyramidState.config Arc pointing to a different LlmConfig instance than the one startup wired

## Possible Remaining Explanations

1. **The config is being replaced (not field-updated) somewhere we didn't find.** A `*cfg = new_config` that creates a fresh LlmConfig without fleet_roster.

2. **The pyramid_state used by the build is not the one we think.** Three PyramidState constructions exist (lines 6293, 6350, 11497). Maybe the build path uses one that was constructed before fleet_roster was set.

3. **The `to_llm_config_with_runtime()` path (line 5921) resets the config.** This constructs from `to_llm_config()` which has `fleet_roster: None`. If this runs at startup (initial model detection/profile), it creates a fresh config that overwrites fleet_roster before the build starts.

4. **Timing: `to_llm_config_with_runtime()` runs DURING AppState construction** (before line 11604). The initial LlmConfig on PyramidState is created from `to_llm_config()` with None. Then line 11604 sets fleet_roster. But if something between construction and 11604 triggers a config read-clone-modify-write cycle, the fleet_roster might get lost.

## How to Monitor

**Logs:** The Wire Node log file is at:
```
/Users/adamlevine/Library/Application Support/wire-node/wire-node.log
```

**Watch fleet dispatch decisions in real time:**
```bash
tail -f "/Users/adamlevine/Library/Application Support/wire-node/wire-node.log" | grep --line-buffered "Fleet Phase A\|fleet_dispatched\|fleet_returned\|fleet dispatch\|no peer serves\|fleet_roster"
```

**Watch fleet announces:**
```bash
tail -f "/Users/adamlevine/Library/Application Support/wire-node/wire-node.log" | grep --line-buffered "Fleet announce\|announce.*succeeded\|announce.*failed"
```

**Check chronicle events for fleet activity:**
```bash
sqlite3 "/Users/adamlevine/Library/Application Support/wire-node/pyramid.db" "SELECT event_type, source, count(*) FROM pyramid_compute_events GROUP BY event_type, source;"
```

**Build the app:**
```bash
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"
cd "/Users/adamlevine/AI Project Files/agent-wire-node"
cargo tauri build 2>&1 | tail -20
```

**Install:**
```bash
rm -rf "/Applications/Wire Node.app" && cp -R "src-tauri/target/release/bundle/macos/Wire Node.app" /Applications/
```

**BEHEM (5090 PC) also needs updates:** `git pull && cargo tauri build` then reinstall on Windows.

## Debug Strategy

1. Add `tracing::info!("fleet_roster SET on pyramid config")` right after line 11609
2. Add a log inside `llm_config_with_cache` (mod.rs:924) showing `fleet_roster.is_some()` on the cloned config
3. Grep for ALL sites that do `*live = ` or `*cfg = ` (full config replacement) — there may be one we missed
4. Check if `to_llm_config_with_runtime()` runs before line 11604

## Files with Diagnostic Logging Already Added

- `src-tauri/src/pyramid/llm.rs` — Fleet Phase A entry check (line 876+): logs has_fleet, matched_rule_name, fleet_roster_present, providers
- `src-tauri/src/pyramid/llm.rs` — Fleet dispatch skip logs (after line 902): logs when find_peer_for_rule returns None or JWT is empty
- `src-tauri/src/server.rs` — Fleet announce handler (line 1676+): logs received announcement with serving_rules

## Key Code Locations

| What | File | Line |
|---|---|---|
| fleet_roster created | main.rs | 11569 |
| fleet_roster set (DB hydration) | main.rs | 11587 |
| fleet_roster set (fallback) | main.rs | 11608-11609 |
| ConfigSynced: dispatch_policy updated (fleet_roster NOT touched) | main.rs | 11727-11729 |
| Profile apply: config replaced, fleet_roster preserved | main.rs | 5921-5953 |
| to_llm_config: fleet_roster hardcoded None | mod.rs | 688 |
| llm_config_with_cache: clones from shared config | mod.rs | 924-925 |
| Fleet Phase A check | llm.rs | 876-880 |
| Fleet announce handler | server.rs | 1634-1683 |

## Session Summary (for context)

This session built:
1. Compute Market Phase 1 (queue, GPU loop, Market tab) — working ✓
2. Fleet routing (heartbeat roster, JWT, dispatch/announce) — announces work ✓, dispatch doesn't fire ✗
3. Fleet-as-dispatch-provider (rule-name routing) — dispatch policy correct ✓
4. Node Identity Phase 1a (handle paths, multi-machine registration) — working ✓
5. Compute Chronicle (persistent event log, 9 write points, frontend) — working ✓
6. JWT persistence fix (fleet 403 root cause) — fixed ✓
7. Heartbeat self-healing JWT delivery — fixed ✓
8. Fleet dispatch debug — **IN PROGRESS: fleet_roster is None on build's LlmConfig**

## Commits (agent-wire-node)

- `567eec6` — Phase 1 compute market + fleet routing
- `3ead771` — Fleet routing by rule name
- `783e2f4` — Node identity handle paths
- `a766ad0` — HeartbeatFleetEntry serde fix
- `48edec8` — DADBEAR + handle_path display fix
- `b63d183` — Compute Chronicle
- `c38eb67` — JWT persistence (fleet 403 fix)
- `022f6a9` — Heartbeat self-healing JWT
- `593c7a9` — Merge conflict resolution
- (uncommitted) — Diagnostic logging for fleet Phase A

## Commits (GoodNewsEveryone)

- `a550b3ab` — Wire market infrastructure + fleet JWT
- `9bc42464` — Entity creation fix
- `438e45e8` — Node identity migrations + registration + heartbeat
- `32949cc8` — Backfill sanitization fix
- `22e4f19e` — Heartbeat returns jwt_public_key
