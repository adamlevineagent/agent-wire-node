/**
 * Shared types for the intent planner system.
 * Used by IntentBar, PlanWidgets, and OperationsMode.
 */

export interface PlannerContext {
    pyramids: { slug: string; node_count: number; content_type: string }[];
    corpora: { slug: string; path: string; doc_count: number }[];
    agents: { id: string; name: string; status: string }[];
    fleet: { online_count: number; task_count: number };
    balance: number;
}

export interface PlanStep {
    id: string;
    description: string;
    estimated_cost: number | null;
    on_error?: 'abort' | 'continue';
    // Exactly one of these two step types:
    command?: string;
    args?: Record<string, unknown>;
    navigate?: { mode: string; view?: string; props?: Record<string, unknown> };
}

export interface WidgetSchema {
    type: string;
    field?: string;
    label?: string;
    placeholder?: string;
    multi?: boolean;
    filter?: string;
    amount?: number;
    breakdown?: Record<string, unknown>;
    default?: boolean;
    summary?: string;
    details?: string;
    options?: { value: string; label: string }[];
}

export interface PlanResult {
    plan_id: string;
    intent: string;
    steps: PlanStep[];
    total_estimated_cost: number | null;
    ui_schema: WidgetSchema[];
}

/** Current format version — bump when PlanStep shape changes to clear stale operations.
 * v3: raw API commands (wire_api_call/operator_api_call with method/path/body)
 * v4: named vocabulary commands (archive_agent, wire_query, etc.) — executor handles HTTP */
export const OPERATION_FORMAT_VERSION = 4;

export interface OperationEntry {
    id: string;
    intent: string;
    status: 'running' | 'completed' | 'failed';
    steps: PlanStep[];
    currentStep: number;
    startedAt: number;
    result?: unknown;
    error?: string;
    stepErrors?: { stepId: string; command?: string; args?: unknown; error: string }[];
    format_version: number;
}
