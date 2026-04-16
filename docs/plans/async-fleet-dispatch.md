# Async Fleet Dispatch

**Problem:** Cloudflare tunnel origins time out at ~100s (HTTP 524). The current fleet dispatch path holds the HTTP response open until GPU inference completes. Any job exceeding ~100s is silently killed from the dispatcher's perspective, even though the peer finishes the work and the result exists nowhere.

**Fix:** ACK + callback + outbox + contribution-controlled operational policy. The peer acknowledges the job immediately, runs inference, persists the result, then delivers it to the dispatcher via callback. No HTTP connection lives longer than a few seconds. No result is ever lost to a transient failure. Every operational knob is tunable without a rebuild.

This is the foundational async compute pattern for the Wire. Private fleet gets it first. The compute market uses the same protocol shape — the only difference is who sits at the callback URL (dispatcher's tunnel for fleet, Wire's proxy for standard market, relay chain for relay market).

---

## Protocol

```
Dispatcher                            Peer (GPU)
    │                                    │
    ├─ POST /v1/compute/fleet-dispatch ─►│  (includes job_id + callback_url)
    │                                    │  verify identity, resolve model,
    │                                    │  admission + idempotent insert + heartbeat start
    │◄─ 202 { job_id }                  │  (immediate, within seconds)
    │                                    │
    │                                    │  spawn worker (bumps expires_at every tick)
    │                                    │  enqueue → GPU → inference...
    │                                    │  ...any duration...
    │                                    │  write result, status='ready'
    │                                    │
    │◄─ POST {callback_url} ────────────│  (result + job_id)
    │    peek, verify peer_id,           │
    │    pop-and-send on match; 200 OK   │  mark status='delivered'
    │    resolve pending future          │
    ▼                                    ▼
```

Both POSTs travel through Cloudflare tunnels. Both complete in seconds. The tunnel timeout is irrelevant to the long-running work.

---

## Core Primitives (Systemic Scaffolding)

Three newtypes and one bundle that remove entire classes of bug from this feature and future ones.

### `FleetIdentity` and `verify_fleet_identity`

The fleet JWT plumbing — verifier, aud, op, nid, emptiness checks — lives in one place. Handlers do not spell out individual claim checks; they call the verifier and get a typed identity back or reject.

```rust
pub struct FleetIdentity {
    pub nid: String,   // dispatcher node_id, guaranteed non-empty
    pub op: String,    // operator_id, guaranteed == self_operator_id
}

/// Decodes, verifies, and normalizes a fleet JWT into an identity.
/// Single source of truth for fleet authentication.
pub fn verify_fleet_identity(
    bearer_token: &str,
    public_key: &str,
    self_operator_id: &str,
) -> Result<FleetIdentity, FleetAuthError> {
    // 1. jsonwebtoken::decode with set_audience(&["fleet"]) and validate_exp=true.
    //    Enforces aud and exp at decode time.
    // 2. Require claims.op == self_operator_id (non-empty both sides).
    // 3. Require claims.nid to be Some(s) with !s.is_empty().
    // 4. Return FleetIdentity { nid, op }.
}

pub enum FleetAuthError {
    InvalidToken,         // decode failure (sig, aud, exp)
    OperatorMismatch,     // claims.op != self_operator_id
    MissingNid,           // claims.nid absent or empty
    MissingOperator,      // self_operator_id empty
}
```

All three fleet-authenticated handlers (`handle_fleet_dispatch`, `handle_fleet_result`, `handle_fleet_announce`) open with a single call to `verify_fleet_identity`. The "did you check claims.op?" / "did you check nid emptiness?" / "did you accidentally re-check aud?" audit findings become unrepresentable.

No `#[serde(alias = ...)]` on claim names. The Wire JWT contract uses exactly `op` and `nid`; a test in the verifier's unit suite asserts both. Aliases would risk cross-contamination with adjacent claim shapes (`DocumentClaims` uses `sub` for a different purpose).

### `TunnelUrl`

A validated, normalized tunnel URL. Freeform strings don't leak past the roster ingress point.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelUrl(url::Url);

impl TunnelUrl {
    /// Parse, require scheme + host, strip trailing slash, reject empty path-only.
    pub fn parse(s: &str) -> Result<Self, TunnelUrlError> { ... }

    /// Authority-only view, for callback validation.
    pub fn authority(&self) -> (&str, Option<&str>, Option<u16>) { ... }

    /// Raw path from the underlying URL — exposed for validators that
    /// need to match path prefixes (e.g. validate_callback_url).
    pub fn path(&self) -> &str { self.0.path() }

    /// String view for logging, heartbeat body construction, and any
    /// interop with code that still takes `&str`. Replaces all prior
    /// `tunnel_url.clone()` / `unwrap_or_default()` patterns.
    pub fn as_str(&self) -> &str { self.0.as_str() }

    /// Construct an endpoint URL (scheme+authority + this absolute path).
    /// Replaces the base's path — tunnels are assumed root-served.
    /// Returns an owned String for the HTTP client.
    pub fn endpoint(&self, absolute_path: &str) -> String {
        debug_assert!(absolute_path.starts_with('/'));
        format!("{}://{}{}{}",
            self.0.scheme(),
            self.0.host_str().unwrap(),
            self.0.port().map(|p| format!(":{}", p)).unwrap_or_default(),
            absolute_path)
    }
}

// Wire compatibility — TunnelUrl serializes as a plain string, deserializes
// via TunnelUrl::parse. Existing saved state files, heartbeat responses,
// and fleet announcements continue to interoperate without migration.
impl Serialize for TunnelUrl {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.as_str().serialize(s)
    }
}
impl<'de> Deserialize<'de> for TunnelUrl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        TunnelUrl::parse(&s).map_err(serde::de::Error::custom)
    }
}
```

`TunnelUrl` has no `Default` impl — a default tunnel URL is meaningless. Call sites that currently do `ts.tunnel_url.clone().unwrap_or_default()` on `Option<String>` become `ts.tunnel_url.as_ref().map(|t| t.as_str()).unwrap_or("")`. There is no transparent `FromStr`; construction is always fallible via `parse`.

Rule: the only way to get a `TunnelUrl` is through `TunnelUrl::parse`. Every place that stores a tunnel URL — `FleetPeer.tunnel_url`, `TunnelState.tunnel_url`, serialized roster blobs — holds a `TunnelUrl`, not a `String`. Construction does the normalization once. The "did every ingress site normalize?" audit finding disappears.

Tunnels are assumed root-served. `TunnelUrl::endpoint` replaces the base path with the given absolute path, sidestepping the `url::Url::join` subtleties (absolute-path-join replaces the path; relative-path-join can produce `//` artifacts).

### `FleetDispatchContext`

A single Arc bundle for the three shared runtime handles this feature introduces. Wired through `LlmConfig` and `ServerState` as one field, not three.

```rust
pub struct FleetDispatchContext {
    /// BORROWED HANDLE to the node's tunnel state — mutated elsewhere
    /// (heartbeat loop, tunnel reconnect). This bundle reads it live at
    /// dispatch time to construct callback_url; it does NOT own the state.
    /// Replaces the rejected self_tunnel_url denormalization.
    pub tunnel_state: Arc<tokio::sync::RwLock<TunnelState>>,

    /// OWNED by this feature. Dispatcher's in-flight pending jobs awaiting
    /// callback resolution. Nobody else writes to this.
    pub pending: Arc<PendingFleetJobs>,

    /// OWNED by this feature. Operational policy
    /// (contribution-controlled, hot-reloaded via the ConfigSynced listener
    /// registered in Init Ordering).
    pub policy: Arc<tokio::sync::RwLock<FleetDeliveryPolicy>>,
}
```

**Invariant: every field in `FleetDispatchContext` is `Arc<...>`** so the bundle is shareable as `Arc<FleetDispatchContext>` — cloning the outer Arc clones no inner state.

**Ownership caveat:** the `tunnel_state` field is a borrowed handle. The same `Arc<RwLock<TunnelState>>` lives on `AppState.tunnel_state` (lib.rs:39) and is written by the heartbeat loop and tunnel-reconnect machinery. This bundle reads it but does not own its lifetime or modification rights. Do not replace the inner `Arc` or drop the outer one assuming local control. Future additions to the bundle must distinguish borrowed vs owned explicitly.

`LlmConfig` gets one new field: `pub fleet_dispatch: Option<Arc<FleetDispatchContext>>`. `ServerState` gets the same field (same Arc). `with_runtime_overlays_from` carries forward one Arc. `clone_with_cache_access` is untouched — `self.clone()` propagates the Arc automatically. The "did you thread this into N call sites?" audit finding becomes "did you put the Arc on the bundle?" — a single yes/no at one site.

