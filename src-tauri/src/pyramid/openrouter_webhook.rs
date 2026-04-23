// pyramid/openrouter_webhook.rs — Phase 11 broadcast webhook receiver.
//
// Implements the HTTP handler + OTLP parser + correlation logic + leak
// detection sweep for the OpenRouter Broadcast integration per
// `docs/specs/evidence-triage-and-dadbear.md` Part 4.
//
// ── Design ──────────────────────────────────────────────────────────
//
// OpenRouter's Broadcast feature pushes every API call as an OTLP
// trace to user-configured destinations. Wire Node configures a
// Webhook destination pointing at `{tunnel_url}/hooks/openrouter`,
// served by this module. Each trace is:
//
//   1. Authenticated via a shared secret header (`X-Webhook-Secret`)
//      stored in `pyramid_providers.broadcast_config_json`. Constant-
//      time comparison (`subtle::ConstantTimeEq`).
//   2. Parsed out of OTLP JSON shape into a `BroadcastTrace` struct
//      carrying the attribute keys the spec defines
//      (`trace.metadata.*`, `gen_ai.usage.*`, etc.).
//   3. Correlated against `pyramid_cost_log` first by
//      `generation_id`, then by `(slug, step_name, model)` as a
//      fallback when the generation_id is missing.
//   4. If a match is found:
//      - Sets `broadcast_confirmed_at = now()`
//      - Stores the broadcast cost in `broadcast_cost_usd`
//      - Computes `|bc - ac| / ac`; if over the policy threshold,
//        flips status to `'discrepancy'` and emits
//        `CostReconciliationDiscrepancy`.
//      - **NEVER** rewrites `actual_cost` to match the broadcast. The
//        synchronous ledger is preserved intact so the user can audit
//        both sides of a disagreement.
//   5. If no match is found:
//      - Inserts a `pyramid_orphan_broadcasts` row with the full
//        payload for investigation
//      - Emits `OrphanBroadcastDetected` so the oversight page can
//        surface the potential credential leak.
//   6. Returns `200 OK`. OpenRouter does not retry.
//
// ── Test connection handling ────────────────────────────────────────
//
// When the user saves the webhook destination on the OpenRouter
// dashboard, OpenRouter sends a no-op test payload with
// `X-Test-Connection: true`. We accept either the header or an empty
// payload and return 200 without any correlation side effects.
//
// ── Authentication ──────────────────────────────────────────────────
//
// A publicly-exposed webhook without auth is a leak attack surface.
// The handler:
//   1. Reads `X-Webhook-Secret` from the request headers
//   2. Looks up the OpenRouter provider row in `pyramid_providers`
//   3. Parses `broadcast_config_json` (schema: `{ "secret": "..." }`)
//   4. Compares with `subtle::ConstantTimeEq::ct_eq` — the comparison
//      runs in constant time relative to the secret length so timing
//      oracles cannot leak the secret byte-by-byte.
//   5. On mismatch: returns 401 with NO secret value logged.
//   6. On no secret configured: returns 503 so first-time setup
//      doesn't look like a bug.

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use subtle::ConstantTimeEq;

use super::db::{self, CorrelatedCostLogRow};
use super::event_bus::{BuildEventBus, TaggedBuildEvent, TaggedKind};
use super::provider_health::{record_provider_error, CostReconciliationPolicy, ProviderErrorKind};

/// Broadcast payload decoded from the OTLP span attributes. The
/// webhook parses every span in the request and produces one
/// `BroadcastTrace` per span for correlation.
#[derive(Debug, Clone, Default)]
pub struct BroadcastTrace {
    pub generation_id: Option<String>,
    pub session_id: Option<String>,
    pub pyramid_slug: Option<String>,
    pub build_id: Option<String>,
    pub step_name: Option<String>,
    pub depth: Option<i64>,
    pub chunk_index: Option<i64>,
    pub chain_id: Option<String>,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub user: Option<String>,
    pub raw_attributes_json: String,
}

impl BroadcastTrace {
    /// True when a trace is effectively empty — no metadata,
    /// nothing to correlate. Used to skip test-connection no-op
    /// payloads without writing orphan rows for them.
    pub fn is_empty(&self) -> bool {
        self.generation_id.is_none()
            && self.session_id.is_none()
            && self.pyramid_slug.is_none()
            && self.build_id.is_none()
            && self.step_name.is_none()
            && self.model.is_none()
            && self.cost_usd.is_none()
    }
}

