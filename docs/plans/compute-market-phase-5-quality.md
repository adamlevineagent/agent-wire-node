# Phase 5: Quality & Challenges

**What ships:** Quality enforcement for the compute market through four mechanisms: proactive quality probes, reactive challenges with economic staking, timing anomaly detection, and DADBEAR quality holds. No dedicated review system (Addendum D reasoning preserved — cheap compromised reviews are the exact attack vector; quality enforcement is integrity infrastructure, not a market function).

**Prerequisites:** Phase 3 (settlement — needed for clawback), observation aggregation function (built in this phase, required for reputation)

---

## I. Overview

Phase 5 delivers ENFORCEMENT, not just dispute infrastructure.

The audit (Theme 5, 8 sub-findings) found the original spec was all reactive — no proactive detection until Phase 8, incompatible challenge infrastructure, missing clawback RPC, unspecified timing anomaly detection, no challenge staking (Sybil attack surface), no self-dealing check, and no observation aggregation. This revision addresses every finding.

Four enforcement mechanisms, each addressing a different attack vector:

1. **Proactive quality probes** — Wire dispatches known-answer test jobs (extends existing honeypot pattern from `wire_challenge_bank`). Catches lazy providers. *(Audit 5e fix: proactive detection pulled to Phase 5 from Phase 8)*
2. **Reactive challenges** — Economic staking with adjudication panel. Catches noticed-bad output. *(Audit 5a/5b/5f fix: new compute-specific system with clawback and staking)*
3. **Timing anomaly detection** — Statistical flagging of impossible response times. Catches speed-based fraud. *(Audit 5h fix: full algorithm specification)*
4. **DADBEAR quality holds** — Provider market participation frozen during disputes. Prevents continued bad service. *(Theme 1 Phase 5 fix: breaker holds bridged from Wire to local node)*

---

## II. Compute Challenge Infrastructure

### Why the existing systems don't work

**Audit finding 5a: challenge infrastructure structurally incompatible.**

The existing systems solve different problems:

- **`wire_challenge_bank`** (migration `20260308100000`): Pre-validated answer keys for entity extraction, dedup detection, reliability rating, scrape adjudication, spot checks, flag adjudication, honeypots, and contribution adjudication. The core assumption is that an `answer_key` exists to grade against. For arbitrary LLM prompts, no answer key exists — the same prompt can produce legitimately different outputs across runs on the same model. Tier 1 answer-key matching is structurally inapplicable.

- **`wire_contribution_adjudication_cases`** (migration `20260314000000`): Contribution quality disputes — flags, velocity markers, takedown batches. The data model is contribution-centric (one open case per contribution, lead/attached flag economics, deposit forfeiture, bounty clawback on contributions). A compute dispute needs job-centric resolution: was this specific inference result adequate for the price paid?

- **`append_adjudication_response`** (migration `20260308600000`): Tristate adjudication (match/noise/garbage) for scrape divergence. Accumulates responses, but responses are structurally `(verdict_enum)` — compute adjudication needs `(verdict, reasoning, comparison_result)` with model re-run capability.

**Conclusion:** Build a compute-specific challenge system. Reuse the *pattern* (cases, panels, staking, resolution RPCs) but not the tables or RPCs.

### Compute Challenge Protocol

#### Filing a Challenge

A challenge is filed by a requester (or any operator who received the output via a chain) against a completed job.

**Required evidence by challenge type:**

| Challenge Type | Evidence Required | Privacy Exposure |
|---|---|---|
| `timing` | Job ID only. Wire already has all timing data (chronicle events, observations). | None — metadata only |
| `quality` | Job ID + opt-in prompt disclosure + opt-in result disclosure + independent re-run result | Full — requester decides |
| `format` | Job ID + result structure description (no content needed if format violation is structural) | Minimal |

**Challenge stake (audit 5f fix — DD-9 pattern):**
- Stake amount = `challenge_stake_multiplier` (economic_parameter contribution) * job `actual_cost`
- Minimum floor: `challenge_stake_floor` (economic_parameter) — prevents zero-cost challenges on tiny jobs
- Stake is debited from challenger at filing time via `debit_operator_atomic`
- Rejected challenge: stake forfeited to challenged provider (credit via `credit_operator_atomic`)
- Upheld challenge: stake refunded + bounty (bounty = `challenge_bounty_rate` economic_parameter as percentage of `actual_cost`)

The economic gate makes Sybil challenge spam unprofitable: even if 1/3 of false challenges are erroneously upheld, the 2/3 forfeiture exceeds the 1/3 bounty gain. The stake multiplier is tuned via supersedable economic_parameter contribution.

#### Privacy vs Evidence Tension — Resolution

**Audit finding 5d: "Must choose." This phase chooses both, differentiated by challenge type.**

The Wire never sees payloads (by design — Wire is pure control plane). But adjudication panels need evidence to evaluate quality disputes. The resolution:

**Timing challenges: no privacy issue.**
Evidence is timing metadata only — chronicle events (dispatch timestamp, result timestamp, latency_ms), Wire observations (tokens_per_sec, output_tokens), and network-aggregated performance baselines. The Wire already has all of this. No prompt or result content involved.

**Quality challenges: requester opt-in disclosure.**
The requester explicitly chooses to reveal the prompt and result for adjudication. This is a privacy tradeoff — the requester decides whether their quality concern is worth the disclosure. The challenge filing flow:
1. Requester selects "quality challenge" on a completed job
2. UI displays prominent warning: "Quality challenges require disclosing the prompt and response to the adjudication panel. This data will be encrypted and accessible only to panel members for the duration of the dispute. If you prefer not to disclose, timing-only challenges are available."
3. Requester confirms and submits prompt + result + independent re-run result
4. Wire stores the evidence encrypted (AES-256-GCM, key derived from panel composition — panel members receive the key via secure channel)
5. On case resolution: evidence is purged. No permanent storage of prompts on Wire.

**Privacy-sensitive requesters: timing-only path.**
Cannot dispute output quality without revealing the output. This is an inherent tradeoff. Timing challenges can still catch many fraud types (model downgrade produces different speed profiles, cached responses have suspiciously consistent timing).

#### Adjudication Panel

Panel composition:
- N randomly-selected operators from the active operator pool (N = `adjudication_panel_size` economic_parameter, minimum 3)
- Exclusion rules: NOT the requester's operator, NOT the provider's operator, NOT any operator with an active quality hold
- Selected by deterministic shuffle seeded from `job_id || case_id` (reproducible, not gameable)

Panel workflow:
1. Each panelist receives the evidence package (for quality challenges: prompt, result, model claimed; for timing challenges: timing data, network baselines)
2. For quality disputes: each panelist independently re-runs the prompt on the same model via their own infrastructure. Compares output quality against the challenged result. Submits verdict + reasoning.
3. For timing disputes: each panelist evaluates whether the timing is within plausible bounds given the model and token counts. No re-run needed.
4. Weighted majority verdict. Quorum: all N panelists must respond within `adjudication_timeout` (economic_parameter). Non-responsive panelists are replaced.

