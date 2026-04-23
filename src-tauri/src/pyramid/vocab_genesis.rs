// pyramid/vocab_genesis.rs — Genesis vocabulary seed tables for Phase 6c-A.
//
// Hardcoded-vocabulary kill: the 11 AnnotationType variants, 4 NodeShape
// variants, and 11 role names (formerly split across `GENESIS_BINDINGS` +
// the cascade_handler per-slug seed) all live here as const tables.
// `vocab_entries::seed_genesis_vocabulary` iterates these on every
// `init_pyramid_db` and idempotently publishes each missing entry into
// `pyramid_config_contributions` as a `vocabulary_entry` subtype row.
//
// Per the architectural-lens principle (Wire's build pipeline is itself
// contributions — an agent should be able to improve the system), these
// seed tables are the ONLY place the genesis strings are hardcoded.
// Phase 6c-B / C / D flipped the consumers (AnnotationType enum, MCP/FE
// constants, NodeShape enum, role_binding::GENESIS_BINDINGS) to read from
// the contribution-driven registry that Phase 6c-A shipped.
//
// Shape notes:
//   - annotation types carry a `reactive` flag. `steel_man` + `red_team`
//     are `true` today — Phase 6c-B's `process_annotation_hook` emits
//     `annotation_reacted` observation events on any reactive type,
//     which Phase 7 will consume for chain dispatch. The four
//     "next-v5" reactive verbs (`hypothesis`, `gap`,
//     `purpose_declaration`, `purpose_shift`) are NOT in the genesis
//     tuple below — adding them is a vocab publish (contribution
//     write), not a code deploy, per the 6c-B flip.
//   - annotation types also carry a `creates_delta` flag (6c-B). True
//     for `correction` only in genesis; lifts the pre-v5 hardcoded
//     `AnnotationType::Correction => create_delta(...)` arm into vocab.
//   - role names carry `handler_chain_id` values. Phase 6c-D deleted the
//     parallel `GENESIS_BINDINGS` const in `role_binding.rs`; role-binding
//     seeding now reads this registry directly. `cascade_handler` is
//     included here too — `db::create_slug` still seeds it separately
//     per-slug because its default depends on fresh-vs-backfilled; the
//     vocab entry documents its canonical fresh-pyramid default
//     (judge-gated).
//   - node shapes have no `handler_chain_id` today (shapes don't
//     dispatch on their own — they govern how nodes render / what
//     payload a node carries).

// ── Annotation Types (11 genesis entries) ───────────────────────────
//
// Tuple shape: (name, description, handler_chain_id, reactive, creates_delta)
//
// Phase 6c-B added `creates_delta` — before v5 `process_annotation_hook`
// had a hardcoded `AnnotationType::Correction => create_delta(...)` arm.
// That arm is now vocab-driven: the hook reads `creates_delta` from the
// vocab entry, so operators can publish a new annotation_type that also
// creates deltas (e.g. a future `counter_correction`) with a contribution
// write, no code deploy. Only `correction` carries `creates_delta = true`
// in genesis to preserve the pre-v5 behavior exactly.
pub const GENESIS_ANNOTATION_TYPES: &[(&str, &str, Option<&str>, bool, bool)] = &[
    (
        "observation",
        "Neutral fact-based observation attached to a node.",
        None,
        false,
        false,
    ),
    (
        "correction",
        "Correction to an existing claim in the node.",
        None,
        false,
        true,
    ),
    (
        "question",
        "Open question raised against the node — candidate for FAQ / evidence loop.",
        None,
        false,
        false,
    ),
    (
        "friction",
        "Friction point: something a user or agent struggled with.",
        None,
        false,
        false,
    ),
    (
        "idea",
        "Speculative idea or proposal tied to the node's content.",
        None,
        false,
        false,
    ),
    (
        "era",
        "Temporal era marker — anchors a chronicle range to the node.",
        None,
        false,
        false,
    ),
    (
        "transition",
        "Transition marker — denotes a shift from one era / phase to the next.",
        None,
        false,
        false,
    ),
    (
        "health_check",
        "Self-applied health check result (pass / fail / notes).",
        None,
        false,
        false,
    ),
    (
        "directory",
        "Directory-scope annotation — applies to a folder rather than a single file.",
        None,
        false,
        false,
    ),
    // v5 Phase 7 reactives — steel_man + red_team are the two Phase 7a
    // wire to emit `annotation_reacted` observation events. Phase 7c adds
    // the 4 missing v5 verbs (`gap`, `hypothesis`, `purpose_declaration`,
    // `purpose_shift`) as pure vocab entries — no Rust enum change
    // required post-6c-B. 6c-B flipped the consumers to vocab lookups, so
    // these four are picked up on the very next `init_pyramid_db` tick
    // (and any running slug after `invalidate_cache()`).
    (
        "steel_man",
        "Good-faith reconstruction of an opposing position. Triggers debate_steward.",
        Some("starter-debate-steward"),
        true,
        false,
    ),
    (
        "red_team",
        "Adversarial challenge to a position. Triggers debate_steward.",
        Some("starter-debate-steward"),
        true,
        false,
    ),
    // Phase 7c — 4 v5 reactive verbs added as pure vocab entries.
    // Per project_convergence_decision.md + project_wire_canonical_vocabulary.md
    // the enum is vocab-driven post-6c-B, so these ship without an enum
    // edit. Handler-chain routing maps:
    //   gap                  → starter-gap-dispatcher   (Phase 7c materializes Gap nodes)
    //   hypothesis           → starter-debate-steward  (shares debate substrate with steel_man)
    //   purpose_declaration  → starter-meta-layer-oracle (declaration may trigger crystallization)
    //   purpose_shift        → starter-meta-layer-oracle (existing oracle path via purpose_shifted events)
    (
        "gap",
        "Explicit missing evidence or open question. Triggers gap_dispatcher to create a Gap node with demand state.",
        Some("starter-gap-dispatcher"),
        true,
        false,
    ),
    (
        "hypothesis",
        "Proposed causal or structural claim awaiting evidence. Triggers debate_steward for substrate gathering.",
        Some("starter-debate-steward"),
        true,
        false,
    ),
    (
        "purpose_declaration",
        "Declaration of intended purpose for a pyramid. Triggers meta_layer_oracle to check for crystallization.",
        Some("starter-meta-layer-oracle"),
        true,
        false,
    ),
    (
        "purpose_shift",
        "Explicit purpose change annotation. Triggers meta_layer_oracle to re-evaluate meta-layer coverage.",
        Some("starter-meta-layer-oracle"),
        true,
        false,
    ),
    // Post-build accretion v5 Phase 9c-1: close the debate-collapse
    // dormant-emitter gap. The 7a debate_steward chain only APPENDS
    // positions/red_teams; it has no path that removes them, so
    // `debate_collapsed` was an observation helper with no production
    // caller. Phase 9c-1 ships the full collapse feature: a dedicated
    // `debate_collapse` annotation type + a dedicated
    // `starter-debate-collapse` handler chain that finalizes the
    // debate (transitions `debate` → `scaffolding` + NULLs the
    // shape_payload_json) and emits the canonical `debate_collapsed`
    // observation event for audit. Separate from debate_steward
    // because the semantics are opposite (steward appends,
    // collapser finalizes) — mixing them in one chain would muddy
    // the responsibility. `handler_chain_id` on the vocab entry is
    // how the annotation_reacted override path (6c-B / audit 7a-gen)
    // dispatches — no new role_name needed.
    (
        "debate_collapse",
        "Collapse a Debate node back to Scaffolding (positions resolved or abandoned). Triggers starter-debate-collapse to finalize the debate and emit debate_collapsed.",
        Some("starter-debate-collapse"),
        true,
        false,
    ),
];

