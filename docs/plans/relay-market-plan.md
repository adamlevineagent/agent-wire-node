# Relay Market Plan

**Original draft:** 2026-04-13
**Last revision:** 2026-04-16 (post-audit unification pass)
**Scope:** The relay network — all Wire data transport flows node-to-node through requester-chosen relay chains. The Wire is pure control plane. Structural privacy through topology ambiguity, distributional opacity, and tunnel rotation.
**Prerequisites:** Compute market Phase 2 ships first (exchange infrastructure + `fill_compute_job` relay_count param). Storage market Phase S1 ships first (settlement pattern + pull routing). DD-A slug `market:relay` applied throughout.
**Companion docs:** `compute-market-architecture.md` §III (canonical privacy model), `compute-market-architecture.md` §VIII.6 (DD-A through DD-O decisions), `storage-market-conversion-plan.md`, `async-fleet-dispatch.md` (transport scaffolding), `fleet-mps-build-plan.md` (participation policy canonical with `allow_relay_serving` + `allow_relay_usage`), `GoodNewsEveryone/docs/architecture/wire-compute-privacy-tiers.md` (SOTA canonical).

---

## 2026-04-16 Unification Pass — What's Canonical Where

The 2026-04-13 draft of this plan was already the closest-to-SOTA of the three market plans — variable relay count + distributional opacity + tunnel rotation was canonical from the original. The 2026-04-16 audit landed a handful of alignment fixes:

- **Nomenclature clarified (DD-A, DD-B):** "Wire-proxied at launch" framing is replaced by "Wire-as-bootstrap-relay" — the protocol shape is identical in both modes; only the `callback_url` / `next_hop` values change as non-Wire relay nodes come online. Phase R1 is about other nodes deploying the relay endpoint code so the Wire can exit bootstrap. Slug canonical: `market:relay`. `CallbackKind::Relay` per DD-B (shipped code variant; no rename).
- **Scaffolding reuse (DD-D, DD-E):** Relay identity verifier = `verify_relay_identity` at `pyramid/relay_identity.rs` (parallel to `fleet_identity.rs`, `aud: "relay"`). Operational policy = `relay_delivery_policy` contribution (forward timeout, max concurrent relays, drain grace — parallel to `fleet_delivery_policy` / `market_delivery_policy`; rotation-cadence fields stay on `privacy_policy`, see below). Relay is STREAMING, not outbox-batched — no outbox for relay itself; body §VI.2 shows the pipe-bytes pattern.
- **Participation policy (DD-I):** Full 10-field canonical list lives in `fleet-mps-build-plan.md`. Storage and relay do NOT introduce parallel contributions. `allow_relay_serving` gates offer publication + forward acceptance. `allow_relay_usage` gates the requester's `relay_count` request being honored (worker-mode with `allow_relay_usage: false` goes direct 0-relay).
- **Relay offer derivation:** Relay offers on the Wire derive directly from the `relay_pricing` contribution (pricing/strategy) + runtime capacity state on `RelayMarketState` (`max_concurrent_relays`, `current_active_relays`, observed quality metrics). The fleet-MPS `ServiceDescriptor` / `AvailabilitySnapshot` pattern is compute-specific; relay does NOT introduce a parallel descriptor type. Offers are updated on each heartbeat (observed quality) and on each pricing-contribution supersession.
- **DADBEAR integration:** Per-hop observation events (`relay_hop_started`, `relay_hop_completed`, `relay_hop_failed`). Aggregated-window DADBEAR work items for decisions (`relay_pricing_adjust`, `relay_capacity_adjust`). Breaker holds on `market:relay` slug stop relay serving without affecting compute/storage on the same node.
- **TunnelUrl (DD-D):** Every URL field in onion token layers, offer rows, next_hop references, rotation state uses `TunnelUrl::parse` at ingress. Rotation state struct uses `TunnelUrl`, not `String` (§VI.3 corrected in-body; the 2-line change below).
- **Rotation param ownership (OB-5 resolution):** Fields split deliberately — `privacy_policy` owns `tunnel_rotation_interval_s` + `tunnel_drain_grace_s` (requester-affecting cadence fields, per compute-market-architecture §III). `relay_delivery_policy` owns `forward_timeout_secs`, `max_concurrent_relays`, operational backoff (operator-affecting operational timing). No overlap.
- **Pillar 37:** Every timing constant in relay code reads from `relay_delivery_policy` or `privacy_policy`. No hardcoded seconds/counts.
- **Payload-agnostic chain:** Same onion-wrapped token carries compute prompts AND storage document bodies (R2). `target_path` in the innermost layer is per-market (`/v1/compute/job-dispatch` for compute; `/v1/storage/pull` for storage). §II onion layer spec updated to reflect this.

