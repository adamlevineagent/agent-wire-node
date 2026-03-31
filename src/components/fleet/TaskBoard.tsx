import { useState, useEffect, useCallback, Component, type ReactNode } from 'react';
import { useAppContext } from '../../contexts/AppContext';

// Error boundary to catch render crashes and show fallback UI
class TaskBoardErrorBoundary extends Component<{ children: ReactNode }, { error: string | null }> {
    state = { error: null as string | null };
    static getDerivedStateFromError(error: Error) {
        return { error: error.message || 'TaskBoard crashed unexpectedly' };
    }
    render() {
        if (this.state.error) {
            return (
                <div className="fleet-task-board">
                    <div className="fleet-task-header"><h3>Task Board</h3></div>
                    <div className="corpora-error">
                        <span>Error: {this.state.error}</span>
                        <button className="stewardship-btn stewardship-btn-ghost" onClick={() => this.setState({ error: null })}>Retry</button>
                    </div>
                </div>
            );
        }
        return this.props.children;
    }
}

interface WireTask {
    id: string;
    title?: string;
    context?: string;
    status: string;
    priority?: string;
    assigned_to?: string;
    created_at?: string;
    updated_at?: string;
    completed_at?: string;
    creator_id?: string;
    scope?: string;
    [key: string]: unknown;
}

type TaskStatus = 'backlog' | 'claimed' | 'active' | 'review' | 'done';

const STATUS_COLUMNS: { key: TaskStatus; label: string }[] = [
    { key: 'backlog', label: 'Backlog' },
    { key: 'claimed', label: 'Claimed' },
    { key: 'active', label: 'Active' },
    { key: 'review', label: 'Review' },
    { key: 'done', label: 'Done' },
];

function normalizeStatus(status: string | undefined | null): TaskStatus {
    if (!status) return 'backlog';
    const s = status.toLowerCase();
    if (s === 'backlog' || s === 'pending' || s === 'open' || s === 'new') return 'backlog';
    if (s === 'claimed') return 'claimed';
    if (s === 'active' || s === 'in_progress' || s === 'running') return 'active';
    if (s === 'review') return 'review';
    if (s === 'done' || s === 'completed' || s === 'complete') return 'done';
    return 'backlog';
}

type Priority = 'urgent' | 'high' | 'normal' | 'low';

function priorityClass(p: string | undefined): string {
    if (!p) return '';
    const lp = p.toLowerCase() as Priority;
    if (lp === 'urgent') return 'fleet-task-priority-urgent';
    if (lp === 'high') return 'fleet-task-priority-high';
    if (lp === 'low') return 'fleet-task-priority-low';
    return '';
}

function priorityLabel(p: string | undefined): string | null {
    if (!p) return null;
    const lp = p.toLowerCase();
    if (lp === 'normal') return null; // normal is default, no badge
    if (lp === 'urgent' || lp === 'high' || lp === 'low') return lp.charAt(0).toUpperCase() + lp.slice(1);
    return null;
}

/** Returns valid transitions for a task in a given column */
function getValidTransitions(column: TaskStatus): { label: string; action: () => { body: Record<string, string>; method: 'PATCH' | 'PUT' } }[] {
    // Returns descriptors — actual execution handled by caller
    switch (column) {
        case 'backlog':
            return [{ label: 'Claim', action: () => ({ body: { action: 'claim' }, method: 'PATCH' }) }];
        case 'claimed':
            return [
                { label: 'Move to Active', action: () => ({ body: { action: 'move', column: 'active' }, method: 'PUT' }) },
                { label: 'Send Back to Backlog', action: () => ({ body: { action: 'move', column: 'backlog' }, method: 'PUT' }) },
            ];
        case 'active':
            return [
                { label: 'Move to Review', action: () => ({ body: { action: 'move', column: 'review' }, method: 'PUT' }) },
            ];
        case 'review':
            return [
                { label: 'Send Back to Active', action: () => ({ body: { action: 'move', column: 'active' }, method: 'PUT' }) },
            ];
        case 'done':
            return [
                { label: 'Reopen to Backlog', action: () => ({ body: { action: 'move', column: 'backlog' }, method: 'PUT' }) },
            ];
        default:
            return [];
    }
}

