# Fleet MPS Build Plan

**Date:** 2026-04-15
**Status:** Ready for implementation
**Depends on:** `docs/plans/fleet-mps-three-objects.md`
**Goal:** Turn the MPS architecture into a concrete build sequence with exact code touchpoints, acceptance gates, and failure containment.

---

## Objective

Implement the refined fleet control plane directly, not a stopgap:

- `compute_participation_policy` as the durable operator-intent contribution
- `ServiceDescriptor` as the semi-stable local serving identity
- `AvailabilitySnapshot` as the volatile local eligibility state
- `PeerKnowledgeState` as this node's cached/reconciled belief about peers
- a single reconciliation protocol spanning cache hydration, announce, and pull

This plan preserves fleet privacy (no capability inventory in Wire heartbeat), removes the "online but blank" failure mode, and establishes a reusable control plane for future fleet + market unification.

---

## Hard Constraints

### Wire Node laws

- **Law 3: one contribution store** — operator-facing fleet participation intent must be a config contribution, not a Rust constant, ad hoc DB row, or onboarding flag.
- **Law 4: every LLM call gets a StepContext** — any new fleet capability LLM work is forbidden; the plan must stay mechanical and transport-only.
- **Law 5 / Pillar 37** — no hardcoded intelligence-shaping thresholds hidden in prompts or model logic.

### Wire pillars

- **Pillar 31: Local is local, Wire is Wire** — heartbeat discovery remains Wire-mediated; capability details remain peer-private.
- **Pillar 42: always include frontend/UX** — backend fleet-state changes must ship with Settings + Market surfaces in the same initiative.
- **Pillar 38: fix all bugs when found** — remove the empty-means-unknown ambiguity rather than papering over it with retries.

---

## Canonical Runtime Model

### Durable contribution

- `compute_participation_policy`
- Canonical fields:
  - `mode: coordinator | hybrid | worker`
  - `allow_market_visibility: bool`
  - `allow_serving_while_degraded: bool`
  - `allow_fleet_dispatch: bool`
  - `allow_fleet_serving: bool`
- The booleans above may be derived defaults under the `mode`, but keeping them explicit leaves room for future fleet/market unification.

### Derived local objects

- `ServiceDescriptor`
  - `declared_role`
  - `servable_rules`
  - `models_loaded`
  - `visibility`
  - `protocol_version`
  - `descriptor_version`
  - `computed_at`

- `AvailabilitySnapshot`
  - `queue_depths`
  - `total_queue_depth`
  - `health_status`
  - `tunnel_status`
  - `degraded`
  - `availability_version`
  - `last_updated`

### Remote belief object

- `PeerKnowledgeState`
  - peer identity (`node_id`, `handle_path`, `name`, `tunnel_url`)
  - `declared_role`
  - `service_descriptor: Option<ServiceDescriptor>`
  - `availability_snapshot: Option<AvailabilitySnapshot>`
  - `capability_status: unknown | fresh | stale | failed`
  - `last_capability_sync_at`
  - `last_capability_error`
  - `last_capability_source: announce | pull | cache`

### Reconciliation rule

Every transport source writes through one reducer:

- heartbeat discovery
- local cache hydration
- peer announce
- peer pull response

No secondary merge logic is allowed in UI or dispatch code.

---

## Workstreams

## WS1: Contribution + Schema Surface

### Outcome

Introduce `compute_participation_policy` as a real config contribution with bundled schema definition and an initial default seed.

### Code touchpoints

- `src-tauri/assets/bundled_contributions.json`
  - add bundled `schema_definition` for `compute_participation_policy`
  - add bundled default contribution row for the operator-global default
- `src-tauri/src/pyramid/schema_registry.rs`
  - no new mechanism expected, but this schema must appear cleanly in the registry view
- `src-tauri/src/pyramid/config_contributions.rs`
  - validate accepts/supersession flow for the new schema type
- `src-tauri/src/pyramid/generative_config.rs`
  - ensure this schema type can be created/edited through the existing config system if needed
- `src-tauri/src/pyramid/wire_migration.rs`
  - bundled manifest walk seeds the new schema + default contribution

### Notes

- Do **not** create a new table for the policy. This is a contribution.
- The long-term naming should be `compute_participation_policy`, not `fleet_policy`.

### Acceptance