All body sections below are canonical for their topic; no overlay-vs-body split.

---

## I. Architecture

### The Wire as Pure Control Plane

At maturity, the Wire handles:
- **Matching** (compute exchange, storage routing, relay selection)
- **Settlement** (credit escrow, rotator arm, atomic RPCs)
- **Routing decisions** (which relays, what chain, key exchange)
- **Performance observation** (aggregation of completed job metrics)
- **Market data** (heartbeat responses, demand signals, fleet rosters)

The Wire does NOT handle:
- **Data transport** — ALL payloads flow node-to-node through the relay network
- **Payload inspection** — the Wire never sees prompt content, document bodies, or inference results
- **Bandwidth** — the data plane IS the network of nodes, not the Wire server

The Wire's bandwidth requirements are API calls only: JSON, kilobytes, matching and settlement metadata. The Wire scales by adding compute to its server, not bandwidth. The relay network scales by adding nodes.

### Bootstrap Mode

Before sufficient relay capacity exists, the Wire acts as a relay itself — forwarding payloads between nodes. This is a temporary convenience. As relay nodes join and capacity grows, the Wire's relay role naturally diminishes. The transition is gradual: the Wire's relay workload decreases as the network's relay capacity increases. Eventually the Wire stops relaying entirely.

The Wire's bootstrap relay mode uses the same `/v1/relay/forward` endpoint and the same settlement as any other relay. It's not special infrastructure — the Wire is just another relay node during bootstrap.

### Variable Relay Count

The requester chooses how many relay hops to use for each job: **0, 1, 2, 5, 12 — any number**. This is a contribution on the requester's dispatch policy (`schema_type: privacy_policy`):

```yaml
# Privacy policy contribution
default_relay_count: 2              # default for normal work
sensitive_relay_count: 5            # for sensitive builds (steward can escalate)
relay_count_range: [0, 20]          # min/max bounds
```

**Why variable count creates plausible deniability for everyone, including zero-relay users:**

A provider receives a request from a tunnel URL. That tunnel URL could be:
- The requester directly (0 relays)
- The last of 1 relay
- The last of 2 relays
- The last of 12 relays

**The provider cannot tell.** The request format is identical regardless of chain length. There is no hop count header, no chain metadata, no observable difference. Every tunnel URL on the network is simultaneously a potential requester, relay, or provider — same software, same endpoints, same auth.

A user who chooses 0 relays (cheapest, fastest) gets **plausible deniability**: the provider sees their tunnel URL but can't prove it's a direct connection versus the last hop of an N-relay chain. The plausible deniability strengthens as more traffic flows through the network — the more connections exist, the more ambiguous each one becomes.

### Distributional Opacity

**The network NEVER publishes aggregate relay statistics.** No dashboard shows "40% of traffic uses 2 relays." No heartbeat carries relay distribution data. No market surface reveals usage patterns.

Why: if the distribution is unknown, every connection is maximally ambiguous. An attacker can't say "there's a 70% chance this is direct" because they don't know the probability. Information absence IS the security model.

- The Wire knows aggregate numbers internally for capacity planning but never publishes them
- Individual nodes know only their own relay usage
- Relay nodes know only their own hop traffic
- Even the Wire can't observe the full chain (it sets up routing instructions but doesn't monitor actual data flow — the bytes go node-to-node, not through the Wire)

### Tunnel URL Rotation

Nodes periodically rotate their tunnel URL, breaking all temporal correlation:

- Node requests a new tunnel URL from Cloudflare (or tunnel provider)
- Pushes new URL to Wire via heartbeat (Wire needs it for routing)
- Old URL decommissions after in-flight connections drain
- Any correlation built against the old URL is instantly worthless

