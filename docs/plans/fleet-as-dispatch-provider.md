# Plan: Fleet as a Dispatch Policy Provider (v4 — post-MPS-audit)

**Date:** 2026-04-14
**Problem:** Fleet routing matches by provider-specific model names. This creates an unsolvable identity mismatch across providers (OpenRouter slugs vs Ollama names).
**Fix:** Fleet is a `provider_id` in the dispatch policy's provider chain. Fleet dispatch happens BEFORE the pool acquisition loop in llm.rs (fleet is not pool-limited). `resolve_route` stays pure. Model names never cross node boundaries — only routing rule names.

---

## Root Cause

The current fleet pre-check (llm.rs ~line 627) runs AFTER dispatch policy resolution. The model name is already committed to a provider's namespace. Fleet can never match across provider types.

## The Fix: Two-Phase Provider Resolution

After `resolve_route` returns a `ResolvedRoute` with providers chain, llm.rs handles providers in two phases:

**Phase A: Fleet providers** — extract any `provider_id == "fleet"` entries from the chain. Try fleet dispatch (no pool needed). Timeout reads from the matched rule's `max_wait_secs`, not hardcoded. On success, return with fleet provenance. On failure, continue.

**Phase B: Pool providers** — the remaining entries go through the existing pool acquisition loop (llm.rs:859-904) unchanged.

This preserves the pool acquisition loop structure exactly. Fleet is a pre-pool dispatch, not a restructure of the loop.

---

## Concrete Code Changes

### 1. Add `is_local` flag to `RouteEntry` (dispatch_policy.rs)

**MPS finding:** `derive_serving_rules` used string-matching (`provider_id.contains("ollama") || contains("local")`) to detect local providers. Fragile — any provider with "local" in the name false-matches.

```rust
// dispatch_policy.rs line 29:
pub struct RouteEntry {
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_name: Option<String>,
    /// True for providers that run on local hardware (Ollama, local GPU).
    /// Used by fleet to determine which rules this node can serve.
    #[serde(default)]
    pub is_local: bool,  // ADD
}
```

Set `is_local: true` in the default dispatch policy YAML (local_mode.rs) for `ollama-local`. Fleet entries and `openrouter` entries have `is_local: false` (the default).

### 2. Add `matched_rule_name` to `ResolvedRoute` (dispatch_policy.rs)

**Audit finding:** The matched rule's name is lost after resolution. Fleet dispatch needs it.

```rust
// dispatch_policy.rs line 182:
pub struct ResolvedRoute {
    pub providers: Vec<RouteEntry>,
    pub bypass_pool: bool,
    pub sequential_rule_name: Option<String>,
    pub matched_rule_name: String,  // ADD: always populated from rule.name
    pub escalation_timeout_secs: u64,
    pub max_wait_secs: u64,
}
```

In `resolve_route` (line 218), populate it:
```rust
return ResolvedRoute {
    providers: rule.route_to.clone(),
    bypass_pool: rule.bypass_pool,
    sequential_rule_name: if rule.sequential { Some(rule.name.clone()) } else { None },
    matched_rule_name: rule.name.clone(),  // ADD
    escalation_timeout_secs: self.escalation.wait_timeout_secs,
    max_wait_secs: self.escalation.max_wait_secs,
};
```

Also populate in the default/fallback ResolvedRoute (line 231+):
```rust
matched_rule_name: String::new(),  // no match = empty
```

### 3. Add `resolve_local_for_rule` to `DispatchPolicy` (dispatch_policy.rs)

**Audit finding:** The receiving node gets `rule_name` from the fleet request but `resolve_route` takes `(work_type, tier, step_name, depth)`.

```rust
impl DispatchPolicy {
    /// Resolve by rule name (for fleet dispatch receiving).
    /// Returns the first non-fleet local provider's provider_id and model_id.
    pub fn resolve_local_for_rule(&self, rule_name: &str) -> Option<(String, Option<String>)> {
        for rule in &self.rules {
            if rule.name == rule_name {
                // Find the first non-fleet provider with is_local == true
                for entry in &rule.route_to {
                    if entry.provider_id != "fleet" && entry.is_local {
                        return Some((entry.provider_id.clone(), entry.model_id.clone()));
                    }
                }
                // Fallback: first non-fleet provider (even if not marked local)
                for entry in &rule.route_to {
                    if entry.provider_id != "fleet" {
                        return Some((entry.provider_id.clone(), entry.model_id.clone()));
                    }
                }
            }
        }
        None
    }
}
```

