// src/types/pyramid.ts — Frontend types for post-build accretion v5.
// Mirrors src-tauri/src/pyramid/types.rs additions.
// See .lab/architecture/agent-wire-node-post-build-plan-v5.md

// ── Annotation verbs ────────────────────────────────────────────────────
// Phase 6c-C: annotation types are now sourced at runtime from the Wire
// node's `GET /vocabulary/annotation_type` registry (the zero-auth
// endpoint shipped in 6c-A). Compile-time literal unions would reject
// operator-published types that arrive without a code deploy — the whole
// point of the vocabulary registry is that new types (e.g.
// `counter_correction`) are accepted by every surface through contribution
// writes. Components that need a dropdown of valid types should use
// `useAnnotationTypes()` from `src/hooks/useVocabulary.ts`.
//
// The `FALLBACK_ANNOTATION_TYPES` constant is kept as a safety net for
// compile-time code that needs a starter list before the network fetch
// completes (loading states, etc.).
export type AnnotationType = string;

export const FALLBACK_ANNOTATION_TYPES: string[] = [
  "observation",
  "correction",
  "question",
  "friction",
  "idea",
  "era",
  "transition",
  "health_check",
  "directory",
  "steel_man",
  "red_team",
];

// ── Node shape ──────────────────────────────────────────────────────────
// Phase 6c-D: node shape is now sourced from the vocab registry (vocab_kind
// = "node_shape"). The prior 4-variant literal union blocked operator-
// published shapes from reaching the UI — an agent who publishes a new
// node-shape vocab entry (e.g. "annotation_cluster") can immediately write
// rows with that shape; the UI fetches the full list via `useNodeShapes()`
// from `src/hooks/useVocabulary.ts`.
//
// NULL in DB maps to "scaffolding" (existing behavior). `FALLBACK_NODE_SHAPES`
// is the minimal starter set for loading-state rendering before the network
// fetch completes.
export type NodeShape = string;

export const FALLBACK_NODE_SHAPES: string[] = [
  "scaffolding",
  "debate",
  "meta_layer",
  "gap",
];

// ── Purpose ─────────────────────────────────────────────────────────────
export interface Purpose {
  id: number;
  slug: string;
  purpose_text: string;
  stock_purpose_key?: string | null;
  decomposition_chain_ref?: string | null;
  created_at: string;
  superseded_by?: number | null;
  supersede_reason?: string | null;
}

// ── Role binding ────────────────────────────────────────────────────────
// Per-pyramid mapping of role name to handler chain id. ONLY for new roles
// introduced by post-build accretion (judge, reconciler, debate_steward,
// meta_layer_oracle, synthesizer, gap_dispatcher, sweep, accretion_handler,
// authorize_question, cascade_handler). Existing dispatch is unchanged.
export interface RoleBinding {
  id: number;
  slug: string;
  role_name: string;
  handler_chain_id: string;
  scope: string;
  created_at: string;
  superseded_by?: number | null;
}

// Phase 6c-D: role names are sourced from the vocab registry (vocab_kind =
// "role_name"). The prior `ROLE_NAMES` literal union blocked operator-
// published roles — an agent who publishes a new role vocab entry can bind
// a handler chain to it the moment the entry is active. Use `useRoleNames()`
// from `src/hooks/useVocabulary.ts` for runtime lookups.
//
// `FALLBACK_ROLE_NAMES` is the minimal starter set for loading-state
// rendering before the network fetch completes; it mirrors the genesis
// seed in `src-tauri/src/pyramid/vocab_genesis.rs::GENESIS_ROLE_NAMES`.
export type RoleName = string;

export const FALLBACK_ROLE_NAMES: string[] = [
  "accretion_handler",
  "reconciler",
  "evidence_tester",
  "judge",
  "debate_steward",
  "meta_layer_oracle",
  "synthesizer",
  "gap_dispatcher",
  "sweep",
  "authorize_question",
  "cascade_handler",
];

export const CASCADE_HANDLER_VARIANTS = [
  "starter-cascade-judge-gated",
  "starter-cascade-immediate-redistill",
  "starter-cascade-accrete-only",
] as const;

export type CascadeHandlerVariant = (typeof CASCADE_HANDLER_VARIANTS)[number];

// ── Node-shape-specific payloads (stored in pyramid_nodes.shape_payload_json) ─

export interface VoteLean {
  up_count: number;
  down_count: number;
  per_position?: Record<string, [number, number]> | null;
}

export interface RedTeamEntry {
  from_position: string;
  argument: string;
  // Node-id references (genuine evidence) — e.g. "L1-001".
  evidence_anchors?: string[];
  // Annotation provenance + idempotency tokens (e.g. "annotation#42").
  // v5 audit P6 split these off from evidence_anchors.
  source_annotation_ids?: string[];
}

export interface DebatePosition {
  label: string;
  steel_manning: string;
  red_teams?: RedTeamEntry[];
  // Node-id references supporting this position (genuine evidence).
  evidence_anchors?: string[];
  // Annotation provenance + idempotency tokens. See RedTeamEntry.
  source_annotation_ids?: string[];
}

export interface DebateTopic {
  concern: string;
  positions: DebatePosition[];
  cross_refs?: string[];
  vote_lean?: VoteLean | null;
}

export interface MetaLayerTopic {
  purpose_question: string;
  parent_meta_layer_id?: string | null;
  covered_substrate_nodes?: string[];
}

export interface GapCandidate {
  resolution_type: string;
  cost_estimate?: string | null;
  authorization_required?: boolean;
}

export interface GapTopic {
  concern: string;
  description: string;
  demand_state: string; // "open" | "dispatched" | "closed" | "tombstoned"
  candidate_resolutions?: GapCandidate[];
  // Node-id references to substrate supporting the gap (genuine evidence).
  evidence_anchors?: string[];
  // Annotation provenance + idempotency tokens (e.g. "annotation#42").
  // v5 audit P6 split these off from evidence_anchors so the evidence
  // channel stays node-id only.
  source_annotation_ids?: string[];
}

// Shape payload is stored as JSON in `pyramid_nodes.shape_payload_json`;
// the sibling `pyramid_nodes.node_shape` column is the discriminator. Rust
// serializes the payload untagged (#[serde(untagged)]), so the JSON body is
// the raw inner struct with NO `kind`/`payload` wrapping. Frontend drill
// views read `node_shape` from the same row and narrow to the matching
// variant before parsing `shape_payload_json`.
//
// Consumer pattern (pseudo):
//   if (node.node_shape === "debate") {
//     const payload = JSON.parse(node.shape_payload_json) as DebateTopic;
//   }
//
// This union is the union of valid payload shapes, not a tagged enum.
export type ShapePayload = DebateTopic | MetaLayerTopic | GapTopic;

// Helper to parse shape_payload_json given the sibling node_shape discriminator.
// Returns null when shape is "scaffolding" or the payload is absent/invalid.
export function parseShapePayload(
  nodeShape: NodeShape,
  shapePayloadJson: string | null | undefined
): ShapePayload | null {
  if (nodeShape === "scaffolding" || !shapePayloadJson) return null;
  try {
    return JSON.parse(shapePayloadJson) as ShapePayload;
  } catch {
    return null;
  }
}