The pre-existing `fleet_roster` and `compute_queue` stay as separate fields on `LlmConfig` — their wiring predates this feature and retrofitting them into the bundle is out of scope. New Arcs join the bundle; old ones stay put.

### One State, Two Columns: `expires_at` + `worker_heartbeat_at`

The outbox schema has two timing columns. Together they eliminate the parallel in-memory `in_flight_workers` HashSet and collapse the overlapping retention rules.

**`expires_at`** drives all sweep-time state transitions. Set on every status write:
- INSERT (`status='pending'`): `expires_at = now + worker_heartbeat_tolerance_secs`
- Worker heartbeat tick: UPDATE `expires_at = now + worker_heartbeat_tolerance_secs`
- UPDATE to `status='ready'`: `expires_at = now + ready_retention_secs`
- UPDATE to `status='delivered'`: `expires_at = now + delivered_retention_secs`
- UPDATE to `status='failed'`: `expires_at = now + failed_retention_secs`

Sweep is **one predicate**: `WHERE expires_at <= datetime('now')`. Matching rows transition per their current `status`:
- `pending` → `ready` with synthesized `FleetAsyncResult::Error("worker heartbeat lost")` payload. Expires_at = now + ready_retention_secs. Predicate B delivers it on the next tick — the dispatcher hears about the peer-side failure within seconds, not minutes.
- `ready` → `failed` (retries exhausted or wall-clock cap on delivery attempts)
- `delivered` → DELETE
- `failed` → DELETE

**Status semantics, clean separation:**

| Status | Meaning |
|--------|---------|
| `pending` | Worker live and running inference |
| `ready` | Have a result (Success OR Error) that needs to be delivered |
| `delivered` | Dispatcher received the result; retention only |
| `failed` | Gave up trying to deliver to dispatcher; retention only |

The `failed` state is reached ONLY from `ready` (delivery retries exhausted, or wall-clock cap on ready). Peer-side terminations — worker crash, heartbeat lost, startup recovery — synthesize an Error payload and go through `ready` → delivery. The dispatcher receives `fleet_result_failed` via the normal callback path, not through a separate out-of-band channel.

Startup recovery follows the same principle: `pending` rows on boot become `ready` with synthesized Error payloads, queued for immediate delivery. See "Peer Startup Recovery."

No `in_flight_workers` HashSet. No parallel in-memory state. No `(dispatcher_node_id, job_id)` compound-key synchronization. No RAII-guard-or-panic-leak tradeoff. The database is the single source of truth for "is this job alive."

No `in_flight_workers` HashSet. No parallel in-memory state. No `(dispatcher_node_id, job_id)` compound-key synchronization. No RAII-guard-or-panic-leak tradeoff. The database is the single source of truth for "is this job alive."

**Invariant: all outbox UPDATEs are compare-and-swap on `status`.** Worker writes and sweep writes can race on the same row. Every UPDATE to the outbox MUST include the expected source status in its WHERE clause, and callers MUST check rows-affected:

| Writer | UPDATE | Lose condition |
|--------|--------|----------------|
| Worker heartbeat tick | `SET expires_at=? WHERE job_id=? AND status='pending'` | rowcount=0 → sweep promoted to `ready`, exit heartbeat loop |
| Worker on completion | `SET status='ready', result_json=?, expires_at=? WHERE job_id=? AND status='pending'` | rowcount=0 → sweep promoted row already (also to `ready` with synth Error). Drop worker's result; sweep's Error payload will be delivered |
| Sweep A: pending → ready (heartbeat lost) | `SET status='ready', result_json=<synth Error>, ready_at=now, expires_at=now+ready_retention_secs WHERE job_id=? AND status='pending'` | rowcount=0 → worker wrote `ready` first with real result; no-op (worker's result stands) |
| Sweep A: ready → failed (retries exhausted) | `SET status='failed', expires_at=? WHERE job_id=? AND status='ready'` | rowcount=0 → callback succeeded racing ahead; no-op |
| Delivery success | `SET status='delivered', delivered_at=now, expires_at=? WHERE job_id=? AND status='ready'` | rowcount=0 → callback 2xx but sweep concurrently promoted ready→failed on exhaustion. The callback ALREADY succeeded (dispatcher received, 2xx); discard the CAS failure and record `fleet_delivery_cas_lost`. The `failed` row will be retried by Predicate B and the dispatcher's `/v1/fleet/result` will 200-orphan it (idempotent). No lost deliveries; one wasted retry. |

Record rowcount-0 outcomes as chronicle events (`fleet_worker_sweep_lost`, `fleet_sweep_noop`, `fleet_delivery_cas_lost`) for observability. Without this invariant, the single-column state machine loses its single-source-of-truth claim.

**`worker_heartbeat_at`** is distinct from `expires_at` only for observability — it records the last actual heartbeat timestamp (expires_at is that + tolerance). In sweeps, only `expires_at` matters. In the UI and chronicle, `worker_heartbeat_at` shows live progress.

**Retry cadence** (distinct from expiry): `last_attempt_at` + `delivery_attempts` drive the per-row backoff. Orthogonal columns, separate sweep predicate. Clean separation: one predicate for state transition, one for delivery retry.

---

## Operational Policy (Contribution-Controlled)

All operational timings and caps are fields on a new `fleet_delivery_policy` schema_type contribution, loaded via the same `config_contributions::sync_config_to_operational_with_registry` machinery as `dispatch_policy`.

**Rust struct `Default` impl holds only bootstrap sentinels — deliberately conservative, not tuned operational values.** The struct's purpose is to let the node boot before any contribution has synced; the numbers below (`dispatch_ack_timeout_secs: 10`, etc.) are chosen to be obviously safe rather than obviously right. Operators tune via the seed YAML at `docs/seeds/fleet_delivery_policy.yaml`, which ships with the repo and is loaded at first boot (Init Ordering step 8). The "defaults in both YAML and Rust are the same" appearance is a coincidence — the canonical values live in YAML; the Rust impl exists only so a node with a broken DB can still accept dispatches with conservative behavior until a contribution lands.

```yaml
schema_type: fleet_delivery_policy
version: 1

# Dispatcher side
dispatch_ack_timeout_secs: 10        # POST /v1/compute/fleet-dispatch timeout (ACK only)
timeout_grace_secs: 2                # Grace window after max_wait_secs before falling through
orphan_sweep_interval_secs: 30       # PendingFleetJobs cleanup cadence
orphan_sweep_multiplier: 2           # Job swept when elapsed > (expected_timeout * this)

# Peer side
callback_post_timeout_secs: 30       # POST to callback_url timeout
outbox_sweep_interval_secs: 15       # Outbox scan cadence
worker_heartbeat_interval_secs: 10   # How often a live worker bumps expires_at
worker_heartbeat_tolerance_secs: 30  # Worker considered dead if expires_at past by this much
backoff_base_secs: 1                 # First retry after this delay
backoff_cap_secs: 64                 # Maximum delay between retries
max_delivery_attempts: 20            # After this many failures, mark 'failed'
ready_retention_secs: 1800           # Wall-clock cap for 'ready' rows (drives expires_at on transition)
delivered_retention_secs: 3600       # Delete 'delivered' rows after this (drives expires_at on transition)
failed_retention_secs: 604800        # 7 days (drives expires_at on transition)

# Admission control
max_inflight_jobs: 32                # 0 = unlimited; else peer 503s beyond this many pending|ready rows
admission_retry_after_secs: 30       # Retry-After header value on 503

# Peer discovery (pre-existing Pillar 37 fold)
peer_staleness_secs: 120             # FleetRoster.find_peer_for_rule ignores peers older than this
```

`ready_retention_secs` interaction note: it's a wall-clock cap measured from when the row first became `ready`. With `max_delivery_attempts=20` and `backoff_cap=64`, cumulative backoff can span ~15 minutes. Operators running very slow rules should raise `ready_retention_secs` so retries don't get clipped. Documented, not hidden.

Hot-reload on `ConfigSynced`. Stored as `Arc<tokio::sync::RwLock<FleetDeliveryPolicy>>` inside `FleetDispatchContext`. Chosen over `dispatch_policy`'s `Option<Arc<DispatchPolicy>>` Arc-swap pattern because this policy is updated by field-level mutation (a partial contribution may change only one field) and read on every sweep tick — RwLock allows in-place updates without a full policy rebuild. Both patterns work; the choice is deliberate.

Loader lives at `pyramid/fleet_delivery_policy.rs`: YAML parse, struct defaults, DB helpers `upsert_fleet_delivery_policy` / `read_fleet_delivery_policy`, new match arm in `config_contributions::sync_config_to_operational_with_registry`.

---

## Wire Protocol

**Wire-breaking change.** The request shape changes incompatibly with the current sync path (body-level `fleet_jwt` removed, new required fields added). Fleet-wide coordinated upgrade required. No compatibility shim — same-operator fleets upgrade together.

Upgrade-direction asymmetry: new-dispatcher → old-peer fails (old peer's serde parse rejects the absence of required body-level `fleet_jwt`). Old-dispatcher → new-peer is tolerated (serde's default extra-field behavior ignores the old body-level `fleet_jwt`; the new peer reads the header, not the body). In practice, upgrade all fleet peers within a single heartbeat window (~60s) to avoid the broken direction.

### FleetDispatchRequest (dispatcher → peer)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchRequest {
    /// UUID (any version — v4, v7, future) generated by the dispatcher.
    /// Primary key component in peer's outbox and dispatcher's PendingFleetJobs map.
    pub job_id: String,

    pub rule_name: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub temperature: f32,
    pub max_tokens: usize,
    pub response_format: Option<serde_json::Value>,

    /// Where to deliver the result. The dispatching party decides the delivery path:
    ///   Fleet:           dispatcher's own tunnel + /v1/fleet/result
    ///   Market standard: Wire's result proxy endpoint (Phase 3)
    ///   Market relay:    relay chain entry URL (Phase 3)
    pub callback_url: String,
}
```

**Fleet JWT is in the `Authorization` header only.** Not in the body. The handler's first action is `verify_fleet_identity(...)` — dispatcher identity comes from the signed `nid` claim, never from a body field.

`job_id` validation: any parseable UUID string. Empty or malformed → 400.

### FleetDispatchAck (peer → dispatcher, immediate)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchAck {
    pub job_id: String,
    /// Current queue depth on the peer at ACK time. Informational only
    /// (future load-balancing work in llm.rs:806 TODO will consume this).
    /// u64 to match compute_queue sizing without truncation.
    pub peer_queue_depth: u64,
}
```

