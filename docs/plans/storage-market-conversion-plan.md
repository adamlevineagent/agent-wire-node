# Storage Market Conversion Plan

**Original draft:** 2026-04-13
**Last revision:** 2026-04-16 (post-audit unification pass)
**Scope:** Convert the existing storage market from centrally-planned flat-rate hosting to a proper market with provider-set pricing, competitive auto-pricing, hosting grants, rotator arm platform levy, and network-observed quality signals.
**Prerequisite:** Compute market Phase 1 ships first (rotator arm infrastructure, atomic credit RPCs pattern, contribution-driven config). Storage S1 does NOT require compute Phase 2 (audit fix — dependency graph in seams §VIII corrected).
**Companion docs:** `compute-market-architecture.md` (canonical for slug namespace, CallbackKind, privacy model, shared primitives, DD-A through DD-O decisions in §VIII.6), `async-fleet-dispatch.md` (transport pattern), `fleet-mps-build-plan.md` (participation policy canonical).

---

## 2026-04-16 Unification Pass — What's Canonical Where

The 2026-04-13 draft of this plan was reconciled against four foundations that landed between 2026-04-13 and 2026-04-16: DADBEAR canonical architecture (shipped), Fleet MPS (compute_participation_policy contribution shipped; three-objects still pending), async-fleet-dispatch (Phases 1-3 shipped), SOTA privacy model (post-B1 rewrite of compute §III). The 2026-04-16 audit (`audit-2026-04-16-three-market-refresh.md`) closed every TBD. Rather than carry a layered overlay, this revision inlines the shared decisions (canonical DDs in architecture §VIII.6) and fixes the specific body issues in place. All references to "overlay," "Foundation 1-4," and layered corrections have been removed; body sections are now canonical for their topic. Deltas from the original 2026-04-13 draft:

- **Slug namespace:** `"market:storage"` per DD-A. Applied throughout.
- **Participation policy:** storage consumes `allow_storage_hosting` (offer publication + DADBEAR supervisor dispatch gate for `storage_host` work items) and `allow_storage_pulling` (pull-routing outbound). Full 10-field canonical list lives in `fleet-mps-build-plan.md` per DD-I — no parallel `storage_participation_policy` contribution.
- **DADBEAR integration:** `market.rs`'s host/drop/evaluate_opportunities loop becomes a DADBEAR observation source. Daemon writes `storage_host_candidate` events; compiler produces `storage_host` / `storage_drop` / `storage_retention_response` / `storage_chunk_pin` work items; supervisor dispatches them (pull body, verify hash, report pin). Crash recovery comes free. Breaker holds on `market:storage` slug stop all storage work items without touching compute.
- **Outbox:** Reuse shipped `fleet_result_outbox` per DD-D. Storage settle-retry uses `CallbackKind::MarketStandard` (same variant as compute — per DD-B there is no `Storage` variant; the shipped variants `Fleet / MarketStandard / Relay` cover the intended cross-market reuse). Storage's immediate need is settle metadata delivery, which the `pyramid/messages.rs` helper pattern doesn't apply to (no ChatML conversion).
- **Auth:** `wire_document_token` (JWT) verified via `verify_storage_identity` at `pyramid/storage_identity.rs` (parallel to `fleet_identity.rs`, `aud: "storage"`). Shipped Wire signing key (same as fleet).
- **TunnelUrl:** Every URL field (node tunnels, provider lookups at routing time) goes through `TunnelUrl::parse` at ingress. No raw String URL fields in new tables or IPC shapes. Note: `wire_storage_offers` does NOT carry a tunnel URL column — tunnel URL is on `wire_nodes` (joined at routing time).
- **SOTA privacy (compute-market-architecture §III):** Storage pulls inherit variable relay count + distributional opacity + tunnel rotation. Launch = Wire-as-bootstrap-relay for non-0-relay pulls. 0-relay = direct consumer→provider with plausible deniability. Document bodies are plaintext (public content) — no E2E encryption needed, unlike compute prompts.
- **Pull streaming vs outbox:** Pulls ≤ `sync_stream_max_bytes` economic_parameter (seeded Phase S1 at 100 MB) use synchronous streaming through the relay chain or direct tunnel. Pulls > threshold use chunked-asset manifest (§VI "Chunked Storage for Large Files") — each chunk is a synchronous stream up to the threshold. No async outbox for pulls themselves; outbox (if added later) only covers settle retry, not body delivery.
- **Settlement RPC:** `settle_document_serve_v2(p_token_id, p_hosting_node_id, p_serve_latency_ms)` — consumer / document / matched_rate all resolved from token row inside RPC (OB-2 fix; provider never sees consumer identity).
- **`min_replicas`:** NO DEFAULT on `wire_hosting_grants.min_replicas` per DD-O. Caller supplies from policy.
- **CallbackKind:** Fleet/MarketStandard/Relay per DD-B (no rename — docs now match shipped code).

All other sections below stand as-is except where inline edits land specific decisions.

---

## I. What Exists Today