- Fresh DB boot seeds one active `compute_participation_policy`
- Registry can resolve its schema definition
- Superseding the policy produces exactly one new active contribution

### Tests

- bundled-manifest seed test
- config contribution accept/supersede test for the new schema type
- schema-registry visibility test

---

## WS2: Settings Control Surface

### Outcome

Ship the operator-facing 3-state control as a preset surface over `compute_participation_policy`.

### Code touchpoints

- `src/components/Settings.tsx`
  - add a new Fleet Participation section
  - 3-state control labels:
    - `Coordinator`
    - `Hybrid`
    - `Worker`
  - explanatory copy for each mode
- `src/styles/dashboard.css`
  - styles for segmented mode control / state messaging
- `src-tauri/src/main.rs`
  - new IPCs to read/save fleet participation policy
- `src-tauri/src/pyramid/config_contributions.rs`
  - use existing contribution APIs from the IPC handlers

### Notes

- The UI is a preset surface, not the source of truth.
- Saving the control should supersede the active policy contribution and trigger a live reload path.

### Acceptance

- Changing mode updates the active contribution
- Restart preserves the chosen mode
- The UI shows current mode without reading dispatch policy folklore

### Tests

- Tauri IPC roundtrip test for save/load
- frontend state test for mode hydration and save feedback

---

## WS3: Local Derived State Split

### Outcome

Replace the current fused capability derivation with separate `ServiceDescriptor` and `AvailabilitySnapshot`.

### Code touchpoints

- `src-tauri/src/fleet.rs`
  - add new structs:
    - `ServiceDescriptor`
    - `AvailabilitySnapshot`
    - `PeerKnowledgeState`
  - keep identity fields separate from capability belief
- `src-tauri/src/main.rs`
  - factor the current heartbeat-time derivation into reusable helpers:
    - compute descriptor from local mode + dispatch policy + role policy
    - compute availability from compute queue + tunnel + health
- `src-tauri/src/pyramid/local_mode.rs`
  - remove the implicit role logic from hardcoded dispatch-policy YAML generation in later workstreams
- `src-tauri/src/pyramid/dispatch_policy.rs`
  - continue to derive `servable_rules` mechanically from `is_local`, but now as input to `ServiceDescriptor`

### Notes

- Queue depth must move out of the descriptor.
- Unknown must become an explicit peer knowledge status rather than an empty vector convention.

### Acceptance

- Local node can compute descriptor without availability
- Local node can compute availability without descriptor churn
- Empty `servable_rules` on a computed descriptor means "known empty"

### Tests

- descriptor derivation tests from mock role/local-mode/policy
- availability derivation tests from mock queue/tunnel state

---

## WS4: Peer Knowledge Reducer

### Outcome

Introduce one reducer that merges heartbeat discovery, cache hydration, announce, and pull responses into `PeerKnowledgeState`.

### Code touchpoints

- `src-tauri/src/fleet.rs`
  - replace direct `FleetPeer` mutation helpers with reducer-style merge helpers:
    - `merge_discovery_from_heartbeat`
    - `merge_from_cache`
    - `merge_from_announce`
    - `merge_from_pull`
- `src-tauri/src/main.rs`
  - heartbeat path uses reducer, not direct struct mutation
- `src-tauri/src/server.rs`
  - announce handler writes through reducer

### Notes

- Reducer chooses freshness semantics once.
- No UI-side fallback merge logic allowed.

### Acceptance

- A discovered peer can exist with `capability_status=unknown`
- A cache hydrate can populate stale descriptor/availability before live sync
- Announce and pull update the same peer entry cleanly

### Tests

- reducer unit tests for source precedence and freshness updates
- stale/fresh/failed transitions

---

## WS5: Capability Pull Endpoint + Reconciliation Loop

### Outcome

Add private authenticated capability pull and wire it into a reconciliation loop.

### Code touchpoints

- `src-tauri/src/server.rs`
  - add `GET /v1/fleet/capabilities`
  - authenticate with fleet JWT
  - return local service descriptor + availability snapshot + declared role + version fields
- `src-tauri/src/fleet.rs`
  - add client helper to fetch peer capabilities
- `src-tauri/src/main.rs`
  - reconciliation triggers:
    - on peer discovery
    - on app startup after cache hydration
    - on unknown peer before skip
    - periodic stale refresh