Verdicts:
- `upheld` — challenge is valid, provider delivered substandard service
- `rejected` — challenge is invalid, provider's output was acceptable
- `inconclusive` — panel cannot determine. Stake refunded to both parties, no reputation effect. Job flagged for increased probing of this provider.

Panelist compensation:
- Per-adjudication fee: `adjudication_panelist_fee` (economic_parameter)
- Funded from the challenge stake pool (stake is large enough to cover panel fees + bounty; if insufficient, Wire platform absorbs the difference and the economic_parameter is adjusted upward)
- Panelists who don't respond within timeout: no fee, replacement selected

### Self-Dealing Prevention

**Audit finding 5g: no operator check in matching.**

Three protections:

1. **Match-time gate.** `match_compute_job` must add:
   ```
   -- Self-dealing prevention (audit 5g)
   AND o.operator_id != p_requester_operator_id
   ```
   to the offer selection WHERE clause. Same-operator jobs are structurally blocked at the exchange level.

2. **Observation exclusion.** If a same-operator job somehow executes (legacy data, direct API bypass), observations from that job are excluded from reputation aggregation. Column addition to `wire_compute_observations`:
   ```
   same_operator BOOLEAN NOT NULL DEFAULT false
   ```
   Set during `settle_compute_job` by comparing `v_job.requester_operator_id` against `v_job.provider_operator_id`. Excluded from all aggregate queries.

3. **Minimum distinct requesters.** Provider performance data contributes to public reputation only after serving `min_distinct_requesters` (economic_parameter) distinct requester operators. Below that threshold, reputation shows "insufficient data" — no score, no matching preference. Prevents a provider from bootstrapping reputation via a small number of colluding requesters.

---

## III. Clawback RPC

**Audit finding 5b: completely missing. Full spec.**

```sql
CREATE OR REPLACE FUNCTION clawback_compute_job(
  p_job_id UUID,
  p_verdict_id UUID  -- the resolved challenge case ID
) RETURNS TABLE(
  provider_debited INTEGER,
  challenger_credited INTEGER,
  negative_balance_claim BOOLEAN
)
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_job wire_compute_jobs%ROWTYPE;
  v_case wire_compute_challenge_cases%ROWTYPE;
  v_provider_balance BIGINT;
  v_debit_amount INTEGER;
  v_bounty INTEGER;
  v_challenger_total INTEGER;
  v_negative_claim BOOLEAN := false;
  v_bounty_rate INTEGER;
BEGIN
  -- Fetch the completed job
  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id AND status = 'completed'
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found or not completed: %', p_job_id;
  END IF;

  -- Fetch the resolved challenge case
  SELECT * INTO v_case FROM wire_compute_challenge_cases
    WHERE id = p_verdict_id AND verdict = 'upheld'
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Challenge case not found or not upheld: %', p_verdict_id;
  END IF;

  -- Look up bounty rate from economic_parameter
  SELECT COALESCE(
    (SELECT (c.structured_data->>'value')::INTEGER
     FROM wire_contributions c
     WHERE c.type = 'economic_parameter'
       AND c.structured_data->>'parameter_name' = 'challenge_bounty_rate_pct'
     ORDER BY c.created_at DESC LIMIT 1),
    20  -- fallback: 20% of actual_cost as bounty
  ) INTO v_bounty_rate;

  v_debit_amount := COALESCE(v_job.provider_payout, 0);
  v_bounty := CEIL(COALESCE(v_job.actual_cost, 0)::NUMERIC * v_bounty_rate / 100);

  -- Debit provider for their payout amount
  -- Check provider balance first
  SELECT credit_balance INTO v_provider_balance
    FROM wire_operators WHERE id = v_job.provider_operator_id
    FOR UPDATE;

  IF v_provider_balance >= v_debit_amount THEN
    -- Provider can cover full clawback
    PERFORM debit_operator_atomic(v_job.provider_operator_id, v_debit_amount::BIGINT,
      'compute_clawback', p_verdict_id::text, 'compute_market');
  ELSE
    -- Provider cannot cover full clawback — debit what's available,
    -- create negative balance claim for the remainder.
    -- Provider must clear negative balance before accepting new market jobs.
    IF v_provider_balance > 0 THEN
      PERFORM debit_operator_atomic(v_job.provider_operator_id, v_provider_balance,
        'compute_clawback_partial', p_verdict_id::text, 'compute_market');
    END IF;

    -- Record the remaining debt
    INSERT INTO wire_compute_negative_claims (
      operator_id, job_id, verdict_id, claimed_amount, recovered_amount, created_at
    ) VALUES (
      v_job.provider_operator_id, p_job_id, p_verdict_id,
      v_debit_amount - GREATEST(v_provider_balance, 0), 0, now()
    );
    v_negative_claim := true;
  END IF;

  -- Credit challenger: stake refund + bounty
  -- Stake was already debited at challenge filing. Refund it.
  PERFORM credit_operator_atomic(v_case.challenger_operator_id, v_case.stake_amount::BIGINT,
    'challenge_stake_refund', p_verdict_id::text, 'compute_market');
  -- Pay bounty
  PERFORM credit_operator_atomic(v_case.challenger_operator_id, v_bounty::BIGINT,
    'challenge_bounty', p_verdict_id::text, 'compute_market');

  v_challenger_total := v_case.stake_amount + v_bounty;

  -- Graph Fund treatment: Graph Fund keeps its rotator slot payment.
  -- The levy was on the transaction, not on the quality. Clawback targets
  -- the provider's earnings, not the platform levy.

  -- Mark job as clawed back
  UPDATE wire_compute_jobs SET
    status = 'clawed_back',
    clawback_verdict_id = p_verdict_id
  WHERE id = p_job_id;

  -- Place quality hold on provider's offers
  UPDATE wire_compute_offers SET
    status = 'quality_hold',
    updated_at = now()
  WHERE node_id = v_job.provider_node_id
    AND status = 'active';

  RETURN QUERY SELECT v_debit_amount, v_challenger_total, v_negative_claim;
END;
$$;

GRANT EXECUTE ON FUNCTION clawback_compute_job(UUID, UUID) TO service_role;
```

**New table: `wire_compute_negative_claims`**
For providers who have insufficient balance at clawback time. Provider must clear negative claims before their offers can be re-activated.

```sql
CREATE TABLE wire_compute_negative_claims (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  operator_id     UUID NOT NULL REFERENCES wire_operators(id),
  job_id          UUID NOT NULL REFERENCES wire_compute_jobs(id),
  verdict_id      UUID NOT NULL,
  claimed_amount  INTEGER NOT NULL,
  recovered_amount INTEGER NOT NULL DEFAULT 0,
  cleared_at      TIMESTAMPTZ,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_negative_claims_operator
  ON wire_compute_negative_claims(operator_id) WHERE cleared_at IS NULL;

ALTER TABLE wire_compute_negative_claims ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_negative_claims TO service_role;
```

**Matching gate for negative claims:**
Add to `match_compute_job` WHERE clause:
```
AND NOT EXISTS (
  SELECT 1 FROM wire_compute_negative_claims nc
  WHERE nc.operator_id = o.operator_id AND nc.cleared_at IS NULL
)
```

