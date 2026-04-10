// Phase 13 shared hook — reduces Phase 13 build events into a single
// per-slug "row state" that both the detailed `PyramidBuildViz` and
// the compact `CrossPyramidTimeline` can render. Factoring the reducer
// out of the component lets both views consume the same logic.

import { useCallback, useReducer } from 'react';

// ── Event types (matching Rust TaggedKind serde shape) ─────────────

export type TaggedBuildEvent = {
    slug: string;
    kind: TaggedKind;
};

// Discriminated union of every event variant the build viz reducer
// handles. Narrower than the full backend TaggedKind enum — we only
// list the variants this hook consumes.
export type KnownTaggedKind =
    | { type: 'chain_step_started'; step_name: string; step_idx: number; primitive: string; depth: number }
    | { type: 'chain_step_finished'; step_name: string; step_idx: number; status: string; elapsed_seconds: number }
    | { type: 'llm_call_started'; slug: string; build_id: string; step_name: string; primitive: string; model_tier: string; model_id: string; cache_key: string; depth: number; chunk_index: number | null }
    | { type: 'llm_call_completed'; slug: string; build_id: string; step_name: string; cache_key: string; tokens_prompt: number; tokens_completion: number; cost_usd: number; latency_ms: number; model_id: string }
    | { type: 'cache_hit'; slug: string; step_name: string; cache_key: string; chunk_index: number | null; depth: number }
    | { type: 'cache_miss'; slug: string; step_name: string; cache_key: string; chunk_index: number | null; depth: number }
    | { type: 'step_retry'; slug: string; build_id: string; step_name: string; attempt: number; max_attempts: number; error: string; backoff_ms: number }
    | { type: 'step_error'; slug: string; build_id: string; step_name: string; error: string; depth: number; chunk_index: number | null }
    | { type: 'web_edge_started'; slug: string; build_id: string; step_name: string; source_node_count: number }
    | { type: 'web_edge_completed'; slug: string; build_id: string; step_name: string; edges_created: number; latency_ms: number }
    | { type: 'evidence_processing'; slug: string; build_id: string; step_name: string; question_count: number; action: string; model_tier: string }
    | { type: 'triage_decision'; slug: string; build_id: string; step_name: string; item_id: string; decision: string; reason: string }
    | { type: 'gap_processing'; slug: string; build_id: string; step_name: string; depth: number; gap_count: number; action: string }
    | { type: 'cluster_assignment'; slug: string; build_id: string; step_name: string; depth: number; node_count: number; cluster_count: number }
    | { type: 'node_rerolled'; slug: string; build_id: string; node_id: string | null; step_name: string; note: string; new_cache_entry_id: number; manifest_id: number | null }
    | { type: 'cache_invalidated'; slug: string; build_id: string; cache_key: string; reason: string }
    | { type: 'manifest_generated'; slug: string; build_id: string; manifest_id: number; depth: number; node_id: string }
    | { type: 'cost_update'; cost_so_far_usd: number; estimate_usd: number };

// Wider type used by the listener so unknown variants don't break
// parsing. The reducer filters down to `KnownTaggedKind` by
// switching on `type`.
export type TaggedKind = KnownTaggedKind | { type: string; [key: string]: unknown };

// ── State shape ──────────────────────────────────────────────────

export type StepStatus =
    | 'pending'
    | 'running'
    | 'completed'
    | 'cached'
    | 'partial_cache'
    | 'failed'
    | 'retrying';

export interface StepCall {
    cacheKey: string;
    status: 'running' | 'completed' | 'cached' | 'failed' | 'retrying';
    modelId: string;
    tokensPrompt?: number;
    tokensCompletion?: number;
    costUsd?: number;
    latencyMs?: number;
    attempt?: number;
    maxAttempts?: number;
    error?: string;
}

