# Fleet MPS: Three First-Class Objects

**Date:** 2026-04-15
**Source:** MPS audit by debugging session after fleet dispatch was working but fragile
**Status:** Plan approved, not yet implemented. Next session's primary build target.

---

## The Problem

One concept (dispatch policy) is doing three jobs:
- Encoding operator intent (should this node dispatch? serve?)
- Being the only capability transport (announce carries serving_rules)
- Empty capability data means both "serves nothing" AND "we don't know yet"

The system can be technically alive and still feel broken.

## The Three Objects

### 1. Node Role — operator intent (durable config)
- `fleet_policy` contribution schema
- Two booleans or one enum: `dispatch_enabled`, `serve_enabled`
- Three modes: **Coordinator** (dispatch yes, serve no), **Hybrid** (both), **Worker** (dispatch no, serve yes)
- Dispatch policy generated FROM this, not hand-edited
- UI: 3-state control in Settings

### 2. Capability Snapshot — what this node can serve (derived runtime)
- Derived from: local mode state + loaded model + dispatch policy + queue depth + tunnel
- Fields: `servable_rules`, `models_loaded`, `total_queue_depth`, `capability_version`, `computed_at`
- Empty means "known empty" (computed, nothing to serve), not "unknown"

### 3. Peer Fleet State — what we know about another node (runtime + cache)
- `capability_status`: `unknown | fresh | stale | failed`
- `last_capability_sync_at`, `last_capability_error`, `last_capability_source` (announce | pull | cache)
- `declared_role` (Coordinator | Hybrid | Worker)
- Dispatch only skips after fresh capability proves no match OR pull failed

## Capability Transport: Private, Reliable, Not Single-Shot

- Heartbeat stays discovery-only (who exists). No GPU inventory to the Wire.
- Announce stays as push (peer-to-peer, fast updates).
- NEW: `GET /v1/fleet/capabilities` pull endpoint for reconciliation.
- Pull fires on: peer discovery, app startup, peer state `unknown`, before skipping a peer, periodic stale refresh.
- Result: privacy preserved, no longer fragile.

## Warm-Start Cache

- Persist last-known peer capabilities locally.
- On restart, peer cards show last-known state marked "stale" until refreshed.
- No more blank cards after restart.

## Dispatch Selection

- Phase A uses explicit role + explicit capability state.
- Worker peers = eligible serving targets. Coordinator peers = not.
- Unknown capability → try pull before concluding "no peer."
- Load balancing: compare local queue depth vs peer queue depth (use BOTH GPUs).

## UI

- Peer cards always show: role, capability status, queue load, serving rules, models.
- Replace blank sections with explicit states: "Capabilities pending", "Stale", "Sync failed", "Serve disabled by role."
- Settings: 3-state role control for the local node.

## Observability

Chronicle events: `fleet_capability_announced`, `fleet_capability_pull_started/succeeded/failed`, `fleet_dispatch_skipped_unknown_capability`, `fleet_dispatch_skipped_no_matching_rule`, `fleet_dispatch_skipped_role_disallows_serving`.

`get_fleet_health` IPC so the Market tab can explain behavior.

## Structural Cleanup (Bundled)

- Split LlmConfig into durable config vs runtime bindings.
- Remove hardcoded dispatch policy YAML from local_mode.rs → bundled contribution.

## Implementation Order

1. `fleet_policy` contribution + 3-state UI
2. `CapabilitySnapshot` + explicit peer capability state
3. Peer-to-peer capability pull endpoint + reconciliation loop
4. Local last-known capability cache
5. Dispatch logic: role + capability state + load balancing
6. Market UI + fleet health surfaces
7. LlmConfig split + bundled policy contribution

## Ship Criteria

- After restart, peers appear with role + last-known state
- Within one sync cycle, capability converges without lucky announce
- Worker nodes never dispatch outward
- Coordinator nodes never appear as serving targets
- No peer card can be "online but blank"
- Fleet stays peer-to-peer and privacy-preserving