### Node Side (`market.rs`, 268 lines)
- Heartbeat response includes `storage_market`: top opportunities ranked by `pulls_30d / current_replicas`
- Daemon scores opportunities, auto-hosts top ones that fit in `storage_cap_gb`
- Auto-drops underperformers (zero pulls, zero credits) at >90% capacity
- Reports host/drop to Wire via `/api/v1/node/host` and `/api/v1/node/drop`
- State persisted to `market_state.json`
- `credits_earned: f64` — Pillar 9 violation

### Wire Side
- `wire_document_availability` — tracks which nodes host which documents
- `wire_document_tokens` — token-based pull authorization (consumer gets token, redeems at node)
- `settle_document_serve` — atomic 1-credit-per-pull settlement (raw SQL, no `balance_after`, no platform levy)
- `wire_fund_grants` — Wire-funded hosting grants for cultural content
- `wire_retention_challenges` — proof-of-retention via byte-range hashing
- `wire_purge_directives` — content removal orders

### What's Wrong
1. **Flat 1 credit/pull** — no competition, no price discovery
2. **No platform levy** — Wire take "funded from platform revenue" (not actually collected)
3. **Central planning** — Wire tells nodes what to host (top opportunities in heartbeat)
4. **Settlement bypasses atomic RPCs** — raw `UPDATE wire_operators`, missing `balance_after`
5. **No provider-set pricing** — everyone charges the same
6. **No quality signals** — all providers are equivalent regardless of speed/reliability
7. **No hosting grants from users** — only Wire Fund Grants (platform-funded)
8. **`credits_earned: f64`** — Pillar 9 violation
9. **`min_replicas` hardcoded to 2** — not contribution-driven
10. **Consumer sees no preview** — pull costs 1 credit, always, no choice

---

## II. Architecture After Conversion

### Core Changes

1. **Provider-set pricing.** Providers set their own per-pull credit rate (or use competitive auto-pricing strategy). The market discovers the right price per corpus based on demand and competition.

2. **Competitive auto-pricing.** Providers set a strategy (match best, undercut, premium) with floor and ceiling bounds. The daemon resolves the effective rate from market data in the heartbeat. Same primitive as compute market.

3. **Hosting grants from anyone.** Any operator can fund a hosting grant for any corpus. The grant pays out 1 credit per tick on a document rotator (cycles through all documents in the corpus). Providers hosting documents in that corpus earn from the grant. The grant is a contribution (supersedable, exhaustible).

4. **Rotator arm platform levy (76/2/2).** Same as compute: 95% provider, 2.5% Wire, 2.5% Graph Fund. Applied to both per-pull settlement AND hosting grant payouts. Pure integer via rotator arm.

5. **Network-observed quality.** Serve latency, availability, retention challenge pass rate, uptime — all measured by the network, not self-reported. Feed into routing decisions.