Note: the fallback to non-fleet non-local providers is defensive but the three-node design decision (below) means the fleet handler only uses the `is_local` path. If no local provider resolves, the fleet handler returns an error.

### 4. Add `skip_fleet_dispatch` to `LlmCallOptions` (llm.rs)

```rust
pub skip_fleet_dispatch: bool,  // default false; fleet handler sets true
```

Default to `false` in Default impl.

### 5. Fleet dispatch before pool acquisition loop (llm.rs)

Delete the current fleet pre-check (lines 627-688). Replace with fleet handling AFTER `resolve_route` but BEFORE the pool acquisition loop (~line 855):

```rust
// ── Phase A: Fleet providers (pre-pool) ──────────────────────
// Fleet is not pool-limited. Try fleet dispatch before the pool acquisition loop.
// On success: return immediately with fleet provenance.
// On failure: filter fleet from providers, continue to pool loop.
if let Some(ref route) = resolved_route {
    if !options.skip_fleet_dispatch && !route.matched_rule_name.is_empty() {
        let has_fleet = route.providers.iter().any(|e| e.provider_id == "fleet");
        if has_fleet {
            if let Some(ref roster_handle) = config.fleet_roster {
                let roster = roster_handle.read().await;
                if let Some(peer) = roster.find_peer_for_rule(&route.matched_rule_name) {
                    let jwt = roster.fleet_jwt.clone().unwrap_or_default();
                    if !jwt.is_empty() {
                        let peer_clone = peer.clone();
                        let rule_name = route.matched_rule_name.clone();
                        // Fleet timeout reads from the matched rule's escalation config
                        let fleet_timeout_secs = route.max_wait_secs;
                        drop(roster); // release lock before async

                        match fleet_dispatch_by_rule(
                            &peer_clone, &rule_name,
                            system_prompt, user_prompt,
                            temperature, max_tokens,
                            response_format,
                            &jwt,
                            fleet_timeout_secs,
                        ).await {
                            Ok(fleet_resp) => {
                                // Return with fleet provenance on the LlmResponse
                                return Ok(LlmResponse {
                                    content: fleet_resp.content,
                                    prompt_tokens: fleet_resp.prompt_tokens,
                                    completion_tokens: fleet_resp.completion_tokens,
                                    model: Some(fleet_resp.model),
                                    finish_reason: fleet_resp.finish_reason,
                                    provider_id: Some("fleet".to_string()),
                                    fleet_peer_id: Some(peer_clone.node_id.clone()),
                                    fleet_peer_model: fleet_resp.peer_model.clone(),
                                    actual_cost_usd: None,  // fleet is free
                                    ..Default::default()
                                });
                            }
                            Err(e) => {
                                // Remove dead peer, continue to pool providers
                                let mut roster_w = roster_handle.write().await;
                                roster_w.remove_peer(&peer_clone.node_id);
                                tracing::warn!("Fleet dispatch failed, trying pool providers: {}", e);
                            }
                        }
                    }
                }
            }
        }
    }
}

// Filter "fleet" from providers before pool loop (fleet already tried or skipped)
if let Some(ref mut route) = resolved_route {
    route.providers.retain(|e| e.provider_id != "fleet");
}

// ── Phase B: Pool providers (existing code, unchanged) ──────
// The pool acquisition loop at line 859+ runs on the filtered providers list.
```

**Key:** The pool acquisition loop (lines 859-904) is UNCHANGED. It just operates on a providers list that no longer contains "fleet" entries.

### 6. Fleet dispatch provenance on LlmResponse (llm.rs or types)

**MPS finding:** No visibility into which peer served a fleet call or what model it used.

Add to `LlmResponse` (or wherever the response struct lives):
```rust
pub fleet_peer_id: Option<String>,    // node_id of the peer that served this call
pub fleet_peer_model: Option<String>, // model the peer actually used (returned in response)
```

These flow through StepContext → cost log → build viz, giving the operator full fleet dispatch provenance.

### 7. Fleet dispatch function by rule (fleet.rs)

```rust
pub async fn fleet_dispatch_by_rule(
    peer: &FleetPeer,
    rule_name: &str,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    fleet_jwt: &str,
    timeout_secs: u64,  // from matched rule's escalation config, not hardcoded
) -> Result<FleetDispatchResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("Fleet HTTP client error: {}", e))?;

    let request = FleetDispatchRequest {
        rule_name: rule_name.to_string(),
        system_prompt: system_prompt.to_string(),
        user_prompt: user_prompt.to_string(),
        temperature,
        max_tokens,
        response_format: response_format.cloned(),
        fleet_jwt: fleet_jwt.to_string(),
    };

    let url = format!("{}/v1/compute/fleet-dispatch", peer.tunnel_url);
    // ... POST, parse FleetDispatchResponse ...
}
```