/// Outcome of processing a single broadcast trace. Returned so tests
/// can assert the correlation decision without relying on side
/// effects.
#[derive(Debug, Clone, PartialEq)]
pub enum BroadcastOutcome {
    /// Correlated to a cost_log row and within the discrepancy
    /// threshold. Row's `broadcast_confirmed_at` is now set.
    Confirmed { cost_log_id: i64 },
    /// Correlated to a cost_log row but the broadcast cost diverged
    /// beyond the threshold. Row's status is now `'discrepancy'`.
    Discrepancy {
        cost_log_id: i64,
        synchronous_cost_usd: Option<f64>,
        broadcast_cost_usd: Option<f64>,
        ratio: Option<f64>,
    },
    /// Correlated to a cost_log row whose synchronous path failed
    /// (status = 'estimated'). Broadcast recovery filled in the
    /// authoritative values and flipped status to `'broadcast'`.
    Recovered { cost_log_id: i64 },
    /// No matching row. An orphan broadcast row has been inserted.
    Orphan { orphan_id: i64 },
    /// Empty test-connection ping. Handler returned 200 with no
    /// side effects.
    TestPing,
}

/// Authentication error returned from the handler. Mapped to the
/// HTTP response codes per the spec:
///   NoSecretConfigured → 503 (first-time setup)
///   MissingHeader      → 401
///   WrongSecret        → 401
#[derive(Debug, Clone, PartialEq)]
pub enum WebhookAuthError {
    NoSecretConfigured,
    MissingHeader,
    WrongSecret,
}

/// Verify the webhook auth header against the OpenRouter provider's
/// configured secret. Constant-time comparison; the secret never
/// appears in error messages or log output.
///
/// Returns `Ok(())` if auth passes. Returns the specific error
/// variant for the handler to translate into an HTTP status code.
pub fn verify_webhook_secret(
    conn: &rusqlite::Connection,
    provider_id: &str,
    header_value: Option<&str>,
) -> std::result::Result<(), WebhookAuthError> {
    let expected =
        load_webhook_secret(conn, provider_id).map_err(|_| WebhookAuthError::NoSecretConfigured)?;
    let Some(expected) = expected else {
        return Err(WebhookAuthError::NoSecretConfigured);
    };
    let Some(got) = header_value else {
        return Err(WebhookAuthError::MissingHeader);
    };
    // Constant-time equality over the byte slices.
    if got.as_bytes().ct_eq(expected.as_bytes()).into() {
        Ok(())
    } else {
        Err(WebhookAuthError::WrongSecret)
    }
}

/// Load the shared secret from `pyramid_providers.broadcast_config_json`.
/// Schema: `{ "secret": "<value>" }`. Returns `Ok(None)` when no
/// provider row exists or the config does not set a secret yet.
fn load_webhook_secret(conn: &rusqlite::Connection, provider_id: &str) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT broadcast_config_json FROM pyramid_providers WHERE id = ?1")?;
    let mut rows = stmt.query(rusqlite::params![provider_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let cfg_json: Option<String> = row.get(0)?;
    let Some(cfg_json) = cfg_json else {
        return Ok(None);
    };
    if cfg_json.trim().is_empty() {
        return Ok(None);
    }
    let parsed: Value = match serde_json::from_str(&cfg_json) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    Ok(parsed
        .get("secret")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty()))
}

/// Parse an OTLP JSON payload into zero or more `BroadcastTrace`
/// entries. OTLP shape:
///
/// ```json
/// {
///   "resourceSpans": [
///     { "scopeSpans": [
///         { "spans": [
///             { "attributes": [ { "key": "...", "value": { "stringValue": "..." } } ] }
///         ] }
///     ] }
///   ]
/// }
/// ```
///
/// We walk every span and extract the attributes we care about. An
/// empty input produces an empty Vec (test-connection pings).
pub fn parse_otlp_payload(payload: &Value) -> Result<Vec<BroadcastTrace>> {
    let Some(resource_spans) = payload.get("resourceSpans").and_then(|v| v.as_array()) else {
        return Ok(vec![]);
    };

    let mut out = Vec::new();
    for rs in resource_spans {
        let Some(scope_spans) = rs.get("scopeSpans").and_then(|v| v.as_array()) else {
            continue;
        };
        for ss in scope_spans {
            let Some(spans) = ss.get("spans").and_then(|v| v.as_array()) else {
                continue;
            };
            for span in spans {
                out.push(parse_single_span(span));
            }
        }
    }
    Ok(out)
}