export interface StepState {
    stepName: string;
    primitive: string;
    modelTier: string;
    status: StepStatus;
    calls: StepCall[];
    totalCostUsd: number;
    totalTokensPrompt: number;
    totalTokensCompletion: number;
    cacheHits: number;
    cacheMisses: number;
    depth: number;
    /// Human summary of the most recent meta event (cluster
    /// assignment / web edges / triage etc.) so the timeline row
    /// can show a hint line under the step name.
    activityHint?: string;
}

export interface CostAccumulator {
    estimatedUsd: number;
    actualUsd: number | null;
    cacheSavingsUsd: number;
}

export interface BuildRowState {
    slug: string;
    steps: StepState[];
    cost: CostAccumulator;
    /// Map of cache_key → StepCall index for O(1) update on
    /// LlmCallCompleted.
    callIndex: Map<string, { stepName: string; callIndex: number }>;
    /// Per-step cumulative counters.
    currentStep?: string;
    lastUpdateMs: number;
    /// Flattened log of triage decisions + manifests + rerolls for
    /// the activity panel.
    activityLog: ActivityLogEntry[];
}

export interface ActivityLogEntry {
    kind: string;
    stepName?: string;
    message: string;
    elapsedSecs: number;
}

export function initialBuildRowState(slug: string): BuildRowState {
    return {
        slug,
        steps: [],
        cost: { estimatedUsd: 0, actualUsd: null, cacheSavingsUsd: 0 },
        callIndex: new Map(),
        currentStep: undefined,
        lastUpdateMs: Date.now(),
        activityLog: [],
    };
}

// ── Reducer ──────────────────────────────────────────────────────

function findOrCreateStep(state: BuildRowState, stepName: string, depth?: number, primitive?: string, modelTier?: string): StepState {
    let step = state.steps.find(s => s.stepName === stepName);
    if (!step) {
        step = {
            stepName,
            primitive: primitive ?? '',
            modelTier: modelTier ?? '',
            status: 'pending',
            calls: [],
            totalCostUsd: 0,
            totalTokensPrompt: 0,
            totalTokensCompletion: 0,
            cacheHits: 0,
            cacheMisses: 0,
            depth: depth ?? 0,
        };
        state.steps.push(step);
    } else {
        if (depth !== undefined) step.depth = depth;
        if (primitive !== undefined && !step.primitive) step.primitive = primitive;
        if (modelTier !== undefined && !step.modelTier) step.modelTier = modelTier;
    }
    return step;
}

function logActivity(state: BuildRowState, entry: Omit<ActivityLogEntry, 'elapsedSecs'>) {
    state.activityLog.push({
        ...entry,
        elapsedSecs: Math.round((Date.now() - state.lastUpdateMs) / 1000),
    });
    // Cap log at 200 entries so the in-memory state doesn't grow
    // unbounded during a long build.
    if (state.activityLog.length > 200) {
        state.activityLog.splice(0, state.activityLog.length - 200);
    }
}

function derivedStepStatus(step: StepState): StepStatus {
    if (step.status === 'failed' || step.status === 'retrying') return step.status;
    if (step.calls.length === 0) return step.status === 'running' ? 'running' : 'pending';
    const hasPending = step.calls.some(c => c.status === 'running');
    if (hasPending) return 'running';
    const allCached = step.calls.every(c => c.status === 'cached');
    if (allCached) return 'cached';
    const anyCached = step.calls.some(c => c.status === 'cached');
    const allCompleted = step.calls.every(c => c.status === 'completed' || c.status === 'cached');
    if (anyCached && allCompleted) return 'partial_cache';
    if (allCompleted) return 'completed';
    const anyFailed = step.calls.some(c => c.status === 'failed');
    return anyFailed ? 'failed' : 'running';
}

const KNOWN_EVENT_TYPES = new Set<string>([
    'chain_step_started',
    'chain_step_finished',
    'llm_call_started',
    'llm_call_completed',
    'cache_hit',
    'cache_miss',
    'step_retry',
    'step_error',
    'web_edge_started',
    'web_edge_completed',
    'evidence_processing',
    'triage_decision',
    'gap_processing',
    'cluster_assignment',
    'node_rerolled',
    'cache_invalidated',
    'manifest_generated',
    'cost_update',
]);

