// pyramid/vocab_genesis.rs — Genesis vocabulary seed tables for Phase 6c-A.
//
// Hardcoded-vocabulary kill: the 11 AnnotationType variants, 4 NodeShape
// variants, and 10 role names in `GENESIS_BINDINGS` all live here as
// const tables. `vocab_entries::seed_genesis_vocabulary` iterates these
// on every `init_pyramid_db` and idempotently publishes each missing
// entry into `pyramid_config_contributions` as a `vocabulary_entry`
// subtype row.
//
// Per the architectural-lens principle (Wire's build pipeline is itself
// contributions — an agent should be able to improve the system), these
// seed tables are the ONLY place the genesis strings are hardcoded.
// Phase 6c-B / C / D flip the consumers (AnnotationType enum, MCP/FE
// constants, NodeShape enum / GENESIS_BINDINGS) to read from the
// contribution-driven registry that Phase 6c-A ships.
//
// Shape notes:
//   - annotation types carry a `reactive` flag. `steel_man` + `red_team`
//     are `true` today (Phase 7 will wire them to emit
//     `annotation_reacted`); `hypothesis`, `gap`, `purpose_declaration`,
//     `purpose_shift` are seeded with `reactive: true` so their INTENT
//     is captured in the registry before Phase 7 implements the
//     dispatch. Non-reactive types carry `false`.
//   - role names carry `handler_chain_id` values matching Phase 1's
//     `GENESIS_BINDINGS` table in `role_binding.rs`. `cascade_handler`
//     is also included here — it was previously seeded separately by
//     `db::create_slug` because its default depends on fresh-vs-
//     backfilled; the vocab entry documents its canonical default
//     (judge-gated for fresh pyramids).
//   - node shapes have no `handler_chain_id` today (shapes don't
//     dispatch on their own — they govern how nodes render / what
//     payload a node carries).

// ── Annotation Types (11 genesis entries) ───────────────────────────
//
// Tuple shape: (name, description, handler_chain_id, reactive)
pub const GENESIS_ANNOTATION_TYPES: &[(&str, &str, Option<&str>, bool)] = &[
    (
        "observation",
        "Neutral fact-based observation attached to a node.",
        None,
        false,
    ),
    (
        "correction",
        "Correction to an existing claim in the node.",
        None,
        false,
    ),
    (
        "question",
        "Open question raised against the node — candidate for FAQ / evidence loop.",
        None,
        false,
    ),
    (
        "friction",
        "Friction point: something a user or agent struggled with.",
        None,
        false,
    ),
    (
        "idea",
        "Speculative idea or proposal tied to the node's content.",
        None,
        false,
    ),
    (
        "era",
        "Temporal era marker — anchors a chronicle range to the node.",
        None,
        false,
    ),
    (
        "transition",
        "Transition marker — denotes a shift from one era / phase to the next.",
        None,
        false,
    ),
    (
        "health_check",
        "Self-applied health check result (pass / fail / notes).",
        None,
        false,
    ),
    (
        "directory",
        "Directory-scope annotation — applies to a folder rather than a single file.",
        None,
        false,
    ),
    // v5 Phase 7 reactives — steel_man + red_team are the two Phase 7 will
    // wire to emit `annotation_reacted` observation events. The other two
    // reactives (`hypothesis`, `gap`, `purpose_declaration`, `purpose_shift`)
    // are listed in the PLAN but do not exist in the AnnotationType enum
    // today; this seeder ships ONLY the 11 enum variants so registry
    // parity with the enum is exact. 6c-B/C will flip consumers to read
    // from the registry; future phases that extend the 4 v5 verbs will
    // simply publish additional vocab entries — no code deploy needed.
    (
        "steel_man",
        "Good-faith reconstruction of an opposing position. Triggers debate_steward.",
        Some("starter-debate-steward"),
        true,
    ),
    (
        "red_team",
        "Adversarial challenge to a position. Triggers debate_steward.",
        Some("starter-debate-steward"),
        true,
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
// Phase 1's `GENESIS_BINDINGS` ships the first 10; `cascade_handler`
// was seeded separately by `db::create_slug` with a per-new-vs-
// backfilled default (see `role_binding::CASCADE_HANDLER_NEW_DEFAULT`).
// The vocab entry for cascade_handler documents the canonical fresh-
// pyramid default so the registry represents the full role catalog.
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