fn parse_single_span(span: &Value) -> BroadcastTrace {
    let mut trace = BroadcastTrace::default();
    let Some(attrs) = span.get("attributes").and_then(|v| v.as_array()) else {
        trace.raw_attributes_json = span.to_string();
        return trace;
    };

    trace.raw_attributes_json = serde_json::to_string(attrs).unwrap_or_default();

    for attr in attrs {
        let Some(key) = attr.get("key").and_then(|v| v.as_str()) else {
            continue;
        };
        let value = attr.get("value");

        // OpenRouter's canonical attribute keys per the spec's
        // "OTLP attribute key conventions" table.
        match key {
            "gen_ai.request.model" => {
                trace.model = extract_string_value(value);
            }
            "gen_ai.usage.prompt_tokens" => {
                trace.prompt_tokens = extract_int_value(value);
            }
            "gen_ai.usage.completion_tokens" => {
                trace.completion_tokens = extract_int_value(value);
            }
            "session.id" => {
                trace.session_id = extract_string_value(value);
            }
            "user.id" => {
                trace.user = extract_string_value(value);
            }
            "trace.metadata.pyramid_slug" => {
                trace.pyramid_slug = extract_string_value(value);
            }
            "trace.metadata.build_id" => {
                trace.build_id = extract_string_value(value);
            }
            "trace.metadata.step_name" => {
                trace.step_name = extract_string_value(value);
            }
            "trace.metadata.depth" => {
                trace.depth = extract_int_value(value);
            }
            "trace.metadata.chunk_index" => {
                trace.chunk_index = extract_int_value(value);
            }
            "trace.metadata.chain_id" => {
                trace.chain_id = extract_string_value(value);
            }
            "trace.metadata.generation_id" => {
                if trace.generation_id.is_none() {
                    trace.generation_id = extract_string_value(value);
                }
            }
            "gen_ai.response.id" | "gen_ai.openrouter.generation_id" => {
                trace.generation_id = extract_string_value(value);
            }
            _ => {
                // Cost key path is not standardized — walk for any
                // `.cost` suffix under `gen_ai.usage.*`.
                if key.contains("cost")
                    && (key.starts_with("gen_ai.usage.") || key.starts_with("gen_ai.cost"))
                {
                    if let Some(n) = extract_float_value(value) {
                        trace.cost_usd = Some(n);
                    }
                }
            }
        }
    }

    // Session id fallback → split into slug/build_id when the
    // explicit trace.metadata.* keys are missing. Helps the
    // correlator even when attribute coverage is partial.
    if let Some(session_id) = &trace.session_id {
        if let Some((slug, build_id)) = session_id.split_once('/') {
            if trace.pyramid_slug.is_none() {
                trace.pyramid_slug = Some(slug.to_string());
            }
            if trace.build_id.is_none() {
                trace.build_id = Some(build_id.to_string());
            }
        }
    }

    trace
}

fn extract_string_value(value: Option<&Value>) -> Option<String> {
    let v = value?;
    if let Some(s) = v.get("stringValue").and_then(|x| x.as_str()) {
        return Some(s.to_string());
    }
    // Some OTLP producers serialize bool/int/double containers at
    // the top level rather than nested; accept raw string too.
    v.as_str().map(|s| s.to_string())
}

fn extract_int_value(value: Option<&Value>) -> Option<i64> {
    let v = value?;
    if let Some(s) = v.get("intValue").and_then(|x| x.as_str()) {
        return s.parse::<i64>().ok();
    }
    if let Some(n) = v.get("intValue").and_then(|x| x.as_i64()) {
        return Some(n);
    }
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
}

fn extract_float_value(value: Option<&Value>) -> Option<f64> {
    let v = value?;
    if let Some(n) = v.get("doubleValue").and_then(|x| x.as_f64()) {
        return Some(n);
    }
    if let Some(s) = v.get("doubleValue").and_then(|x| x.as_str()) {
        if let Ok(n) = s.parse::<f64>() {
            return Some(n);
        }
    }
    if let Some(s) = v.get("stringValue").and_then(|x| x.as_str()) {
        if let Ok(n) = s.parse::<f64>() {
            return Some(n);
        }
    }
    v.as_f64()
}

