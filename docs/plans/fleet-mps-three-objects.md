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

## Deeper Canonical Shape (100-Year Refinement)

The three-object framing is a strong corrective to the current fragility, but it is not quite the final shape.
The deeper control plane is five first-class concepts:

### 1. Compute Participation Policy — durable operator intent
- Canonical contribution schema should become `compute_participation_policy`, not a fleet-specific toggle.
- It governs:
  - where this node may send work
  - where this node may accept work from
  - whether it is private fleet-only or market-visible
  - whether serving is allowed during degraded or maintenance states
- The 3-state UI (`Coordinator`, `Hybrid`, `Worker`) is a projection/preset layer over this policy, not the deepest truth.
- This prevents a later rewrite when fleet and market participation unify.

### 2. Local Service Descriptor — semi-stable derived identity
- Split out the durable-ish facts from the volatile ones.
- Fields:
  - `declared_role`
  - `servable_rules`
  - `models_loaded`
  - `visibility` (private fleet / market-visible / disabled)
  - `protocol_version`
  - `descriptor_version`
  - `computed_at`
- This answers: "what can this node serve in principle?"

### 3. Local Availability Snapshot — volatile runtime state
- Separate fast-changing eligibility from service identity.
- Fields:
  - `total_queue_depth`
  - `queue_depths`
  - `health_status`
  - `tunnel_status`
  - `degraded`
  - `last_updated`
  - `availability_version`
- This answers: "should this node get work right now?"

### 4. Peer Knowledge State — local belief about another node
- Cached, reconciled, freshness-aware state for remote peers.
- Holds:
  - last known service descriptor
  - last known availability snapshot
  - freshness metadata
  - source provenance (`announce`, `pull`, `cache`)
  - sync failures / last error
- Unknown, stale, failed, and fresh are explicit beliefs, not inferred from missing fields.

### 5. Fleet Reconciliation Protocol — one anti-entropy system
- Heartbeat remains discovery-only.
- Announce becomes "I changed; here is my newest version."
- Pull becomes "give me the latest descriptor/snapshot for this peer."
- Cache provides warm-start bootstrap.
- All sources feed one reducer/merge path.
- The system guarantee is eventual convergence of peer knowledge without centralizing private capability data in the Wire.

## Canonical Objects (Refined)

### Compute Participation Policy
- Durable contribution.
- Generates dispatch policy.
- Fleet UI is a preset surface over it.

### Service Descriptor
- Derived from participation policy + local mode + loaded model state.
- Semi-stable and versioned.
- Private to peers, not Wire-mediated.

### Availability Snapshot
- Derived from queue, health, tunnel, and maintenance state.
- Volatile and versioned separately from the descriptor.

### Peer Knowledge State
- Local cache of descriptor + availability + freshness + provenance.
- Hydrated from disk on startup, reconciled live afterward.

### Reconciliation Protocol
- Push (`announce`) + pull (`/v1/fleet/capabilities`) + cache hydration.
- One reducer.
- One freshness model.
- One place to reason about "what do we believe about this peer right now?"

## Warm-Start Cache

- Persist last-known peer capabilities locally.
- On restart, peer cards show last-known state marked "stale" until refreshed.
- No more blank cards after restart.

## Dispatch Selection

- Phase A uses participation policy + service descriptor + availability snapshot + peer knowledge.
- Worker peers = eligible serving targets. Coordinator peers = not.
- Unknown peer knowledge → try pull before concluding "no peer."
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
- Replace `fleet_policy` naming in the long-term architecture with `compute_participation_policy`.

## Implementation Order

1. `compute_participation_policy` contribution + 3-state UI preset surface
2. Split capability into `ServiceDescriptor` + `AvailabilitySnapshot`
3. Explicit peer knowledge state + single reducer
4. Peer-to-peer capability pull endpoint + reconciliation loop
5. Local last-known peer knowledge cache
6. Dispatch logic: policy + descriptor + availability + load balancing
7. Market UI + fleet health surfaces
8. LlmConfig split + bundled policy contribution

## Ship Criteria

- After restart, peers appear with role + last-known state
- Within one sync cycle, capability converges without lucky announce
- Worker nodes never dispatch outward
- Coordinator nodes never appear as serving targets
- No peer card can be "online but blank"
- Fleet stays peer-to-peer and privacy-preserving

## Final Architectural Verdict

The maximal shape is not just "three objects." It is a unified private compute control plane:
- **Compute Participation Policy** for durable intent
- **Service Descriptor** for what a node can serve
- **Availability Snapshot** for whether it should serve now
- **Peer Knowledge State** for what this node currently believes about peers
- **Fleet Reconciliation Protocol** for how that belief converges

The three-object plan remains useful as the bridge from current fragility to this deeper shape, but implementation should target the refined model directly where possible.