6. **Pull routing as matching.** When a consumer requests a document, the Wire matches them to the best available provider (cheapest, fastest, most reliable — consumer's choice). Preview-then-commit (Pillar 23).

7. **Hosting contest for new content.** When a new document needs replication, providers race to host it. First providers to host and pass a retention challenge earn a bonus from the hosting grant pool. Incentivizes fast distribution.

8. **Settlement uses atomic RPCs.** All credit operations through `credit_operator_atomic`/`debit_operator_atomic`. Proper `balance_after`. No raw SQL.

9. **All config as contributions.** Storage cap, pricing strategy, min replicas — all contribution-driven. No hardcoded numbers.

10. **Relay network for pulls.** Document pulls route through the relay network (same infrastructure as compute). Consumer chooses relay count (0-N) via privacy policy contribution. With 0 relays: direct connection with plausible deniability (provider can't prove it's direct). With 1+ relays: provider sees last relay's rotating tunnel URL, not consumer. No encryption needed for document bodies (public content). Consumer identity hidden by the relay chain + tunnel rotation.

### What Stays The Same
- Token-based serve pattern (consumer gets token, redeems at node — now through relay chain)
- Retention challenges (Wire-initiated, proof-of-retention)
- Purge directives (content removal)
- The daemon's evaluate-opportunities loop (but with market pricing instead of flat rate)
- `wire_document_availability` table (which nodes host which docs)

### Data Plane vs Control Plane (Same as Compute)

| Channel | What flows | Path |
|---|---|---|
| **Data plane** (relay chain) | Document bodies, pull requests | Consumer ↔ Relays ↔ Provider |
| **Control plane** (Wire API) | Token issuance, settlement, routing, heartbeat | Node ↔ Wire |

The Wire never handles document bodies. All bandwidth flows through the relay network.

---

## III. Pricing Model

### Storage Offers

Each provider publishes a storage offer (contribution, `schema_type: storage_pricing`):

```yaml
# Storage pricing contribution
available_gb: 50
pricing_mode: competitive           # "fixed" | "competitive"
competitive_target: match_best      # "match_best" | "undercut_best" | "premium_over_best"
competitive_offset_bps: 0           # basis points relative to target
floor_per_pull: 1                   # never below 1 credit/pull (minimum)
ceiling_per_pull: 10                # never above this
```

The daemon resolves the effective rate from heartbeat market data, same as compute:

```
best_rate = cheapest active provider for this corpus
my_effective_rate = apply_competitive_strategy(best_rate, strategy)
my_effective_rate = clamp(my_effective_rate, floor, ceiling)
```

The resolved effective rate is pushed to the Wire with the offer update.

### Two Revenue Streams Per Hosted Document

**Pull revenue:** Per-serve, at the provider's effective rate. Settled via token + rotator arm.

**Hosting grant revenue:** Per-tick payouts from corpus-level hosting grants. Settled via document rotator + rotator arm.

Provider's total expected revenue for a document:
```
pull_income = pulls_per_month * effective_rate * 76/80  (rotator arm)
grant_income = grant_payout_frequency * (1 / corpus_doc_count) * 76/80  (document rotator × platform rotator)
total = pull_income + grant_income
```

The daemon's efficiency scoring uses this combined signal:
```
effective_demand = natural_pulls_30d + grant_equivalent_pulls_30d
efficiency = effective_demand / replicas
```

Where `grant_equivalent_pulls_30d` converts the grant payout rate into pull-equivalent demand at the provider's rate. This lets the daemon compare natural demand and subsidized demand on the same scale.

### Hosting Grants

A hosting grant is a contribution (`schema_type: hosting_grant`):

```yaml
# Hosting grant contribution
corpus_id: "uuid"
amount_remaining: 5000         # credits in the pool
payout_interval_s: 3600        # 1 credit per hour
min_replicas: 3                # desired replication level (contribution, not hardcoded)
```

**Document rotator:** Each payout tick, the grant rotator advances through all documents in the corpus (Bjorklund distribution). One document is selected. The provider hosting that document receives the payout (subject to the platform rotator — 76/80 provider, 2/80 Wire, 2/80 Graph Fund).

If multiple providers host the same document, the payout rotates among them (another rotator level — nested rotators: document selection → provider selection → platform levy).

**Grant exhaustion:** When `amount_remaining` hits 0, the grant stops. The signal disappears. Documents that depended on the grant for hosting economics may get dropped by providers if natural demand is insufficient.

**Multiple funders stack:** Multiple grants on the same corpus stack. Each runs its own rotator independently. Total signal is the sum of all active grants' payout rates.

### Hosting Contest (New Content Distribution)

When a new document is published to a corpus with an active hosting grant:

1. The document appears in the opportunity surface (heartbeat market data)
2. Providers race to host it (pull from origin, verify hash, report pin)
3. **First N providers to host AND pass a retention challenge** earn a hosting bonus from the grant pool
4. The bonus is a configurable multiplier on the first payout: e.g., `first_host_bonus_multiplier: 3` means the first payout for this document is 3 credits instead of 1
5. The bonus depletes from the grant pool like any other payout
6. After the initial distribution, normal payout-per-tick resumes

The bonus incentivizes fast replication. Providers who want to earn the bonus maintain spare capacity and monitor for new content. The `first_host_bonus_multiplier` is a field on the hosting grant contribution — the funder decides how much to incentivize speed.

---

## IV. Settlement

### Per-Pull Settlement (Updated)

Replace `settle_document_serve` with a market-aware version:

```sql
-- Per OB-2 fix: signature resolves consumer + document + matched_rate from the token row.
-- The provider (who calls settle) doesn't know the consumer — and MUST NOT, per SOTA privacy.
-- The RPC gets them from wire_document_tokens after verifying the token.
CREATE OR REPLACE FUNCTION settle_document_serve_v2(
  p_token_id UUID,
  p_hosting_node_id UUID,
  p_serve_latency_ms INTEGER
) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_token wire_document_tokens%ROWTYPE;
  v_hosting_operator_id UUID;
  v_rotator_pos INTEGER;
  v_recipient TEXT;
  v_wire_platform_operator_id UUID;
BEGIN
  -- Verify token AND resolve consumer+document+matched_rate atomically
  UPDATE wire_document_tokens
    SET redeemed = true
    WHERE id = p_token_id
      AND routed_to_node_id = p_hosting_node_id
      AND redeemed = false
      AND expires_at > now()
    RETURNING * INTO v_token;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Invalid, expired, or already-redeemed document token';
  END IF;

  -- Resolve hosting operator (per DD-K: h.released_at IS NULL, not h.status = 'active')
  SELECT a.operator_id INTO v_hosting_operator_id
    FROM wire_nodes n
    JOIN wire_agents a ON a.id = n.agent_id
    WHERE n.id = p_hosting_node_id;
  IF v_hosting_operator_id IS NULL THEN
    RAISE EXCEPTION 'Hosting node % has no linked operator', p_hosting_node_id;
  END IF;

  -- Debit consumer using atomic RPC (consumer identity resolved from token, NOT from provider input)
  PERFORM debit_operator_atomic(v_token.consumer_operator_id, v_token.matched_rate,
    'document_serve_consumer', p_token_id, 'storage_market');

  -- Rotator arm: 76 provider / 2 Wire / 2 Graph Fund (same as compute)
  v_rotator_pos := advance_market_rotator(p_hosting_node_id, 'storage', 'default', 'serve');
  v_recipient := market_rotator_recipient(v_rotator_pos);

  IF v_recipient = 'provider' THEN
    PERFORM credit_operator_atomic(v_hosting_operator_id, v_token.matched_rate,
      'document_serve_host', p_token_id, 'storage_market');
  ELSIF v_recipient = 'wire' THEN
    SELECT o.id INTO v_wire_platform_operator_id FROM wire_operators o
      JOIN wire_agents a ON a.operator_id = o.id
      JOIN wire_handles h ON h.agent_id = a.id
      WHERE h.handle = 'agentwireplatform' AND h.released_at IS NULL LIMIT 1;
    PERFORM credit_operator_atomic(v_wire_platform_operator_id, v_token.matched_rate,
      'storage_wire_take', p_token_id, 'storage_market');
  ELSE  -- graph_fund
    INSERT INTO wire_graph_fund (amount, source_type, reference_id)
      VALUES (v_token.matched_rate, 'storage_serve', p_token_id);
  END IF;

  -- Update stats
  UPDATE wire_nodes SET credits_earned_total = credits_earned_total + v_token.matched_rate
    WHERE id = p_hosting_node_id;
  UPDATE wire_document_availability
    SET pulls_served_total = pulls_served_total + 1, last_served = now()
    WHERE document_id = v_token.document_id AND node_id = p_hosting_node_id;

  -- Record observation for network quality tracking (latency comes from provider's settle POST)
  INSERT INTO wire_storage_observations (node_id, document_id, serve_latency_ms, success)
    VALUES (p_hosting_node_id, v_token.document_id, p_serve_latency_ms, true);
END;
$$;

GRANT EXECUTE ON FUNCTION settle_document_serve_v2 TO service_role;
```

### Hosting Grant Payout (New)

```sql
CREATE OR REPLACE FUNCTION process_hosting_grant_payouts(
  p_batch_size INTEGER DEFAULT 100
) RETURNS INTEGER
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_grant RECORD;
  v_doc_id UUID;
  v_hosting_node_id UUID;
  v_hosting_operator_id UUID;
  v_rotator_pos INTEGER;
  v_recipient TEXT;
  v_count INTEGER := 0;
  v_wire_platform_operator_id UUID;
BEGIN
  -- Resolve Wire platform operator once
  SELECT o.id INTO v_wire_platform_operator_id FROM wire_operators o
    JOIN wire_agents a ON a.operator_id = o.id
    JOIN wire_handles h ON h.agent_id = a.id
    WHERE h.handle = 'agentwireplatform' AND h.released_at IS NULL LIMIT 1;  -- DD-K

  -- Find grants due for payout
  FOR v_grant IN
    SELECT g.* FROM wire_hosting_grants g
    WHERE g.amount_remaining > 0
      AND g.status = 'active'
      AND (g.last_payout_at IS NULL
           OR g.last_payout_at + (g.payout_interval_s || ' seconds')::interval < now())
    ORDER BY g.last_payout_at NULLS FIRST
    LIMIT p_batch_size
    FOR UPDATE SKIP LOCKED
  LOOP
    -- Document rotator: pick next document in corpus
    v_doc_id := advance_document_rotator(v_grant.id, v_grant.corpus_id);
    IF v_doc_id IS NULL THEN
      CONTINUE;  -- no documents in this corpus (shouldn't happen)
    END IF;

    -- Find a provider hosting this document (rotate among providers if multiple)
    SELECT da.node_id INTO v_hosting_node_id
      FROM wire_document_availability da
      WHERE da.document_id = v_doc_id
      ORDER BY da.pulls_served_total ASC  -- favor less-served providers (load balance)
      LIMIT 1;
    IF v_hosting_node_id IS NULL THEN
      CONTINUE;  -- document not hosted (grant pays nobody, credit stays in pool)
    END IF;

    -- Resolve hosting operator
    SELECT a.operator_id INTO v_hosting_operator_id
      FROM wire_nodes n JOIN wire_agents a ON a.id = n.agent_id
      WHERE n.id = v_hosting_node_id;

    -- Debit grant pool
    UPDATE wire_hosting_grants SET
      amount_remaining = amount_remaining - 1,
      last_payout_at = now()
    WHERE id = v_grant.id AND amount_remaining > 0;
    IF NOT FOUND THEN CONTINUE; END IF;

    -- Platform rotator: 76/2/2
    v_rotator_pos := advance_market_rotator(v_hosting_node_id, 'storage', 'default', 'grant');
    v_recipient := market_rotator_recipient(v_rotator_pos);

    IF v_recipient = 'provider' THEN
      PERFORM credit_operator_atomic(v_hosting_operator_id, 1,
        'hosting_grant_payout', v_grant.id, 'storage_market');
    ELSIF v_recipient = 'wire' THEN
      PERFORM credit_operator_atomic(v_wire_platform_operator_id, 1,
        'hosting_grant_wire_take', v_grant.id, 'storage_market');
    ELSE
      INSERT INTO wire_graph_fund (amount, source_type, reference_id)
        VALUES (1, 'hosting_grant', v_grant.id);
    END IF;

    v_count := v_count + 1;
  END LOOP;

  RETURN v_count;
END;
$$;

GRANT EXECUTE ON FUNCTION process_hosting_grant_payouts TO service_role;
```

---

## V. Wire-Side Schema Changes

### New Tables

```sql
-- Storage offers: provider capacity and pricing
CREATE TABLE wire_storage_offers (
  id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  node_id               UUID NOT NULL REFERENCES wire_nodes(id),
  operator_id           UUID NOT NULL REFERENCES wire_operators(id),
  -- Pricing (resolved effective rate, not the strategy — strategy is local contribution)
  effective_per_pull_rate INTEGER NOT NULL DEFAULT 1,
  -- Capacity
  available_gb          REAL,               -- informational, not financial (OK as REAL)
  used_gb               REAL,
  documents_hosted      INTEGER DEFAULT 0,
  -- Quality (network-observed)
  observed_avg_serve_latency_ms INTEGER,
  observed_availability_pct     INTEGER,    -- 0-10000 basis points (Pillar 9)
  observed_retention_pass_rate  INTEGER,    -- 0-10000 basis points
  observed_serve_count          INTEGER DEFAULT 0,
  --
  status                TEXT NOT NULL DEFAULT 'active',
  created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE wire_storage_offers ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_storage_offers TO service_role;

CREATE INDEX idx_storage_offers_node ON wire_storage_offers(node_id);
CREATE INDEX idx_storage_offers_active ON wire_storage_offers(status) WHERE status = 'active';

-- Hosting grants: anyone can fund hosting for a corpus
CREATE TABLE wire_hosting_grants (
  id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  funder_operator_id    UUID NOT NULL REFERENCES wire_operators(id),
  corpus_id             UUID NOT NULL,
  amount_remaining      INTEGER NOT NULL,       -- credits in pool, depletes by 1 per payout
  payout_interval_s     INTEGER NOT NULL,       -- seconds between payouts
  min_replicas          INTEGER NOT NULL,          -- Per DD-O: no DEFAULT. Caller always supplies from policy.
  first_host_bonus_multiplier INTEGER DEFAULT 1, -- bonus for first providers to host new docs
  status                TEXT NOT NULL DEFAULT 'active',  -- 'active' | 'exhausted' | 'cancelled'
  document_rotator_pos  INTEGER NOT NULL DEFAULT 0,      -- cycles through corpus documents
  last_payout_at        TIMESTAMPTZ,
  created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE wire_hosting_grants ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_hosting_grants TO service_role;

CREATE INDEX idx_hosting_grants_corpus ON wire_hosting_grants(corpus_id) WHERE status = 'active';
CREATE INDEX idx_hosting_grants_payout ON wire_hosting_grants(last_payout_at) WHERE status = 'active';

-- Storage quality observations (append-only, network-measured)
CREATE TABLE wire_storage_observations (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  node_id         UUID NOT NULL REFERENCES wire_nodes(id),
  document_id     UUID,
  serve_latency_ms INTEGER,
  success         BOOLEAN NOT NULL,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE wire_storage_observations ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_storage_observations TO service_role;

CREATE INDEX idx_storage_obs_node ON wire_storage_observations(node_id, created_at DESC);
```

### Modifications to Existing Tables

```sql
-- Add matched_rate to document tokens (currently flat 1 credit, now market-set)
ALTER TABLE wire_document_tokens ADD COLUMN matched_rate INTEGER NOT NULL DEFAULT 1;

-- Add credits_earned_total to wire_nodes — written by settle_document_serve_v2 (§IV).
-- This column is currently missing from the deployed wire_nodes schema; settle RPC
-- would fail without it. Storage S1 migration MUST add it.
ALTER TABLE wire_nodes ADD COLUMN IF NOT EXISTS credits_earned_total INTEGER NOT NULL DEFAULT 0;

-- Strip consumer identity from token for privacy-lite
-- The token still has consumer_operator_id for settlement, but the node
-- never sees it — the settlement RPC uses it server-side only.
-- No schema change needed, just API behavior: the node's serve endpoint
-- receives the token_id but NOT the consumer_operator_id.

-- wire_graph_fund.source_type CHECK constraint — ALREADY extended in compute Phase 1
-- migration 20260414100000_market_prerequisites.sql for all 5 market values
-- (compute_service, compute_reservation, storage_serve, hosting_grant, relay_hop).
-- Storage S1 does NOT need to re-extend; cite the Phase 1 migration.
```

---

## VI. Node-Side Changes

### Updated `market.rs`

The storage daemon gains:
- Competitive pricing resolution (reads strategy contribution + market data from heartbeat)
- Combined demand signal (natural pulls + hosting grant equivalent)
- Offer updates pushed to Wire (effective rate, capacity, quality stats)
- `credits_earned: i64` (fix Pillar 9)

```rust
pub struct MarketState {
    pub hosted_documents: HashMap<String, HostedDocument>,
    pub total_hosted_bytes: u64,
    pub last_evaluation_at: Option<String>,
    pub is_evaluating: bool,
    // NEW
    pub effective_per_pull_rate: i64,        // resolved from competitive strategy
    pub total_credits_earned: i64,           // Pillar 9: integer
    pub session_credits_earned: i64,
    pub pricing_strategy: Option<PricingStrategy>,  // from contribution
}

pub struct PricingStrategy {
    pub mode: String,                 // "fixed" | "competitive"
    pub competitive_target: String,   // "match_best" | "undercut_best" | "premium_over_best"
    pub competitive_offset_bps: i64,
    pub floor_per_pull: i64,
    pub ceiling_per_pull: i64,
}

pub struct MarketOpportunity {
    pub document_id: String,
    pub corpus_id: String,
    pub pulls_30d: u64,
    pub current_replicas: u64,
    pub word_count: u64,
    pub body_hash: String,
    // NEW
    pub grant_payout_rate_per_day: i64,   // Pillar 9 fix (OB-4): credit amount, i64 not u64
    pub best_provider_rate: i64,          // Pillar 9 fix (OB-4): credit amount, i64 not u64
    pub corpus_doc_count: u64,            // total docs in corpus (count, not a credit amount — u64 OK)
}
```

### Updated Evaluation

```rust
pub async fn evaluate_opportunities(
    opportunities: &[MarketOpportunity],
    market_state: &mut MarketState,
    // ...
) {
    // Step 1: Resolve competitive pricing from strategy + market data
    if let Some(strategy) = &market_state.pricing_strategy {
        market_state.effective_per_pull_rate = resolve_competitive_price(
            strategy,
            opportunities.first().map(|o| o.best_provider_rate).unwrap_or(1),
        );
    }

    // Step 2: Score with combined signal (pulls + grant equivalent)
    let scored: Vec<_> = opportunities.iter().map(|o| {
        // Grant signal: how many payouts per 30 days would this node receive
        // for hosting this document? Grant cycles through corpus docs via document
        // rotator, so each doc gets ~(payouts_per_day * 30) / corpus_doc_count payouts.
        // At our per-pull rate, each payout is worth 1 credit = 1 pull equivalent.
        let grant_payouts_30d: i64 = if o.corpus_doc_count > 0 {
            (o.grant_payout_rate_per_day * 30) / (o.corpus_doc_count as i64).max(1)
        } else {
            0
        };
        // Guard: if corpus is very large (1000 docs, 24 payouts/day),
        // grant_payouts_30d = 720/1000 = 0 (integer truncation).
        // Use grant_payout_rate_per_day directly as a tiebreaker signal
        // when the per-doc share rounds to zero.
        let effective_demand: i64 = (o.pulls_30d as i64)
            + grant_payouts_30d
            + if grant_payouts_30d == 0 && o.grant_payout_rate_per_day > 0 { 1 } else { 0 };
        let efficiency = if o.current_replicas > 0 {
            effective_demand as f64 / o.current_replicas as f64
        } else {
            effective_demand as f64
        };
        (o, efficiency)
    }).collect();

    // Step 3: Host/drop based on combined signal (same logic as before)
    // ...
}
```

### New IPC Commands

```rust
#[tauri::command] async fn storage_pricing_update(state, strategy: PricingStrategy) -> Result<()>;
#[tauri::command] async fn storage_offer_get(state) -> Result<StorageOffer>;
#[tauri::command] async fn storage_hosting_grants(state, corpus_id: String) -> Result<Vec<HostingGrant>>;
#[tauri::command] async fn storage_create_hosting_grant(state, corpus_id, amount, interval_s) -> Result<HostingGrant>;
```

### Frontend Components — Steward-Mediated

Storage management is part of the unified `MarketDashboard.tsx` (see compute plan). The steward manages storage pricing, capacity, and hosting decisions autonomously. Storage-specific surfaces:

```
src/components/storage/
  StorageStatusSection.tsx       — Part of steward status report: hosted documents,
                                   corpus coverage, grant economics, quality metrics.
                                   Informational — steward handles adjustments.
  HostingGrantManager.tsx        — Create/view/cancel hosting grants for corpora.
                                   This IS a direct action (funding a grant is a spending
                                   decision the operator makes, not the steward).
```

**NOTE:** Storage pricing, capacity limits, and hosting decisions are steward-managed (DD-10 in compute plan). The operator sets direction ("maximize storage revenue" or "keep disk usage under 15GB"), the steward handles the rest.

### Economic Gates (same pattern as compute)

- **Pull request routing**: 1 credit search fee (refunded into pull cost on success)
- **Hosting grant creation**: the grant amount IS the gate
- **Offer creation**: 1 credit
- All operations generate ledger entries → audit trail

### Chunked Storage for Large Files

Files exceeding a threshold (contribution-driven) are stored as chunks with an uploader-defined manifest:

```yaml
# Chunked asset manifest (schema_type: chunked_asset)
asset_id: "llama-3.1-70b-instruct-q4_k_m"
total_size_bytes: 42949672960
checksum_sha256: "abc123..."
chunk_strategy: layer_boundary
chunks:
  - id: "chunk-001"
    byte_range: [0, 5368709120]
    label: "embed + layers 0-15"
    sha256: "def456..."
  - id: "chunk-002"
    byte_range: [5368709120, 10737418240]
    label: "layers 16-31"
    sha256: "ghi789..."
reassembly: concatenate
```

Storage providers host individual chunks. Consumers pull chunks from multiple providers (parallel from different nodes) and reassemble locally. Not model-specific — works for any large file with natural break points. The hosting grant mechanism applies per-chunk.

---

## VII. Wire-Side API Changes

### Updated Endpoints

```
POST /api/v1/storage/offers          — Create/update storage offer (effective rate, capacity)
GET  /api/v1/storage/offers/mine     — List my active offers
POST /api/v1/storage/grants          — Create a hosting grant for a corpus
GET  /api/v1/storage/grants/:corpus  — List active grants for a corpus
DELETE /api/v1/storage/grants/:id    — Cancel a grant (remaining credits refund to funder)
GET  /api/v1/storage/market-surface  — Per-corpus: providers, rates, grant levels, quality
POST /api/v1/storage/settle          — Provider reports serve completion; calls settle_document_serve_v2
```

### Updated Pull Routing (Relay-First)

The existing pull flow gains matching + relay chain:

```
Consumer → GET /api/v1/wire/documents/:id/route?relay_count=2&preference=cheapest
  Wire finds all providers hosting this document
  Filters: status = 'active', heartbeat fresh per `staleness_thresholds.heartbeat_staleness_s` economic_parameter (Pillar 37 — no hardcoded minute count)
  Sorts by consumer preference:
    - "cheapest": lowest effective_per_pull_rate
    - "fastest": lowest observed_avg_serve_latency_ms  
    - "best": weighted score of price + latency + reliability
  Wire selects relay chain (if relay_count > 0)
  Wire charges: pull fee + relay fees from consumer
  Returns: {
    relay_chain (routing instructions, nested tokens),
    matched_rate,
    total_relay_fee,
    token_id
  }
  Preview (Pillar 23): consumer sees total cost (pull + relay) before committing
  NOTE: provider_tunnel_url NOT returned to consumer (privacy)

Consumer → Relay A → Relay B → Provider (pull request + token, through relay chain)
  Provider serves the document body back through relay chain
  Provider redeems token server-side (control plane → Wire)

Wire settlement:
  settle_document_serve_v2(token_id, hosting_node_id, serve_latency_ms)
  -- consumer, document, matched_rate all resolved from the token row server-side per OB-2 fix
  Rotator arm applies (76/2/2) for pull fee
  settle_relay_hop() for each relay (76/2/2 each)

For relay_count=0:
  Wire returns provider tunnel_url directly to consumer
  Consumer pulls directly — plausible deniability (can't prove it's direct)
  No relay fees charged
```

### Heartbeat Extension

The heartbeat response gains storage market data for competitive pricing:

```json
{
  "storage_market": [
    {
      "document_id": "uuid",
      "corpus_id": "uuid",
      "pulls_30d": 450,
      "current_replicas": 3,
      "word_count": 5000,
      "body_hash": "hex",
      "grant_payout_rate_per_day": 24,
      "best_provider_rate": 2,
      "corpus_doc_count": 150
    }
  ],
  "storage_quality_profile": {
    "your_avg_serve_latency_ms": 120,
    "your_availability_bps": 9950,
    "your_retention_pass_rate_bps": 10000,
    "your_total_serves_30d": 1247
  }
}
```

---

## VIII. Phase Breakdown

### Phase S1: Settlement & Pricing Foundation

**What ships:** Settlement uses atomic RPCs + rotator arm. Provider-set pricing. Fixed `credits_earned` to integer.

**Wire workstream:**
- Migration: `wire_storage_offers` table, `wire_storage_observations` table
- Migration: `matched_rate` column on `wire_document_tokens`
- Migration: `credits_earned_total` column on `wire_nodes` (written by `settle_document_serve_v2`; must exist first)
- Note: `wire_graph_fund` CHECK for `'storage_serve'`, `'hosting_grant'` is ALREADY extended by compute Phase 1 migration `20260414100000_market_prerequisites.sql` (prospective 5-value CHECK). No re-extend needed.
- `settle_document_serve_v2` RPC (atomic RPCs, rotator arm, matched rate) — signature `(p_token_id, p_hosting_node_id, p_serve_latency_ms)` resolves consumer+document+matched_rate from token row (OB-2 fix)
- `POST /api/v1/storage/settle` API route exposes the RPC to provider settlement reporting
- Updated pull routing endpoint with provider selection + rate matching
- Seeds: `economic_parameter` contributions for:
  - `sync_stream_max_bytes` = 100_000_000 (100 MB) — above this, chunked-asset manifest is required (§VI "Chunked Storage for Large Files")

**Node workstream:**
- `market.rs`: `credits_earned: i64`, `effective_per_pull_rate: i64`
- `PricingStrategy` struct, `resolve_competitive_price` function
- Storage offer push to Wire (effective rate from competitive pricing)
- `storage_pricing_update` IPC command

**Frontend workstream:**
- `StoragePricingPanel.tsx`: Fixed/competitive strategy configuration
- `StorageEarningsTracker.tsx`: Updated for market-rate earnings

**Verification:** Two nodes hosting same document. Consumer pulls. Routed to cheapest provider. Settlement at matched rate via rotator arm. Credits flow correctly.

### Phase S2: Hosting Grants & Quality Signals

**What ships:** Anyone can fund hosting grants. Network-observed quality. Hosting contest for new content.

**Wire workstream:**
- `wire_hosting_grants` table
- `process_hosting_grant_payouts` RPC (document rotator + platform rotator)
- `advance_document_rotator` helper function
- Quality observation aggregation (serve latency, availability, retention pass rate)
- Grant CRUD endpoints
- Hosting contest: first-host bonus logic in the host report endpoint
- Heartbeat extension: grant payout rates, quality profile, corpus doc counts

**Node workstream:**
- Updated `MarketOpportunity` with grant and quality fields
- Combined demand signal in `evaluate_opportunities` (pulls + grant equivalent)
- Quality stats pushed with storage offer
- `storage_create_hosting_grant` IPC

**Frontend workstream:**
- `HostingGrantManager.tsx`: Create/view/cancel grants
- `StorageQualityView.tsx`: Network-observed quality metrics
- `StorageOfferView.tsx`: Combined revenue projection (pulls + grants)

**Verification:** Create hosting grant for a corpus with low natural demand. Provider sees improved economics and hosts documents from that corpus. Grant depletes. Quality metrics visible in UI.

### Phase S3: Market Surface & Consumer Experience

**What ships:** Consumer sees pricing preview. Market surface shows per-corpus economics. Min replicas as contribution.

**Wire workstream:**
- `GET /api/v1/storage/market-surface` endpoint
- Pull preview endpoint (shows available providers, rates, quality before committing)
- Min replicas as `economic_parameter` contribution (per-corpus)
- `safe_drop_document` updated to read min_replicas from contribution

**Node workstream:**
- Market surface consumption in daemon (fuller picture for hosting decisions)
- Storage capacity contribution (`schema_type: storage_capacity`)

**Frontend workstream:**
- `StorageMarketSurface.tsx`: Browse corpora, see providers/pricing/grants
- Pull preview UI: "This pull costs N credits. 3 providers available."

**Verification:** Consumer browses market surface. Sees corpus with 3 providers at different rates. Pulls document. Gets preview. Confirms. Routed to cheapest. Credits settle at market rate.

---

## IX. Contribution Types Introduced

| Schema Type | Purpose | Where |
|---|---|---|
| `storage_pricing` | Per-pull rate, competitive strategy, floor/ceiling | Node config store |
| `storage_capacity` | Available GB, hosting preferences | Node config store |
| `hosting_grant` | Corpus-level hosting subsidy with payout schedule | Wire contributions |
| `economic_parameter` | Min replicas (per-corpus), first-host bonus multiplier | Wire contributions |

---

## X. Pillar Conformance

| Pillar | How Respected |
|---|---|
| 1 (Everything is contribution) | Pricing, grants, capacity, min replicas — all contributions. |
| 7 (UFF) | No creator/source-chain split for service payments. Wire 2.5% + Graph Fund 2.5% via rotator arm. Provider 95%. Same as compute. |
| 9 (Integer economics) | All `i64`. Rotator arm for platform levy. Quality metrics in basis points (0-10000). No f64 in financial paths. |
| 12 (Emergent pricing) | Competitive auto-pricing discovers equilibrium. No Wire-set prices. |
| 23 (Preview-then-commit) | Pull preview shows provider options and rates before committing. |
| 35 (Graph Fund) | 2.5% via rotator arm on both per-pull and hosting grant payouts. |
| 37 (Never prescribe) | All parameters contribution-driven. Min replicas, pricing floors/ceilings, bonus multipliers — all supersedable. |
| 42 (Always include frontend) | Every phase has frontend workstream. |

---

## XI. Relationship to Compute Market

The storage market conversion uses the same economic primitives built for the compute market:

| Primitive | Compute | Storage |
|---|---|---|
| Rotator arm (76/2/2) | Per-settlement, per-reservation | Per-pull, per-grant-payout |
| Competitive auto-pricing | Per-model offers | Per-node storage offers |
| Atomic credit RPCs | All settlement | All settlement |
| Network-observed quality | Latency, tokens/sec, flag rate | Serve latency, availability, retention pass |
| Contribution-driven config | All parameters | All parameters |
| Wire platform operator | Estimation risk absorption | N/A (no estimation needed — pull cost is fixed at match time) |

What storage does NOT need from compute:
- No FIFO queues (storage is persistent, not sequential)
- No order book with real-time matching (storage matching is slow)
- No per-model queues (no models in storage)
- No deposit/estimation dance (pull cost is known upfront at matched rate)
- No E2E encryption for document bodies (public content — encryption is compute-only for prompts)

What storage SHARES with compute via the relay network:
- Variable relay count (0-N, consumer's choice from privacy policy)
- Relay chain setup and settlement (same infrastructure)
- Tunnel URL rotation (same mechanism, same privacy benefits)
- Plausible deniability for 0-relay users (same principle)

What storage adds that compute doesn't have:
- Hosting grants (availability subsidies)
- Document rotator (grant payouts cycle through corpus)
- Retention challenges (proof-of-storage)
- Hosting contest (first-to-host bonus)
- Corpus-level economics (grants apply to collections, not individual items)