/// Process a single broadcast trace against the cost log. Runs the
/// correlation → confirmation pipeline and emits events as needed.
/// Returns the outcome so tests can inspect the decision.
pub fn process_trace(
    conn: &rusqlite::Connection,
    trace: &BroadcastTrace,
    provider_id_for_health: &str,
    policy: &CostReconciliationPolicy,
    bus: Option<&Arc<BuildEventBus>>,
) -> Result<BroadcastOutcome> {
    if trace.is_empty() {
        return Ok(BroadcastOutcome::TestPing);
    }

    // Pull slug for fallback correlation. Prefer the explicit
    // pyramid_slug attribute; otherwise split the session_id.
    let slug_for_lookup = trace.pyramid_slug.clone().or_else(|| {
        trace
            .session_id
            .as_ref()
            .and_then(|s| s.split_once('/').map(|p| p.0.to_string()))
    });

    let matched = db::correlate_broadcast_to_cost_log(
        conn,
        trace.generation_id.as_deref(),
        slug_for_lookup.as_deref(),
        trace.step_name.as_deref(),
        trace.model.as_deref(),
    )?;

    let Some(row) = matched else {
        return insert_orphan_and_emit(conn, trace, provider_id_for_health, bus);
    };

    // Recovery path: synchronous ledger failed earlier and the row
    // is still flagged 'estimated'. Use the broadcast to populate
    // authoritative values and flip status to 'broadcast'.
    if row.reconciliation_status.as_deref() == Some("estimated") {
        if let Some(cost) = trace.cost_usd {
            db::record_broadcast_recovery(
                conn,
                row.id,
                cost,
                trace.prompt_tokens,
                trace.completion_tokens,
                &trace.raw_attributes_json,
            )?;
            return Ok(BroadcastOutcome::Recovered {
                cost_log_id: row.id,
            });
        }
    }

    // Normal path: compute discrepancy ratio and decide whether
    // this is a clean confirmation or a fail-loud discrepancy.
    let sync_cost = row.actual_cost;
    let broadcast_cost = trace.cost_usd;
    let ratio = match (sync_cost, broadcast_cost) {
        (Some(ac), Some(bc)) if ac.abs() > f64::EPSILON => Some(((ac - bc).abs() / ac.abs()).abs()),
        (Some(ac), Some(bc)) if ac.abs() <= f64::EPSILON => {
            // Divide-by-zero: if the sync cost is zero and the
            // broadcast says nonzero, the ratio is effectively
            // infinite. Treat anything above EPSILON as a
            // discrepancy.
            if bc.abs() > f64::EPSILON {
                Some(1.0)
            } else {
                Some(0.0)
            }
        }
        _ => None,
    };

    let is_discrepancy = ratio.map(|r| r > policy.discrepancy_ratio).unwrap_or(false);

    db::record_broadcast_confirmation(
        conn,
        row.id,
        broadcast_cost,
        &trace.raw_attributes_json,
        ratio,
        is_discrepancy,
    )?;

    if is_discrepancy {
        emit_discrepancy(bus, &row, sync_cost, broadcast_cost, ratio);
        // Feed the provider health state machine so repeated
        // discrepancies can degrade the provider.
        let provider_for_health = row
            .provider_id
            .clone()
            .unwrap_or_else(|| provider_id_for_health.to_string());
        let _ = record_provider_error(
            conn,
            &provider_for_health,
            ProviderErrorKind::CostDiscrepancy,
            policy,
            bus,
        );
        return Ok(BroadcastOutcome::Discrepancy {
            cost_log_id: row.id,
            synchronous_cost_usd: sync_cost,
            broadcast_cost_usd: broadcast_cost,
            ratio,
        });
    }

    Ok(BroadcastOutcome::Confirmed {
        cost_log_id: row.id,
    })
}

fn insert_orphan_and_emit(
    conn: &rusqlite::Connection,
    trace: &BroadcastTrace,
    provider_id: &str,
    bus: Option<&Arc<BuildEventBus>>,
) -> Result<BroadcastOutcome> {
    let orphan_id = db::insert_orphan_broadcast(
        conn,
        Some(provider_id),
        trace.generation_id.as_deref(),
        trace.session_id.as_deref(),
        trace.pyramid_slug.as_deref(),
        trace.build_id.as_deref(),
        trace.step_name.as_deref(),
        trace.model.as_deref(),
        trace.cost_usd,
        trace.prompt_tokens,
        trace.completion_tokens,
        &trace.raw_attributes_json,
    )?;
    if let Some(bus) = bus {
        let _ = bus.tx.send(TaggedBuildEvent {
            slug: trace.pyramid_slug.clone().unwrap_or_default(),
            kind: TaggedKind::OrphanBroadcastDetected {
                orphan_id,
                provider_id: Some(provider_id.to_string()),
                generation_id: trace.generation_id.clone(),
                session_id: trace.session_id.clone(),
                pyramid_slug: trace.pyramid_slug.clone(),
                step_name: trace.step_name.clone(),
                model: trace.model.clone(),
                cost_usd: trace.cost_usd,
            },
        });
    }
    tracing::warn!(
        orphan_id = orphan_id,
        generation_id = ?trace.generation_id,
        session_id = ?trace.session_id,
        model = ?trace.model,
        "orphan broadcast detected — potential credential exfiltration"
    );
    Ok(BroadcastOutcome::Orphan { orphan_id })
}

