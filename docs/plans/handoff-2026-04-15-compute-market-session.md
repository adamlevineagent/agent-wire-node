# Session Handoff: Compute Market + Fleet Routing + Chronicle

**Date:** 2026-04-14 to 2026-04-15 (continuous session)
**Scope:** Built the compute infrastructure from zero to fleet-dispatched pyramid builds between two physical machines.

---

## What Shipped

### Compute Queue (Phase 1)
- Transparent per-model FIFO queues replacing the global semaphore
- GPU processing loop with panic guard
- Market tab with real-time QueueLiveView
- Wire migrations: rotator arm, 4 market tables, 8 settlement RPCs, system entities, economic parameter seeds

### Fleet Routing
- Fleet discovery via heartbeat roster (Wire as broker)
- Fleet peer announce via tunnels (peer-to-peer capabilities)
- Fleet JWT authentication (Ed25519, self-healing via heartbeat)
- Fleet dispatch by routing rule name (not model name — model names never cross node boundaries)
- Fleet-as-dispatch-provider: fleet is a `provider_id` in the dispatch policy, not a pre-check
- Routing order: cache → route resolution → fleet dispatch → local queue

### Node Identity (Phase 1a)
- Each machine gets its own handle path (`@playful/behem`, `@playful/mac-lan`)
- Operator owns nodes directly (`wire_nodes.operator_id`)
- `node_identity.json` with handle + reconnection token (bcrypt)
- Registration by `(operator_id, node_handle)` — multiple machines per account
- Stale node cleanup, collision avoidance on handles

### Compute Chronicle
- Persistent event log (`pyramid_compute_events`) — 17 columns, 6 indexes, 5 views
- 9 write points: enqueued, started, completed, failed, fleet_dispatched, fleet_returned, fleet_dispatch_failed, fleet_received, cloud_returned
- Rich task context: chain_name, content_type, depth, task_label threaded from chain executor
- Semantic job_paths (no UUIDs)
- DADBEAR correlation (work_item_id, attempt_id columns)
- 4 IPC commands with 11 filter dimensions
- Frontend Chronicle tab with stats, filters, event table, fleet analytics

### DADBEAR Canonical Architecture (built by parallel session)
- Compiler, supervisor, prompt materializer, observation events, hold events
- Phases 1-7 shipped by the other session during this one

---

## What We Learned (Impacts Future Plans)

### 1. LlmConfig is a "rebuildable value object with embedded runtime state" — recurring bug class
Multiple config rebuild paths (profile apply, ConfigSynced) can silently drop runtime handles (fleet_roster, compute_queue, provider_pools). Fixed with `with_runtime_overlays_from` canonical helper. **The 100-year fix is splitting LlmConfig into durable config vs runtime bindings.** Every new runtime field risks the same bug until this split happens.

### 2. Fleet routing must happen BEFORE local queueing
The compute queue interceptor clones the config and strips fleet_roster. If fleet Phase A runs after queueing, fleet dispatch never fires. Fixed by reordering: cache → route resolution → fleet dispatch → local queue. **Any future compute path must preserve this ordering.**

### 3. Peer-to-peer announce works but needs the JWT public key persisted
The JWT public key was received at registration but never saved to session.json. Every app restart lost it → fleet announces got 403. Fixed by persisting to AuthState + self-healing via heartbeat delivery. **Any new JWT/auth credential must be persisted, not just held in memory.**

### 4. Fleet dispatch policy is per-node, not global
GPU servers should serve fleet but not dispatch outward. Laptops/orchestrators dispatch to fleet. Currently the hardcoded dispatch policy in `local_mode.rs` gives every node `provider_id: fleet`. **This must become a per-node operator choice** — a `fleet_dispatch_enabled` toggle, not a hardcoded YAML string.

### 5. The heartbeat is the reliable channel, announce is optimization
The heartbeat (Wire-mediated, every 60s) is guaranteed. The announce (peer-to-peer, tunnel-dependent) can fail silently. `serving_rules` must be derivable from heartbeat data alone. Currently the announce is required for `serving_rules` — the heartbeat only carries identity/tunnel. **Consider adding serving_rules to the heartbeat response for robustness.**

### 6. Cloudflare tunnels have ~120s origin timeout
Long LLM calls (>120s) get 524 from Cloudflare. **The 100-year fix is ACK + async result delivery** (accept job immediately, POST result back when done). This matches the market's webhook delivery architecture. TODO is in server.rs.

### 7. No UUIDs where LLMs or operators see them
Semantic paths everywhere: node handles (`@playful/behem`), DADBEAR work item IDs, chronicle job_paths. The fleet roster, chronicle events, dispatch logs — all use human-readable paths. **This is a hard rule going forward.**

### 8. The dispatch policy hardcoded in local_mode.rs violates Law 3 and Pillar 37
It should be a proper contribution YAML managed by the generative config system. TODO is in local_mode.rs.