- `src-tauri/src/main.rs`
  - add `get_fleet_health` IPC for UI/diagnostics

### Notes

- Announce remains push.
- Pull is not a fallback hack; it is half of the anti-entropy protocol.
- Use version fields so announce can later degrade to "changed, fetch me" if payload slimming is needed.

### Acceptance

- Newly discovered peer converges to fresh capability without needing a lucky announce
- Restart converges from stale cache to fresh live state within one sync cycle
- Pull failures mark peer as `failed`, not silently empty

### Tests

- endpoint auth test
- pull client success/failure tests
- reconciliation loop tests for discovery -> pull -> fresh transition

---

## WS6: Warm Cache

### Outcome

Persist last-known peer knowledge locally and hydrate it on startup.

### Code touchpoints

- `src-tauri/src/fleet.rs`
  - serializable cache shape for `PeerKnowledgeState`
- `src-tauri/src/main.rs`
  - load cache during startup before heartbeat loop settles
  - write cache after reducer updates
- data-dir cache file near existing node app state

### Notes

- This is a cache, not durable user-facing truth.
- It must store freshness/source metadata so stale presentation is explicit.

### Acceptance

- After restart, cards show prior role/capability marked stale instead of blank
- Cache corruption fails soft and falls back to empty discovery

### Tests

- cache roundtrip test
- stale hydration on startup test
- corrupt-cache fallback test

---

## WS7: Dispatch Engine Refactor

### Outcome

Dispatch decisions consume participation policy + peer knowledge + availability, not raw `serving_rules` presence.

### Code touchpoints

- `src-tauri/src/pyramid/llm.rs`
  - Phase A fleet check uses:
    - role eligibility
    - capability status
    - descriptor rule match
    - availability freshness
    - queue depth comparison
  - if peer is unknown, trigger pull before declaring no match
  - convert current "fleet first, use exclusively" to real queue-aware balancing
- `src-tauri/src/fleet.rs`
  - selection helper becomes something like `find_best_peer_for_rule`
- `src-tauri/src/server.rs`
  - receiving side should reject fleet jobs when local role/policy disallows serving

### Notes

- `Coordinator` must never serve.
- `Worker` must never dispatch outward.
- `Hybrid` can do both.

### Acceptance

- Worker nodes do not outward-dispatch even if a stale old dispatch policy would have
- Coordinator nodes are never selected as serving peers
- Unknown peer state causes pull-before-skip
- Local and remote GPUs are both usable under queue-aware balancing

### Tests

- role-gating tests
- unknown->pull->dispatch test
- queue-aware peer selection test

---

## WS8: Market + Health UI

### Outcome

Market surfaces the same truth the dispatcher uses.

### Code touchpoints

- `src/components/MarketDashboard.tsx`
  - replace raw roster type with peer knowledge + health shape
  - always show:
    - role
    - capability status
    - queue load
    - serving rules/models if known
  - empty sections become explicit states:
    - `Capabilities pending`
    - `Capabilities stale`
    - `Capability sync failed`
    - `Serve disabled by role`
- `src/styles/dashboard.css`
  - status pills, role chips, degraded/stale visual language
- `src-tauri/src/main.rs`
  - `get_fleet_roster` may remain as raw state
  - add `get_fleet_health` for operator-facing explanations

### Notes

- Peer cards should stop implying brokenness through absence.
- The UI must not infer role from missing arrays.

### Acceptance

- No peer card can appear online-but-blank
- Operator can explain each skipped dispatch from the Market surface

### Tests

- frontend rendering tests for unknown/stale/failed/disabled states

---

## WS9: Chronicle + Diagnostics

### Outcome

Capability sync and dispatch skip reasons become first-class observability.

### Code touchpoints

- `src-tauri/src/pyramid/compute_chronicle.rs`
  - add event helpers / accepted event names
- `src-tauri/src/pyramid/db.rs`
  - ensure fleet analytics queries tolerate new event types
- `src-tauri/src/pyramid/llm.rs`
  - log/record:
    - `fleet_capability_pull_started`
    - `fleet_capability_pull_succeeded`
    - `fleet_capability_pull_failed`
    - `fleet_dispatch_skipped_unknown_capability`
    - `fleet_dispatch_skipped_no_matching_rule`
    - `fleet_dispatch_skipped_role_disallows_serving`
- `src-tauri/src/main.rs` / `server.rs`
  - record announce / reconciliation outcomes