---

## IV. Proactive Quality Probes

**Audit finding 5e: top critical finding — no proactive detection. The existing honeypot infrastructure provides the pattern.**

### Design: Compute Honeypots

The existing `wire_challenge_bank` has `is_honeypot = true` for content work (migration `20260311500000`). Extend this pattern to compute:

1. Wire creates synthetic known-answer test jobs at random intervals
2. Jobs are dispatched like normal market jobs — provider cannot distinguish from real jobs
3. On completion: Wire compares result against expected output

### Probe Construction

**New source type for `wire_challenge_bank`:** `compute_probe`

Each probe contains:
- `task_payload`: the prompt (carefully constructed to have deterministic-enough outputs)
- `answer_key`: expected semantic content (NOT exact match — LLM output is non-deterministic)
- `model_id`: the model being tested
- Scoring: semantic similarity threshold (stored in answer_key as `min_similarity_score`)

**Probe categories:**
| Category | What it catches | Example |
|---|---|---|
| Factual recall | Cached/random responses | "What is the capital of Burkina Faso?" — must contain "Ouagadougou" |
| Reasoning | Model downgrade (7b vs 70b) | Multi-step logic problem — quality detectably different between model tiers |
| Instruction following | Lazy routing | "Reply in exactly 3 bullet points about X" — structural compliance check |
| Length calibration | Token count fraud | "Write a 500-word essay on X" — output length must be in plausible range |

### Probe Dispatch

- Wire platform dispatches probes as normal market jobs via `match_compute_job`
- The Wire platform operator is the requester (handle: `agentwireplatform`)
- Probe frequency per provider per model: `probe_interval_jobs` (economic_parameter — e.g., 1 in every N jobs is a probe)
- Randomized: actual dispatch uses PRNG seeded from `(node_id, model_id, job_counter)` — deterministic for audit but unpredictable to provider
- Probe cost: Wire platform funds these from platform revenue (not a market charge to providers or requesters)

### Probe Evaluation

On probe completion in `settle_compute_job`:
1. Check if job is a probe (internal flag, NOT visible to provider)
2. If probe: compare result against answer_key using semantic similarity
3. Pass threshold: `probe_pass_threshold` (economic_parameter)
4. Result recorded in `wire_compute_probe_results` table

**Probe failure actions:**
- Single failure: increment provider's `probe_fail_count`. Increase probe frequency for this provider+model to `probe_elevated_frequency` (economic_parameter).
- Cluster of failures (`probe_fail_threshold` in `probe_fail_window` economic_parameters): trigger quality hold on all offers for that model from that provider.

### What probes catch
- Lazy providers (cached responses) — probe responses won't match expected quality for novel prompts
- Model downgrade (claiming 70b but running 7b) — output quality detectably different
- Random response generators — fail semantic similarity check
- Routing fraud (provider claims local GPU but routes to cheaper API) — detectable via timing + quality correlation

### What probes don't catch
- Subtle quality degradation within the same model family (e.g., same model but with aggressive quantization) — this requires steward comparison testing, which becomes a DADBEAR compiler mapping in the collapsed Phase 6+. Acknowledged as a known gap at Phase 5.

### New tables

```sql
CREATE TABLE wire_compute_probe_results (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  job_id          UUID NOT NULL REFERENCES wire_compute_jobs(id),
  challenge_id    UUID NOT NULL REFERENCES wire_challenge_bank(id),
  node_id         UUID NOT NULL REFERENCES wire_nodes(id),
  model_id        TEXT NOT NULL,
  passed          BOOLEAN NOT NULL,
  similarity_score REAL,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_probe_results_node_model
  ON wire_compute_probe_results(node_id, model_id, created_at DESC);

ALTER TABLE wire_compute_probe_results ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_probe_results TO service_role;
```

---

## V. Timing Anomaly Detection

**Audit finding 5h: one sentence in the plan, no algorithm, no trigger, no threshold source, no action. Full spec follows.**

### Detection Algorithm

For each completed job, compute the expected minimum latency:

```
expected_min_latency_ms = (output_tokens / model_max_tps) * 1000
```

Where `model_max_tps` is derived from two sources (in priority order):
1. **Network-observed maximum:** `MAX(tokens_per_sec)` from `wire_compute_observations` for this `(model_id)` across all providers, over the last 7 days. This represents the fastest legitimate hardware running this model.
2. **Model-family baseline:** `model_family_max_tps` (economic_parameter contribution keyed by model family). Used when insufficient observations exist.

If `actual_latency_ms < expected_min_latency_ms * safety_factor`:
- Flag the job as a timing anomaly
- `safety_factor`: `timing_safety_factor` economic_parameter (allows responses that are faster than the fastest observed, up to a bound — accounts for measurement variance and hardware improvements)

### Trigger: Inline with Settlement

Timing anomaly detection runs inside `settle_compute_job` — zero additional cost, no separate sweep needed. After the observation INSERT:

```sql
-- Timing anomaly detection (Phase 5)
DECLARE
  v_max_tps REAL;
  v_expected_min_ms INTEGER;
  v_safety_factor REAL;
BEGIN
  -- Get network-observed max tps for this model
  SELECT COALESCE(
    (SELECT MAX(tokens_per_sec) FROM wire_compute_observations
     WHERE model_id = v_job.model_id
       AND created_at > now() - interval '7 days'
       AND NOT COALESCE(same_operator, false)),
    (SELECT (c.structured_data->>'value')::REAL
     FROM wire_contributions c
     WHERE c.type = 'economic_parameter'
       AND c.structured_data->>'parameter_name' = 'model_family_max_tps_' || split_part(v_job.model_id, '/', 1)
     ORDER BY c.created_at DESC LIMIT 1),
    100.0  -- conservative fallback if no data exists anywhere
  ) INTO v_max_tps;

  -- Get safety factor
  SELECT COALESCE(
    (SELECT (c.structured_data->>'value')::REAL
     FROM wire_contributions c
     WHERE c.type = 'economic_parameter'
       AND c.structured_data->>'parameter_name' = 'timing_safety_factor'
     ORDER BY c.created_at DESC LIMIT 1),
    0.7
  ) INTO v_safety_factor;

  v_expected_min_ms := CEIL((p_completion_tokens::REAL / v_max_tps) * 1000 * v_safety_factor);

  IF p_latency_ms > 0 AND p_latency_ms < v_expected_min_ms THEN
    -- Flag this job
    UPDATE wire_compute_jobs SET timing_anomaly = true WHERE id = p_job_id;

    -- Record anomaly event
    INSERT INTO wire_compute_anomaly_events (
      job_id, node_id, model_id, anomaly_type,
      actual_ms, expected_min_ms, max_tps_used, safety_factor
    ) VALUES (
      p_job_id, v_job.provider_node_id, v_job.model_id, 'timing',
      p_latency_ms, v_expected_min_ms, v_max_tps, v_safety_factor
    );
  END IF;
END;
```

### Column additions