### 8. FleetDispatchRequest: `rule_name` only, no backward-compat `model` (fleet.rs)

**MPS finding:** Both nodes are Adam's hardware, update together. Carrying a dead `model` field adds complexity for a scenario that won't happen.

```rust
pub struct FleetDispatchRequest {
    pub rule_name: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub temperature: f32,
    pub max_tokens: usize,
    pub response_format: Option<serde_json::Value>,
    pub fleet_jwt: String,
}
```

### 9. FleetDispatchResponse: add `peer_model` (fleet.rs)

**MPS finding:** The operator needs to know what model the peer actually used.

```rust
pub struct FleetDispatchResponse {
    pub content: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub model: String,
    pub finish_reason: Option<String>,
    pub peer_model: Option<String>,  // ADD: the model the peer resolved and used
}
```

The receiving handler populates `peer_model` with the resolved model name.

### 10. Fleet dispatch receiving handler update (server.rs)

```rust
async fn handle_fleet_dispatch(...) {
    // ... JWT verification + operator check (unchanged) ...

    let rule_name = body["rule_name"].as_str().unwrap_or("").to_string();
    if rule_name.is_empty() {
        return Err("Missing rule_name");
    }

    // Resolve model from dispatch policy by rule name — LOCAL providers only
    let (resolved_provider, resolved_model) = {
        let config = state.pyramid.config.read().await;
        if let Some(ref policy) = config.dispatch_policy {
            // resolve_local_for_rule returns is_local providers only
            match policy.resolve_local_for_rule(&rule_name) {
                Some((provider_id, model_id)) => (provider_id, model_id.unwrap_or_default()),
                None => return Err("No local provider for rule"),
            }
        } else {
            return Err("No dispatch policy configured");
        }
    };

    if resolved_model.is_empty() {
        return Err("Cannot resolve model for rule");
    }

    // CRITICAL: Fleet jobs go THROUGH the queue, not around it.
    // The queue serializes fleet jobs with local builds — same GPU.
    // The fleet handler is a SUBMITTER, not the executor.
    let mut fleet_config = /* clone base config */;
    fleet_config.model = resolved_model.clone();
    // fleet_config.compute_queue stays Some — job enters the queue
    fleet_config.fleet_roster = None;  // prevent re-dispatch to fleet

    let options = LlmCallOptions {
        skip_fleet_dispatch: true,  // prevent re-dispatch loop
        // skip_concurrency_gate: false (default) — queue handles serialization
        ..Default::default()
    };

    // Call through normal LLM path — transparent queue routing enqueues the job,
    // GPU loop executes it, result flows back through the oneshot channel.
    let result = call_model_unified_with_audit_and_ctx(&fleet_config, None, messages, &options).await;

    match result {
        Ok(llm_response) => {
            let response = FleetDispatchResponse {
                content: llm_response.content,
                prompt_tokens: llm_response.prompt_tokens,
                completion_tokens: llm_response.completion_tokens,
                model: llm_response.model.unwrap_or_default(),
                finish_reason: llm_response.finish_reason,
                peer_model: Some(resolved_model),  // tell requester what model we used
            };
            Ok(warp::reply::json(&response))
        }
        Err(e) => Err(format!("LLM call failed: {}", e)),
    }
}
```

**Three-node design decision:** The receiving node ONLY tries local providers (via `resolve_local_for_rule` which filters by `is_local`). If no local provider resolves, return an error — never fall through to cloud. This prevents surprise billing. The error propagates to the requester who falls through to their own next pool provider.

### 11. FleetPeer + FleetAnnouncement: add serving_rules, keep models_loaded (fleet.rs)

```rust
pub struct FleetPeer {
    pub node_id: String,
    pub name: String,
    pub tunnel_url: String,
    pub models_loaded: Vec<String>,          // KEEP: observability
    pub serving_rules: Vec<String>,          // ADD: routing rule names this peer can serve
    pub queue_depths: HashMap<String, usize>, // KEEP: per-model depths
    pub total_queue_depth: usize,            // ADD: total across all models (for fleet load balancing)
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

pub struct FleetAnnouncement {
    pub node_id: String,
    pub name: Option<String>,
    pub tunnel_url: String,
    pub models_loaded: Vec<String>,          // KEEP
    pub serving_rules: Vec<String>,          // ADD
    pub queue_depths: HashMap<String, usize>,
    pub total_queue_depth: usize,            // ADD
    pub operator_id: String,
}
```