### Acceptance

- Every dispatch skip path is explainable in logs/chronicle
- Operators can distinguish "no capable peer" from "unknown peer state" from "role disallows serving"

---

## WS10: Structural Cleanup

### Outcome

Remove the recurring runtime/durable config coupling and stop hardcoding fleet intent in `local_mode.rs`.

### Code touchpoints

- `src-tauri/src/pyramid/mod.rs`
  - split `LlmConfig` durable fields from runtime bindings
- `src-tauri/src/pyramid/llm.rs`
  - consume runtime overlays from a dedicated runtime struct
- `src-tauri/src/main.rs`
  - stop relying on `with_runtime_overlays_from` as the primary integrity guard
- `src-tauri/src/pyramid/local_mode.rs`
  - stop emitting hardcoded fleet-inclusive dispatch policy YAML
  - move defaults to bundled contributions + generated policy flow

### Notes

- This workstream is bundled because the fleet system already exposed this as a recurring bug class.
- If sequence pressure exists, land it after WS1-WS9 but before declaring the initiative complete.

### Acceptance

- Config reloads cannot silently drop fleet runtime state
- Local mode no longer encodes operator fleet intent via hardcoded YAML

---

## File Map

### Backend core

- `src-tauri/src/fleet.rs`
- `src-tauri/src/server.rs`
- `src-tauri/src/main.rs`
- `src-tauri/src/pyramid/llm.rs`
- `src-tauri/src/pyramid/mod.rs`
- `src-tauri/src/pyramid/dispatch_policy.rs`
- `src-tauri/src/pyramid/local_mode.rs`

### Config / schema plumbing

- `src-tauri/assets/bundled_contributions.json`
- `src-tauri/src/pyramid/config_contributions.rs`
- `src-tauri/src/pyramid/schema_registry.rs`
- `src-tauri/src/pyramid/generative_config.rs`
- `src-tauri/src/pyramid/wire_migration.rs`

### Frontend

- `src/components/Settings.tsx`
- `src/components/MarketDashboard.tsx`
- `src/styles/dashboard.css`

### Observability

- `src-tauri/src/pyramid/compute_chronicle.rs`
- `src-tauri/src/pyramid/db.rs`

---

## Recommended Landing Sequence

1. WS1 + WS2
2. WS3 + WS4
3. WS5
4. WS6
5. WS7
6. WS8 + WS9
7. WS10

This order keeps the system coherent at each checkpoint:

- first establish the durable intent surface
- then the data model
- then the transport
- then restart resilience
- then the dispatcher
- then operator-facing explanation
- then the structural cleanup

---

## Ship Gates

### Gate A: Policy truth

- `compute_participation_policy` exists and drives the local role UI
- mode changes survive restart and reload

### Gate B: Capability truth

- no internal path uses empty arrays as a proxy for unknown
- peers can exist in `unknown` state cleanly

### Gate C: Reconciliation truth

- discovery + pull converge without announce luck
- warm cache prevents blank restart cards

### Gate D: Dispatch truth

- dispatch respects role policy
- dispatch tries pull before skip on unknown peer state
- balancing considers both local and remote queue depth

### Gate E: UX truth

- operator can tell why a peer is not currently serving
- no peer card appears online-but-blank

### Gate F: Structural truth

- fleet runtime handles are no longer at risk during config rebuilds
- `local_mode.rs` no longer hardcodes fleet operator intent

---

## Risks To Watch

- **Schema creep:** do not let `compute_participation_policy` become a second dispatch-policy dialect. It should express intent, not transport minutiae.
- **Merge ambiguity:** all peer updates must flow through one reducer.
- **UI drift:** Market must render the same state the dispatcher reads.
- **Partial landing trap:** do not ship pull without unknown/stale/failed semantics, or the blank-card ambiguity remains.
- **Role/policy confusion:** `Coordinator/Hybrid/Worker` is a preset surface. Preserve the deeper policy naming in backend code.

---

## Definition Of Done

The initiative is done when:

- role intent is contribution-backed
- local service identity and availability are split
- peer knowledge converges via cache + announce + pull
- dispatch consumes explicit policy and belief state
- Market/Settings show explicit role and sync state
- restart never produces an online-but-blank fleet peer card
- the architecture is ready to unify fleet and compute market participation later without renaming the core concepts