`wire_compute_jobs`:
```sql
ALTER TABLE wire_compute_jobs ADD COLUMN IF NOT EXISTS timing_anomaly BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE wire_compute_jobs ADD COLUMN IF NOT EXISTS clawback_verdict_id UUID;
```

### New table: `wire_compute_anomaly_events`

```sql
CREATE TABLE wire_compute_anomaly_events (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  job_id          UUID NOT NULL REFERENCES wire_compute_jobs(id),
  node_id         UUID NOT NULL REFERENCES wire_nodes(id),
  model_id        TEXT NOT NULL,
  anomaly_type    TEXT NOT NULL CHECK (anomaly_type IN ('timing', 'probe_fail', 'cluster')),
  actual_ms       INTEGER,
  expected_min_ms INTEGER,
  max_tps_used    REAL,
  safety_factor   REAL,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_anomaly_events_node
  ON wire_compute_anomaly_events(node_id, model_id, created_at DESC);

ALTER TABLE wire_compute_anomaly_events ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_anomaly_events TO service_role;
```

### Action Escalation

Timing anomaly alone does NOT trigger clawback — could be measurement noise, network jitter, or genuinely fast hardware.

Escalation ladder:
1. **Single anomaly:** Recorded. Increased probe frequency for that provider+model (`probe_elevated_frequency` economic_parameter).
2. **Cluster:** `timing_anomaly_cluster_threshold` anomalies in `timing_anomaly_cluster_window` jobs (both economic_parameters). Triggers automatic offer suspension for that provider+model.
3. **Offer suspension:** Status set to `'timing_suspended'`. Provider must pass `timing_clearance_probe_count` (economic_parameter) consecutive probes to re-activate.

---

## VI. DADBEAR Quality Holds

**Audit Theme 1 Phase 5 fix: breaker holds exist locally but aren't bridged to Wire.**

The local DADBEAR hold system (SQLite `dadbear_hold_events` + `dadbear_holds_projection`, Rust functions `trip_breaker`/`resume_breaker`/`place_hold`/`clear_hold` in `auto_update_ops.rs`) already provides the hold machinery. This section bridges Wire enforcement decisions to the local node.

### Wire → Node Quality Hold Propagation

**Trigger conditions (any one):**
- Upheld challenge → `clawback_compute_job` sets `status = 'quality_hold'` on `wire_compute_offers`
- Probe failure cluster → proactive quality system sets `status = 'quality_hold'`
- Timing anomaly cluster → timing system sets `status = 'timing_suspended'`

**Heartbeat delivery:**
The heartbeat response already carries offer status. When a node's heartbeat response includes offers with `status IN ('quality_hold', 'timing_suspended')`:
1. Node reads the quality hold status from heartbeat response
2. Node calls `place_hold(conn, bus, "market:compute", "quality_hold", reason)` — uses the existing generic hold function
3. Hold name: `"quality_hold"` (distinct from `"breaker"` and `"frozen"` — DADBEAR holds are open-ended strings)

**Local effects of quality hold:**
- DADBEAR supervisor sees hold on `market:compute` slug
- All market job acceptance is blocked (the dispatch path checks holds before accepting)
- New offer creation blocked (checked at offer submission API)
- Local builds, fleet internal work, and pyramid operations are NOT affected (different slugs)

### Hold Clearing

Sequence:
1. Cooling period: `quality_hold_cooling_period` (economic_parameter) must elapse after hold placement
2. After cooling: Wire dispatches `quality_clearance_probe_count` (economic_parameter) quality probes targeting this provider's held models
3. All probes must pass
4. On all-pass: Wire clears `quality_hold` → sets offer status back to `'active'` → heartbeat delivers clearance
5. Node receives clearance in heartbeat → calls `clear_hold(conn, bus, "market:compute", "quality_hold")`
6. On any probe fail: hold remains, cooling period resets from current time

### Negative Claim Gate

Providers with uncleared negative claims (`wire_compute_negative_claims WHERE cleared_at IS NULL`) cannot have their quality hold cleared. They must clear the financial debt first, then the quality hold cooling period begins.

---

## VII. Reputation System

**Audit finding: reputation is display-only with no matching effect.**

### Reputation Signals

All signals are computed from existing data. No new data collection.

| Signal | Source | Weight |
|---|---|---|
| Challenge upheld rate | `wire_compute_challenge_cases WHERE verdict = 'upheld' / total` | High (direct quality failure) |
| Timing anomaly rate | `wire_compute_anomaly_events.count / total_jobs` | Medium (may be noise) |
| Probe pass rate | `wire_compute_probe_results WHERE passed = true / total` | High (direct quality measurement) |
| Speed percentile | `wire_compute_observations.tokens_per_sec` percentile within model | Low (informational) |
| Total jobs served | `wire_compute_jobs WHERE status = 'completed'` count | Confidence weighting |

### Reputation Score Computation