### 12. find_peer_for_rule (fleet.rs)

```rust
pub fn find_peer_for_rule(&self, rule_name: &str) -> Option<&FleetPeer> {
    let staleness_limit = chrono::Utc::now() - chrono::Duration::seconds(120);
    self.peers.values()
        .filter(|p| p.last_seen > staleness_limit)
        .filter(|p| p.serving_rules.contains(&rule_name.to_string()))
        .min_by_key(|p| p.total_queue_depth)
}
```

### 13. derive_serving_rules (fleet.rs)

Uses the `is_local` flag on `RouteEntry` — no string matching.

```rust
/// Derive which routing rules this node can serve locally.
/// A rule is servable if it has a RouteEntry with is_local: true
/// whose model is currently loaded (or model_id is None and something is loaded).
pub fn derive_serving_rules(
    dispatch_policy: &DispatchPolicy,
    loaded_models: &[String],
) -> Vec<String> {
    let mut serving = Vec::new();
    for rule in &dispatch_policy.rules {
        for entry in &rule.route_to {
            if entry.provider_id == "fleet" {
                continue;  // skip fleet entries
            }
            if entry.is_local {
                let model_match = match &entry.model_id {
                    Some(m) => loaded_models.contains(m),
                    None => !loaded_models.is_empty(),
                };
                if model_match {
                    serving.push(rule.name.clone());
                    break;
                }
            }
        }
    }
    serving
}
```

### 14. Default dispatch policy includes fleet (local_mode.rs)

In `commit_enable_local_mode` (~line 836), prepend fleet to the provider chain and add `is_local: true` to ollama-local:

```yaml
routing_rules:
  - name: ollama-catchall
    match_config: {}
    route_to:
      - provider_id: fleet
      - provider_id: ollama-local
        is_local: true
```

For existing dispatch_policy contributions without fleet: they work unchanged. No "fleet" entry means fleet is not tried. Fleet is opt-in via the dispatch policy. New policies from local mode get fleet by default.

### 15. Fleet announce with serving_rules (main.rs)

In the heartbeat processing code where FleetAnnouncement is constructed (~line 12121):

```rust
let serving_rules = {
    let config = pyramid_state.config.read().await;
    if let Some(ref policy) = config.dispatch_policy {
        crate::fleet::derive_serving_rules(policy, &loaded_models)
    } else {
        vec![]
    }
};

let announcement = FleetAnnouncement {
    // ... existing fields ...
    models_loaded: loaded_models,    // keep for observability
    serving_rules,                   // add for routing
    total_queue_depth: {
        let q = compute_queue.queue.lock().await;
        q.total_depth()
    },
    // ...
};
```

### 16. Re-announce on ConfigSynced (main.rs)

In the ConfigSynced listener (~line 11230), after rebuilding DispatchPolicy:

Clone these Arc handles into the listener closure: `fleet_roster`, `auth_state`, `tunnel_state`, `compute_queue`. After policy reload, derive new serving_rules and announce:

```rust
// After dispatch policy rebuild:
let loaded_models = /* read from local_mode_state DB, same query as heartbeat */;
let serving_rules = derive_serving_rules(&new_policy, &loaded_models);
let roster = fleet_roster.read().await;
if !roster.peers.is_empty() {
    let announcement = FleetAnnouncement {
        node_id: auth_state.read().await.node_id.clone().unwrap_or_default(),
        tunnel_url: tunnel_state.read().await.tunnel_url.clone().unwrap_or_default(),
        serving_rules,
        models_loaded: loaded_models,
        total_queue_depth: compute_queue.queue.lock().await.total_depth(),
        operator_id: auth_state.read().await.operator_id.clone().unwrap_or_default(),
        name: None,
    };
    announce_to_fleet(&roster, &announcement).await;
}
```

### 17. Frontend: show serving_rules on peer cards (MarketDashboard.tsx)

Update TypeScript FleetPeer interface:
```typescript
interface FleetPeer {
    // ... existing ...
    serving_rules: string[];
    total_queue_depth: number;
}
```

Show serving_rules as tags alongside models_loaded on peer cards. Show total_queue_depth as a load indicator.

---

## Three-Node Behavior

A dispatches to B. B has `skip_fleet_dispatch: true`, so B's dispatch policy skips fleet. B resolves the rule to a local provider via `resolve_local_for_rule` (which only returns `is_local` entries). If B has no local GPU, `resolve_local_for_rule` returns `None`, the handler returns an error, and A falls through to its own next pool provider (e.g., OpenRouter). **No surprise cloud billing on fleet jobs.**