**Rotation settings** are part of the `privacy_policy` contribution (see `market-seed-contributions.md` seed #8). No separate `tunnel_rotation_policy` type — rotation fields (`tunnel_rotation_interval_s`, `tunnel_drain_grace_s`) live alongside relay count and fan-out settings in one contribution.

**What rotation breaks:**
- A provider who sees tunnel URL X twice can't correlate them (X changed between requests)
- A relay building a pattern "I forwarded to URL Y 47 times" loses the pattern when Y rotates
- Even if someone maps URL → node identity, the mapping expires on rotation
- Combined with variable relay count: the provider sees a DIFFERENT tunnel URL on each request (because the relay chain uses different relays, each with rotating URLs)

### The Three Orthogonal Privacy Mechanisms

| Mechanism | What it breaks | How |
|---|---|---|
| Variable relay count | Topology inference | Can't tell if connection is direct or relayed |
| Distributional opacity | Probabilistic inference | Can't estimate likelihood of any topology |
| Tunnel rotation | Temporal correlation | Can't link connections over time |

Each is independent. Each strengthens the others. Together they make traffic analysis practically impossible at scale without simultaneously compromising multiple nodes within a rotation window.

---

## II. How Relay Works

### Relay Chain Setup

1. Requester's dispatch policy specifies `relay_count: N`
2. Requester calls Wire match endpoint (compute or storage)
3. Wire selects N relay nodes (each from a different operator, different from requester and provider operators)
4. Wire generates a **relay chain token** — onion-wrapped per-hop instructions:

**Token format:** Each relay's instructions are encrypted with that relay's public key (registered at node setup, rotated with tunnel URL). The Wire wraps them in layers — outermost layer for Relay 1, innermost for Relay N:

```
Layer 1 (encrypted with Relay A's public key):
  {
    job_id: "uuid",
    next_hop: "https://relay-b-tunnel.example.com",
    next_layer: <encrypted blob for Relay B>,
    target_path: "/v1/relay/forward",
    wire_signature: "..."
  }

Layer 2 (encrypted with Relay B's public key, inside next_layer above):
  {
    job_id: "uuid",
    next_hop: "https://provider-tunnel.example.com",
    next_layer: null,  // last relay — but relay can't know this (it just sees no next_layer)
    target_path: "/v1/compute/job-dispatch",  // or /documents/:id for storage
    wire_signature: "..."
  }

NOTE: No hop_index in the token. The relay CANNOT determine its position in the chain.
It sees: job_id, next_hop, next_layer, target_path. The Wire ALWAYS includes a next_layer
for every relay — even the last one gets a dummy encrypted blob that the final destination
silently discards. This way, every relay sees the same structure (next_hop + next_layer)
regardless of position. No relay can distinguish "I'm the last" from "I'm in the middle."
The Wire tracks hop order internally for settlement but never reveals it to relays.
```

Each relay:
- Decrypts its layer with its private key → gets `next_hop` + `next_layer`
- Forwards the payload + `next_layer` as the new token to `next_hop`
- Cannot read inner layers (encrypted with other relays' keys)
- Cannot determine its position (it always sees exactly one layer)

**Key management:** Each node registers a relay public key with the Wire at setup time (separate from the tunnel URL, rotated independently). The Wire uses these keys to build the onion token. No relay-to-relay key exchange needed — the Wire mediates.

5. Wire gives the requester: Relay 1's tunnel URL + the chain token (outermost layer) + provider's ephemeral public key (for E2E encryption on compute payloads)
6. Requester sends encrypted payload + chain token to Relay 1

### Relay Forwarding

Each relay:
1. Receives `POST /v1/relay/forward` with body stream + `x-relay-token` header
2. Peels one layer off the relay chain token → gets `next_hop` URL
3. Streams the body to `next_hop` (no buffering — constant memory regardless of payload size)
4. Streams the response back to the caller
5. Reports completion to Wire for settlement (async, fire-and-forget)

The relay doesn't know:
- How many total hops there are
- Whether it's hop 1, hop 3, or hop N
- Whether the next hop is another relay or the final provider
- Whether the previous hop is the requester or another relay
- What the payload contains (encrypted for compute, opaque blob either way)

### End-to-End Encryption (Compute Payloads)

For compute jobs, the prompt is sensitive:
1. Wire generates an ephemeral keypair for the provider (per-job)
2. Wire delivers the public key to the requester (without revealing provider identity)
3. Requester encrypts the prompt with the provider's public key
4. Encrypted payload flows through all relays as ciphertext
5. Provider decrypts with the ephemeral private key, runs inference
6. Provider encrypts result with a reverse key
7. Encrypted result flows back through the relay chain

For storage pulls: no encryption needed. Document bodies are public content. Relays forward plaintext. Privacy is about hiding WHO pulled, not WHAT was pulled.

### Direct Connection (0 Relays)

When `relay_count: 0`:
1. Wire gives the requester the provider's tunnel URL directly
2. Requester sends payload straight to provider
3. No relay chain, no relay fees
4. Provider sees requester's tunnel URL — but can't prove it's a direct connection (plausible deniability from variable relay count)
5. Cheapest and fastest, but least private (requester's tunnel URL is exposed, even if correlation is uncertain)

### Relay Selection Criteria

The Wire picks N relay nodes with these constraints:
1. Each relay from a DIFFERENT operator (no single-operator chain)
2. All relay operators different from requester AND provider operators
3. Active status, fresh heartbeat (per `staleness_thresholds.heartbeat_staleness_s` economic_parameter — not a hardcoded minute count)
4. Sufficient bandwidth capacity (concurrent_relays < max)
5. Ordered by: reliability first, then cheapest (privacy integrity > cost)

---

## III. Relay Market Economics

### Relay Offers

Each relay node publishes a relay offer (contribution, `schema_type: relay_pricing`):

```yaml
# Relay pricing contribution
pricing_mode: competitive
competitive_target: match_best
competitive_offset_bps: 0
floor_per_hop: 1                    # minimum 1 credit per relay hop
ceiling_per_hop: 5
```

The daemon resolves the effective per-hop rate from market data (same competitive pricing primitive as compute and storage). Pushes the resolved rate to the Wire.

### Settlement

Each relay hop settles independently:
- Requester pays: sum of all relay fees (on top of compute/storage cost)
- Each relay settles via rotator arm (76/2/2): 95% to relay, 2.5% Wire, 2.5% Graph Fund
- Settlement triggered when the relay reports successful forward

```sql
CREATE OR REPLACE FUNCTION settle_relay_hop(
  p_job_id UUID,
  p_relay_node_id UUID,
  p_hop_index INTEGER,
  p_matched_rate INTEGER,
  p_bytes_forwarded BIGINT
) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_relay_operator_id UUID;
  v_rotator_pos INTEGER;
  v_recipient TEXT;
  v_wire_platform_operator_id UUID;
BEGIN
  -- Resolve relay operator
  SELECT a.operator_id INTO v_relay_operator_id
    FROM wire_nodes n JOIN wire_agents a ON a.id = n.agent_id
    WHERE n.id = p_relay_node_id;

  -- Relay fees were pre-paid by the requester at fill time and held by the Wire platform
  -- operator as escrow. Now that the relay has done the work, debit Wire platform and
  -- credit the relay (or Wire take / Graph Fund via rotator arm).

  SELECT o.id INTO v_wire_platform_operator_id FROM wire_operators o
    JOIN wire_agents a ON a.operator_id = o.id
    JOIN wire_handles h ON h.agent_id = a.id
    WHERE h.handle = 'agentwireplatform' AND h.released_at IS NULL LIMIT 1;  -- DD-K

  -- Debit the escrow (Wire platform holds the pre-paid relay fees)
  PERFORM debit_operator_atomic(v_wire_platform_operator_id, p_matched_rate,
    'relay_escrow_release', p_job_id, 'relay_market');

  -- Rotator arm: 76/2/2 (shared infrastructure with compute + storage)
  v_rotator_pos := advance_market_rotator(p_relay_node_id, 'relay', 'default', 'hop');
  v_recipient := market_rotator_recipient(v_rotator_pos);

  IF v_recipient = 'provider' THEN
    PERFORM credit_operator_atomic(v_relay_operator_id, p_matched_rate,
      'relay_hop', p_job_id, 'relay_market');
  ELSIF v_recipient = 'wire' THEN
    -- Wire take: credits stay with Wire platform (already there from escrow)
    -- Just log the ledger entry — no credit movement needed
    PERFORM credit_operator_atomic(v_wire_platform_operator_id, p_matched_rate,
      'relay_wire_take', p_job_id, 'relay_market');
  ELSE
    INSERT INTO wire_graph_fund (amount, source_type, reference_id)
      VALUES (p_matched_rate, 'relay_hop', p_job_id);
  END IF;

  -- Record observation (hop_index comes from Wire's routing setup, not the relay's self-knowledge)
  INSERT INTO wire_relay_observations (job_id, node_id, hop_index, bytes_forwarded, success)
    VALUES (p_job_id, p_relay_node_id, p_hop_index, p_bytes_forwarded, true);
END;
$$;

GRANT EXECUTE ON FUNCTION settle_relay_hop TO service_role;
```

### Cost to Requester

For a job with N relay hops:
```
total_relay_cost = sum(relay_1_rate + relay_2_rate + ... + relay_N_rate)
total_job_cost = compute_or_storage_cost + total_relay_cost
```

Shown in the build preview (Pillar 23):
```
Compute cost: 4 credits (llama-70b, 1500 input tokens)
Relay cost:   2 credits (2 hops × 1 credit/hop)
Total:        6 credits
```

For 0 relays: relay cost is 0. Cheapest option.

### Relay Quality Signals (Network-Observed)

| Metric | How measured | What it means |
|---|---|---|
| Hop latency | Time from receive to forward completion | Network speed |
| Reliability | Successful forwards / total attempts (basis points) | Uptime quality |
| Throughput | Bytes forwarded / time | Bandwidth capacity |
| Uptime | Derived from heartbeat gaps | Availability |

All integer or basis-point metrics (Pillar 9). Fed into relay selection at routing time.

### Incentive Pools for Relay Capacity

Anyone can fund relay capacity via the universal incentive pool mechanism:

```yaml
# Incentive pool for relay capacity
criteria_type: relay_capacity
criteria_params:
  min_bandwidth_mbps: 10
  min_reliability_bps: 9500
amount_remaining: 5000
payout_interval_s: 3600
```

Qualifying relay nodes earn from the pool. Multiple funders stack. Same mechanism as compute model availability and storage hosting grants.

---

## IV. Wire-Side Schema

### Prerequisites

- Extend `wire_graph_fund.source_type` CHECK to include `'relay_hop'`
- Shared rotator infrastructure already exists (`advance_market_rotator`, `market_rotator_recipient`)

### New Tables

```sql
-- Relay offers: bandwidth and pricing
CREATE TABLE wire_relay_offers (
  id                          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  node_id                     UUID NOT NULL REFERENCES wire_nodes(id),
  operator_id                 UUID NOT NULL REFERENCES wire_operators(id),
  effective_per_hop_rate      INTEGER NOT NULL DEFAULT 1,
  -- Capacity
  max_concurrent_relays       INTEGER NOT NULL DEFAULT 5,
  current_active_relays       INTEGER NOT NULL DEFAULT 0,
  -- Quality (network-observed)
  observed_avg_hop_latency_ms INTEGER,
  observed_reliability_bps    INTEGER,        -- 0-10000 basis points
  observed_hop_count          INTEGER DEFAULT 0,
  --
  status                      TEXT NOT NULL DEFAULT 'active',
  created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(node_id)  -- one relay offer per node
);

ALTER TABLE wire_relay_offers ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_relay_offers TO service_role;

-- Relay quality observations (append-only)
CREATE TABLE wire_relay_observations (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  job_id          UUID NOT NULL,
  node_id         UUID NOT NULL REFERENCES wire_nodes(id),
  hop_index       INTEGER NOT NULL,
  bytes_forwarded BIGINT,
  hop_latency_ms  INTEGER,
  success         BOOLEAN NOT NULL,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE wire_relay_observations ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_relay_observations TO service_role;

CREATE INDEX idx_relay_obs_node ON wire_relay_observations(node_id, created_at DESC);
```

---

## V. Wire-Side Relay Selection

```sql
CREATE OR REPLACE FUNCTION select_relay_chain(
  p_requester_node_id UUID,
  p_provider_node_id UUID,
  p_relay_count INTEGER                -- how many relays the requester wants
) RETURNS TABLE(
  hop_index INTEGER,
  relay_node_id UUID,
  relay_tunnel_url TEXT,
  relay_rate INTEGER
)
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_excluded_operators UUID[];
  v_selected_operators UUID[];
BEGIN
  IF p_relay_count = 0 THEN
    RETURN;  -- no relays, empty result set
  END IF;

  -- NOTE: If fewer than p_relay_count qualifying operators exist, this function
  -- returns fewer rows than requested. The caller MUST check the row count:
  -- - If count < p_relay_count AND bootstrap mode enabled: Wire fills remaining
  --   hops itself (acting as relay). The Wire uses its own tunnel URL for the gap.
  -- - If count < p_relay_count AND bootstrap mode disabled: raise exception
  --   'Insufficient relay capacity (% available, % requested)', count, p_relay_count.
  -- The caller (fill endpoint) handles the fallback logic, not this function.

  -- Exclude requester and provider operators
  SELECT ARRAY_AGG(DISTINCT a.operator_id) INTO v_excluded_operators
    FROM wire_nodes n JOIN wire_agents a ON a.id = n.agent_id
    WHERE n.id IN (p_requester_node_id, p_provider_node_id);

  v_selected_operators := '{}';

  -- Select p_relay_count relays, each from a different operator
  RETURN QUERY
    WITH ranked AS (
      SELECT r.node_id, r.operator_id, r.effective_per_hop_rate,
             n.tunnel_url,
             r.observed_reliability_bps,
             ROW_NUMBER() OVER (
               PARTITION BY r.operator_id
               ORDER BY r.observed_reliability_bps DESC NULLS LAST,
                        r.effective_per_hop_rate ASC
             ) as op_rank,
             DENSE_RANK() OVER (
               ORDER BY r.observed_reliability_bps DESC NULLS LAST,
                        r.effective_per_hop_rate ASC
             ) as global_rank
        FROM wire_relay_offers r
        JOIN wire_nodes n ON n.id = r.node_id
        WHERE r.status = 'active'
          -- Pillar 37: staleness threshold from economic_parameter contribution (DD-K style)
          AND n.last_seen_at > now() - (
                (SELECT COALESCE((c.structured_data->>'heartbeat_staleness_s')::INTEGER, 300)
                   FROM wire_contributions c
                   WHERE c.type = 'economic_parameter'
                     AND c.structured_data->>'parameter_name' = 'staleness_thresholds'
                     AND c.released_at IS NULL
                   ORDER BY c.created_at DESC LIMIT 1) || ' seconds')::interval
          AND r.current_active_relays < r.max_concurrent_relays
          AND r.operator_id != ALL(v_excluded_operators)
    ),
    distinct_operators AS (
      SELECT DISTINCT ON (operator_id)
        node_id, operator_id, effective_per_hop_rate, tunnel_url
      FROM ranked
      WHERE op_rank = 1
      ORDER BY operator_id, global_rank
      LIMIT p_relay_count
    )
    SELECT ROW_NUMBER() OVER ()::INTEGER as hop_index,
           d.node_id, d.tunnel_url, d.effective_per_hop_rate
    FROM distinct_operators d;
END;
$$;

GRANT EXECUTE ON FUNCTION select_relay_chain TO service_role;
```

---

## VI. Node-Side Architecture

### Relay Daemon (`relay_market.rs`)

The third specialized daemon. Simplest of the three:

```rust
pub struct RelayMarketState {
    pub is_relaying: bool,
    pub total_hops_completed: u64,
    pub total_credits_earned: i64,
    pub session_hops_completed: u64,
    pub session_credits_earned: i64,
    pub effective_per_hop_rate: i64,
    pub pricing_strategy: Option<PricingStrategy>,
    pub active_relay_count: u32,            // currently forwarding
    pub max_concurrent_relays: u32,         // capacity limit (contribution)
}
```

**No queue needed.** Relay forwarding is streaming — bytes in, bytes out, constant memory. No GPU, no processing time, no serialization.

### Relay Endpoint

```rust
// In server.rs
let relay_forward = warp::post()
    .and(warp::path!("v1" / "relay" / "forward"))
    .and(warp::body::stream())
    .and(warp::header::<String>("x-relay-token"))
    .and(with_state(state.clone()))
    .and_then(handle_relay_forward);

async fn handle_relay_forward(
    body_stream: impl Stream<Item = Result<Bytes, warp::Error>>,
    relay_token: String,
    state: SharedState,
) -> Result<impl warp::Reply, warp::Rejection> {
    // 1. Verify relay_token JWT (signed by Wire)
    // 2. Peel one layer off the nested token → extract next_hop URL
    // 3. Check capacity (active_relay_count < max_concurrent_relays)
    // 4. Increment active_relay_count
    // 5. Stream body to next_hop URL (pipe, don't buffer)
    // 6. Stream response back to caller
    // 7. Decrement active_relay_count
    // 8. Report completion to Wire for settlement (fire-and-forget POST)
    //
    // NOTE: The relay does NOT know:
    // - Its position in the chain (hop 1? hop 5? last hop?)
    // - Whether next_hop is another relay or the final provider
    // - Whether the caller is the requester or another relay
    // - What the payload contains (encrypted blob for compute, opaque bytes for storage)
}
```

**Streaming is critical:** The relay pipes bytes as they arrive. It never buffers the full payload in memory. A 10MB document body flows through with constant ~64KB memory usage. This means relay nodes genuinely only need bandwidth, not RAM.

### Tunnel Rotation

```rust
pub struct TunnelRotationState {
    pub current_tunnel_url: TunnelUrl,                // per DD-D: TunnelUrl newtype, not raw String
    pub previous_tunnel_url: Option<TunnelUrl>,       // kept alive during drain period
    pub rotation_interval_s: u64,                     // from privacy_policy contribution
    pub drain_grace_s: u64,                           // from privacy_policy contribution
    pub last_rotated_at: Option<chrono::DateTime<chrono::Utc>>,
}
```

**Field ownership:** `rotation_interval_s` and `drain_grace_s` are rotation-cadence fields on `privacy_policy` (the requester's privacy decision, not a relay operator decision). `max_concurrent_relays` / `forward_timeout_secs` / backoff knobs are on `relay_delivery_policy` (operator decision). See the 2026-04-16 unification note above for the ownership split rationale.

Rotation loop in `main.rs`:
1. Check if `elapsed > rotation_interval_s`
2. Request new tunnel from Cloudflare
3. Set `previous_tunnel_url = current_tunnel_url`
4. Set `current_tunnel_url = new_url`
5. Push new URL to Wire via heartbeat (or immediate POST)
6. After `drain_grace_s`, close the previous tunnel
7. Any in-flight connections on the old URL complete normally during drain

### IPC Commands

```rust
#[tauri::command] async fn relay_market_enable(state) -> Result<()>;
#[tauri::command] async fn relay_market_disable(state) -> Result<()>;
#[tauri::command] async fn relay_market_get_state(state) -> Result<RelayMarketState>;
#[tauri::command] async fn relay_pricing_update(state, strategy: PricingStrategy) -> Result<()>;
#[tauri::command] async fn tunnel_rotation_configure(state, interval_s, drain_s) -> Result<()>;
#[tauri::command] async fn tunnel_rotate_now(state) -> Result<String>;
#[tauri::command] async fn fleet_announce(state) -> Result<()>;  // manual fleet re-announce
```

### Frontend Components — Steward-Mediated

Relay management is part of the unified `MarketDashboard.tsx` (see compute plan). The steward manages relay pricing and capacity autonomously. Relay-specific surfaces:

```
src/components/relay/
  RelayStatusSection.tsx         — Part of steward status report: hops completed,
                                   bandwidth used, earnings. Informational.
  TunnelRotationPanel.tsx        — Rotation config, manual rotate button.
                                   This IS a direct action (rotation timing is a
                                   privacy decision the operator makes).
```

### Economic Gates

- **Relay chain setup**: each relay hop costs at least 1 credit. The requester pays.
- **No zero-cost relaying.** Even the minimum fee prevents relay abuse.
- All relay settlements generate ledger entries → audit trail.

### Fleet Discovery — Direct Peer-to-Peer

Fleet nodes (same operator) discover each other directly via tunnels:
1. Node comes online → Wire provides fleet roster (same-operator peers + tunnel URLs)
2. Node announces to each peer: `POST {peer_tunnel}/v1/fleet/announce`
3. Peers update roster instantly. Zero delay.
4. State changes announced peer-to-peer (model loaded, going offline)
5. Fleet traffic never touches relays — direct tunnel-to-tunnel, same operator

---

## VII. Privacy Policy Contribution

The requester controls their privacy via a contribution (`schema_type: privacy_policy`):

```yaml
# Privacy policy contribution (on requester node)
default_relay_count: 2
sensitive_relay_count: 5
relay_count_range: [0, 20]
auto_escalate_on_sensitive_content: true   # steward can increase relay count for sensitive builds
```

The chain executor reads this when dispatching. The `WireComputeProvider` passes the relay count to the Wire match endpoint. The Wire sets up the chain accordingly.

The steward can adjust relay count per-job based on content sensitivity — a composable function in the steward's action chains. No new system needed.

---

## VIII. Phase Breakdown

### Phase R1: Relay Infrastructure + Tunnel Rotation

**What ships:** Relay endpoint functional. Relay offers published. Tunnel rotation operational. No integration with compute/storage yet (manual testing only).

**Wire workstream:**
- Migration: `wire_relay_offers`, `wire_relay_observations` tables
- Extend `wire_graph_fund` CHECK for `'relay_hop'`
- `settle_relay_hop` RPC
- `select_relay_chain` RPC (variable N)
- Relay offer CRUD endpoints
- Nested relay chain token generation (layered JWT)

**Node workstream:**
- `relay_market.rs`: `RelayMarketState`, enable/disable, pricing strategy, capacity limits
- `server.rs`: `/v1/relay/forward` streaming endpoint with JWT peel + forward
- `tunnel_rotation.rs`: Rotation loop, Cloudflare tunnel management, drain logic
- Relay daemon loop in `main.rs`: heartbeat-driven, pushes offer updates + current tunnel URL
- Bandwidth self-limiting (concurrent relay cap from contribution)

**Frontend workstream:**
- `RelayMarketPanel.tsx`: Enable/disable, status
- `RelayPricingPanel.tsx`: Pricing strategy
- `TunnelRotationPanel.tsx`: Rotation config, manual rotate

**Verification:** Relay node enables. Send a payload through 3-hop chain manually. Each hop streams and forwards. Settlement credits each relay. Tunnel rotation works and Wire gets new URL.

### Phase R2: Compute + Storage Integration

**What ships:** Compute and storage markets automatically use relay chains based on requester's privacy policy. E2E encryption for compute. Seamless — requester and provider don't know the relay topology.

**Wire workstream:**
- Integrate `select_relay_chain` into compute `match_compute_job` and storage pull routing
- Relay fees included in cost preview
- Relay chain setup in fill/dispatch handlers (build nested token, route through chain)
- Ephemeral key exchange for compute E2E encryption
- Bootstrap mode: Wire acts as relay when insufficient relay capacity
- **No aggregate relay statistics published anywhere** (distributional opacity)

**Node workstream:**
- `WireComputeProvider`: passes `relay_count` from privacy policy to Wire match endpoint
- `privacy_policy` contribution type: default_relay_count, sensitive_relay_count
- Storage pull client: relay support transparent (Wire handles routing)
- Compute provider: ephemeral key generation for E2E encryption
- Fleet-internal routing: bypasses relay (same-operator, no relays needed)

**Frontend workstream:**
- Privacy policy configuration panel
- Privacy indicator on jobs: "N relay hops" (own usage visible, not network aggregate)
- Relay cost breakdown in build preview

**Verification:** Pyramid build with `relay_count: 3`. All prompts flow through 3-hop relay chains. Provider can't identify requester. Relays earn credits. Build completes correctly. Tunnel rotates mid-build without disruption.

---

## IX. Pillar Conformance

| Pillar | How Respected |
|---|---|
| 1 (Contribution) | Relay pricing, capacity, privacy policy, rotation config — all contributions. |
| 7 (UFF) | No creator/source-chain split. Wire 2.5% + Graph Fund 2.5% via rotator arm. Relay receives 95%. |
| 9 (Integer) | All i64. Per-hop rate is integer credits. Quality in basis points. |
| 12 (Emergent pricing) | Competitive auto-pricing. Market discovers relay rates. |
| 23 (Preview) | Relay fees shown in cost preview. Relay count shown before commit. |
| 35 (Graph Fund) | 2.5% via rotator arm on each relay hop settlement. |
| 37 (No hardcoded) | Relay count, rotation interval, capacity limits, hop fees — all contributions. |
| 42 (Frontend) | Both phases have frontend workstreams. |

---

## X. The Mature Network

At scale, the Wire network looks like this to an outside observer:

```
[cloud of rotating tunnel URLs]
     ↕ encrypted streams ↕
[cloud of rotating tunnel URLs]
```

- Can't tell who's a requester, relay, or provider (same software, same endpoints)
- Can't tell how many hops any connection traverses (0 to N, unknown distribution)
- Can't correlate connections over time (tunnel URLs rotate)
- Can't observe the routing topology (Wire sets it up, data flows node-to-node)
- Can't prove any two connections involve the same node
- Can't determine whether a direct connection is actually direct

The Wire itself can't observe the data flow after setup — it issues routing instructions and the bytes flow between nodes. The Wire sees: "I told N relays to form a chain. Settlement says it completed." It never saw the payload.

**Three orthogonal privacy mechanisms:**
1. **Variable relay count** → topology ambiguity (can't infer chain length)
2. **Distributional opacity** → probabilistic ambiguity (can't estimate likelihood)
3. **Tunnel rotation** → temporal ambiguity (can't correlate over time)

Each is independent. Each strengthens the others. Together they make traffic analysis practically impossible without simultaneously compromising multiple nodes within a rotation window — and even then, the variable relay count means the attacker can't know if they've compromised enough nodes to reconstruct the full path.

---

## XI. What Relay Does NOT Need

- **No queues** — forwarding is streaming, near-instant, no GPU processing
- **No order book** — Wire selects relays, relays don't bid/ask
- **No deposit/estimation** — relay cost is fixed per-hop, known upfront
- **No reservation mechanism** — relay is one-shot, not queued
- **No model management** — relays don't run models
- **No storage** — relays don't keep data
- **No capacity mirror** — relay offers include max_concurrent, Wire tracks active count