```sql
CREATE OR REPLACE FUNCTION compute_provider_reputation(
  p_node_id UUID,
  p_model_id TEXT
) RETURNS TABLE(
  reputation_score INTEGER,  -- 0-10000 basis points (10000 = perfect)
  confidence TEXT,           -- 'insufficient' | 'low' | 'medium' | 'high'
  challenge_rate_bps INTEGER,
  anomaly_rate_bps INTEGER,
  probe_pass_rate_bps INTEGER
)
LANGUAGE plpgsql AS $$
DECLARE
  v_total_jobs INTEGER;
  v_distinct_requesters INTEGER;
  v_min_requesters INTEGER;
  v_min_jobs INTEGER;
  v_challenge_rate INTEGER;
  v_anomaly_rate INTEGER;
  v_probe_rate INTEGER;
  v_score INTEGER;
  v_confidence TEXT;
BEGIN
  -- Count completed jobs
  SELECT COUNT(*) INTO v_total_jobs
  FROM wire_compute_jobs
  WHERE provider_node_id = p_node_id AND model_id = p_model_id AND status = 'completed';

  -- Count distinct requesters
  SELECT COUNT(DISTINCT requester_operator_id) INTO v_distinct_requesters
  FROM wire_compute_jobs
  WHERE provider_node_id = p_node_id AND model_id = p_model_id AND status = 'completed';

  -- Get minimum thresholds from economic_parameters
  SELECT COALESCE(
    (SELECT (c.structured_data->>'value')::INTEGER FROM wire_contributions c
     WHERE c.type = 'economic_parameter'
       AND c.structured_data->>'parameter_name' = 'min_distinct_requesters'
     ORDER BY c.created_at DESC LIMIT 1),
    3
  ) INTO v_min_requesters;

  SELECT COALESCE(
    (SELECT (c.structured_data->>'value')::INTEGER FROM wire_contributions c
     WHERE c.type = 'economic_parameter'
       AND c.structured_data->>'parameter_name' = 'reputation_min_jobs'
     ORDER BY c.created_at DESC LIMIT 1),
    10
  ) INTO v_min_jobs;

  -- Insufficient data check
  IF v_total_jobs < v_min_jobs OR v_distinct_requesters < v_min_requesters THEN
    RETURN QUERY SELECT 0, 'insufficient'::TEXT, 0, 0, 0;
    RETURN;
  END IF;

  -- Challenge upheld rate (basis points)
  SELECT COALESCE(
    CEIL((COUNT(*) FILTER (WHERE cc.verdict = 'upheld'))::NUMERIC / NULLIF(v_total_jobs, 0) * 10000),
    0
  )::INTEGER INTO v_challenge_rate
  FROM wire_compute_challenge_cases cc
  JOIN wire_compute_jobs j ON j.id = cc.job_id
  WHERE j.provider_node_id = p_node_id AND j.model_id = p_model_id;

  -- Anomaly rate (basis points)
  SELECT COALESCE(
    CEIL(COUNT(*)::NUMERIC / NULLIF(v_total_jobs, 0) * 10000),
    0
  )::INTEGER INTO v_anomaly_rate
  FROM wire_compute_anomaly_events
  WHERE node_id = p_node_id AND model_id = p_model_id;

  -- Probe pass rate (basis points, inverted — 10000 = all passed)
  SELECT COALESCE(
    CEIL((COUNT(*) FILTER (WHERE passed))::NUMERIC / NULLIF(COUNT(*), 0) * 10000),
    10000  -- no probes yet = no failures
  )::INTEGER INTO v_probe_rate
  FROM wire_compute_probe_results
  WHERE node_id = p_node_id AND model_id = p_model_id;

  -- Composite score: start at 10000, deduct for failures
  -- Challenge rate is weighted 3x (direct quality failure is worst signal)
  -- Anomaly rate is weighted 1x (may be noise)
  -- Probe failures weighted 2x (proactive detection is reliable)
  v_score := GREATEST(0,
    10000
    - (v_challenge_rate * 3)
    - (v_anomaly_rate * 1)
    - ((10000 - v_probe_rate) * 2)
  );

  -- Confidence tiers
  IF v_total_jobs >= 100 AND v_distinct_requesters >= 10 THEN
    v_confidence := 'high';
  ELSIF v_total_jobs >= 30 AND v_distinct_requesters >= 5 THEN
    v_confidence := 'medium';
  ELSE
    v_confidence := 'low';
  END IF;

  RETURN QUERY SELECT v_score, v_confidence, v_challenge_rate, v_anomaly_rate, v_probe_rate;
END;
$$;

GRANT EXECUTE ON FUNCTION compute_provider_reputation(UUID, TEXT) TO service_role;
```

### Reputation Effect on Matching

**Dual-gate continuous enforcement (audit correction — Pillar 21 pattern):**

Reputation is checked at match time, not just at offer creation. A provider can pass creation checks and later degrade below threshold.

Add to `match_compute_job` WHERE clause:
```sql
-- Reputation gate (Phase 5)
AND (
  -- Allow providers with insufficient data (new entrants)
  -- BUT require minimum reputation for established providers
  (SELECT confidence FROM compute_provider_reputation(o.node_id, o.model_id)) = 'insufficient'
  OR (SELECT reputation_score FROM compute_provider_reputation(o.node_id, o.model_id))
     >= COALESCE(
       (SELECT (c.structured_data->>'value')::INTEGER FROM wire_contributions c
        WHERE c.type = 'economic_parameter'
          AND c.structured_data->>'parameter_name' = 'min_reputation_score'
        ORDER BY c.created_at DESC LIMIT 1),
       5000  -- fallback: 50% minimum
     )
)
```

**Below threshold behavior:**
- Offers are NOT deleted — provider can recover
- Status set to `'reputation_suspended'`
- Provider receives notification via heartbeat
- Recovery: serve enough successful jobs (via probes during hold clearance) to rebuild reputation above threshold

**Reputation-weighted matching:**
When multiple offers match (same model, budget within range), prefer higher-reputation providers. Add to the ORDER BY in `match_compute_job`:
```sql
ORDER BY
  CASE p_latency_preference
    WHEN 'immediate' THEN q.total_depth
    WHEN 'best_price' THEN -q.total_depth
    ELSE q.total_depth
  END,
  -- Reputation tiebreak: prefer higher reputation
  (SELECT COALESCE(reputation_score, 5000) FROM compute_provider_reputation(o.node_id, o.model_id)) DESC
```

---

## VIII. Observation Aggregation

**Audit finding 5c: `wire_compute_offers.observed_*` columns exist but nothing populates them.**

### Aggregation Function

```sql
CREATE OR REPLACE FUNCTION aggregate_compute_observations()
RETURNS INTEGER  -- number of offers updated
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_updated INTEGER := 0;
  v_offer RECORD;
BEGIN
  FOR v_offer IN
    SELECT DISTINCT node_id, model_id FROM wire_compute_offers WHERE status = 'active'
  LOOP
    UPDATE wire_compute_offers SET
      observed_median_tps = (
        SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY tokens_per_sec)
        FROM wire_compute_observations
        WHERE node_id = v_offer.node_id AND model_id = v_offer.model_id
          AND created_at > now() - interval '24 hours'
          AND NOT COALESCE(same_operator, false)
      ),
      observed_p95_latency_ms = (
        SELECT percentile_cont(0.95) WITHIN GROUP (ORDER BY latency_ms)
        FROM wire_compute_observations
        WHERE node_id = v_offer.node_id AND model_id = v_offer.model_id
          AND created_at > now() - interval '24 hours'
          AND NOT COALESCE(same_operator, false)
      ),
      observed_job_count = (
        SELECT COUNT(*)
        FROM wire_compute_observations
        WHERE node_id = v_offer.node_id AND model_id = v_offer.model_id
          AND NOT COALESCE(same_operator, false)
      ),
      updated_at = now()
    WHERE node_id = v_offer.node_id AND model_id = v_offer.model_id;

    v_updated := v_updated + 1;
  END LOOP;

  RETURN v_updated;
END;
$$;

GRANT EXECUTE ON FUNCTION aggregate_compute_observations() TO service_role;
```

### Scheduling

Two trigger points:
1. **pg_cron:** Run every 5 minutes (`SELECT cron.schedule('aggregate_observations', '*/5 * * * *', 'SELECT aggregate_compute_observations()')`)
2. **Settlement-inline:** Every Nth settlement (N = `observation_aggregation_interval` economic_parameter), call `aggregate_compute_observations()` at the end of `settle_compute_job`. Keeps data fresh during active trading.

### Column addition for self-dealing exclusion

```sql
ALTER TABLE wire_compute_observations
  ADD COLUMN IF NOT EXISTS same_operator BOOLEAN NOT NULL DEFAULT false;
```

Set during observation INSERT in `settle_compute_job`:
```sql
INSERT INTO wire_compute_observations (
  job_id, node_id, model_id, input_tokens, output_tokens, latency_ms, tokens_per_sec, same_operator
) VALUES (
  p_job_id, v_job.provider_node_id, v_job.model_id, p_prompt_tokens, p_completion_tokens, p_latency_ms,
  CASE WHEN p_latency_ms > 0 THEN p_completion_tokens::REAL / (p_latency_ms::REAL / 1000) ELSE 0 END,
  v_job.requester_operator_id = v_job.provider_operator_id
);
```