---

## Rule Name Convergence

Routing rule names are generated by:
- `local_mode.rs:commit_enable_local_mode` → hardcoded "ollama-catchall"
- Generative config system → LLM-generated names

For fleet to work, both nodes need at least one matching rule name. The default "ollama-catchall" matches for any two nodes with local mode enabled. For custom policies, the generative config system should be guided to use consistent rule naming (or the operator ensures consistency).

**Mitigation if names diverge:** Fleet dispatch fails gracefully (peer has no matching rule → falls through to local/cloud). Correct behavior — if the peer can't serve the requested capability, don't dispatch to it.

---

## What Stays the Same

- `FleetRoster` structure (peers HashMap, fleet_jwt)
- Fleet JWT authentication (unchanged)
- Fleet announce mechanism (fire-and-forget)
- Server endpoint URLs (`/v1/compute/fleet-dispatch`, `/v1/fleet/announce`)
- Compute queue integration (fleet jobs enter same queue via normal LLM path)
- `resolve_route` in dispatch_policy.rs (unchanged — stays pure)
- `models_loaded` on FleetPeer (kept for observability)
- Pool acquisition loop (llm.rs:859-904) unchanged

## What Gets Removed

- The fleet pre-check block in llm.rs (~line 627-688)
- `find_peer_for_model()` on FleetRoster
- `model` field on FleetDispatchRequest

## What Gets Added

- `is_local: bool` on RouteEntry (dispatch_policy.rs)
- `matched_rule_name: String` on ResolvedRoute (dispatch_policy.rs)
- `resolve_local_for_rule()` on DispatchPolicy (dispatch_policy.rs)
- `serving_rules: Vec<String>` + `total_queue_depth: usize` on FleetPeer and FleetAnnouncement
- `find_peer_for_rule()` on FleetRoster
- `derive_serving_rules()` function
- `skip_fleet_dispatch: bool` on LlmCallOptions
- `fleet_peer_id: Option<String>` + `fleet_peer_model: Option<String>` on LlmResponse
- Fleet Phase A handling in llm.rs (between resolve_route and pool loop)
- `fleet_dispatch_by_rule()` with configurable timeout
- `peer_model` on FleetDispatchResponse

## Files Modified

| File | Change |
|---|---|
| `dispatch_policy.rs` | `is_local` on RouteEntry. `matched_rule_name` on ResolvedRoute. `resolve_local_for_rule` method. |
| `llm.rs` | Delete pre-check (627-688). Add Phase A fleet handling before pool loop with provenance. Add `skip_fleet_dispatch` to LlmCallOptions. Filter fleet from providers before pool loop. `fleet_peer_id` + `fleet_peer_model` on LlmResponse. |
| `fleet.rs` | serving_rules + total_queue_depth on FleetPeer/Announcement. find_peer_for_rule. derive_serving_rules (uses is_local flag). fleet_dispatch_by_rule (configurable timeout). FleetDispatchRequest: rule_name only. FleetDispatchResponse: add peer_model. |
| `server.rs` | Fleet handler: resolve rule_name via resolve_local_for_rule. Return error if no local provider (no cloud fallback). Include peer_model in response. Fleet job goes through queue (compute_queue stays Some). |
| `main.rs` | Announce with serving_rules. Re-announce on ConfigSynced (clone fleet_roster, auth, tunnel, compute_queue handles). |
| `local_mode.rs` | Default dispatch policy: prepend `provider_id: fleet`, add `is_local: true` to ollama-local. |
| `MarketDashboard.tsx` | Show serving_rules + total_queue_depth on peer cards. |

## Verification

1. `cargo check` passes
2. Laptop dispatch policy has fleet in chain → fleet dispatch fires by rule_name
3. 5090 receives rule_name → resolves local model via `is_local` → executes → returns with peer_model
4. `skip_fleet_dispatch` prevents re-dispatch on receiving node
5. No fleet peers → "fleet" entry skipped, pool loop runs on remaining providers
6. Fleet failure → provider removed, pool loop continues with next providers
7. Model names never cross node boundaries — only rule names
8. `resolve_route` unchanged (pure, no fleet_roster param)
9. Pool acquisition loop unchanged (fleet filtered before it runs)
10. Fleet timeout reads from `route.max_wait_secs`, not hardcoded
11. `derive_serving_rules` uses `is_local` flag, not string matching
12. Three-node case: receiving node returns error if no local provider, no cloud billing
13. Fleet dispatch response includes `peer_model` for observability
14. `LlmResponse` carries `fleet_peer_id` + `fleet_peer_model` for StepContext/cost log