function isKnownEvent(event: TaggedKind): event is KnownTaggedKind {
    return KNOWN_EVENT_TYPES.has(event.type);
}

export function reduceBuildRowEvent(state: BuildRowState, event: TaggedKind): BuildRowState {
    // Copy-on-write — we mutate in a shallow clone so React sees a
    // new reference. Nested Maps/arrays are cloned lazily.
    const next: BuildRowState = {
        ...state,
        steps: state.steps.map(s => ({ ...s, calls: [...s.calls] })),
        cost: { ...state.cost },
        callIndex: new Map(state.callIndex),
        activityLog: [...state.activityLog],
    };

    if (!isKnownEvent(event)) {
        return next;
    }

    switch (event.type) {
        case 'chain_step_started': {
            const step = findOrCreateStep(next, event.step_name, event.depth, event.primitive);
            step.status = 'running';
            next.currentStep = event.step_name;
            logActivity(next, { kind: 'step_started', stepName: event.step_name, message: `Started ${event.step_name}` });
            break;
        }
        case 'chain_step_finished': {
            const step = findOrCreateStep(next, event.step_name);
            step.status = event.status === 'ok' ? 'completed' : 'failed';
            // If calls landed, re-derive from them (more precise
            // than the raw chain_step_finished verdict).
            if (step.calls.length > 0) {
                step.status = derivedStepStatus(step);
            }
            logActivity(next, {
                kind: 'step_finished',
                stepName: event.step_name,
                message: `${event.step_name} ${event.status} (${event.elapsed_seconds.toFixed(1)}s)`,
            });
            break;
        }
        case 'llm_call_started': {
            const step = findOrCreateStep(next, event.step_name, event.depth, event.primitive, event.model_tier);
            step.status = 'running';
            step.calls.push({
                cacheKey: event.cache_key,
                status: 'running',
                modelId: event.model_id,
            });
            next.callIndex.set(event.cache_key, {
                stepName: event.step_name,
                callIndex: step.calls.length - 1,
            });
            break;
        }
        case 'llm_call_completed': {
            const idx = next.callIndex.get(event.cache_key);
            const step = findOrCreateStep(next, event.step_name);
            if (idx) {
                const call = step.calls[idx.callIndex];
                if (call) {
                    call.status = 'completed';
                    call.tokensPrompt = event.tokens_prompt;
                    call.tokensCompletion = event.tokens_completion;
                    call.costUsd = event.cost_usd;
                    call.latencyMs = event.latency_ms;
                    call.modelId = event.model_id;
                }
            } else {
                // Call arrived without a prior Started — synthesize
                // the row so the cost still lands.
                step.calls.push({
                    cacheKey: event.cache_key,
                    status: 'completed',
                    modelId: event.model_id,
                    tokensPrompt: event.tokens_prompt,
                    tokensCompletion: event.tokens_completion,
                    costUsd: event.cost_usd,
                    latencyMs: event.latency_ms,
                });
            }
            step.totalCostUsd += event.cost_usd;
            step.totalTokensPrompt += event.tokens_prompt;
            step.totalTokensCompletion += event.tokens_completion;
            step.cacheMisses += 1;
            next.cost.estimatedUsd += event.cost_usd;
            step.status = derivedStepStatus(step);
            break;
        }
        case 'cache_hit': {
            const step = findOrCreateStep(next, event.step_name, event.depth);
            step.calls.push({
                cacheKey: event.cache_key,
                status: 'cached',
                modelId: '(cached)',
                costUsd: 0,
                latencyMs: 0,
            });
            step.cacheHits += 1;
            // Cache savings: we don't know the original cost from
            // the event payload, so use a conservative heuristic —
            // every cache hit saves roughly the average call cost
            // for this step. A future refinement can thread the
            // original cost through the event.
            const avgCost = step.totalCostUsd / Math.max(1, step.cacheMisses);
            next.cost.cacheSavingsUsd += avgCost;
            step.status = derivedStepStatus(step);
            break;
        }
        case 'cache_miss':
            // No state change — a cache miss just means the call
            // fell through to HTTP, which is represented by the
            // LlmCallStarted/Completed pair that follows.
            break;
        case 'step_retry': {
            const step = findOrCreateStep(next, event.step_name);
            step.status = 'retrying';
            const lastCall = step.calls[step.calls.length - 1];
            if (lastCall) {
                lastCall.status = 'retrying';
                lastCall.attempt = event.attempt;
                lastCall.maxAttempts = event.max_attempts;
                lastCall.error = event.error;
            }
            logActivity(next, {
                kind: 'retry',
                stepName: event.step_name,
                message: `Retry ${event.attempt}/${event.max_attempts}: ${event.error}`,
            });
            break;
        }
        case 'step_error': {
            const step = findOrCreateStep(next, event.step_name, event.depth);
            step.status = 'failed';
            const lastCall = step.calls[step.calls.length - 1];
            if (lastCall) {
                lastCall.status = 'failed';
                lastCall.error = event.error;
            }
            logActivity(next, {
                kind: 'error',
                stepName: event.step_name,
                message: `Error: ${event.error}`,
            });
            break;
        }
        case 'web_edge_started': {
            const step = findOrCreateStep(next, event.step_name);
            step.activityHint = `Generating web edges over ${event.source_node_count} nodes`;
            break;
        }
        case 'web_edge_completed': {
            const step = findOrCreateStep(next, event.step_name);
            step.activityHint = `${event.edges_created} edges in ${event.latency_ms}ms`;
            logActivity(next, {
                kind: 'web_edges',
                stepName: event.step_name,
                message: `${event.edges_created} edges`,
            });
            break;
        }
        case 'evidence_processing': {
            const step = findOrCreateStep(next, event.step_name, undefined, undefined, event.model_tier);
            step.activityHint = `${event.action}: ${event.question_count} questions`;
            break;
        }
        case 'triage_decision': {
            logActivity(next, {
                kind: 'triage',
                stepName: event.step_name,
                message: `${event.item_id.slice(0, 8)} → ${event.decision}`,
            });
            break;
        }
        case 'gap_processing': {
            const step = findOrCreateStep(next, event.step_name, event.depth);
            step.activityHint = `Gaps (${event.action}): ${event.gap_count}`;
            break;
        }
        case 'cluster_assignment': {
            const step = findOrCreateStep(next, event.step_name, event.depth);
            step.activityHint = `${event.node_count} → ${event.cluster_count} clusters at L${event.depth}`;
            logActivity(next, {
                kind: 'cluster',
                stepName: event.step_name,
                message: `Clustered ${event.node_count} nodes into ${event.cluster_count} groups`,
            });
            break;
        }
        case 'node_rerolled': {
            logActivity(next, {
                kind: 'reroll',
                stepName: event.step_name,
                message: `Rerolled ${event.node_id ?? event.step_name}: ${event.note.slice(0, 80)}`,
            });
            break;
        }
        case 'cache_invalidated': {
            logActivity(next, {
                kind: 'invalidated',
                message: `Cache invalidated (${event.reason}): ${event.cache_key.slice(0, 12)}`,
            });
            break;
        }
        case 'manifest_generated': {
            logActivity(next, {
                kind: 'manifest',
                message: `Change manifest ${event.manifest_id} → ${event.node_id}`,
            });
            break;
        }
        case 'cost_update': {
            next.cost.actualUsd = event.cost_so_far_usd;
            break;
        }
        default:
            // Unknown event — ignore gracefully.
            break;
    }

    return next;
}

// ── Hook ────────────────────────────────────────────────────────

export function useBuildRowState(slug: string) {
    const [state, dispatch] = useReducer(
        (s: BuildRowState, ev: TaggedKind) => reduceBuildRowEvent(s, ev),
        slug,
        initialBuildRowState,
    );

    const handleEvent = useCallback((ev: TaggedKind) => {
        dispatch(ev);
    }, []);

    return { state, handleEvent };
}