---

## IX. Compute Challenge Tables

The challenge case and panelist response system, specific to compute disputes.

```sql
-- ═══════════════════════════════════════════════════════════════════════════
-- Compute Challenge Cases — separate from contribution adjudication
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE wire_compute_challenge_cases (
  id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  job_id                UUID NOT NULL REFERENCES wire_compute_jobs(id),
  challenger_operator_id UUID NOT NULL REFERENCES wire_operators(id),
  challenge_type        TEXT NOT NULL CHECK (challenge_type IN ('timing', 'quality', 'format')),
  -- Evidence (encrypted for quality challenges, metadata-only for timing)
  evidence              JSONB NOT NULL,
  evidence_encrypted    BOOLEAN NOT NULL DEFAULT false,
  -- Economics
  stake_amount          INTEGER NOT NULL,
  -- Resolution
  verdict               TEXT CHECK (verdict IN ('upheld', 'rejected', 'inconclusive')),
  panel_size            INTEGER NOT NULL,
  responses_received    INTEGER NOT NULL DEFAULT 0,
  -- Lifecycle
  status                TEXT NOT NULL DEFAULT 'open'
                        CHECK (status IN ('open', 'paneling', 'resolved')),
  created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
  resolved_at           TIMESTAMPTZ,
  -- One open case per job
  UNIQUE(job_id) -- only one challenge per job (can re-challenge after resolution with new case)
);

CREATE INDEX idx_compute_challenges_provider
  ON wire_compute_challenge_cases(status)
  WHERE status = 'open';

ALTER TABLE wire_compute_challenge_cases ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_challenge_cases TO service_role;

-- ═══════════════════════════════════════════════════════════════════════════
-- Panelist Responses
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE wire_compute_challenge_responses (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  case_id         UUID NOT NULL REFERENCES wire_compute_challenge_cases(id),
  panelist_operator_id UUID NOT NULL REFERENCES wire_operators(id),
  verdict         TEXT NOT NULL CHECK (verdict IN ('upheld', 'rejected', 'inconclusive')),
  reasoning       TEXT,
  comparison_hash TEXT,  -- hash of re-run result (proves panelist actually re-ran)
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(case_id, panelist_operator_id)  -- one response per panelist per case
);

ALTER TABLE wire_compute_challenge_responses ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_challenge_responses TO service_role;
```

### Challenge Filing RPC

```sql
CREATE OR REPLACE FUNCTION file_compute_challenge(
  p_job_id UUID,
  p_challenger_operator_id UUID,
  p_challenge_type TEXT,
  p_evidence JSONB
) RETURNS TABLE(case_id UUID, stake_charged INTEGER, panel_size INTEGER)
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_job wire_compute_jobs%ROWTYPE;
  v_stake_multiplier INTEGER;
  v_stake_floor INTEGER;
  v_stake INTEGER;
  v_panel_size INTEGER;
  v_case_id UUID;
BEGIN
  -- Fetch completed job
  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id AND status = 'completed';
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found or not completed';
  END IF;

  -- Verify challenger is the requester (or chain participant — future extension)
  IF v_job.requester_operator_id != p_challenger_operator_id THEN
    RAISE EXCEPTION 'Only the requester can challenge a job';
  END IF;

  -- Verify no existing open case
  IF EXISTS (SELECT 1 FROM wire_compute_challenge_cases WHERE job_id = p_job_id AND status != 'resolved') THEN
    RAISE EXCEPTION 'Active challenge already exists for this job';
  END IF;

  -- Quality challenges require prompt disclosure evidence
  IF p_challenge_type = 'quality' AND (
    p_evidence->>'prompt_hash' IS NULL OR
    p_evidence->>'result_sample' IS NULL OR
    p_evidence->>'rerun_result_sample' IS NULL
  ) THEN
    RAISE EXCEPTION 'Quality challenges require prompt_hash, result_sample, and rerun_result_sample in evidence';
  END IF;

  -- Calculate stake (DD-9 pattern)
  SELECT COALESCE(
    (SELECT (c.structured_data->>'value')::INTEGER FROM wire_contributions c
     WHERE c.type = 'economic_parameter'
       AND c.structured_data->>'parameter_name' = 'challenge_stake_multiplier_pct'
     ORDER BY c.created_at DESC LIMIT 1),
    50  -- default: 50% of actual_cost
  ) INTO v_stake_multiplier;

  SELECT COALESCE(
    (SELECT (c.structured_data->>'value')::INTEGER FROM wire_contributions c
     WHERE c.type = 'economic_parameter'
       AND c.structured_data->>'parameter_name' = 'challenge_stake_floor'
     ORDER BY c.created_at DESC LIMIT 1),
    5  -- minimum 5 credits
  ) INTO v_stake_floor;

  v_stake := GREATEST(CEIL(COALESCE(v_job.actual_cost, 0)::NUMERIC * v_stake_multiplier / 100), v_stake_floor);

  -- Get panel size
  SELECT COALESCE(
    (SELECT (c.structured_data->>'value')::INTEGER FROM wire_contributions c
     WHERE c.type = 'economic_parameter'
       AND c.structured_data->>'parameter_name' = 'adjudication_panel_size'
     ORDER BY c.created_at DESC LIMIT 1),
    3
  ) INTO v_panel_size;

  -- Debit stake from challenger
  PERFORM debit_operator_atomic(p_challenger_operator_id, v_stake::BIGINT,
    'challenge_stake', p_job_id::text, 'compute_market');

  -- Create case
  INSERT INTO wire_compute_challenge_cases (
    job_id, challenger_operator_id, challenge_type, evidence,
    evidence_encrypted, stake_amount, panel_size
  ) VALUES (
    p_job_id, p_challenger_operator_id, p_challenge_type, p_evidence,
    p_challenge_type = 'quality', v_stake, v_panel_size
  ) RETURNING id INTO v_case_id;

  RETURN QUERY SELECT v_case_id, v_stake, v_panel_size;
END;
$$;

GRANT EXECUTE ON FUNCTION file_compute_challenge(UUID, UUID, TEXT, JSONB) TO service_role;
```

### Challenge Resolution RPC

