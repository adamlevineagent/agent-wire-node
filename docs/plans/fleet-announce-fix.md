# Fleet Announce Fix — Peer-to-Peer State Propagation

**Date:** 2026-04-15
**Problem:** Fleet peers see each other in the roster (heartbeat works) but fleet dispatch never fires because `serving_rules` is empty on every peer. The announce (which carries serving_rules) is either not reaching peers or not being processed correctly.
**Root cause:** TBD — diagnostic logging added to identify the exact failure point. Likely one of: announce not firing, JWT missing/rejected, announce body parse failure, or `serving_rules` empty in the announcement.
**Design principle:** Fleet is peer-to-peer. The Wire brokers initial discovery. Peers announce capabilities directly via tunnels. The announce must be as reliable as document serving (same tunnels, same infrastructure).

---

## Diagnosis

The fleet dispatch decision chain in llm.rs:876-884:

```
1. resolved_route has "fleet" provider?        → YES (dispatch policy has it)
2. skip_fleet_dispatch is false?               → YES (normal build path)
3. matched_rule_name is not empty?             → YES ("ollama-catchall")
4. config.fleet_roster is Some?                → YES (wired at startup)
5. roster.find_peer_for_rule("ollama-catchall") → NONE ← THIS FAILS
```

`find_peer_for_rule` filters by: not stale (120s), `serving_rules.contains(rule_name)`. The peers ARE in the roster (heartbeat puts them there), but their `serving_rules` is `Vec::new()` because:

- `update_from_heartbeat` creates peers with `serving_rules: Vec::new()` (line 70)
- Only `update_from_announcement` populates `serving_rules` (line 110)
- The announce is either not firing, not reaching the peer, or being rejected

## Possible Failure Points

### F1: Fleet JWT is None — announce skips entirely
`announce_to_fleet` (fleet.rs:309-313) returns immediately if `roster.fleet_jwt` is None. The JWT comes from the heartbeat response. If the heartbeat response doesn't include `fleet_jwt`, or the parsing fails, announces never fire.

**Check:** Is `fleet_jwt` populated in the roster? The heartbeat handler we modified returns it, but only if `signFleetToken` succeeds.

### F2: Announce POST returns non-200 — fire-and-forget loses the error
The announce is `tokio::spawn` with a 5-second timeout. Failures are logged at `warn` level but the caller never knows. If every announce fails (JWT rejected, tunnel unreachable, wrong path), `serving_rules` stays empty forever.

**Possible causes:**
- Fleet JWT verification fails on the receiving node (wrong key, expired, wrong audience)
- Operator ID mismatch (JWT's `op` claim doesn't match receiving node's `operator_id`)
- The announce body doesn't deserialize into `FleetAnnouncement` (struct mismatch between sender and receiver — different code versions?)
- Tunnel URL is stale or wrong

### F3: Announce fires but `serving_rules` is empty in the announcement
`derive_serving_rules` returns empty if:
- `loaded_models` is empty (local mode disabled or no Ollama model)
- Dispatch policy has no rules with `is_local: true`
- Dispatch policy is None

### F4: FleetRoster uses node_id as HashMap key but the announce uses a different node_id
The heartbeat returns `node_id` (UUID from wire_nodes). The announce sends `announcement.node_id` (from auth.node_id). If these differ (e.g., the node re-registered and got a new UUID), the announce updates a DIFFERENT HashMap entry than the heartbeat created.

### F5: The announce POST is going to the wrong path
`format!("{}/v1/fleet/announce", peer.tunnel_url)` — if the tunnel URL already has a trailing slash or path component, the URL could be malformed.

## The Fix

### Step 1: Add visibility (diagnostic logging) — ALREADY DONE
Logging added to:
- llm.rs Phase A: log peer count, serving_rules per peer, JWT presence
- server.rs handle_fleet_announce: log received announcement with serving_rules

### Step 2: Fix all identified issues

**2a. Ensure fleet_jwt propagates correctly**
The heartbeat response includes `fleet_jwt`. The parsing at main.rs:12689-12692 extracts it. Verify: is the `fleet_jwt` field name in the heartbeat response JSON EXACTLY `fleet_jwt`? If the server returns it as `fleetJwt` (camelCase) but the parser looks for `fleet_jwt` (snake_case), it silently misses.

Check: GoodNewsEveryone heartbeat handler — what key name does it use in the response JSON? The handler we wrote uses `fleet_jwt` (snake_case) in the response body. The Rust parser uses `response.get("fleet_jwt")`. These should match.

**2b. Make the announce NOT fire-and-forget for diagnostics**
Currently announces are `tokio::spawn` and the result is discarded. For the first announce after heartbeat, await the result and log it. Subsequent announces can stay fire-and-forget.

**2c. Validate tunnel URL format before POST**
Before constructing `{tunnel_url}/v1/fleet/announce`, verify the URL parses correctly. Log the full URL being POSTed to.

**2d. Add fleet roster state to `get_fleet_roster` IPC**
The frontend's Market tab calls `get_fleet_roster`. Ensure it returns `serving_rules` per peer so the UI can show what each peer serves. Currently the MarketDashboard shows handle paths but not serving rules.

**2e. Add a fleet health check endpoint**
New IPC command: `get_fleet_health` that returns:
- Roster peer count
- Per-peer: serving_rules, last_seen, last_announce_result
- Fleet JWT present/expired
- Last announce timestamp and result per peer

This gives the operator (and us) real-time visibility into why fleet dispatch isn't firing.

### Step 3: Ensure serving_rules derives correctly on both nodes

Both nodes need:
1. Local mode enabled with a model loaded → `load_local_mode_state` returns the model
2. Dispatch policy with `is_local: true` on a route entry → `derive_serving_rules` finds it
3. The derived `serving_rules` is non-empty → announce carries it

Verify both by reading the DB on each node (we already checked the laptop — it has the right dispatch policy and model).

### Step 4: Handle the node_id key mismatch

If the heartbeat creates a peer keyed by one UUID and the announce updates a different key (because auth.node_id differs), the peer gets orphaned state. The fix: the announce should use the same node_id as the heartbeat roster entry. Or: key the roster by `handle_path` instead of `node_id` (the long-term fix from the node identity plan).

## Implementation

### Files to modify

| File | Change |
|---|---|
| `fleet.rs` | Log announce send result. Add last_announce_result to FleetPeer. |
| `main.rs` | Log fleet JWT presence after heartbeat parsing. First announce: await result instead of fire-and-forget. |
| `server.rs` | Already has diagnostic logging (added above). |
| `llm.rs` | Already has diagnostic logging (added above). |
| `main.rs` | Add `get_fleet_health` IPC command. |
| `MarketDashboard.tsx` | Show serving_rules per peer. Show fleet health status. |

### What to verify after fix

1. Fleet JWT is present in roster after heartbeat
2. Announce POST reaches the peer (200 response)
3. Peer's `serving_rules` is non-empty after receiving announce
4. `find_peer_for_rule("ollama-catchall")` returns a peer
5. Fleet dispatch fires and produces chronicle events with source "fleet"
6. The Chronicle shows fleet_dispatched → fleet_returned events
7. The MarketDashboard shows serving_rules on peer cards

## What NOT to change

- Don't route fleet state through the Wire heartbeat (fleet is peer-to-peer)
- Don't change the announce architecture (it's correct, the implementation has a bug)
- Don't remove fire-and-forget on announces (just add diagnostic visibility)
