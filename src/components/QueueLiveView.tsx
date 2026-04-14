import { useState, useEffect, useCallback, useRef } from 'react';
import { listen, UnlistenFn } from '@tauri-apps/api/event';

// ── Queue event types (matching Rust TaggedKind snake_case serde) ────

interface QueueJobEnqueued {
    type: 'queue_job_enqueued';
    model_id: string;
    queue_depth: number;
}

interface QueueJobStarted {
    type: 'queue_job_started';
    model_id: string;
    source?: string; // "local" | "fleet"
}

interface QueueJobCompleted {
    type: 'queue_job_completed';
    model_id: string;
    latency_ms: number;
}

type QueueEvent = QueueJobEnqueued | QueueJobStarted | QueueJobCompleted;

interface TaggedBuildEvent {
    slug: string;
    kind: { type: string; [key: string]: unknown };
}

// ── State types ─────────────────────────────────────────────────────

interface RecentJob {
    modelId: string;
    latencyMs: number;
    completedAt: number;
    source?: string; // "local" | "fleet"
}

interface ModelQueueState {
    modelId: string;
    depth: number;
    isExecuting: boolean;
    currentJobStartedAt: number | null;
    currentJobSource: string | null; // "local" | "fleet"
    completedCount: number;
    totalLatencyMs: number;
    fleetJobCount: number;
    recentJobs: RecentJob[];
}

interface QueueState {
    models: Map<string, ModelQueueState>;
    totalProcessed: number;
    totalEnqueued: number;
}

const MAX_RECENT_JOBS = 10;

// ── Helpers ─────────────────────────────────────────────────────────

function isQueueEvent(kind: { type: string }): kind is QueueEvent {
    return kind.type === 'queue_job_enqueued'
        || kind.type === 'queue_job_started'
        || kind.type === 'queue_job_completed';
}

function formatLatency(ms: number): string {
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
}

function formatModelName(modelId: string): string {
    // Show the last segment of model paths for readability
    const parts = modelId.split('/');
    return parts[parts.length - 1] ?? modelId;
}

function reduceQueueEvent(state: QueueState, event: QueueEvent): QueueState {
    const next: QueueState = {
        models: new Map(state.models),
        totalProcessed: state.totalProcessed,
        totalEnqueued: state.totalEnqueued,
    };

    const modelId = event.model_id;
    let model = next.models.get(modelId);
    if (!model) {
        model = {
            modelId,
            depth: 0,
            isExecuting: false,
            currentJobStartedAt: null,
            currentJobSource: null,
            completedCount: 0,
            totalLatencyMs: 0,
            fleetJobCount: 0,
            recentJobs: [],
        };
    } else {
        // Clone for immutability
        model = { ...model, recentJobs: [...model.recentJobs] };
    }

    switch (event.type) {
        case 'queue_job_enqueued':
            model.depth = event.queue_depth;
            next.totalEnqueued += 1;
            break;

        case 'queue_job_started':
            model.isExecuting = true;
            model.currentJobStartedAt = Date.now();
            model.currentJobSource = event.source ?? 'local';
            if (event.source === 'fleet') {
                model.fleetJobCount += 1;
            }
            // Depth decreases by 1 when a job is picked up
            model.depth = Math.max(0, model.depth - 1);
            break;

        case 'queue_job_completed':
            model.isExecuting = false;
            model.currentJobStartedAt = null;
            model.completedCount += 1;
            model.totalLatencyMs += event.latency_ms;
            model.recentJobs.push({
                modelId,
                latencyMs: event.latency_ms,
                completedAt: Date.now(),
                source: model.currentJobSource ?? 'local',
            });
            model.currentJobSource = null;
            if (model.recentJobs.length > MAX_RECENT_JOBS) {
                model.recentJobs = model.recentJobs.slice(-MAX_RECENT_JOBS);
            }
            next.totalProcessed += 1;
            break;
    }

    next.models.set(modelId, model);
    return next;
}

// ── Component ───────────────────────────────────────────────────────