fn emit_discrepancy(
    bus: Option<&Arc<BuildEventBus>>,
    row: &CorrelatedCostLogRow,
    sync_cost: Option<f64>,
    broadcast_cost: Option<f64>,
    ratio: Option<f64>,
) {
    if let Some(bus) = bus {
        let _ = bus.tx.send(TaggedBuildEvent {
            slug: row.slug.clone(),
            kind: TaggedKind::CostReconciliationDiscrepancy {
                cost_log_id: row.id,
                step_name: row.step_name.clone(),
                provider_id: row.provider_id.clone(),
                synchronous_cost_usd: sync_cost,
                broadcast_cost_usd: broadcast_cost,
                discrepancy_ratio: ratio,
            },
        });
    }
    tracing::warn!(
        cost_log_id = row.id,
        slug = row.slug.as_str(),
        step_name = ?row.step_name,
        sync_cost = ?sync_cost,
        broadcast_cost = ?broadcast_cost,
        ratio = ?ratio,
        "cost reconciliation discrepancy detected"
    );
}

/// Leak detection sweep. Called on a cadence from the background
/// task in `main.rs`. Flips synchronous rows past the grace period
/// whose broadcast never arrived.
pub fn run_leak_sweep(
    conn: &rusqlite::Connection,
    policy: &CostReconciliationPolicy,
    bus: Option<&Arc<BuildEventBus>>,
) -> Result<usize> {
    if !policy.broadcast_required {
        return Ok(0);
    }
    let flipped = db::sweep_broadcast_missing(conn, policy.broadcast_grace_period_secs)?;
    if flipped > 0 {
        if let Some(bus) = bus {
            let _ = bus.tx.send(TaggedBuildEvent {
                slug: String::new(),
                kind: TaggedKind::BroadcastMissing {
                    rows_flipped: flipped,
                    grace_period_secs: policy.broadcast_grace_period_secs,
                },
            });
        }
        tracing::warn!(
            rows_flipped = flipped,
            grace_period_secs = policy.broadcast_grace_period_secs,
            "leak detection sweep flipped unconfirmed rows to 'broadcast_missing'"
        );
    }
    Ok(flipped)
}