---

## TODOs in Code (grep for these)

| Location | TODO |
|---|---|
| `llm.rs` Phase A | Load balancing: use BOTH fleet + local GPU simultaneously based on queue depth comparison |
| `server.rs` fleet handler | ACK + async result delivery to eliminate Cloudflare 524 timeouts |
| `local_mode.rs` | Dispatch policy should be proper YAML contribution, fleet dispatch should be per-node operator choice |

---

## What's Next: Market Phases 2-9

The Wire-side infrastructure is ready (rotator arm, tables, RPCs from Phase 1 migrations). The node-side has the compute queue, fleet routing, and chronicle. The next phases build on this:

- **Phase 2:** Exchange matching + settlement on node side + queue mirror push
- **Phase 3:** Bridge operations (OpenRouter → market)
- **Phase 4:** Quality/review system
- **Phase 5:** Fleet → market integration (fleet peers can also be market providers)
- **Phases 6-9:** Daemon intelligence, sentinel, steward

**Key integration point:** The Compute Chronicle is the observability layer for all future phases. Each phase's events should write to `pyramid_compute_events` as they ship. The DADBEAR work item system is the durable dispatch layer — market jobs should flow through it.

---

## Architecture Diagram (Current State)

```
Laptop (@playful/mac-lan)                    5090 (@playful/behem)
┌─────────────────────────┐                  ┌─────────────────────────┐
│ Pyramid Build            │                  │ DADBEAR / Stale Engine  │
│   ↓                      │                  │   ↓                     │
│ Dispatch Policy          │                  │ Dispatch Policy         │
│   [fleet, ollama-local]  │                  │   [ollama-local]        │
│   ↓                      │                  │   ↓                     │
│ Fleet Phase A            │   ──tunnel──→    │ /v1/compute/fleet-dispatch
│   find_peer(@behem)      │                  │   ↓                     │
│   dispatch by rule_name  │                  │ Compute Queue (FIFO)    │
│   ↓                      │   ←──tunnel──    │   ↓                     │
│ Chronicle records        │                  │ GPU Loop (5090)         │
│   fleet_dispatched       │                  │   ↓                     │
│   fleet_returned         │                  │ Result → HTTP response  │
│                          │                  │                         │
│ Compute Queue (FIFO)     │                  │ Chronicle records       │
│   ↓ (fallback)           │                  │   fleet_received        │
│ GPU Loop (M-series)      │                  │   started/completed     │
└─────────────────────────┘                  └─────────────────────────┘
```

---

## Commits This Session

### agent-wire-node
| Commit | What |
|---|---|
| `567eec6` | Phase 1 compute market + initial fleet routing |
| `3ead771` | Fleet routing by rule name (fleet-as-dispatch-provider) |
| `783e2f4` | Node identity handle paths (Phase 1a) |
| `a766ad0` | HeartbeatFleetEntry serde fix |
| `48edec8` | DADBEAR Phases 1-7 + handle_path display |
| `b63d183` | Compute Chronicle (all 10 steps) |
| `c38eb67` | JWT persistence fix (fleet 403 root cause) |
| `022f6a9` | Heartbeat self-healing JWT delivery |
| `088c9b0` | Fleet routing before local queueing (THE dispatch fix) |
| `b0da911` | Canonical with_runtime_overlays_from |
| `725a454` | Typed fleet errors, stop evicting peers on timeouts |
| `f34a017` | Load balancing TODO |
| `6410689` | Dispatch policy TODO (proper YAML, per-node fleet toggle) |

### GoodNewsEveryone
| Commit | What |
|---|---|
| `a550b3ab` | Wire market infrastructure + fleet JWT |
| `9bc42464` | Entity creation fix (slug, name, handle_length) |
| `438e45e8` | Node identity (migration + registration + heartbeat) |
| `32949cc8` | Backfill sanitization fix |
| `22e4f19e` | Heartbeat returns jwt_public_key |

---

## Bugs Found and Fixed (14 total)

1. Concurrent match race in match_compute_job (Wire migration)
2. Missing platform operator guards in settlement/match RPCs
3. GPU loop panic kills all future LLM calls (catch_unwind)
4. operator_id not propagated on re-registration (3 code paths)
5. Fleet announce only on first discovery (now every heartbeat)
6. Queue depths empty in fleet announcements
7. HeartbeatFleetEntry.name deserialization failure (serde default)
8. Backfill sanitization order (LOWER before REGEXP_REPLACE)
9. IP rate limiter useless behind proxy (127.0.0.1)
10. Token rotation killing other machines' API tokens
11. JWT public key not persisted in session.json
12. Fleet routing after queueing (queue stripped fleet_roster)
13. HTTP profile-apply dropped fleet_roster (with_runtime_overlays_from fix)
14. BEHEM dispatching stale work to laptop (per-node fleet policy)