export function QueueLiveView() {
    const [state, setState] = useState<QueueState>({
        models: new Map(),
        totalProcessed: 0,
        totalEnqueued: 0,
    });
    const stateRef = useRef(state);
    stateRef.current = state;

    // Subscribe to cross-build-event, filter by slug === '__compute__'
    useEffect(() => {
        let unlisten: UnlistenFn | null = null;
        let active = true;

        (async () => {
            try {
                unlisten = await listen<TaggedBuildEvent>('cross-build-event', (ev) => {
                    if (!active) return;
                    const payload = ev.payload;
                    if (!payload || payload.slug !== '__compute__') return;
                    const kind = payload.kind;
                    if (!isQueueEvent(kind)) return;
                    setState(prev => reduceQueueEvent(prev, kind));
                });
            } catch (e) {
                console.warn('QueueLiveView: listen failed', e);
            }
        })();

        return () => {
            active = false;
            if (unlisten) unlisten();
        };
    }, []);

    const modelEntries = Array.from(state.models.values())
        .sort((a, b) => {
            // Active models first, then by depth, then by name
            if (a.isExecuting !== b.isExecuting) return a.isExecuting ? -1 : 1;
            if (a.depth !== b.depth) return b.depth - a.depth;
            return a.modelId.localeCompare(b.modelId);
        });

    const totalDepth = modelEntries.reduce((sum, m) => sum + m.depth, 0);
    const hasActivity = modelEntries.length > 0;

    return (
        <div className="queue-live-view">
            {/* Header */}
            <div className="queue-header">
                <div className="queue-header-title">
                    <h3>Compute Queue</h3>
                    {hasActivity && (
                        <span className="queue-header-stats">
                            {state.totalProcessed} processed
                            <span className="queue-stats-sep" />
                            {totalDepth} queued
                        </span>
                    )}
                </div>
            </div>

            {/* Model cards */}
            {hasActivity ? (
                <div className="queue-model-grid">
                    {modelEntries.map(model => (
                        <QueueModelCard key={model.modelId} model={model} />
                    ))}
                </div>
            ) : (
                <div className="queue-empty-state">
                    <div className="queue-empty-icon">Q</div>
                    <p className="queue-empty-title">No queue activity</p>
                    <p className="queue-empty-desc">
                        Start a build to see the compute queue in action.
                        Queue events appear here in real time as LLM calls are
                        dispatched to local or remote providers.
                    </p>
                </div>
            )}
        </div>
    );
}

// ── Per-model card ──────────────────────────────────────────────────

function QueueModelCard({ model }: { model: ModelQueueState }) {
    const avgLatency = model.completedCount > 0
        ? Math.round(model.totalLatencyMs / model.completedCount)
        : 0;

    // Elapsed time for the current executing job
    const [elapsed, setElapsed] = useState(0);
    useEffect(() => {
        if (!model.isExecuting || !model.currentJobStartedAt) {
            setElapsed(0);
            return;
        }
        const interval = setInterval(() => {
            setElapsed(Date.now() - (model.currentJobStartedAt ?? Date.now()));
        }, 100);
        return () => clearInterval(interval);
    }, [model.isExecuting, model.currentJobStartedAt]);

    return (
        <div className={`queue-model-card ${model.isExecuting ? 'queue-model-card-active' : ''}`}>
            <div className="queue-model-card-header">
                <div className="queue-model-name" title={model.modelId}>
                    {formatModelName(model.modelId)}
                </div>
                {model.isExecuting && (
                    <span className="queue-executing-indicator" title={model.currentJobSource === 'fleet' ? 'Executing (fleet)' : 'Executing'}>
                        <span className="queue-executing-dot" />
                        {model.currentJobSource === 'fleet' && (
                            <span className="queue-fleet-badge">FLEET</span>
                        )}
                    </span>
                )}
            </div>

            <div className="queue-model-stats">
                <div className="queue-stat-row">
                    <span className="queue-stat-label">Depth</span>
                    <span className="queue-stat-value">
                        {model.depth}
                        {model.depth > 0 && (
                            <span
                                className="queue-depth-bar"
                                style={{ '--depth-width': `${Math.min(model.depth * 20, 100)}%` } as React.CSSProperties}
                            />
                        )}
                    </span>
                </div>
                <div className="queue-stat-row">
                    <span className="queue-stat-label">Completed</span>
                    <span className="queue-stat-value">{model.completedCount}</span>
                </div>
                {model.fleetJobCount > 0 && (
                    <div className="queue-stat-row">
                        <span className="queue-stat-label">Fleet jobs</span>
                        <span className="queue-stat-value">{model.fleetJobCount}</span>
                    </div>
                )}
                {avgLatency > 0 && (
                    <div className="queue-stat-row">
                        <span className="queue-stat-label">Avg latency</span>
                        <span className="queue-stat-value">{formatLatency(avgLatency)}</span>
                    </div>
                )}
                {model.isExecuting && elapsed > 0 && (
                    <div className="queue-stat-row">
                        <span className="queue-stat-label">Current</span>
                        <span className="queue-stat-value queue-stat-executing">{formatLatency(elapsed)}</span>
                    </div>
                )}
            </div>

            {/* Recent jobs */}
            {model.recentJobs.length > 0 && (
                <div className="queue-recent-jobs">
                    <div className="queue-recent-header">Recent</div>
                    <div className="queue-recent-list">
                        {model.recentJobs.slice().reverse().slice(0, 5).map((job, i) => (
                            <div key={i} className="queue-recent-item">
                                <span className="queue-recent-latency">{formatLatency(job.latencyMs)}</span>
                                {job.source === 'fleet' && (
                                    <span className="queue-recent-fleet-badge">F</span>
                                )}
                            </div>
                        ))}
                    </div>
                </div>
            )}
        </div>
    );
}
