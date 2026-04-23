// src/types/pyramid.ts — Frontend types for post-build accretion v5.
// Mirrors src-tauri/src/pyramid/types.rs additions.
// See .lab/architecture/agent-wire-node-post-build-plan-v5.md

// ── Annotation verbs ────────────────────────────────────────────────────
// All 11 values — previously-missing Era/Transition/HealthCheck/Directory
// + new SteelMan/RedTeam — surfaced here so the UI dropdown is in sync with
// the Rust enum. MCP CLI's VALID_ANNOTATION_TYPES fixed in parallel (Phase 2
// WS2-D Pillar 38 absorbed).
export type AnnotationType =
  | "observation"
  | "correction"
  | "question"
  | "friction"
  | "idea"
  | "era"
  | "transition"
  | "health_check"
  | "directory"
  | "steel_man"
  | "red_team";

export const ANNOTATION_TYPES: AnnotationType[] = [
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
// NULL in DB maps to "scaffolding" (existing behavior). New shape nodes are
// written by role handlers (reconciler → debate, synthesizer → meta_layer,
// gap_dispatcher → gap). Not user-creatable from the wizard.
export type NodeShape = "scaffolding" | "debate" | "meta_layer" | "gap";

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

export const ROLE_NAMES = [
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
] as const;

export type RoleName = (typeof ROLE_NAMES)[number];

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
  evidence_anchors?: string[];
}

export interface DebatePosition {
  label: string;
  steel_manning: string;
  red_teams?: RedTeamEntry[];
  evidence_anchors?: string[];
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
