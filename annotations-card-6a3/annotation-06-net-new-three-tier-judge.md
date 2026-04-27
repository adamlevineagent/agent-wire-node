# Annotation 6 — net-new: Three-tier judge architecture

```yaml
contribution_type: annotation
annotation_verb: delta-finding
target: 33-judge-starter.md#the-three-tiers
body:
  axis: net-new
  finding: >
    V2 introduces a three-tier judge role (rule-based / small-LLM / full-LLM) with
    operator-configurable escalation cascade as a dedicated throughput primitive that
    shapes how often substrate work fires. Agent-wire-node embeds judgment in staleness
    check code paths (stale_helpers.rs dispatch_file_stale_check, dadbear_compiler.rs
    map_event_to_primitive) with no explicit tier architecture, no operator-configurable
    escalation, and no tier-distribution telemetry. V2's judge is a generic role that
    answers different questions for different callers (cascade: "did this change
    materially?"; reconciler: "same concern or different?"; oracle: "can substrate
    answer this now?"; gap-dispatcher: "should we pursue this gap?"). The three tiers
    are capabilities, not mandated usage — operators configure cascade to route every
    question to one tier or escalate progressively. This is genuinely new substrate
    with no agent-wire-node counterpart.
  evidence:
    v2_citation: "33-judge-starter.md § Purpose (lines 11-18); § The three tiers (lines 58-97); § Tier selection policy (lines 98-115); § Chain body (lines 118-249)"
    legacy_citation: "src-tauri/src/pyramid/stale_helpers.rs dispatch_file_stale_check (embedded LLM triage, lines 1-80); src-tauri/src/pyramid/dadbear_compiler.rs (compile-time work item gating)"
  vocab_ref: vocab/playful/vocabulary_entry/v1
  dict_ref: dict/playful/master/v1
  generalized_understanding: >
    The three-tier judge is v2's throughput primitive — it governs the cost/signal
    trade-off for every substrate decision. Rule-based tier handles mechanical checks
    (staleness ratio >0.7 = material, hash-identity, empty-context, high-ratio,
    reconciliation cheap-check) at <5ms with no token cost. Small-LLM triages ambiguous
    cases with a concise prompt. Full-LLM handles genuinely hard cases with the
    operator's frontier model. The default cascade is rule → small → full with ambiguity
    escalation, but operators configure per-pyramid. Key design properties:
    (a) budget_hint (low/medium/high) overrides escalation per call site;
    (b) skip_tiers enables deterministic testing; (c) every invocation emits telemetry
    (tier_reached, decision, confidence, time_ms, cost_tokens) — operators read
    distributions against their own expectations, not mandated thresholds;
    (d) the 60% posture notes acknowledge that rule-tier rules are starter-set only
    and will need tuning as real usage exposes miscalibrations. Judge never blocks
    callers — worst case returns decision=ambiguous with confidence=low. The judge
    is bound via role-binding (31) so operators can swap in custom judges per-pyramid.
```

**Axis label:** net-new
**V2 citation:** `33-judge-starter.md` § Purpose (lines 11–18) + § The three tiers (lines 58–115) + § Chain body (lines 118–249)
**Legacy citation:** N/A — no counterpart in agent-wire-node
**Vocab ref:** `vocab/playful/vocabulary_entry/v1`
**Dict ref:** `dict/playful/master/v1`