function isOlderThan7Days(dateStr: string | undefined): boolean {
    if (!dateStr) return false;
    const d = new Date(dateStr);
    const now = new Date();
    return (now.getTime() - d.getTime()) > 7 * 24 * 60 * 60 * 1000;
}

function formatTimestamp(dateStr: string | undefined): string {
    if (!dateStr) return '';
    try {
        return new Date(dateStr).toLocaleString();
    } catch {
        return dateStr;
    }
}

export function TaskBoard() {
    return <TaskBoardErrorBoundary><TaskBoardInner /></TaskBoardErrorBoundary>;
}

function TaskBoardInner() {
    const { wireApiCall } = useAppContext();
    const [tasks, setTasks] = useState<WireTask[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [actionInFlight, setActionInFlight] = useState<string | null>(null);
    const [expandedTaskId, setExpandedTaskId] = useState<string | null>(null);
    const [openDropdownId, setOpenDropdownId] = useState<string | null>(null);
    const [doneCollapsed, setDoneCollapsed] = useState(true);
    const [showCreateForm, setShowCreateForm] = useState(false);

    // Create form state
    const [createTitle, setCreateTitle] = useState('');
    const [createContext, setCreateContext] = useState('');
    const [createPriority, setCreatePriority] = useState<string>('normal');
    const [creating, setCreating] = useState(false);

    const fetchTasks = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
            const response = await wireApiCall('GET', '/api/v1/wire/tasks') as any;
            // Response wrapped in wireEnvelopeWithTasks — data at response.data.tasks
            const fromEnvelope = response?.data?.tasks;
            const taskList = Array.isArray(fromEnvelope)
                ? fromEnvelope
                : Array.isArray(response?.tasks)
                    ? response.tasks
                    : Array.isArray(response)
                        ? response
                        : [];
            setTasks(taskList);
        } catch (err: unknown) {
            const msg = typeof err === 'string' ? err : (err as any)?.message || 'Failed to load tasks';
            setError(msg);
        } finally {
            setLoading(false);
        }
    }, [wireApiCall]);

    useEffect(() => {
        fetchTasks();
    }, [fetchTasks]);

    const handleTaskAction = async (taskId: string, method: 'PATCH' | 'PUT', body: Record<string, string>) => {
        setActionInFlight(taskId);
        try {
            await wireApiCall(method, `/api/v1/wire/tasks/${taskId}`, body);
            await fetchTasks();
        } catch (err: any) {
            const msg = typeof err === 'string' ? err : err?.message || 'Unknown error';
            alert(`Action failed: ${msg}`);
        } finally {
            setActionInFlight(null);
            setOpenDropdownId(null);
        }
    };

    const handleClaim = (taskId: string) => handleTaskAction(taskId, 'PATCH', { action: 'claim' });

    const handleComplete = (taskId: string) => handleTaskAction(taskId, 'PUT', { action: 'complete' });

    const handleArchive = async (taskId: string) => {
        setActionInFlight(taskId);
        try {
            await wireApiCall('PUT', `/api/v1/wire/tasks/${taskId}`, { action: 'archive' });
            await fetchTasks();
        } catch (err: any) {
            const msg = typeof err === 'string' ? err : err?.message || 'Unknown error';
            alert(`Archive failed: ${msg}`);
        } finally {
            setActionInFlight(null);
        }
    };

    const handleArchiveAllDone = async () => {
        const doneTasks = tasks.filter(t => normalizeStatus(t.status) === 'done');
        if (doneTasks.length === 0) return;
        setActionInFlight('archive-all-done');
        try {
            for (const task of doneTasks) {
                await wireApiCall('PUT', `/api/v1/wire/tasks/${task.id}`, { action: 'archive' });
            }
            await fetchTasks();
        } catch (err: any) {
            const msg = typeof err === 'string' ? err : err?.message || 'Unknown error';
            alert(`Archive all failed: ${msg}`);
        } finally {
            setActionInFlight(null);
        }
    };

    const handleCreate = async () => {
        if (!createTitle.trim()) return;
        setCreating(true);
        try {
            await wireApiCall('POST', '/api/v1/wire/tasks', {
                title: createTitle.trim(),
                context: createContext.trim() || undefined,
                priority: createPriority,
                scope: 'fleet',
            });
            setCreateTitle('');
            setCreateContext('');
            setCreatePriority('normal');
            setShowCreateForm(false);
            await fetchTasks();
        } catch (err: any) {
            const msg = typeof err === 'string' ? err : err?.message || 'Unknown error';
            alert(`Create failed: ${msg}`);
        } finally {
            setCreating(false);
        }
    };

    if (loading) {
        return (
            <div className="fleet-task-board">
                <div className="corpora-loading">
                    <div className="loading-spinner" />
                    <span>Loading tasks...</span>
                </div>
            </div>
        );
    }

    if (error) {
        return (
            <div className="fleet-task-board">
                <div className="fleet-task-header">
                    <h3>Task Board</h3>
                </div>
                <div className="corpora-error">
                    <span>{error}</span>
                    <button
                        className="stewardship-btn stewardship-btn-ghost"
                        onClick={fetchTasks}
                    >
                        Retry
                    </button>
                </div>
            </div>
        );
    }

    // Group tasks by normalized status
    const grouped: Record<TaskStatus, WireTask[]> = {
        backlog: [],
        claimed: [],
        active: [],
        review: [],
        done: [],
    };
    for (const task of tasks) {
        const col = normalizeStatus(task.status);
        grouped[col].push(task);
    }

    return (
        <div className="fleet-task-board">
            <div className="fleet-task-header">
                <h3>Task Board</h3>
                <div className="fleet-task-header-actions">
                    <button
                        className="stewardship-btn stewardship-btn-ghost"
                        onClick={() => setShowCreateForm(!showCreateForm)}
                    >
                        {showCreateForm ? 'Cancel' : '+ New Task'}
                    </button>
                    <span className="fleet-task-count">
                        {tasks.length} task{tasks.length !== 1 ? 's' : ''}
                    </span>
                </div>
            </div>

            {/* Create task form */}
            {showCreateForm && (
                <div className="fleet-task-create-form">
                    <input
                        type="text"
                        className="fleet-task-input"
                        placeholder="Task title (required)"
                        value={createTitle}
                        onChange={e => setCreateTitle(e.target.value)}
                        onKeyDown={e => { if (e.key === 'Enter' && createTitle.trim()) handleCreate(); }}
                        autoFocus
                    />
                    <textarea
                        className="fleet-task-textarea"
                        placeholder="Context (optional)"
                        value={createContext}
                        onChange={e => setCreateContext(e.target.value)}
                        rows={2}
                    />
                    <div className="fleet-task-create-row">
                        <select
                            className="fleet-task-select"
                            value={createPriority}
                            onChange={e => setCreatePriority(e.target.value)}
                        >
                            <option value="low">Low</option>
                            <option value="normal">Normal</option>
                            <option value="high">High</option>
                            <option value="urgent">Urgent</option>
                        </select>
                        <button
                            className="stewardship-btn stewardship-btn-primary"
                            onClick={handleCreate}
                            disabled={!createTitle.trim() || creating}
                        >
                            {creating ? 'Creating...' : 'Create Task'}
                        </button>
                    </div>
                </div>
            )}

            {tasks.length === 0 && !showCreateForm ? (
                <div className="fleet-mesh-empty">
                    <p>No tasks available.</p>
                </div>
            ) : (
                <div className="fleet-task-columns">
                    {STATUS_COLUMNS.map((col) => {
                        const isDoneCol = col.key === 'done';
                        const colTasks = grouped[col.key];
                        const collapsed = isDoneCol && doneCollapsed && colTasks.length > 0;

                        return (
                            <div key={col.key} className={`fleet-task-column ${isDoneCol ? 'fleet-task-column-done' : ''}`}>
                                <div
                                    className="fleet-task-column-header"
                                    onClick={isDoneCol ? () => setDoneCollapsed(!doneCollapsed) : undefined}
                                    style={isDoneCol ? { cursor: 'pointer' } : undefined}
                                >
                                    <span className="fleet-task-column-title">
                                        {col.label}
                                        {isDoneCol && colTasks.length > 0 && (
                                            <span className="fleet-task-collapse-indicator">
                                                {doneCollapsed ? ' +' : ' -'}
                                            </span>
                                        )}
                                    </span>
                                    <div className="fleet-task-column-header-actions">
                                        <span className="fleet-task-column-count">{colTasks.length}</span>
                                        {isDoneCol && colTasks.length > 0 && !doneCollapsed && (
                                            <button
                                                className="stewardship-btn stewardship-btn-ghost fleet-task-archive-all-btn"
                                                onClick={(e) => { e.stopPropagation(); handleArchiveAllDone(); }}
                                                disabled={actionInFlight === 'archive-all-done'}
                                            >
                                                {actionInFlight === 'archive-all-done' ? 'Archiving...' : 'Archive All'}
                                            </button>
                                        )}
                                    </div>
                                </div>
                                {!collapsed && (
                                    <div className="fleet-task-column-body">
                                        {colTasks.length === 0 ? (
                                            <p className="fleet-task-column-empty">No tasks</p>
                                        ) : (
                                            colTasks.map((task) => {
                                                const normalized = normalizeStatus(task.status);
                                                const isExpanded = expandedTaskId === task.id;
                                                const isActioning = actionInFlight === task.id;
                                                const pLabel = priorityLabel(task.priority);
                                                const pClass = priorityClass(task.priority);
                                                const dimmed = isDoneCol && isOlderThan7Days(task.completed_at || task.updated_at);
                                                const transitions = getValidTransitions(normalized);
                                                const canComplete = normalized === 'active' || normalized === 'review';

                                                return (
                                                    <div
                                                        key={task.id}
                                                        className={`fleet-task-card ${dimmed ? 'fleet-task-card-dimmed' : ''}`}
                                                    >
                                                        <div className="fleet-task-card-top">
                                                            <div
                                                                className="fleet-task-card-title"
                                                                onClick={() => setExpandedTaskId(isExpanded ? null : task.id)}
                                                                style={{ cursor: 'pointer' }}
                                                                title="Click to expand"
                                                            >
                                                                {task.title || task.id}
                                                            </div>
                                                            {pLabel && (
                                                                <span className={`fleet-task-priority-badge ${pClass}`}>
                                                                    {pLabel}
                                                                </span>
                                                            )}
                                                        </div>

                                                        {!isExpanded && task.context && (
                                                            <div className="fleet-task-card-desc">
                                                                {task.context}
                                                            </div>
                                                        )}

                                                        <div className="fleet-task-card-meta">
                                                            <span className={`fleet-task-status fleet-task-status-${normalized}`}>
                                                                {task.status}
                                                            </span>
                                                            {task.assigned_to && (
                                                                <span className="fleet-task-assignee">{task.assigned_to}</span>
                                                            )}
                                                        </div>

                                                        {/* Expanded detail */}
                                                        {isExpanded && (
                                                            <div className="fleet-task-detail">
                                                                {task.context && (
                                                                    <div className="fleet-task-detail-section">
                                                                        <span className="fleet-task-detail-label">Context</span>
                                                                        <span className="fleet-task-detail-value">{task.context}</span>
                                                                    </div>
                                                                )}
                                                                {task.assigned_to && (
                                                                    <div className="fleet-task-detail-section">
                                                                        <span className="fleet-task-detail-label">Assignee</span>
                                                                        <span className="fleet-task-detail-value">{task.assigned_to}</span>
                                                                    </div>
                                                                )}
                                                                {task.created_at && (
                                                                    <div className="fleet-task-detail-section">
                                                                        <span className="fleet-task-detail-label">Created</span>
                                                                        <span className="fleet-task-detail-value">{formatTimestamp(task.created_at)}</span>
                                                                    </div>
                                                                )}
                                                                {task.updated_at && (
                                                                    <div className="fleet-task-detail-section">
                                                                        <span className="fleet-task-detail-label">Updated</span>
                                                                        <span className="fleet-task-detail-value">{formatTimestamp(task.updated_at)}</span>
                                                                    </div>
                                                                )}
                                                                {task.completed_at && (
                                                                    <div className="fleet-task-detail-section">
                                                                        <span className="fleet-task-detail-label">Completed</span>
                                                                        <span className="fleet-task-detail-value">{formatTimestamp(task.completed_at)}</span>
                                                                    </div>
                                                                )}
                                                                {task.scope && (
                                                                    <div className="fleet-task-detail-section">
                                                                        <span className="fleet-task-detail-label">Scope</span>
                                                                        <span className="fleet-task-detail-value">{task.scope}</span>
                                                                    </div>
                                                                )}
                                                            </div>
                                                        )}

                                                        {/* Action buttons */}
                                                        <div className="fleet-task-card-actions">
                                                            {/* Claim button for backlog */}
                                                            {normalized === 'backlog' && (
                                                                <button
                                                                    className="stewardship-btn stewardship-btn-ghost fleet-task-claim-btn"
                                                                    onClick={() => handleClaim(task.id)}
                                                                    disabled={isActioning}
                                                                >
                                                                    {isActioning ? 'Claiming...' : 'Claim'}
                                                                </button>
                                                            )}

                                                            {/* Complete button for active/review */}
                                                            {canComplete && (
                                                                <button
                                                                    className="stewardship-btn stewardship-btn-ghost fleet-task-complete-btn"
                                                                    onClick={() => handleComplete(task.id)}
                                                                    disabled={isActioning}
                                                                >
                                                                    {isActioning ? 'Completing...' : 'Complete'}
                                                                </button>
                                                            )}

                                                            {/* Move dropdown for transitions */}
                                                            {transitions.length > 0 && !(normalized === 'backlog') && (
                                                                <div className="fleet-task-dropdown-wrap">
                                                                    <button
                                                                        className="stewardship-btn stewardship-btn-ghost fleet-task-move-btn"
                                                                        onClick={() => setOpenDropdownId(openDropdownId === task.id ? null : task.id)}
                                                                        disabled={isActioning}
                                                                    >
                                                                        Move {openDropdownId === task.id ? '\u25B4' : '\u25BE'}
                                                                    </button>
                                                                    {openDropdownId === task.id && (
                                                                        <div className="fleet-task-dropdown-menu">
                                                                            {transitions.map(t => {
                                                                                const { body, method } = t.action();
                                                                                return (
                                                                                    <button
                                                                                        key={t.label}
                                                                                        className="fleet-task-dropdown-item"
                                                                                        onClick={() => handleTaskAction(task.id, method, body)}
                                                                                        disabled={isActioning}
                                                                                    >
                                                                                        {t.label}
                                                                                    </button>
                                                                                );
                                                                            })}
                                                                        </div>
                                                                    )}
                                                                </div>
                                                            )}

                                                            {/* Archive button on every card */}
                                                            <button
                                                                className="stewardship-btn stewardship-btn-ghost fleet-task-archive-btn"
                                                                onClick={() => handleArchive(task.id)}
                                                                disabled={isActioning}
                                                                title="Archive task"
                                                            >
                                                                Archive
                                                            </button>
                                                        </div>
                                                    </div>
                                                );
                                            })
                                        )}
                                    </div>
                                )}
                            </div>
                        );
                    })}
                </div>
            )}
        </div>
    );
}
