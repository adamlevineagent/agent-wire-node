// Shared types for the Market → Compute surface. Centralized so the
// hero + dashboard + advanced components don't duplicate the
// IPC-shape definitions.

export interface ComputeMarketStateSnapshot {
    schema_version: number;
    offers: Record<string, unknown>;
    active_jobs: Record<string, unknown>;
    total_jobs_completed: number;
    total_credits_earned: number;
    session_jobs_completed: number;
    session_credits_earned: number;
    is_serving: boolean;
    last_evaluation_at: string | null;
    queue_mirror_seq: Record<string, number>;
}

export interface LocalModeStatus {
    enabled: boolean;
    base_url?: string | null;
    model?: string | null;
    detected_context_limit?: number | null;
    available_models?: string[];
    reachable?: boolean;
}

/// A chronicle event row as returned by `get_compute_events`. We only
/// look at a subset of fields for the hero surface; the Advanced
/// drawer's event stream uses the full shape.
export interface ComputeEvent {
    event_type: string;
    source: string;
    timestamp: string;
    model_id?: string | null;
    job_path?: string | null;
    metadata?: Record<string, unknown>;
}