// ── Node Shapes (4 genesis entries) ─────────────────────────────────
//
// Tuple shape: (name, description)
pub const GENESIS_NODE_SHAPES: &[(&str, &str)] = &[
    (
        "scaffolding",
        "Default scaffolding shape. Canonical pyramid node carrying distilled / topics / entities / decisions / terms.",
    ),
    (
        "debate",
        "Debate node: holds steel_man / red_team exchanges and resolution state.",
    ),
    (
        "meta_layer",
        "Meta-layer node: crystallized reflection over sibling nodes, emerges via meta_layer_oracle.",
    ),
    (
        "gap",
        "Gap node: marks a known-unknown surfaced by gap_dispatcher, carries demand signal.",
    ),
];

// ── Role Names (10 genesis entries + cascade_handler = 11 total) ────
//
// Tuple shape: (name, description, handler_chain_id)
//
// Phase 6c-D deleted `role_binding::GENESIS_BINDINGS`; this table is the
// ONLY hardcoded role-name source now. `cascade_handler` is seeded
// separately per-slug by `db::create_slug` with a per-new-vs-backfilled
// default (see `role_binding::CASCADE_HANDLER_NEW_DEFAULT` +
// `CASCADE_HANDLER_EXISTING_DEFAULT`). The vocab entry for cascade_handler
// documents the canonical fresh-pyramid default so the registry
// represents the full role catalog.
pub const GENESIS_ROLE_NAMES: &[(&str, &str, &str)] = &[
    (
        "accretion_handler",
        "Handles accretion events (new source material arriving) for a pyramid.",
        "starter-accretion-handler",
    ),
    (
        "reconciler",
        "Reconciles conflicting contributions / orphaned nodes after a build.",
        "starter-reconciler",
    ),
    (
        "evidence_tester",
        "Runs evidence loops against questions to verify / refute claims.",
        "starter-evidence-tester",
    ),
    (
        "judge",
        "Arbitrates debate outcomes and gates cascade propagation.",
        "starter-judge",
    ),
    (
        "debate_steward",
        "Manages debate nodes: dispatches on steel_man / red_team annotations.",
        "starter-debate-steward",
    ),
    (
        "meta_layer_oracle",
        "Crystallizes meta-layer nodes by reading purpose and sibling state.",
        "starter-meta-layer-oracle",
    ),
    (
        "synthesizer",
        "Synthesizes partial answers into node distillates.",
        "starter-synthesizer",
    ),
    (
        "gap_dispatcher",
        "Detects gaps and dispatches gap nodes for evidence acquisition.",
        "starter-gap-dispatcher",
    ),
    (
        "sweep",
        "Scheduled sweep role — periodic reconciliation / stale detection.",
        "starter-sweep",
    ),
    (
        "authorize_question",
        "Authorizes question pyramids (accept / reject question slots).",
        "starter-authorize-question",
    ),
    (
        "cascade_handler",
        "Handles cascade propagation when ancestor content shifts. Default for fresh pyramids is judge-gated.",
        "starter-cascade-judge-gated",
    ),
];