```sql
CREATE OR REPLACE FUNCTION resolve_compute_challenge(
  p_case_id UUID
) RETURNS TABLE(verdict TEXT, actions_taken TEXT[])
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_case wire_compute_challenge_cases%ROWTYPE;
  v_upheld_count INTEGER;
  v_rejected_count INTEGER;
  v_inconclusive_count INTEGER;
  v_total INTEGER;
  v_verdict TEXT;
  v_actions TEXT[] := '{}';
BEGIN
  SELECT * INTO v_case FROM wire_compute_challenge_cases
    WHERE id = p_case_id AND status = 'paneling'
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Case not found or not in paneling status';
  END IF;

  -- Count verdicts
  SELECT
    COUNT(*) FILTER (WHERE verdict = 'upheld'),
    COUNT(*) FILTER (WHERE verdict = 'rejected'),
    COUNT(*) FILTER (WHERE verdict = 'inconclusive'),
    COUNT(*)
  INTO v_upheld_count, v_rejected_count, v_inconclusive_count, v_total
  FROM wire_compute_challenge_responses
  WHERE case_id = p_case_id;

  -- Need quorum
  IF v_total < v_case.panel_size THEN
    RAISE EXCEPTION 'Quorum not met: % of % responses', v_total, v_case.panel_size;
  END IF;

  -- Weighted majority
  IF v_upheld_count > v_total / 2 THEN
    v_verdict := 'upheld';
  ELSIF v_rejected_count > v_total / 2 THEN
    v_verdict := 'rejected';
  ELSE
    v_verdict := 'inconclusive';
  END IF;

  -- Apply verdict
  IF v_verdict = 'upheld' THEN
    -- Clawback + quality hold
    PERFORM clawback_compute_job(v_case.job_id, p_case_id);
    v_actions := v_actions || 'clawback_executed';
    v_actions := v_actions || 'quality_hold_placed';

  ELSIF v_verdict = 'rejected' THEN
    -- Forfeit stake to provider
    DECLARE
      v_provider_op UUID;
    BEGIN
      SELECT provider_operator_id INTO v_provider_op
        FROM wire_compute_jobs WHERE id = v_case.job_id;
      PERFORM credit_operator_atomic(v_provider_op, v_case.stake_amount::BIGINT,
        'challenge_stake_forfeit', p_case_id::text, 'compute_market');
    END;
    v_actions := v_actions || 'stake_forfeited_to_provider';

  ELSE -- inconclusive
    -- Refund stake to challenger, no reputation effect
    PERFORM credit_operator_atomic(v_case.challenger_operator_id, v_case.stake_amount::BIGINT,
      'challenge_stake_refund_inconclusive', p_case_id::text, 'compute_market');
    v_actions := v_actions || 'stake_refunded';
  END IF;

  -- Pay panelists
  DECLARE
    v_panelist_fee INTEGER;
    v_resp RECORD;
  BEGIN
    SELECT COALESCE(
      (SELECT (c.structured_data->>'value')::INTEGER FROM wire_contributions c
       WHERE c.type = 'economic_parameter'
         AND c.structured_data->>'parameter_name' = 'adjudication_panelist_fee'
       ORDER BY c.created_at DESC LIMIT 1),
      2  -- default: 2 credits per panelist
    ) INTO v_panelist_fee;

    FOR v_resp IN SELECT panelist_operator_id FROM wire_compute_challenge_responses WHERE case_id = p_case_id
    LOOP
      PERFORM credit_operator_atomic(v_resp.panelist_operator_id, v_panelist_fee::BIGINT,
        'adjudication_panelist_fee', p_case_id::text, 'compute_market');
    END LOOP;
    v_actions := v_actions || ('panelists_paid_' || v_total);
  END;

  -- Resolve case
  UPDATE wire_compute_challenge_cases SET
    verdict = v_verdict, status = 'resolved', resolved_at = now(),
    responses_received = v_total
  WHERE id = p_case_id;

  -- Purge encrypted evidence if quality challenge (privacy commitment)
  IF v_case.evidence_encrypted THEN
    UPDATE wire_compute_challenge_cases SET evidence = '{}'::jsonb
      WHERE id = p_case_id;
    v_actions := v_actions || 'evidence_purged';
  END IF;

  RETURN QUERY SELECT v_verdict, v_actions;
END;
$$;

GRANT EXECUTE ON FUNCTION resolve_compute_challenge(UUID) TO service_role;
```

---

## X. Frontend Workstream

### Challenge Activity Panel

Located on the Market tab, under a "Quality" sub-section:

- **Challenges Filed by This Node:** Table of outbound challenges — job ID, challenge type, stake, verdict, bounty earned/lost
- **Challenges Against This Node:** Table of inbound challenges — job ID, challenge type, provider model, verdict, clawback amount
- **Active Cases:** In-progress challenges awaiting panel resolution
- **Economics Summary:** Total stake wagered, total bounties earned, total clawbacks suffered, net quality economics

### Provider Reputation Display

On the market surface (offer list), each provider shows:
- **Reputation score** (0-100%, derived from basis points) with confidence indicator (low/medium/high)
- **Challenge rate** (upheld challenges per 1000 jobs)
- **Probe pass rate** (percentage)
- **"Insufficient data"** label for new providers below threshold
- Color coding: green (> 80%), yellow (50-80%), red (< 50%), grey (insufficient)

### Quality Hold Status

When this node is under quality hold — prominent banner at top of Market tab:
- Hold type and reason (challenge upheld, probe failure cluster, timing anomaly cluster)
- When hold was placed
- Cooling period remaining
- Clearance probe status (N of M passed)
- Action button: "View details" → links to the challenge/anomaly that triggered the hold

### Timing Anomaly Indicator

On per-job views in the Market tab:
- Jobs with `timing_anomaly = true` show a warning icon
- Tooltip: "This job's response time was faster than physically plausible for the claimed hardware. This does not necessarily indicate fraud but triggers increased monitoring."

---

## XI. Chronicle Events

All quality enforcement actions are recorded as chronicle events for auditability.

| Event | Payload |
|---|---|
| `compute_challenge_filed` | `{ case_id, job_id, challenge_type, stake_amount }` |
| `compute_challenge_resolved` | `{ case_id, verdict, actions_taken[], clawback_amount }` |
| `compute_probe_dispatched` | `{ probe_job_id, target_node_id, target_model_id, challenge_id }` |
| `compute_probe_completed` | `{ probe_job_id, passed, similarity_score }` |
| `compute_timing_anomaly` | `{ job_id, node_id, model_id, actual_ms, expected_min_ms }` |
| `compute_quality_hold_placed` | `{ node_id, hold_type, trigger_case_id }` |
| `compute_quality_hold_cleared` | `{ node_id, hold_type, probes_passed }` |
| `compute_reputation_suspended` | `{ node_id, model_id, reputation_score, threshold }` |
| `compute_clawback_executed` | `{ job_id, verdict_id, provider_debited, challenger_credited }` |

---

## XII. Verification Criteria

End-to-end paths that must work:

1. **Challenge → upheld → clawback + hold:** Requester files quality challenge with opt-in disclosure. Panel resolves upheld (majority). Clawback debits provider, credits challenger (stake refund + bounty). Quality hold placed on provider offers. DADBEAR breaker hold placed on local node via heartbeat. Provider cannot accept new market jobs.