/// Request struct for the warp filter. Wraps the raw JSON body so
/// the filter can decode it with serde.
#[derive(Debug, Deserialize)]
pub struct WebhookRequest {
    #[serde(flatten)]
    pub payload: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
    use rusqlite::Connection;
    use serde_json::json;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        // Seed a slug row so cost_log FK constraints don't reject
        // the test inserts. `content_type` is NOT NULL with a CHECK
        // constraint — use `code` to satisfy it.
        conn.execute(
            "INSERT OR IGNORE INTO pyramid_slugs (slug, content_type, source_path)
             VALUES (?1, 'code', '')",
            rusqlite::params!["test-slug"],
        )
        .unwrap();
        conn
    }

    fn sample_otlp(
        generation_id: &str,
        slug: &str,
        build_id: &str,
        step: &str,
        model: &str,
        cost: f64,
    ) -> Value {
        json!({
            "resourceSpans": [{
                "resource": {},
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "otlp-trace",
                        "spanId": "otlp-span",
                        "name": "chat",
                        "attributes": [
                            { "key": "gen_ai.request.model", "value": { "stringValue": model } },
                            { "key": "gen_ai.usage.prompt_tokens", "value": { "intValue": "100" } },
                            { "key": "gen_ai.usage.completion_tokens", "value": { "intValue": "50" } },
                            { "key": "gen_ai.usage.cost", "value": { "doubleValue": cost } },
                            { "key": "gen_ai.response.id", "value": { "stringValue": generation_id } },
                            { "key": "session.id", "value": { "stringValue": format!("{slug}/{build_id}") } },
                            { "key": "trace.metadata.pyramid_slug", "value": { "stringValue": slug } },
                            { "key": "trace.metadata.build_id", "value": { "stringValue": build_id } },
                            { "key": "trace.metadata.step_name", "value": { "stringValue": step } },
                            { "key": "trace.metadata.depth", "value": { "intValue": "0" } }
                        ]
                    }]
                }]
            }]
        })
    }

    fn insert_synchronous_cost_log(
        conn: &Connection,
        slug: &str,
        model: &str,
        step: &str,
        generation_id: &str,
        actual_cost: f64,
    ) -> i64 {
        db::insert_cost_log_synchronous(
            conn,
            slug,
            "build_step",
            model,
            100,
            50,
            0.0,
            "test",
            None,
            None,
            Some("test-chain"),
            Some(step),
            Some("fast_extract"),
            Some(42),
            Some(generation_id),
            None,
            Some(actual_cost),
            Some(100),
            Some(50),
            Some("openrouter"),
            "synchronous",
        )
        .unwrap()
    }

    #[test]
    fn parse_otlp_extracts_full_metadata() {
        let payload = sample_otlp(
            "gen-abc123",
            "test-slug",
            "build-1",
            "source_extract",
            "openai/gpt-4",
            0.00123,
        );
        let traces = parse_otlp_payload(&payload).unwrap();
        assert_eq!(traces.len(), 1);
        let t = &traces[0];
        assert_eq!(t.generation_id.as_deref(), Some("gen-abc123"));
        assert_eq!(t.pyramid_slug.as_deref(), Some("test-slug"));
        assert_eq!(t.build_id.as_deref(), Some("build-1"));
        assert_eq!(t.step_name.as_deref(), Some("source_extract"));
        assert_eq!(t.model.as_deref(), Some("openai/gpt-4"));
        assert_eq!(t.prompt_tokens, Some(100));
        assert_eq!(t.completion_tokens, Some(50));
        assert_eq!(t.cost_usd, Some(0.00123));
        assert_eq!(t.depth, Some(0));
    }

    #[test]
    fn parse_otlp_empty_resource_spans_returns_empty() {
        let payload = json!({ "resourceSpans": [] });
        assert!(parse_otlp_payload(&payload).unwrap().is_empty());
    }

    #[test]
    fn session_id_fallback_populates_slug_and_build_id() {
        let payload = json!({
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "attributes": [
                            { "key": "session.id", "value": { "stringValue": "my-slug/build-xyz" } },
                            { "key": "gen_ai.usage.cost", "value": { "doubleValue": 0.0042 } }
                        ]
                    }]
                }]
            }]
        });
        let traces = parse_otlp_payload(&payload).unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].pyramid_slug.as_deref(), Some("my-slug"));
        assert_eq!(traces[0].build_id.as_deref(), Some("build-xyz"));
    }

    #[test]
    fn correlates_by_generation_id() {
        let conn = mem_conn();
        let id = insert_synchronous_cost_log(
            &conn,
            "test-slug",
            "openai/gpt-4",
            "source_extract",
            "gen-abc",
            0.00123,
        );
        let payload = sample_otlp(
            "gen-abc",
            "test-slug",
            "build-1",
            "source_extract",
            "openai/gpt-4",
            0.00123,
        );
        let traces = parse_otlp_payload(&payload).unwrap();
        let policy = CostReconciliationPolicy::default();
        let outcome = process_trace(&conn, &traces[0], "openrouter", &policy, None).unwrap();
        assert_eq!(outcome, BroadcastOutcome::Confirmed { cost_log_id: id });

        // Verify the row is now confirmed.
        let (status, ba): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT reconciliation_status, broadcast_confirmed_at FROM pyramid_cost_log WHERE id = ?1",
                rusqlite::params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status.as_deref(), Some("synchronous"));
        assert!(ba.is_some());
    }

    #[test]
    fn correlates_by_session_fallback_when_generation_id_missing() {
        let conn = mem_conn();
        let id = insert_synchronous_cost_log(
            &conn,
            "test-slug",
            "openai/gpt-4",
            "source_extract",
            "gen-xyz",
            0.00222,
        );
        // Remove the generation_id from the payload so the fallback
        // correlates on (slug, step_name, model).
        let payload = json!({
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "attributes": [
                            { "key": "gen_ai.request.model", "value": { "stringValue": "openai/gpt-4" } },
                            { "key": "session.id", "value": { "stringValue": "test-slug/build-1" } },
                            { "key": "trace.metadata.step_name", "value": { "stringValue": "source_extract" } },
                            { "key": "gen_ai.usage.cost", "value": { "doubleValue": 0.00222 } }
                        ]
                    }]
                }]
            }]
        });
        let traces = parse_otlp_payload(&payload).unwrap();
        let policy = CostReconciliationPolicy::default();
        let outcome = process_trace(&conn, &traces[0], "openrouter", &policy, None).unwrap();
        assert_eq!(outcome, BroadcastOutcome::Confirmed { cost_log_id: id });
    }

    #[test]
    fn no_match_produces_orphan() {
        let conn = mem_conn();
        // No prior insert — broadcast has nothing to correlate.
        let payload = sample_otlp(
            "gen-ghost",
            "unknown-slug",
            "build-?",
            "phantom_step",
            "openai/gpt-4",
            1.23,
        );
        let traces = parse_otlp_payload(&payload).unwrap();
        let policy = CostReconciliationPolicy::default();
        let outcome = process_trace(&conn, &traces[0], "openrouter", &policy, None).unwrap();
        match outcome {
            BroadcastOutcome::Orphan { orphan_id } => {
                assert!(orphan_id > 0);
                let count: i64 = conn
                    .query_row("SELECT COUNT(*) FROM pyramid_orphan_broadcasts", [], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert_eq!(count, 1);
            }
            other => panic!("expected Orphan, got {other:?}"),
        }
    }

    #[test]
    fn discrepancy_beyond_threshold_flips_status() {
        let conn = mem_conn();
        let id = insert_synchronous_cost_log(
            &conn,
            "test-slug",
            "openai/gpt-4",
            "source_extract",
            "gen-drift",
            1.00,
        );
        // Broadcast cost is 1.50 → ratio of 0.5 > default 0.10
        // → discrepancy.
        let payload = sample_otlp(
            "gen-drift",
            "test-slug",
            "build-1",
            "source_extract",
            "openai/gpt-4",
            1.50,
        );
        let traces = parse_otlp_payload(&payload).unwrap();
        let policy = CostReconciliationPolicy::default();
        let outcome = process_trace(&conn, &traces[0], "openrouter", &policy, None).unwrap();
        match outcome {
            BroadcastOutcome::Discrepancy {
                cost_log_id, ratio, ..
            } => {
                assert_eq!(cost_log_id, id);
                assert!(ratio.unwrap() > 0.10);
                let status: String = conn
                    .query_row(
                        "SELECT reconciliation_status FROM pyramid_cost_log WHERE id = ?1",
                        rusqlite::params![id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(status, "discrepancy");
                // actual_cost preserved (NOT rewritten to match broadcast).
                let actual: f64 = conn
                    .query_row(
                        "SELECT actual_cost FROM pyramid_cost_log WHERE id = ?1",
                        rusqlite::params![id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert!((actual - 1.00).abs() < f64::EPSILON);
                // broadcast_cost_usd stored separately.
                let bc: f64 = conn
                    .query_row(
                        "SELECT broadcast_cost_usd FROM pyramid_cost_log WHERE id = ?1",
                        rusqlite::params![id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert!((bc - 1.50).abs() < f64::EPSILON);
            }
            other => panic!("expected Discrepancy, got {other:?}"),
        }
    }

    #[test]
    fn small_discrepancy_below_threshold_confirms() {
        let conn = mem_conn();
        let id = insert_synchronous_cost_log(
            &conn,
            "test-slug",
            "openai/gpt-4",
            "source_extract",
            "gen-small",
            1.00,
        );
        // 3% drift, below 10% threshold.
        let payload = sample_otlp(
            "gen-small",
            "test-slug",
            "build-1",
            "source_extract",
            "openai/gpt-4",
            1.03,
        );
        let traces = parse_otlp_payload(&payload).unwrap();
        let policy = CostReconciliationPolicy::default();
        let outcome = process_trace(&conn, &traces[0], "openrouter", &policy, None).unwrap();
        assert_eq!(outcome, BroadcastOutcome::Confirmed { cost_log_id: id });
    }

    #[test]
    fn leak_sweep_flips_unconfirmed_old_rows() {
        let conn = mem_conn();
        // Insert a row 20 minutes in the past.
        conn.execute(
            "INSERT INTO pyramid_cost_log (
                 slug, operation, model, input_tokens, output_tokens,
                 estimated_cost, source, chain_id, step_name, tier,
                 generation_id, actual_cost, provider_id,
                 reconciliation_status, created_at
             ) VALUES (
                 ?1, 'build_step', 'openai/gpt-4', 100, 50,
                 0.0, 'test', 'c', 's', 'fast_extract',
                 'gen-old', 0.001, 'openrouter',
                 'synchronous', datetime('now', '-20 minutes')
             )",
            rusqlite::params!["test-slug"],
        )
        .unwrap();
        let policy = CostReconciliationPolicy::default();
        let flipped = run_leak_sweep(&conn, &policy, None).unwrap();
        assert_eq!(flipped, 1);
        let status: String = conn
            .query_row(
                "SELECT reconciliation_status FROM pyramid_cost_log WHERE generation_id = 'gen-old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "broadcast_missing");
    }

    #[test]
    fn leak_sweep_respects_broadcast_required_false() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO pyramid_cost_log (
                 slug, operation, model, input_tokens, output_tokens,
                 estimated_cost, source, chain_id, step_name, tier,
                 generation_id, actual_cost, provider_id,
                 reconciliation_status, created_at
             ) VALUES (
                 ?1, 'build_step', 'openai/gpt-4', 100, 50,
                 0.0, 'test', 'c', 's', 'fast_extract',
                 'gen-new', 0.001, 'openrouter',
                 'synchronous', datetime('now', '-20 minutes')
             )",
            rusqlite::params!["test-slug"],
        )
        .unwrap();
        let policy = CostReconciliationPolicy {
            broadcast_required: false,
            ..CostReconciliationPolicy::default()
        };
        let flipped = run_leak_sweep(&conn, &policy, None).unwrap();
        assert_eq!(flipped, 0);
    }

    #[test]
    fn test_connection_ping_is_no_op() {
        let conn = mem_conn();
        let policy = CostReconciliationPolicy::default();
        let trace = BroadcastTrace::default();
        let outcome = process_trace(&conn, &trace, "openrouter", &policy, None).unwrap();
        assert_eq!(outcome, BroadcastOutcome::TestPing);
    }

    #[test]
    fn webhook_auth_rejects_missing_header() {
        let conn = mem_conn();
        // Configure a secret on the default openrouter provider row.
        conn.execute(
            "UPDATE pyramid_providers SET broadcast_config_json = ?1 WHERE id = 'openrouter'",
            rusqlite::params![r#"{"secret":"s3cret-value"}"#],
        )
        .unwrap();
        let result = verify_webhook_secret(&conn, "openrouter", None);
        assert_eq!(result, Err(WebhookAuthError::MissingHeader));
    }

    #[test]
    fn webhook_auth_rejects_wrong_secret() {
        let conn = mem_conn();
        conn.execute(
            "UPDATE pyramid_providers SET broadcast_config_json = ?1 WHERE id = 'openrouter'",
            rusqlite::params![r#"{"secret":"s3cret-value"}"#],
        )
        .unwrap();
        let result = verify_webhook_secret(&conn, "openrouter", Some("wrong"));
        assert_eq!(result, Err(WebhookAuthError::WrongSecret));
    }

    #[test]
    fn webhook_auth_accepts_correct_secret() {
        let conn = mem_conn();
        conn.execute(
            "UPDATE pyramid_providers SET broadcast_config_json = ?1 WHERE id = 'openrouter'",
            rusqlite::params![r#"{"secret":"s3cret-value"}"#],
        )
        .unwrap();
        let result = verify_webhook_secret(&conn, "openrouter", Some("s3cret-value"));
        assert!(result.is_ok());
    }

    #[test]
    fn webhook_auth_503_when_no_secret_configured() {
        let conn = mem_conn();
        // Default row has broadcast_config_json = NULL.
        let result = verify_webhook_secret(&conn, "openrouter", Some("anything"));
        assert_eq!(result, Err(WebhookAuthError::NoSecretConfigured));
    }

    #[test]
    fn recovery_path_fills_actual_cost_on_estimated_row() {
        let conn = mem_conn();
        // Insert an 'estimated' row (the primary path failed).
        let id = db::insert_cost_log_synchronous(
            &conn,
            "test-slug",
            "build_step",
            "openai/gpt-4",
            100,
            50,
            0.0,
            "test",
            None,
            None,
            Some("test-chain"),
            Some("source_extract"),
            Some("fast_extract"),
            Some(42),
            Some("gen-recover"),
            None,
            None, // actual_cost = None → treated as 'estimated' in sibling assertions
            None,
            None,
            Some("openrouter"),
            "estimated",
        )
        .unwrap();
        let payload = sample_otlp(
            "gen-recover",
            "test-slug",
            "build-1",
            "source_extract",
            "openai/gpt-4",
            0.00345,
        );
        let traces = parse_otlp_payload(&payload).unwrap();
        let policy = CostReconciliationPolicy::default();
        let outcome = process_trace(&conn, &traces[0], "openrouter", &policy, None).unwrap();
        assert_eq!(outcome, BroadcastOutcome::Recovered { cost_log_id: id });
        let status: String = conn
            .query_row(
                "SELECT reconciliation_status FROM pyramid_cost_log WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "broadcast");
        let actual: f64 = conn
            .query_row(
                "SELECT actual_cost FROM pyramid_cost_log WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!((actual - 0.00345).abs() < f64::EPSILON);
    }
}
