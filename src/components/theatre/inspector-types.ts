// inspector-types.ts
// Full TypeScript interfaces matching the Rust PyramidNode and DrillResult shapes
// Field names match serde JSON output (snake_case)

export interface PyramidNodeFull {
    id: string;
    slug: string;
    depth: number;
    chunk_index: number | null;
    headline: string;
    distilled: string;
    topics: Topic[];
    corrections: Correction[];
    decisions: Decision[];
    terms: Term[];
    dead_ends: string[];
    self_prompt: string;
    children: string[];
    parent_id: string | null;
    parent_ids?: string[];
    superseded_by: string | null;
    build_id: string | null;
    created_at: string;
    time_range: TimeRange | null;
    weight: number;
    provisional: boolean;
    promoted_from: string | null;
    narrative: NarrativeMultiZoom;
    entities: Entity[];
    key_quotes: KeyQuote[];
    transitions: Transitions;
    current_version: number;
    current_version_chain_phase: string | null;
    node_kind?: string | null;
    question?: string | null;
    question_about?: string | null;
    question_creates?: string | null;
    question_prompt_hint?: string | null;
    answer_node_id?: string | null;
    answer_headline?: string | null;
    answer_distilled?: string | null;
    answered?: boolean | null;
}

export interface Topic {
    name: string;
    current: string;
    entities: string[];
    corrections: Correction[];
    decisions: Decision[];
    /** Pass-through: serde(flatten) extra fields from Rust */
    [key: string]: unknown;
}

export interface Correction {
    wrong: string;
    right: string;
    who: string;
}

export interface Decision {
    decided: string;
    why: string;
    rejected: string;
    stance: string;
    importance: number;
    related: string[];
}

export interface Term {
    term: string;
    definition: string;
}

export interface TimeRange {
    start: string | null;
    end: string | null;
}

export interface NarrativeMultiZoom {
    levels: NarrativeLevel[];
}

export interface NarrativeLevel {
    zoom: number;
    text: string;
}

export interface Entity {
    name: string;
    role: string;
    importance: number;
    liveness: string;
}

export interface KeyQuote {
    text: string;
    speaker: string;
    speaker_role: string;
    importance: number;
    chunk_ref: string | null;
}

export interface Transitions {
    prior: string;
    next: string;
}

export interface DrillResultFull {
    node: PyramidNodeFull;
    children: PyramidNodeFull[];
    web_edges: ConnectedWebEdge[];
    remote_web_edges: ConnectedRemoteWebEdge[];
    evidence: EvidenceLink[];
    gaps: GapReport[];
    question_context: QuestionContext | null;
    question_node?: QuestionNodeDetail | null;
    linked_answer?: PyramidNodeFull | null;
    node_kind?: string | null;
    question?: string | null;
    question_about?: string | null;
    question_creates?: string | null;
    question_prompt_hint?: string | null;
    answer_node_id?: string | null;
    answer_headline?: string | null;
    answer_distilled?: string | null;
    answered?: boolean | null;
}

export interface QuestionNodeDetail {
    question_id: string;
    parent_id: string | null;
    parent_ids?: string[];
    depth: number;
    visual_depth: number;
    question: string;
    about: string;
    creates: string;
    prompt_hint: string;
    is_leaf: boolean;
    children: string[];
    build_id?: string | null;
    answer_node_id?: string | null;
    answered: boolean;
}

export interface ConnectedWebEdge {
    connected_to: string;
    connected_headline: string;
    relationship: string;
    strength: number;
}

export interface ConnectedRemoteWebEdge {
    remote_handle_path: string;
    remote_slug: string;
    relationship: string;
    relevance: number;
    build_id: string;
}

export interface EvidenceLink {
    slug: string;
    source_node_id: string;
    target_node_id: string;
    verdict: "KEEP" | "DISCONNECT" | "MISSING";
    weight: number | null;
    reason: string | null;
    build_id: string | null;
    live: boolean | null;
}

export interface GapReport {
    question_id: string;
    description: string;
    layer: number;
    resolved: boolean;
    resolution_confidence: number;
}

export interface QuestionContext {
    parent_question: string | null;
    sibling_questions: string[];
}