2. **Challenge → rejected → stake forfeit:** Requester files timing challenge. Panel resolves rejected. Challenger's stake forfeited to provider. No reputation effect on provider. Challenger's reputation unaffected (filing a legitimate challenge that's rejected is not penalized beyond stake loss).

3. **Probe dispatched → pass:** Wire dispatches probe to provider. Provider responds. Result compared against answer key. Similarity above threshold. Probe pass recorded. No action.

4. **Probe dispatched → fail → escalation:** Probe fails. Probe frequency increased. Second probe fails. Third probe fails (cluster threshold met). Quality hold placed on provider's offers for that model.

5. **Timing anomaly → escalation:** Settlement detects impossible latency. Anomaly event recorded. Probe frequency increased. More anomalies detected in window. Cluster threshold met. Offer suspended pending clearance probes.

6. **Quality hold → cooling → clearance:** Provider under quality hold. Cooling period elapses. Wire dispatches clearance probes. All pass. Hold cleared on Wire. Heartbeat delivers clearance. DADBEAR hold cleared locally. Provider resumes market participation.

7. **Self-dealing blocked:** Requester operator matches provider operator. `match_compute_job` WHERE clause rejects the match. No job created.

8. **Reputation below threshold → suspension:** Provider's reputation score drops below `min_reputation_score` economic_parameter. Next match attempt against this provider fails (reputation gate in WHERE clause). Provider's offers set to `'reputation_suspended'`. Provider notified via heartbeat.

9. **Negative balance claim → gate:** Provider clawed back but insufficient balance. Negative claim recorded. Provider cannot match new jobs (negative claim gate in WHERE clause). Provider cannot clear quality hold until negative claim is cleared. Provider deposits credits → negative claim cleared → quality hold cooling begins.

---

## XIII. Handoff to Phase 6

Phase 5 leaves working:
- Quality enforcement with 4 mechanisms (probes, challenges, timing, holds)
- Reputation system affecting matching (not just display)
- Observation aggregation populating `observed_*` columns
- Clawback financial infrastructure
- Self-dealing prevention at match time
- DADBEAR quality hold bridge (Wire → local node)

Phase 6 adds: DADBEAR compiler mappings that use quality signals for autonomous provider selection and pricing decisions. The quality data produced by Phase 5 becomes input to the `market:compute` observation sources that Phase 6's compiler processes.

---

## XIV. Audit Corrections Applied

| Audit Finding | Resolution | Section |
|---|---|---|
| **5a: Challenge infrastructure incompatible** | New compute-specific challenge system (`wire_compute_challenge_cases`, `wire_compute_challenge_responses`). Existing `wire_challenge_bank` reused only for probe storage (extended with `compute_probe` source type). Adjudication via new RPCs. | II, IX |
| **5b: No clawback RPC** | Full `clawback_compute_job` RPC with negative balance handling, quality hold trigger, and Graph Fund treatment. | III |
| **5c: No observation aggregation** | `aggregate_compute_observations()` function with pg_cron scheduling and settlement-inline trigger. Self-dealing exclusion via `same_operator` column. | VIII |
| **5d: Privacy vs evidence tension** | Resolved: timing challenges use metadata only (no privacy issue). Quality challenges require opt-in disclosure with encryption + post-resolution purge. Requester chooses. | II (Privacy vs Evidence) |
| **5e: No proactive detection until Phase 8** | Compute honeypots extending existing `wire_challenge_bank` pattern. Wire dispatches known-answer test jobs. Failure triggers escalation. | IV |
| **5f: No challenge staking** | Stake proportional to `actual_cost` with economic_parameter multiplier and floor. DD-9 pattern applied. Rejected challenges forfeit stake to provider. | II (Filing), IX |
| **5g: No self-dealing check** | Three protections: match-time operator gate, observation exclusion flag, minimum distinct requesters for reputation. | II (Self-Dealing Prevention) |
| **5h: Timing anomaly entirely unspecified** | Full algorithm: `expected_min_latency` from network max TPS, safety factor from economic_parameter, inline with settlement, escalation ladder (single → cluster → suspension). | V |
| **Theme 1 Phase 5: DADBEAR breaker holds not bridged** | Wire quality hold → heartbeat delivery → local `place_hold("market:compute", "quality_hold")`. Hold clearing via cooling + probes. | VI |

---

## XV. Economic Parameters Introduced

All configurable via supersedable `economic_parameter` contributions (Pillar 37 compliance).

| Parameter | Purpose | Seed Value Guidance |
|---|---|---|
| `challenge_stake_multiplier_pct` | Stake as % of job actual_cost | Ask Adam |
| `challenge_stake_floor` | Minimum stake in credits | Ask Adam |
| `challenge_bounty_rate_pct` | Bounty as % of actual_cost on upheld challenge | Ask Adam |
| `adjudication_panel_size` | Number of panelists per challenge | Ask Adam |
| `adjudication_panelist_fee` | Credits per panelist per adjudication | Ask Adam |
| `adjudication_timeout` | Time panelists have to respond | Ask Adam |
| `probe_interval_jobs` | 1-in-N jobs is a probe | Ask Adam |
| `probe_elevated_frequency` | Probe frequency when provider under elevated monitoring | Ask Adam |
| `probe_pass_threshold` | Semantic similarity threshold for probe pass | Ask Adam |
| `probe_fail_threshold` | Number of failures triggering quality hold | Ask Adam |
| `probe_fail_window` | Job window for failure clustering | Ask Adam |
| `timing_safety_factor` | Multiplier for minimum expected latency | Ask Adam |
| `model_family_max_tps_{family}` | Max TPS baseline per model family | Ask Adam |
| `timing_anomaly_cluster_threshold` | Anomalies needed for suspension | Ask Adam |
| `timing_anomaly_cluster_window` | Job window for anomaly clustering | Ask Adam |
| `timing_clearance_probe_count` | Probes needed to clear timing suspension | Ask Adam |
| `quality_hold_cooling_period` | Time before clearance probes begin | Ask Adam |
| `quality_clearance_probe_count` | Probes needed to clear quality hold | Ask Adam |
| `min_distinct_requesters` | Minimum unique requesters for reputation eligibility | Ask Adam |
| `reputation_min_jobs` | Minimum job count for reputation eligibility | Ask Adam |
| `min_reputation_score` | Minimum reputation to remain matchable | Ask Adam |
| `observation_aggregation_interval` | Every Nth settlement triggers inline aggregation | Ask Adam |

All seed values intentionally left as "Ask Adam" — no Pillar 37 violations.

---

## XVI. Migration Summary

New tables (6):
- `wire_compute_challenge_cases`
- `wire_compute_challenge_responses`
- `wire_compute_probe_results`
- `wire_compute_anomaly_events`
- `wire_compute_negative_claims`
- `wire_challenge_bank` expansion: new `compute_probe` source type

Column additions (2 tables):
- `wire_compute_jobs`: `timing_anomaly BOOLEAN`, `clawback_verdict_id UUID`
- `wire_compute_observations`: `same_operator BOOLEAN`

New RPCs (6):
- `file_compute_challenge(UUID, UUID, TEXT, JSONB)`
- `resolve_compute_challenge(UUID)`
- `clawback_compute_job(UUID, UUID)`
- `compute_provider_reputation(UUID, TEXT)`
- `aggregate_compute_observations()`
- Timing anomaly detection (inline in `settle_compute_job` — modification, not new RPC)

Offer status additions:
- `wire_compute_offers.status` CHECK expansion: add `'quality_hold'`, `'timing_suspended'`, `'reputation_suspended'`