HTTP 202 Accepted on success. Admission rejection → HTTP 503 with `Retry-After: {admission_retry_after_secs}`. Same-dispatcher retry after prior delivery → HTTP 410 Gone. Cross-dispatcher job_id collision → HTTP 409 Conflict.

### FleetAsyncResult (peer → dispatcher, via callback)

Tagged enum — exactly one variant on the wire:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum FleetAsyncResult {
    Success(FleetDispatchResponse),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetAsyncResultEnvelope {
    pub job_id: String,
    pub outcome: FleetAsyncResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchResponse {
    pub content: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub model: String,
    pub finish_reason: Option<String>,
    pub peer_model: Option<String>,
}
```

`FleetDispatchResponse` is retained as the success payload. The sync-path wire function that returned `FleetDispatchResponse` directly is removed — not the struct.

---

## Peer Side: handle_fleet_dispatch

Order of operations is load-bearing. Everything that could justify rejecting the job happens **before** the 202 ACK. Once ACKed, the peer owns completing it.

1. **`let identity = verify_fleet_identity(auth_header, &public_key, &self_operator_id)?;`** Single call. Returns a `FleetIdentity` — fields are private; access via `.nid()` and `.op()`. The non-empty contract is type-enforced because construction goes only through the verifier. Reject 403 on Err. `identity.nid()` is the dispatcher; use it for the rest of the handler.
2. **Parse request body.** Reject 400 on missing/empty `job_id`, `rule_name`, `user_prompt`, `callback_url`. Reject 400 if `job_id` is not a parseable UUID. Parse `callback_url` via `TunnelUrl::parse` or compatible — reject 400 on unparseable.
3. **Validate `callback_url`**: acquire the roster read-lock, call `validate_callback_url`, release the lock before any `.await`. Reject 403 on authority mismatch or unknown dispatcher.

    ```rust
    {
        let roster = state.fleet_roster.read().await;
        validate_callback_url(
            &body.callback_url,
            &CallbackKind::Fleet { dispatcher_nid: &identity.nid() },
            &*roster,
        ).map_err(|_| warp::reject::custom(Forbidden))?;
    }  // roster read-lock drops here
    ```
4. **Resolve model** from dispatch policy by `rule_name`. Reject 400 if no local provider serves the rule.
5. **Reverse-channel precondition.** Read `roster.fleet_jwt`. If `None`, return `503` with `Retry-After: {admission_retry_after_secs}`, record `fleet_admission_rejected { reason: "no fleet_jwt" }`. Without a JWT the peer cannot deliver callbacks — accepting work would guarantee an undeliverable result.
6. **Transactional admission + idempotent insert.** Open `BEGIN IMMEDIATE` on pyramid.db. All steps inside a single transaction:

   a. `INSERT OR IGNORE` row with `(dispatcher_node_id=identity.nid(), job_id, callback_url, status='pending', expires_at=now+worker_heartbeat_tolerance_secs, ...)` using PK `(dispatcher_node_id, job_id)`.

   b. `SELECT dispatcher_node_id, status FROM fleet_result_outbox WHERE job_id = ?`.

   c. Branch on the SELECT result:

   | Stored `dispatcher_node_id` | Stored `status` | Action |
   |-----------------------------|-----------------|--------|
   | ≠ `identity.nid()` | any | ROLLBACK, return `409 Conflict`. Exit. |
   | = `identity.nid()`, freshly inserted (a) succeeded | `pending` | Proceed to step (d) admission check. |
   | = `identity.nid()`, pre-existing | `pending` / `ready` | Legitimate retry. COMMIT. Return `202` with same `job_id`. Do NOT spawn a new worker — the existing one owns completion. |
   | = `identity.nid()`, pre-existing | `delivered` | Original dispatch completed and delivery succeeded. Dispatcher lost state. ROLLBACK, return `410 Gone`. Exit. |
   | = `identity.nid()`, pre-existing | `failed` | Peer gave up. ROLLBACK, return `410 Gone` with `last_error` in body. Exit. |

   d. Admission check (only on freshly-inserted path). The row just inserted is already counted; to honor `max_inflight_jobs` as the actual limit (not `max − 1`), exclude the new row from the count:
   ```sql
   SELECT COUNT(*) FROM fleet_result_outbox
    WHERE status IN ('pending','ready')
      AND NOT (dispatcher_node_id = ? AND job_id = ?);
   ```
   If `>= max_inflight_jobs` and policy value is non-zero: `DELETE` the row just inserted, ROLLBACK, return `503` with `Retry-After: {admission_retry_after_secs}`, record `fleet_admission_rejected`. Exit. Otherwise COMMIT.

   To disambiguate "freshly inserted" vs "row already existed and matches me", check `conn.changes()` after the `INSERT OR IGNORE`: `1` = inserted, `0` = ignored. Branch 6(c) row 2 (legitimate retry) fires when the SELECT shows the row is ours AND `changes() == 0`.

   The reordering — `INSERT OR IGNORE` before the admission count — means legitimate retries are never spuriously 503'd when the peer is at capacity, because retries don't hit the count path.

7. **Return 202 Accepted** with `FleetDispatchAck { job_id, peer_queue_depth }`. Record `fleet_job_accepted`.
8. **Spawn worker task.** Use `tokio::spawn` before returning the 202 response. The spawned closure owns clones of the relevant `Arc`s (`ServerState` already derives `Clone`), the policy snapshot, and a snapshot `LlmConfig` with **fleet recursion bypass fields explicitly zeroed**:

   ```rust
   let worker_config = {
       let mut c = cfg_snapshot.clone();
       c.fleet_dispatch = None;     // prevents Phase A re-entry
       c.fleet_roster = None;       // belt-and-suspenders — no peer candidates
       c
   };
   let worker_options = LlmCallOptions {
       skip_fleet_dispatch: true,   // explicit Phase A guard
       ..options
   };
   ```

   Without these strips, a recursive call into Phase A would re-dispatch the fleet-received job back out to another peer. This mirrors the existing pattern in `handle_fleet_dispatch` (current server.rs:1558-1567).

   Inside the spawned task, a `tokio::select!` runs inference alongside a heartbeat tick:

   ```rust
   let inference = call_model_unified_with_options_and_ctx(/* see fleet_dispatch-stripped config below */);
   let heartbeat = async {
       let mut ticker = interval(Duration::from_secs(policy.worker_heartbeat_interval_secs));
       loop {
           ticker.tick().await;
           match update_expires_at_if_pending(
               &state, dispatcher_nid, job_id,
               now + policy.worker_heartbeat_tolerance_secs,
           ).await {
               Ok(1) => continue,                              // heartbeat bumped; inference continues
               Ok(0) => {                                      // CAS lost — sweep already promoted
                   record_event("fleet_worker_sweep_lost", ...);
                   return;                                     // exits heartbeat; select! cancels inference
               }
               Ok(_) => unreachable!("compound PK; at most 1 row"),
               Err(e) if is_sqlite_busy(&e) => {               // transient lock — retry next tick
                   tracing::debug!(?e, "heartbeat tick DB-locked; retrying");
                   continue;
               }
               Err(e) => {                                     // fatal DB error — log and bail
                   tracing::error!(?e, "heartbeat DB error; giving up");
                   return;
               }
           }
       }
   };
   tokio::select! {
       result = inference => result,
       _ = heartbeat => Err(WorkerError::SweepWon),            // heartbeat exit signals sweep won or DB fatal
   }
   ```

   Distinguishing `Ok(0)` (CAS lost, sweep won — terminal) from `Err(SqliteBusy)` (transient lock — retry) is load-bearing. The earlier `unwrap_or(0)` collapsed these and would bail legitimate workers under routine DB contention.

   On inference completion, the heartbeat future is dropped automatically (select!). On task panic, both futures are dropped — no RAII guard needed, no HashSet cleanup needed. If the heartbeat loop exits because the sweep won the race, inference is cancelled and the worker returns early without attempting delivery. The sweep's `pending → failed` transition already produced a synthesized error result in the outbox that will be delivered by Predicate B.

9. **On inference completion:** CAS on status. Writing `ready` ONLY if row still `pending`:
   ```sql
   UPDATE fleet_result_outbox
      SET status='ready', result_json=?, ready_at=now, expires_at=now+ready_retention_secs
    WHERE dispatcher_node_id=? AND job_id=? AND status='pending';
   ```
   Check `rowcount`:
   - `rowcount == 1` → worker won the race, row is now `ready`, proceed to step 10.
   - `rowcount == 0` → sweep already transitioned the row to `failed`. Record `fleet_worker_sweep_lost`, drop the inference result, **do not attempt delivery** (the sweep's synthesized error will be delivered instead). Exit.

   Record `fleet_job_completed` only on rowcount=1.

10. **Attempt callback delivery** (see "Callback Delivery").
11. **On 2xx:** CAS on status:
    ```sql
    UPDATE fleet_result_outbox
       SET status='delivered', delivered_at=now, expires_at=now+delivered_retention_secs
     WHERE dispatcher_node_id=? AND job_id=? AND status='ready';
    ```
    rowcount=0 means the sweep promoted `ready → failed` while delivery was in flight — drop the 200, no-op (the failed row will be retried as 'failed' until its retention expires).

12. **On failure:** Do NOT change status. UPDATE `last_attempt_at=now`, `last_error=...`, `delivery_attempts=delivery_attempts+1`. Do NOT bump `expires_at` — wall-clock retention continues. Record `fleet_callback_failed`. Delivery sweep retries per policy.

### Outbox Schema

Lives in the node-level database (same file as `pyramid.db`). Not per-pyramid (Law 4).

```sql
CREATE TABLE IF NOT EXISTS fleet_result_outbox (
    dispatcher_node_id TEXT NOT NULL,      -- from identity.nid() at acceptance
    job_id TEXT NOT NULL,                  -- dispatcher-chosen UUID
    callback_url TEXT NOT NULL,            -- fallback if roster loses dispatcher
    status TEXT NOT NULL,                  -- 'pending' | 'ready' | 'delivered' | 'failed'
    result_json TEXT,                      -- serialized FleetAsyncResult; NULL until 'ready'
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    ready_at TEXT,
    delivered_at TEXT,
    expires_at TEXT NOT NULL,              -- drives all sweep-time transitions
    worker_heartbeat_at TEXT,              -- observability; updated alongside expires_at while worker alive
    delivery_attempts INTEGER NOT NULL DEFAULT 0,
    last_attempt_at TEXT,
    last_error TEXT,
    PRIMARY KEY (dispatcher_node_id, job_id)
);
CREATE INDEX IF NOT EXISTS idx_fleet_outbox_expires ON fleet_result_outbox (expires_at);
CREATE INDEX IF NOT EXISTS idx_fleet_outbox_status_attempts ON fleet_result_outbox (status, last_attempt_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_fleet_outbox_job_id ON fleet_result_outbox (job_id);
```

Compound PK `(dispatcher_node_id, job_id)` prevents cross-dispatcher hijacking. Unique index on `job_id` alone catches any cross-dispatcher UUID reuse (astronomically rare but permitted). No `accepted_pid` column — startup recovery handles process-boundary crashes.

### Peer Startup Recovery

On node startup, before sweep loops begin, a one-time recovery pass runs. `pending` rows become `ready` with a synthesized Error payload — they flow through the normal delivery path, same as any other `ready` row:

```sql
UPDATE fleet_result_outbox
   SET status = 'ready',
       result_json = '{"kind":"Error","data":"worker crashed before completion (node restarted)"}',
       ready_at = datetime('now'),
       expires_at = datetime('now', '+' || ? || ' seconds'),  -- bind ready_retention_secs
       last_error = 'startup recovery'
 WHERE status = 'pending';
```

`'ready'` rows survive unchanged. Predicate B picks both the original `ready` rows and the recovery-synthesized ones up on the next tick. The dispatcher receives `fleet_result_failed` on the same channel as success — no out-of-band notification path. This is the durability boundary: every accepted job produces a delivered outcome, success or failure.

### Callback Delivery

```rust
async fn deliver_fleet_result(
    dispatcher_nid: &str,
    stored_callback_url: &str,
    envelope: &FleetAsyncResultEnvelope,
    roster: &FleetRoster,
    policy: &FleetDeliveryPolicy,
) -> Result<(), FleetDeliveryError> {
    // Prefer current tunnel URL from roster (dispatcher may have rotated tunnel).
    let effective_url = roster.peers.get(dispatcher_nid)
        .map(|p| p.tunnel_url.endpoint("/v1/fleet/result"))
        .unwrap_or_else(|| stored_callback_url.to_string());

    // Read JWT live from roster. Check exp before using.
    let jwt = roster.fleet_jwt.clone().ok_or(FleetDeliveryError::NoJwt)?;
    if is_jwt_expired(&jwt) { return Err(FleetDeliveryError::JwtExpired); }

    HTTP_CLIENT
        .post(&effective_url)
        .header("Authorization", format!("Bearer {}", jwt))
        .json(envelope)
        .timeout(Duration::from_secs(policy.callback_post_timeout_secs))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
```

`tunnel_url.endpoint("/v1/fleet/result")` uses the `TunnelUrl` newtype's own construction rules — no string concatenation, no `url::Url::join` subtleties. The `TunnelUrl` guarantees normalization on construction, so `effective_url` is well-formed by type.

### Delivery Sweep — one predicate for transitions, one for retries

`tokio::spawn`ed loop, started in `main.rs`. Not piggybacked on the Wire heartbeat loop.

```rust
async fn fleet_outbox_sweep_loop(
    db_path: PathBuf,
    ctx: Arc<FleetDispatchContext>,
) {
    loop {
        let interval = ctx.policy.read().await.outbox_sweep_interval_secs.max(1);
        tokio::time::sleep(Duration::from_secs(interval)).await;

        // Predicate A: transition by expiry (one query, branch on status).
        // SQLite work runs inside spawn_blocking to avoid starving the reactor on
        // busy peers with deep outboxes. The blocking body MUST NOT contain .await.
        // Chronicle events are written via synchronous `record_event(&conn, &ctx)`
        // using the same rusqlite::Connection that drives the sweep — matching the
        // pattern already used by server.rs:1589 (spawn_blocking + direct DB write).
        // No broadcast channel, no phantom event type.
        let blk_db = db_path.clone();
        let blk_policy = ctx.policy.clone();
        tokio::task::spawn_blocking(move || {
            let policy = blk_policy.blocking_read().clone();
            let conn = rusqlite::Connection::open(&blk_db)?;
            sweep_expired_blocking(&conn, &policy)
        }).await.ok();

        // Predicate B: retry 'ready' rows whose backoff has elapsed.
        // The candidate SELECT is via spawn_blocking; callback POSTs happen async.
        sweep_retries(&db_path, &ctx).await;
    }
}
```

Passing concrete `(db_path, ctx)` rather than a `SharedState` grab-bag clarifies what the sweep actually needs: a DB path and the dispatch context. Reduces coupling. Chronicle writes use the existing SQLite-native pattern — no broadcast channel.

**Predicate A — state transitions by expiry:**

```sql
SELECT dispatcher_node_id, job_id, status, callback_url, result_json
  FROM fleet_result_outbox
 WHERE expires_at <= datetime('now');
```

For each row, branch on `status`:
- `pending` → UPDATE `status='failed'`, synthesize error result, `expires_at=now+failed_retention_secs`. Record `fleet_worker_stuck`.
- `ready` → UPDATE `status='failed'` (retries exhausted or wall-clock cap). Record `fleet_callback_exhausted`.
- `delivered` → DELETE.
- `failed` → DELETE.

No cascaded rules. No overlapping conditions. The semantics of each status are local to the row's `expires_at` and current `status`.

**Predicate B — delivery retry:**

```sql
SELECT dispatcher_node_id, job_id, callback_url, result_json
  FROM fleet_result_outbox
 WHERE status = 'ready'
   AND (last_attempt_at IS NULL
        OR datetime(last_attempt_at, '+' || ? || ' seconds') <= datetime('now'))
   AND delivery_attempts < ?;  -- bind backoff_at_attempts + max_delivery_attempts
```

For each row, attempt `deliver_fleet_result`. On 2xx: step 11 (mark delivered). On failure: step 12 (bump attempts). Backoff delay is computed in Rust against the row's `delivery_attempts` counter: `min(backoff_base_secs << delivery_attempts, backoff_cap_secs)`. The SQL predicate binds that delay per row — or more simply, selects all candidates and filters in Rust before attempting.

**Rows that exhaust `max_delivery_attempts`** are promoted by Predicate B via an explicit `expires_at` bump, not a direct status write:

```sql
UPDATE fleet_result_outbox
   SET expires_at = datetime('now', '-1 second')
 WHERE dispatcher_node_id=? AND job_id=? AND status='ready' AND delivery_attempts >= ?;
```

Predicate A on the next tick transitions it `ready → failed` via the standard CAS path (records `fleet_callback_exhausted`). Two-step promotion keeps the terminal-state-write machinery monopolized in Predicate A — one place writes `failed`, one place writes `delivered`. Wall-clock cap via `ready_retention_secs` remains as a belt-and-suspenders backstop.

---

## Dispatcher Side

### PendingFleetJobs

```rust
pub struct PendingFleetJobs {
    /// std::sync::Mutex — held only for map operations. Never across .await.
    jobs: std::sync::Mutex<HashMap<String, PendingFleetJob>>,
}

struct PendingFleetJob {
    sender: oneshot::Sender<FleetAsyncResult>,
    dispatched_at: Instant,
    /// MUST be raw `peer.node_id`, NOT `peer.handle_path`. `claims.nid` on the
    /// callback's JWT carries the raw node_id; the forgery check compares them directly.
    /// `LlmResponse.fleet_peer_id` (handle_path for display) is a separate decoration.
    peer_id: String,
    /// Snapshot of `route.max_wait_secs` at registration time (as Duration).
    /// Isolates in-flight jobs from policy hot-reloads that would otherwise
    /// change the sweep window mid-flight. Today `max_wait_secs` is a global
    /// field on dispatch_policy.escalation (no per-rule override); the per-job
    /// snapshot is forward-compatible with adding one.
    expected_timeout: Duration,
}
```

Wrapped in `Arc<PendingFleetJobs>` and bundled into `FleetDispatchContext` (threaded into both `LlmConfig.fleet_dispatch` and `ServerState.fleet_dispatch` via one field).

**Sweep pattern — lock → collect expired keys → unlock → remove:**

```rust
async fn pending_jobs_sweep_once(pending: &PendingFleetJobs, multiplier: u64) {
    // Clamp to a sensible operational range [1, 10]. Larger values are absurd
    // (a 600s rule with multiplier=100 wouldn't sweep for 17 hours), smaller
    // values risk sweeping before the legitimate timeout. Out-of-range values
    // get logged once per reload as fleet_policy_clamped.
    let mult = multiplier.clamp(1, 10) as u32;
    let expired: Vec<String> = {
        let jobs = pending.jobs.lock().unwrap();
        jobs.iter()
            .filter(|(_, j)| j.dispatched_at.elapsed() > j.expected_timeout.saturating_mul(mult))
            .map(|(k, _)| k.clone())
            .collect()
    };
    for key in expired {
        let _ = pending.jobs.lock().unwrap().remove(&key);
        // Drop of PendingFleetJob drops its oneshot::Sender, which signals
        // RecvError to any awaiting Phase A future (handled explicitly in step 10).
        // Record fleet_pending_orphaned.
    }
}
```

### Dispatcher tunnel_state — read live via FleetDispatchContext

`ctx.tunnel_state.read().await` inside Phase A at dispatch time. No denormalization onto `LlmConfig` as a plain string.

### New Endpoint: POST /v1/fleet/result

Added to `fleet_routes` alongside `/v1/compute/fleet-dispatch` and `/v1/fleet/announce`.

**Peek → verify → pop-and-send**, never pop-then-verify. Pop-then-verify would let a forger with a guessed job_id DoS legitimate pending entries by triggering the "popped but 403'd" path.

```rust
async fn handle_fleet_result(
    auth_header: String,
    body: serde_json::Value,
    state: ServerState,
) -> Result<impl warp::Reply, warp::Rejection> {
    // Unwrap fleet dispatch context — 503 if fleet disabled on this node.
    let ctx = match state.fleet_dispatch.as_ref() {
        Some(c) => Arc::clone(c),
        None => return Ok(reply_with_status(json!({"error":"fleet dispatch disabled"}), 503)),
    };

    // Snapshot auth inputs once, drop locks before verify_fleet_identity.
    let pk = state.jwt_public_key.read().await.clone();
    let self_op = state.auth.read().await.operator_id.clone().unwrap_or_default();

    // All fleet auth in one call; FleetIdentity is the typed result.
    let identity = match verify_fleet_identity(&auth_header, &pk, &self_op) {
        Ok(i) => i,
        Err(_) => return Ok(reply_with_status(json!({}), 403)),
    };

    let envelope: FleetAsyncResultEnvelope = match serde_json::from_value(body) {
        Ok(e) => e,
        Err(_) => return Ok(reply_with_status(json!({"error":"invalid body"}), 400)),
    };

    // Inspect + act atomically. Two-step inside one critical section,
    // releasing the lock before any send() or event record.
    enum Action {
        Deliver(PendingFleetJob),
        Forgery,
        Orphan,
    }

    let action = {
        let mut jobs = ctx.pending.jobs.lock().unwrap();
        // Avoid an overlapping immutable-then-mutable borrow by deciding
        // everything from a snapshot of the identity check first.
        let matches_ours = jobs
            .get(&envelope.job_id)
            .map(|pj| pj.peer_id == identity.nid());
        match matches_ours {
            None => Action::Orphan,
            Some(false) => Action::Forgery,
            Some(true) => Action::Deliver(jobs.remove(&envelope.job_id).unwrap()),
        }
    };

    match action {
        Action::Deliver(pj) => {
            let latency_ms = pj.dispatched_at.elapsed().as_millis() as u64;
            let _ = pj.sender.send(envelope.outcome);   // Err acceptable — receiver may have dropped
            record_event("fleet_result_received", { peer_id: pj.peer_id, latency_ms });
            Ok(reply_with_status(json!({}), 200))
        }
        Action::Forgery => {
            record_event("fleet_result_forgery_attempt", { job_id: envelope.job_id, claimed_peer: identity.nid() });
            Ok(reply_with_status(json!({}), 403))
        }
        Action::Orphan => {
            record_event("fleet_result_orphaned", { job_id: envelope.job_id, claimed_peer: identity.nid() });
            Ok(reply_with_status(json!({}), 200))
        }
    }
}
```

Return 200 on "not found" (peer marks delivered and stops retrying). 403 only on integrity failure (different peer claiming to deliver a result), which signals that specific peer without leaking job_id existence.

Uses `Result<impl Reply, Rejection>` so early-return short-circuits compose. The two-phase approach (snapshot identity match, then mutate) avoids the NLL borrow fragility of `match jobs.get() { Some(_) => jobs.remove() }`.

### Modified Dispatch Flow (Phase A in llm.rs)

0. **Snapshot policy and tunnel once at entry** — acquire both read-locks, extract the values needed, release the locks before any other `.await`. The local binding is `fleet_ctx`, not `ctx`, to avoid shadowing the existing `ctx: Option<&StepContext>` parameter of `call_model_unified_with_options_and_ctx`.

   ```rust
   let fleet_ctx = config.fleet_dispatch.as_ref()?.clone();  // None => skip fleet
   let (policy, callback_url) = {
       let p = fleet_ctx.policy.read().await.clone();
       let ts = fleet_ctx.tunnel_state.read().await;
       let url = match (&ts.status, ts.tunnel_url.as_ref()) {
           // Only Connected is dispatch-valid. Connecting means cloudflared
           // hasn't finished announcing the tunnel on Cloudflare's edge yet;
           // callbacks to that URL would 404 at the edge.
           (TunnelConnectionStatus::Connected, Some(u)) => u.endpoint("/v1/fleet/result"),
           _ => return Err(PhaseASkip::TunnelNotReady),  // fall through to local
       };
       (p, url)
   };
   ```

   Combining step 1 and step 3 eliminates the TOCTOU race where the tunnel could transition Connected → Disconnected between the check and URL construction.
1. (Handled in step 0.) `callback_url` is now bound; `policy` is a `FleetDeliveryPolicy` value local to this call that won't see mid-call hot-reload surprises.
2. **Generate** `job_id = uuid::Uuid::new_v4().to_string()`.
3. (Handled in step 0.)
4. **Create** `(sender, receiver) = oneshot::channel::<FleetAsyncResult>()`.
5. **Register** `(job_id, PendingFleetJob { sender, dispatched_at: Instant::now(), peer_id: peer.node_id.clone(), expected_timeout: Duration::from_secs(route.max_wait_secs.max(1)) })` in `fleet_ctx.pending`. **`peer.node_id`, not `handle_path`.** Clamp `route.max_wait_secs` to at least 1 second — a zero value would cause the orphan sweep to evict the entry on its first tick.
6. **POST dispatch** via `fleet_dispatch_by_rule` — returns `Result<FleetDispatchAck, FleetDispatchError>`. Timeout = `Duration::from_secs(policy.dispatch_ack_timeout_secs)`.
7. **On POST failure / 410 / 409:** remove entry, record `fleet_dispatch_failed`, fall through to local.
8. **On 503:** remove entry, record `fleet_peer_overloaded`, try next peer or fall through.
9. **On 202:** record `fleet_dispatched_async` with `peer_queue_depth` from the ACK.
10. **Await receiver with pinned two-phase timeout.** `tokio::time::timeout` consumes its future by value; pin once, then pass `receiver.as_mut()` (which produces a new `Pin<&mut Receiver>` that is itself `impl Future`) to each timeout call. Naively passing `&mut receiver` yields `&mut Pin<&mut Receiver>`, which is NOT a Future and does not compile:

    ```rust
    tokio::pin!(receiver);
    let outcome = match tokio::time::timeout(
        Duration::from_secs(route.max_wait_secs),
        receiver.as_mut(),
    ).await {
        Ok(Ok(r)) => Ok(r),                              // result arrived
        Ok(Err(_recv_err)) => Err(PhaseAError::Orphaned), // sender dropped by sweep
        Err(_elapsed) => {
            // Primary timeout — grace window for in-flight callbacks.
            match tokio::time::timeout(
                Duration::from_secs(policy.timeout_grace_secs),
                receiver.as_mut(),
            ).await {
                Ok(Ok(r)) => Ok(r),
                _ => Err(PhaseAError::Timeout),
            }
        }
    };
    ```

    Branch on the returned `FleetAsyncResult`:

    - `Ok(FleetAsyncResult::Success(resp))` → remove entry (idempotent), return `resp` with fleet provenance, record `fleet_result_received` with end-to-end `latency_ms`.
    - `Ok(FleetAsyncResult::Error(msg))` → **peer ran inference and it failed.** Remove entry, record `fleet_result_failed` with `{peer_id, error: msg}`. Fall through to local. (This is the common real failure mode — GPU OOM, model mismatch, mid-job abort.)
    - `Err(Orphaned)` → remove entry (idempotent — sweep already ran). Fall through to local.
    - `Err(Timeout)` → remove entry, record `fleet_dispatch_timeout`, fall through to local.

### Race analysis

| Scenario | Outcome |
|----------|---------|
| Callback arrives before timeout | `/v1/fleet/result` peeks → verifies nid → pops → sends. Dispatcher receives. |
| Timeout fires before callback | Dispatcher drops receiver. Late callback hits endpoint, peeks → not found → 200 + `fleet_result_orphaned`. Peer marks delivered. |
| Callback and timeout race | Whichever pops first wins; oneshot drop semantics prevent double-delivery. |
| Duplicate callback retry | Peer re-delivers after missed 200. Dispatcher's entry gone. 200 + `fleet_result_orphaned`. Peer marks delivered on the retry's 200. |
| Orphan sweep fires during await | Sweep removes entry → sender dropped → `Ok(Err(RecvError))`. Phase A falls through to local. |

**Known trade-off — duplicate execution under timeout.** When timeout beats callback, the prompt runs on the peer AND locally (fall-through). The peer's eventual result is orphaned. Callers that write side effects under LLM provenance (DADBEAR, evidence contributions) must tolerate idempotent re-execution. The `LlmResponse` returned from fall-through is the local result; the peer's result never reaches the caller. Acceptable — it only fires when `max_wait_secs` is exceeded; observable and tunable.

### Orphan Sweep (dispatcher)

```rust
async fn pending_jobs_sweep_loop(ctx: Arc<FleetDispatchContext>) {
    loop {
        let (interval, multiplier) = {
            let p = ctx.policy.read().await;
            (p.orphan_sweep_interval_secs.max(1), p.orphan_sweep_multiplier)
        };
        tokio::time::sleep(Duration::from_secs(interval)).await;
        pending_jobs_sweep_once(&ctx.pending, multiplier).await;
    }
}
```

Per-job `expected_timeout * multiplier` — long-running rules aren't swept prematurely. (Motivation: isolation from policy hot-reloads changing the sweep window mid-flight. Per-rule `max_wait_secs` overrides do not exist today on `dispatch_policy`; when they're added, this already respects them.)

---

## Chronicle Events

**`source` column values unchanged.** Peer work retains `source='fleet_received'`. Dispatcher-side fleet events retain `source='fleet'`. Only event_type strings change.

**Old event_types removed:** `fleet_dispatched`, `fleet_returned`, `fleet_dispatch_failed`, `fleet_received`.

> `fleet_received` is removed ONLY as an event_type value. It continues to serve as the `source` column value for peer-received work. Queue entries and peer-side chronicle events still tag `source='fleet_received'`.

**DB views must be migrated, not "kept as-is".** `CREATE VIEW IF NOT EXISTS` is a no-op on existing DBs. Migration:

```sql
DROP VIEW IF EXISTS v_compute_fleet_peers;
DROP VIEW IF EXISTS v_compute_by_source;
-- then recreate with updated event_type names
```

New `v_compute_fleet_peers` counts:
- dispatches: `event_type = 'fleet_dispatched_async'`
- successes: `event_type = 'fleet_result_received'`
- failures: `event_type IN ('fleet_dispatch_failed', 'fleet_dispatch_timeout', 'fleet_peer_overloaded')`

New `v_compute_by_source` replaces `fleet_returned` with `fleet_result_received` in its completion set.

`compute_chronicle.rs` Rust queries (`query_summary`, `query_timeline`) reference `event_type IN ('completed', 'cloud_returned')` — unchanged. Leave those alone.

The `event_type='fleet_received'` chronicle write at `server.rs:1569–1594` must be replaced (not retained) with `fleet_job_accepted` at step 7.

**New event_types:**

| Event | Source | Side | When |
|-------|--------|------|------|
| `fleet_dispatched_async` | fleet | dispatcher | 202 ACK received, pending job registered |
| `fleet_dispatch_failed` | fleet | dispatcher | Dispatch POST errored (transport, non-202/non-503/non-409/non-410) |
| `fleet_peer_overloaded` | fleet | dispatcher | Peer returned 503 — admission rejected |
| `fleet_dispatch_timeout` | fleet | dispatcher | Pending job timed out past grace |
| `fleet_result_received` | fleet | dispatcher | `/v1/fleet/result` resolved a pending job with `Success` outcome |
| `fleet_result_failed` | fleet | dispatcher | `/v1/fleet/result` resolved with `Error` outcome (peer inference failed) |
| `fleet_result_orphaned` | fleet | dispatcher | `/v1/fleet/result` received an unknown job_id |
| `fleet_result_forgery_attempt` | fleet | dispatcher | `/v1/fleet/result` JWT nid mismatch |
| `fleet_pending_orphaned` | fleet | dispatcher | Orphan sweep removed a stale entry |
| `fleet_job_accepted` | fleet_received | peer | 202 returned, outbox row written |
| `fleet_admission_rejected` | fleet_received | peer | 503 or 410 returned |
| `fleet_job_completed` | fleet_received | peer | Inference finished, outbox `ready` |
| `fleet_callback_delivered` | fleet_received | peer | Callback returned 2xx |
| `fleet_callback_failed` | fleet_received | peer | Callback failed this attempt |
| `fleet_callback_exhausted` | fleet_received | peer | Max attempts or ready_retention cap reached; row → failed |
| `fleet_worker_heartbeat_lost` | fleet_received | peer | Worker heartbeat stopped; row → ready with synth Error (NOT → failed; error flows through normal delivery) |
| `fleet_worker_sweep_lost` | fleet_received | peer | Worker completed AFTER sweep already promoted row to ready with synth Error. Worker drops its result. Chronicle both outcomes for debugging. |
| `fleet_delivery_cas_lost` | fleet_received | peer | Callback returned 2xx but CAS on `ready→delivered` lost (sweep concurrently promoted `ready→failed`). Idempotent; retry will 200-orphan. |

All peer-side events record `peer_id = identity.nid()` (dispatcher) in metadata. All dispatcher-side events record `peer_id` (peer dispatched to). Keeps `v_compute_fleet_peers`'s `json_extract(metadata, '$.peer_id')` aggregation functional.

---

## Callback URL Validation

Context-specific rules, uniform mechanism. Implemented via `validate_callback_url` which takes a `TunnelUrl`-validated URL and checks authority match.

```rust
pub enum CallbackKind<'a> {
    Fleet { dispatcher_nid: &'a str },
    MarketStandard,   // Phase 3
    Relay,            // Phase 3
}

fn validate_callback_url(
    callback_url: &str,
    kind: &CallbackKind,
    roster: &FleetRoster,
) -> Result<(), CallbackValidationError> {
    let got = TunnelUrl::parse(callback_url)?;
    match kind {
        CallbackKind::Fleet { dispatcher_nid } => {
            let peer = roster.peers.get(*dispatcher_nid)
                .ok_or(CallbackValidationError::UnknownDispatcher)?;
            if got.authority() != peer.tunnel_url.authority() {
                return Err(CallbackValidationError::AuthorityMismatch);
            }
            // Fleet path pin: only /v1/fleet/result accepted.
            if got.path() != "/v1/fleet/result" {
                return Err(CallbackValidationError::PathMismatch);
            }
            Ok(())
        }
        _ => Err(CallbackValidationError::KindNotImplemented),
    }
}
```

**Roster bootstrap window:** if the peer hasn't yet received a heartbeat naming the dispatcher, lookup fails → 403 until heartbeat convergence (≤60s). Same-operator fleets come up together; window is narrow and documented.

| Context | callback_url | Set by | Peer validates against |
|---------|-------------|--------|------------------------|
| Private fleet | `{dispatcher_tunnel}/v1/fleet/result` | Dispatcher | Peer's FleetRoster entry for `identity.nid()` |
| Compute market (standard, Phase 3) | `{wire_host}/v1/compute/result-proxy/{job_id}` | Wire | Pinned Wire host + path prefix |
| Compute market (relay, Phase 3) | `{relay_entry}/v1/relay/result/{job_id}` | Relay chain | Relay registry + path prefix |

---

## Init Ordering

Startup sequence in `main.rs` MUST be (ordering is load-bearing — each step depends on earlier ones):

1. Open pyramid.db, run schema init (`CREATE TABLE IF NOT EXISTS fleet_result_outbox`, `DROP VIEW IF EXISTS` + recreate views).
2. Read `fleet_delivery_policy` from DB (hardcoded struct defaults as bootstrap-only sentinel values if no row — see Operational Policy section).
3. Run peer startup recovery UPDATE (all `pending` → `failed`).
4. Construct `FleetDispatchContext { tunnel_state, pending: Arc::new(PendingFleetJobs::new()), policy: Arc::new(RwLock::new(policy)) }`. Wrap in `Arc`.
5. Acquire `pyramid_state.config.blocking_write()` and set `cfg.fleet_dispatch = Some(Arc::clone(&ctx))` — matches the existing overlay pattern for `fleet_roster` and `compute_queue`. Extend `with_runtime_overlays_from` to carry the new field forward on rebuilds.
6. Wire `Arc::clone(&ctx)` onto `ServerState.fleet_dispatch` (pass as a new parameter to `start_server`).
7. **Extend the existing `ConfigSynced` listener (around `main.rs:11630`) with a `fleet_delivery_policy` branch** that re-reads the DB row and writes through `ctx.policy.write().await`. **Before** the seed in step 8, so the seed's eventual `ConfigSynced` event has a receiver.
8. Best-effort seed of `fleet_delivery_policy` contribution from `docs/seeds/fleet_delivery_policy.yaml` if none present. (Struct defaults cover any race here — they're intentionally conservative sentinels that make it obvious a seed hasn't landed, not tuned operational values.)
9. **Spawn sweep loops** (`fleet_outbox_sweep_loop(db_path, Arc::clone(&ctx))` and `pending_jobs_sweep_loop(Arc::clone(&ctx))`). Before warp, so there's no window where dispatch routes accept work that has no retry machinery attached.
10. Start warp server (fleet routes live).
11. Start heartbeat loop.

**Why this order:** listener before seed (otherwise the seed's broadcast has no subscriber and the initial policy write is silently dropped); sweeps before warp (otherwise dispatches accepted in the startup window have no retry path until sweeps come up); pyramid_state.config mutation via `blocking_write()` rather than a fresh `LlmConfig` rebuild (matches how `fleet_roster` and `compute_queue` are already attached — the live `LlmConfig` lives at `pyramid_state.config: Arc<RwLock<LlmConfig>>` and is mutated in place).

---

## What Changes

| File | Change |
|------|--------|
| `pyramid/fleet_identity.rs` (NEW) | `FleetIdentity` struct, `FleetAuthError`, `verify_fleet_identity`. Single source of truth for fleet JWT verification. Unit tests asserting: non-fleet `aud` rejected, missing/empty `nid` rejected, `op` mismatch rejected, expired JWT rejected, valid token returns populated `FleetIdentity`. |
| `pyramid/tunnel_url.rs` (NEW) | `TunnelUrl` newtype. Public methods: `parse`, `authority`, `endpoint`, `path`, `as_str`. Explicit `Serialize`/`Deserialize` impls that round-trip through the normalized string (so existing saved state + heartbeat/announcement wire format continue to interoperate with no migration). No `Default` — a default tunnel URL is meaningless. Tests: trailing-slash stripping, scheme-presence enforcement, root-path-replacement in `endpoint`, serde round-trip preserves normalization, parse rejection of missing scheme. |
| `pyramid/fleet_delivery_policy.rs` (NEW) | `FleetDeliveryPolicy` struct with YAML parse + struct defaults. DB helpers. |
| `pyramid/config_contributions.rs` | Add match arm for `schema_type = "fleet_delivery_policy"` in `sync_config_to_operational_with_registry`. |
| `fleet.rs` | Store `FleetPeer.tunnel_url` as `TunnelUrl`, not `String`. Normalize on roster ingress (both `update_from_heartbeat` and `update_from_announcement`) automatically via `TunnelUrl::parse`. Reject individual peer entries whose `tunnel_url` fails to parse (log `fleet_peer_url_invalid` per drop; do not reject the rest of the batch). Redefine `FleetDispatchRequest` (body JWT dropped; `job_id`, `callback_url` added). In `fleet_dispatch_by_rule`, remove the body-side `fleet_jwt` setter — header-only from now on. **New signature:** `fleet_dispatch_by_rule(peer: &FleetPeer, job_id: &str, callback_url: &str, rule_name: &str, system_prompt: &str, user_prompt: &str, temperature: f32, max_tokens: usize, response_format: Option<&serde_json::Value>, fleet_jwt: &str, timeout_secs: u64) -> Result<FleetDispatchAck, FleetDispatchError>`. Add `FleetDispatchAck` (with `peer_queue_depth: u64`), `FleetAsyncResult` (tagged enum), `FleetAsyncResultEnvelope`. Add `deliver_fleet_result` (uses roster-live URL via `TunnelUrl::endpoint` + live JWT). Add `validate_callback_url` + `CallbackKind` enum. Define `PendingFleetJobs` (keyed by `job_id` alone; PK in the outbox handles UUID collisions). Define `FleetDispatchContext`. Add `is_jwt_expired` helper. Read `peer_staleness_secs` from policy snapshot in `find_peer_for_rule` (replace hardcoded 120). **New signature:** `find_peer_for_rule(&self, rule_name: &str, staleness_secs: u64) -> Option<&FleetPeer>`. Caller at `llm.rs:849` updates accordingly. `FleetDispatchErrorKind` does NOT grow new variants for 503/409 — dispatcher distinguishes those via `FleetDispatchError.status_code`, leaving `kind: HttpStatus`. |
| `server.rs` | Rewrite `handle_fleet_dispatch` per the step table. Each handler's step 1 is a single call to `verify_fleet_identity`. Add `handle_fleet_result` using peek → verify → pop-and-send. Branch table in step 6 covers all four `(dispatcher, status)` combinations including `delivered`/`failed` retry → 410 Gone. **Also migrate `handle_fleet_announce`** (~line 1641) to use `verify_fleet_identity` — it currently calls the to-be-removed `verify_fleet_jwt` and doesn't check `claims.nid`. Once migrated, an announcement from a peer with missing/empty `nid` is rejected 403 (tightens a latent gap). **Replace** the `event_type='fleet_received'` chronicle write at current lines 1569–1594 with `fleet_job_accepted` at step 7 — do not leave both. `ServerState` already derives `Clone` at line 19; no change needed. Add `fleet_dispatch: Option<Arc<FleetDispatchContext>>` field and new `fleet_dispatch` parameter to `start_server`. Delete `verify_fleet_jwt` (superseded by `verify_fleet_identity`); remove `FleetJwtClaims` if no other call site references it after migration (grep: `rg 'verify_fleet_jwt\|FleetJwtClaims' src-tauri/src`). |
| `pyramid/llm.rs` | Add one field to `LlmConfig`: `fleet_dispatch: Option<Arc<FleetDispatchContext>>`. Extend `with_runtime_overlays_from` to copy that one field (match existing is-none-preserve pattern). Add `fleet_dispatch: None` to `LlmConfig::default()` at line 334. **Extend the `test_with_runtime_overlays_from` unit test** (around line 3320) to assert `fleet_dispatch` Arc pointer-equality survives the overlay call — without this, a future regression dropping the field from the overlay pattern wouldn't be caught. `clone_with_cache_access` untouched (`self.clone()` carries it). Rewrite Phase A per "Modified Dispatch Flow" — snapshot both `fleet_ctx.policy` and `fleet_ctx.tunnel_state` once at step 0 (NB: local binding is `fleet_ctx` to avoid shadowing the function's `ctx: Option<&StepContext>` parameter), then release both locks; `Duration::from_secs` on all timeouts; `tokio::pin!(receiver)` + `receiver.as_mut()` for two-phase await (NOT `&mut receiver`); `peer_id = peer.node_id` (raw, not handle_path). Branch on `FleetAsyncResult::{Success, Error}` explicitly — Success returns the response with fleet provenance, Error records `fleet_result_failed` and falls through to local. **Replace the hardcoded `120` in the `stale=...` tracing diagnostic at line ~845** with a read of the policy snapshot's `peer_staleness_secs` — snapshot happens at the very top of Phase A (before peer-iter), so the value is in scope for the diagnostic, `find_peer_for_rule`, and timeout computations. Remove sync dispatch call site (keep `FleetDispatchResponse` struct — it's the success payload inside `FleetAsyncResult`). |
| `pyramid/mod.rs` | `PyramidConfig::to_llm_config` at line 663 lists every `LlmConfig` field explicitly. Add `fleet_dispatch: None` to the initializer. |
| `pyramid/config_helper.rs` | `config_for_model` at line 47 ends with `..Default::default()` — the new `fleet_dispatch: None` is picked up automatically via `LlmConfig::default()`. **No change needed**; listed only to confirm the audit pass was made. |
| `src-tauri/src/lib.rs` | `AppState` at line 39 holds `tunnel_state: Arc<RwLock<tunnel::TunnelState>>`. After `TunnelState.tunnel_url` changes to `Option<TunnelUrl>`, any consumer reading the field (~40 call sites across main.rs, auth.rs, fleet.rs, pyramid/publication.rs, pyramid/sync.rs, pyramid/routes.rs, pyramid/build_runner.rs, pyramid/wire_publish.rs, pyramid/slug.rs, pyramid/types.rs, pyramid/webbing.rs, pyramid/wire_import.rs, pyramid/public_html/routes_read.rs, pyramid/openrouter_webhook.rs, messaging.rs) must migrate. Patterns: `ts.tunnel_url.clone().unwrap_or_default()` → `ts.tunnel_url.as_ref().map(\|t\| t.as_str()).unwrap_or("")`. `auth::heartbeat(..., tunnel_url.as_deref(), ...)` → same pattern. Grep target: `rg 'tunnel_url' src-tauri/src`. Enumerate + migrate all hits. |
| `pyramid/compute_chronicle.rs` | New event_type string constants. `query_summary` and `query_timeline` SQL unchanged (they reference `cloud_returned`, not `fleet_returned`). `source` values unchanged. |
| `pyramid/db.rs` | Add `fleet_result_outbox` table with compound PK, unique `job_id` index, `expires_at` and `worker_heartbeat_at` columns. Add insert, update-ready, update-delivered, status-transition, expiry-based sweep, retry-eligible sweep queries. Add startup recovery UPDATE (pending → ready with synth Error). **Inside `init_pyramid_db`**, emit `DROP VIEW IF EXISTS v_compute_fleet_peers; DROP VIEW IF EXISTS v_compute_by_source;` BEFORE the `CREATE VIEW IF NOT EXISTS` statements for those views. Both idempotent; running on every startup guarantees upgraded nodes get the new event_type filters. Without the explicit DROPs, `CREATE VIEW IF NOT EXISTS` silently keeps the old view definitions forever and fleet analytics show zero activity post-rename. |
| `main.rs` | Construct `FleetDispatchContext` at startup, wire into `LlmConfig` and `ServerState`. Run peer startup recovery before sweep loops. Spawn `fleet_outbox_sweep_loop` and `pending_jobs_sweep_loop`. Seed `fleet_delivery_policy` from `docs/seeds/fleet_delivery_policy.yaml`. Extend `ConfigSynced` listener at `main.rs:11630` with `fleet_delivery_policy` branch. |
| `tunnel.rs` | Change `TunnelState.tunnel_url` from `Option<String>` to `Option<TunnelUrl>`. `TunnelUrl`'s serde impls preserve the on-disk state file format for well-formed URLs. But `load_tunnel_state` (~line 372) must be tolerant: if a prior version wrote a malformed `tunnel_url` (empty, missing scheme, trailing junk), deserialize at the outer `TunnelState` level and fall the `tunnel_url` field back to `None` with a warn-level log — preserve `tunnel_id` / `tunnel_token` / `status`. Losing the tunnel identity on upgrade would trigger a full re-provision and invalidate all fleet roster entries elsewhere. Use `#[serde(default, deserialize_with = ...)]` or a two-stage parse. In `provision_tunnel` (~line 229), wrap the server-returned URL: `TunnelUrl::parse(&provision.tunnel_url).map_err(...)?` — a malformed URL from the Wire at provision time IS a hard error. |
| `pyramid/dispatch_policy.rs` | No structural change. `max_wait_secs` remains global on EscalationConfig. |
| `docs/seeds/fleet_delivery_policy.yaml` (NEW) | Seed YAML with the defaults from "Operational Policy". |
| Frontend `ComputeChronicle.tsx` / analytics | In `EVENT_TYPE_COLORS`: rename `fleet_dispatched` → `fleet_dispatched_async`, rename `fleet_returned` → `fleet_result_received`, delete `fleet_received` entirely. In `SOURCE_COLORS`: keep `fleet_received` as-is (source column value unchanged). Replace other removed event_type filters with new names. Grep: `"fleet_dispatched"`, `"fleet_returned"`, `"fleet_dispatch_failed"`, `"fleet_received"` (as event_type keys only, not source keys) across `.ts`, `.tsx`, SQL. |

No Wire-side migrations. No new Wire RPCs. Entirely node-to-node.

---

## What This Doesn't Do

- **Streaming.** Callback delivers the complete result. Streaming fleet dispatch is a separate protocol concern.
- **Load balancing.** The TODO at `llm.rs:806` (compare local vs fleet queue depth) is orthogonal. `peer_queue_depth` in `FleetDispatchAck` makes this cheap to add later.
- **Multi-peer redundancy.** Future optimization; protocol supports it (multiple callbacks resolve the same pending job; first wins via peek-verify-pop).
- **Market result proxy / relay chain delivery.** Phase 3; `CallbackKind` is already enum-shaped for extension.
- **Rule-name replication across fleet peers.** Operators editing `dispatch_policy` on one node don't propagate. Diverged rule names silently fall through. Out of scope.
- **Protocol versioning within a fleet.** Path is pinned to `/v1/fleet/result`. Future protocol bumps require coordinated fleet-wide upgrades.
- **Request-level deduplication.** Two concurrent Phase A invocations with identical prompts dispatch independently (two UUIDs, two GPU runs).
- **Body-content validation on retry.** If a dispatcher retries with the same `job_id` but a different body, the peer silently executes the original body. Dispatchers are expected to generate fresh UUIDs per logical call; same-UUID different-body is a client contract violation.
- **Per-rule `max_wait_secs` overrides.** `dispatch_policy.escalation.max_wait_secs` is global today. The per-job `expected_timeout` in `PendingFleetJob` is forward-compatible with adding overrides to `RoutingRule` later; currently every job snapshots the same global value.
