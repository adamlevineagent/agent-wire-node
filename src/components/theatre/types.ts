// theatre/types.ts — Shared TypeScript types for Live Pyramid Theatre

export interface LayerProgress {
    depth: number;
    step_name: string;
    estimated_nodes: number;
    completed_nodes: number;
    failed_nodes: number;
    status: string; // "pending" | "active" | "complete"
    nodes: NodeStatus[] | null;
}

export interface NodeStatus {
    node_id: string;
    status: string; // "complete" | "failed" | "pending"
    label: string | null;
}

export interface LogEntry {
    elapsed_secs: number;
    message: string;
}

export interface BuildProgressV2 {
    done: number;
    total: number;
    layers: LayerProgress[];
    current_step: string | null;
    log: LogEntry[];
}

export interface BuildStatus {
    slug: string;
    status: string; // "idle" | "running" | "complete" | "complete_with_errors" | "failed" | "cancelled"
    progress: { done: number; total: number };
    elapsed_seconds: number;
    failures: number;
}

/** Lightweight node info from pyramid_build_live_nodes IPC */
export interface LiveNodeInfo {
    node_id: string;
    depth: number;
    headline: string;
    parent_id: string | null;
    parent_ids?: string[];
    children: string[];
    node_kind?: string | null;
    question?: string | null;
    question_about?: string | null;
    question_creates?: string | null;
    question_prompt_hint?: string | null;
    answer_node_id?: string | null;
    answer_headline?: string | null;
    answer_distilled?: string | null;
    answered?: boolean | null;
    status: string; // "complete" | "pending" | "superseded"
}

/** Full LLM audit record from pyramid_node_audit IPC */
export interface LlmAuditRecord {
    id: number;
    slug: string;
    build_id: string;
    node_id: string | null;
    step_name: string;
    call_purpose: string;
    depth: number | null;
    model: string;
    system_prompt: string;
    user_prompt: string;
    raw_response: string | null;
    parsed_ok: boolean;
    prompt_tokens: number;
    completion_tokens: number;
    latency_ms: number | null;
    generation_id: string | null;
    status: string; // "pending" | "complete" | "failed"
    created_at: string;
    completed_at: string | null;
    cache_hit: boolean;
    /**
     * Walker Re-Plan Wire 2.1 Wave 1 task 11: WINNING entry's provider_id on
     * success, LAST-ATTEMPTED entry's provider_id on CallTerminal. Values
     * include "fleet" / "market" (routing sentinels) or any registered
     * provider's id. `null` on legacy / pre-walker rows.
     */
    provider_id?: string | null;
}

/** Spatial node for canvas rendering */
export interface SpatialNode {
    id: string;
    depth: number;
    headline: string;
    parentId: string | null;
    parentIds?: string[];
    children: string[];
    status: 'pending' | 'inflight' | 'complete' | 'failed';
    x: number;
    y: number;
    targetX: number;
    targetY: number;
    radius: number;
}
