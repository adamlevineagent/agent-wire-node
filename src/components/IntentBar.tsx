import { useState, useRef, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppContext } from '../contexts/AppContext';
import { PlanWidget } from './planner/PlanWidgets';
import type { PlannerContext, PlanResult, OperationEntry, PlanStep } from '../types/planner';
import { OPERATION_FORMAT_VERSION } from '../types/planner';
import type { SlugInfo } from './pyramid-types';
import type { SyncState } from './Dashboard';
import { executeViaRegistry, type DispatchRegistry } from '../utils/commandDispatch';
import { buildChainDefinition } from '../config/wire-actions';

// --- State machine -----------------------------------------------------------

type IntentBarState =
    | { phase: 'idle' }
    | { phase: 'gathering'; intent: string }
    | { phase: 'planning'; intent: string }
    | { phase: 'preview'; intent: string; plan: PlanResult; context: PlannerContext }
    | { phase: 'executing'; intent: string; plan: PlanResult; currentStep: number }
    | { phase: 'complete'; intent: string; result: unknown }
    | { phase: 'error'; intent: string; error: string };

// --- Gathering helpers -------------------------------------------------------

interface GatheringProgress {
    pyramids: boolean;
    corpora: boolean;
    pulse: boolean;
    roster: boolean;
}

function transformSyncToCorpora(syncState: SyncState): PlannerContext['corpora'] {
    const byCorpus = new Map<string, { slug: string; path: string; doc_count: number }>();
    const folders = syncState.linked_folders ?? {};

    // Build corpus map from linked folders
    for (const [folderPath, folder] of Object.entries(folders)) {
        const slug = folder.corpus_slug;
        if (!byCorpus.has(slug)) {
            byCorpus.set(slug, { slug, path: folderPath, doc_count: 0 });
        }
    }

    // Count docs per corpus
    for (const doc of syncState.cached_documents ?? []) {
        const existing = byCorpus.get(doc.corpus_slug);
        if (existing) {
            existing.doc_count++;
        } else {
            byCorpus.set(doc.corpus_slug, {
                slug: doc.corpus_slug,
                path: doc.source_path,
                doc_count: 1,
            });
        }
    }

    return Array.from(byCorpus.values());
}

async function gatherContext(
    wireApiCall: (method: string, path: string, body?: unknown) => Promise<unknown>,
    creditBalance: number,
    onProgress: (progress: GatheringProgress) => void,
): Promise<PlannerContext> {
    const progress: GatheringProgress = { pyramids: false, corpora: false, pulse: false, roster: false };

    const context: PlannerContext = {
        pyramids: [],
        corpora: [],
        agents: [],
        fleet: { online_count: 0, task_count: 0 },
        balance: creditBalance,
    };

    const [pyramidsResult, syncResult, pulseResult, rosterResult] = await Promise.allSettled([
        invoke<SlugInfo[]>('pyramid_list_slugs').then(slugs => {
            progress.pyramids = true;
            onProgress({ ...progress });
            return slugs;
        }),
        invoke<SyncState>('get_sync_status').then(sync => {
            progress.corpora = true;
            onProgress({ ...progress });
            return sync;
        }),
        wireApiCall('GET', '/api/v1/wire/pulse').then(data => {
            progress.pulse = true;
            onProgress({ ...progress });
            return data;
        }),
        wireApiCall('GET', '/api/v1/wire/roster').then(data => {
            progress.roster = true;
            onProgress({ ...progress });
            return data;
        }),
    ]);

    if (pyramidsResult.status === 'fulfilled') {
        context.pyramids = pyramidsResult.value.map(s => ({
            slug: s.slug,
            node_count: s.node_count,
            content_type: s.content_type,
        }));
    }

    if (syncResult.status === 'fulfilled') {
        context.corpora = transformSyncToCorpora(syncResult.value);
    }

    if (pulseResult.status === 'fulfilled') {
        const pulse = pulseResult.value as Record<string, unknown>;
        const fleet = pulse.fleet as Record<string, unknown> | undefined;
        if (fleet) {
            context.fleet = {
                online_count: (fleet.online_count as number) ?? 0,
                task_count: (fleet.task_count as number) ?? 0,
            };
        }
    }

    if (rosterResult.status === 'fulfilled') {
        const roster = rosterResult.value as Record<string, unknown>[];
        if (Array.isArray(roster)) {
            context.agents = roster.map((a: Record<string, unknown>) => ({
                id: String(a.id ?? a.agent_id ?? ''),
                name: String(a.name ?? a.display_name ?? 'Unknown'),
                status: String(a.status ?? 'offline'),
            }));
        }
    }

    return context;
}

// --- Step execution ----------------------------------------------------------

// TODO: There are TWO things called "wireApiCall" that do different things:
// 1. The `wireApiCall` wrapper from AppContext — application code that calls invoke('wire_api_call').
//    This is used by IntentBar for context gathering, post-execution publishing, etc. FINE to use.
// 2. The `wire_api_call` Tauri command as a PLAN STEP — this is BLOCKED below. Plan steps must use
//    named vocabulary commands (like `submit_contribution`, `archive_agent`) that the dispatch
//    registry translates. The LLM should never produce `wire_api_call` as a step command.
// When we add post-execution chain publishing (Sprint 2 Phase 2), the publish call is application
// code using the wireApiCall wrapper (#1), NOT a planner-generated step.

/** Commands that must never be invoked by the planner. Everything else is allowed. */
const BLOCKED_COMMANDS = new Set([
    // System lifecycle — never planner-invocable
    'logout', 'install_update', 'save_onboarding',
    // Recursive — would call itself
    'planner_call',
    // Auth flow — could trigger emails or change session
    'send_magic_link', 'verify_magic_link', 'verify_otp', 'login', 'auth_complete_ipc',
    // Session/system exposure
    'get_operator_session', 'get_home_dir',
    // Raw API commands — must go through vocabulary registry translation
    'wire_api_call', 'operator_api_call',
    // Internal build operations — not user-facing
    'pyramid_ingest', 'pyramid_parity_run', 'pyramid_meta_run',
    'pyramid_crystallize', 'pyramid_chain_import',
    // Partner system — planner should not impersonate
    'partner_send_message', 'partner_session_new',
    // Destructive vine operation
    'pyramid_vine_rebuild_upper',
    // Destructive data operations — require manual UI, not planner
    'pyramid_delete_slug', 'pyramid_purge_slug',
    // Opens arbitrary local files
    'open_file',
    // Irreversible identity/auth operations
    'regenerate_agent_token', 'merge_agents',
]);

/** Safe-tier commands that can auto-execute without preview (Pillar 23).
 *  Navigate commands and read-only queries. Everything else requires approval. */
const SAFE_COMMANDS = new Set([
    // Navigation
    'go_to_pyramids', 'go_to_knowledge', 'go_to_tools', 'go_to_fleet',
    'go_to_operations', 'go_to_search', 'go_to_compose', 'go_to_dashboard',
    'go_to_identity', 'go_to_settings', 'go_to_fleet_tasks', 'go_to_fleet_mesh',
    // Read-only queries
    'pyramid_list_slugs', 'pyramid_apex', 'pyramid_node', 'pyramid_tree',
    'pyramid_drill', 'pyramid_search', 'pyramid_get_references',
    'pyramid_get_composed_view', 'pyramid_list_question_overlays',
    'pyramid_get_publication_status', 'pyramid_cost_summary', 'pyramid_stale_log',
    'pyramid_annotations_recent', 'pyramid_faq_directory', 'pyramid_faq_category_drill',
    'pyramid_get_config', 'pyramid_auto_update_config_get', 'pyramid_auto_update_status',
    // Read-only system
    'get_config', 'get_health_status', 'get_node_name', 'is_onboarded',
    'get_credits', 'get_tunnel_status', 'get_sync_status',
    'list_my_corpora', 'list_public_corpora',
    'get_compose_drafts',
    // Read-only fleet
    'list_operator_agents',
]);

/** Check if ALL steps in a plan are safe-tier (can auto-execute without preview) */
function isAllSafeTier(steps: PlanStep[]): boolean {
    return steps.every(step => {
        if (step.navigate) return true; // Legacy navigate is always safe
        if (!step.command) return false;
        return SAFE_COMMANDS.has(step.command);
    });
}

async function executeStep(
    step: PlanStep,
    _context: PlannerContext,
    _dispatch: ReturnType<typeof useAppContext>['dispatch'],
    setMode: ReturnType<typeof useAppContext>['setMode'],
    navigateView: ReturnType<typeof useAppContext>['navigateView'],
    _operationId: string,
    registry: DispatchRegistry | null,
): Promise<unknown> {
    if (step.command) {
        if (BLOCKED_COMMANDS.has(step.command)) {
            throw new Error(`Command blocked: ${step.command}`);
        }

        // Vocabulary registry is the allowlist — if we have a registry,
        // the command MUST be in it. No fallthrough to raw invoke.
        if (registry) {
            return executeViaRegistry(
                step.command,
                step.args ?? {},
                registry,
                (mode) => setMode(mode as Parameters<typeof setMode>[0]),
                (mode, view, props) => navigateView(
                    mode as Parameters<typeof navigateView>[0],
                    view,
                    props,
                ),
            );
        }

        // Fallback: no registry loaded yet — direct invoke (transitional)
        return invoke(step.command, step.args ?? {});
    }

    // Legacy navigate path (deprecated — use named commands like go_to_fleet)
    if (step.navigate) {
        setMode(step.navigate.mode as Parameters<typeof setMode>[0]);
        if (step.navigate.view || step.navigate.props) {
            navigateView(
                step.navigate.mode as Parameters<typeof navigateView>[0],
                step.navigate.view ?? '',
                step.navigate.props ?? {},
            );
        }
        return { navigated: step.navigate.mode };
    }

    throw new Error(`Step ${step.id} has no command or navigate`);
}

// --- Component ---------------------------------------------------------------

export function IntentBar() {
    const { state, dispatch, setMode, navigateView, wireApiCall } = useAppContext();
    const [barState, setBarState] = useState<IntentBarState>({ phase: 'idle' });
    const [input, setInput] = useState('');
    const [widgetValues, setWidgetValues] = useState<Record<string, unknown>>({});
    const [gatherProgress, setGatherProgress] = useState<GatheringProgress | null>(null);
    const [vocabRegistry, setVocabRegistry] = useState<DispatchRegistry | null>(null);
    const inputRef = useRef<HTMLInputElement>(null);
    const cancelRef = useRef(false);
    const completeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    const handleApproveRef = useRef<(() => void) | null>(null);

    const isIdle = barState.phase === 'idle';

    // Load vocabulary registry on mount
    useEffect(() => {
        invoke('get_vocabulary_registry')
            .then((reg) => setVocabRegistry(reg as DispatchRegistry))
            .catch((err) => console.warn('Failed to load vocabulary registry:', err));
    }, []);

    // Auto-collapse preview on mode change
    useEffect(() => {
        if (barState.phase === 'preview') {
            setBarState({ phase: 'idle' });
            setWidgetValues({});
            setGatherProgress(null);
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [state.activeMode]);

    // Cleanup complete timer
    useEffect(() => {
        return () => {
            if (completeTimerRef.current) clearTimeout(completeTimerRef.current);
        };
    }, []);

    const handleCancel = useCallback(() => {
        cancelRef.current = true;
        setBarState({ phase: 'idle' });
        setGatherProgress(null);
        setWidgetValues({});
    }, []);

    const handleSubmit = useCallback(async (e: React.FormEvent) => {
        e.preventDefault();
        const intent = input.trim();
        if (!intent || !isIdle) return;

        cancelRef.current = false;
        setBarState({ phase: 'gathering', intent });
        setGatherProgress({ pyramids: false, corpora: false, pulse: false, roster: false });

        try {
            // Phase: Gathering
            const context = await gatherContext(wireApiCall, state.creditBalance, setGatherProgress);
            if (cancelRef.current) return;

            // Phase: Planning (single LLM call with full vocabulary)
            setBarState({ phase: 'planning', intent });

            let plan: PlanResult;
            try {
                plan = await invoke<PlanResult>('planner_call', {
                    intent,
                    context: JSON.parse(JSON.stringify(context)),
                });
            } catch (planErr) {
                if (cancelRef.current) return;
                // Retry once with JSON nudge
                try {
                    plan = await invoke<PlanResult>('planner_call', {
                        intent: intent + '\n\nPlease respond with valid JSON only.',
                        context: JSON.parse(JSON.stringify(context)),
                    });
                } catch (retryErr) {
                    if (cancelRef.current) return;
                    setBarState({ phase: 'error', intent, error: String(retryErr) });
                    return;
                }
            }

            if (cancelRef.current) return;

            // Ensure confirmation widget exists — LLM sometimes omits it
            const hasConfirmation = (plan.ui_schema ?? []).some(
                (w: { type: string }) => w.type === 'confirmation',
            );
            if (!hasConfirmation) {
                plan.ui_schema = [
                    ...(plan.ui_schema ?? []),
                    {
                        type: 'confirmation',
                        summary: plan.intent,
                        details: `${plan.steps.length} step(s) will execute.`,
                    },
                ];
            }

            // Filter out unknown widget types
            const KNOWN_WIDGETS = new Set([
                'corpus_selector', 'text_input', 'select', 'agent_selector',
                'toggle', 'checkbox', 'cost_preview', 'confirmation',
            ]);
            plan.ui_schema = (plan.ui_schema ?? []).filter(
                (w: { type: string }) => KNOWN_WIDGETS.has(w.type),
            );

            // Inject cost preview with per-step classification
            const hasExistingCostPreview = plan.ui_schema.some(
                (w: { type: string }) => w.type === 'cost_preview',
            );
            if (!hasExistingCostPreview && plan.steps.length > 0) {
                // Classify each step's cost by looking up dispatch type in vocabulary registry
                const breakdown: Record<string, unknown> = {};
                for (const step of plan.steps) {
                    if (!step.command) {
                        breakdown[step.description || 'Navigation'] = 'Free';
                        continue;
                    }
                    const entry = vocabRegistry?.[step.command];
                    if (!entry) {
                        breakdown[step.description || step.command] = 'Cost varies';
                        continue;
                    }
                    const dt = entry.dispatch?.type;
                    if (dt === 'navigate') {
                        breakdown[step.description || step.command] = 'Free';
                    } else if (dt === 'tauri') {
                        const isLlm = step.command.includes('build') || step.command.includes('question');
                        breakdown[step.description || step.command] = isLlm ? 'Local LLM cost' : 'Free';
                    } else if (dt === 'wire_api' || dt === 'operator_api') {
                        breakdown[step.description || step.command] = 'Wire credits (dynamic)';
                    } else {
                        breakdown[step.description || step.command] = 'Cost varies';
                    }
                }
                const costWidget = {
                    type: 'cost_preview' as const,
                    amount: undefined as number | undefined,
                    breakdown,
                };
                plan.ui_schema.unshift(costWidget as typeof plan.ui_schema[number]);
            }

            // Inject publish toggle before the confirmation widget
            const confirmIdx = plan.ui_schema.findIndex(
                (w: { type: string }) => w.type === 'confirmation',
            );
            const publishToggle = {
                type: 'toggle' as const,
                field: 'publish_chain',
                label: 'Publish this plan to the Wire (makes it public and reusable)',
                default: false,
            };
            if (confirmIdx >= 0) {
                plan.ui_schema.splice(confirmIdx, 0, publishToggle);
            } else {
                plan.ui_schema.push(publishToggle);
            }

            // Phase: Preview or Auto-Execute
            if (state.autoExecute && isAllSafeTier(plan.steps)) {
                // Safe plan + auto-execute ON → skip preview, execute immediately
                setBarState({ phase: 'preview', intent, plan, context });
                setInput('');
                // Trigger execution in next tick (after state settles)
                setTimeout(() => handleApproveRef.current?.(), 0);
            } else {
                // Effectful plan OR auto-execute OFF → show preview for approval
                setBarState({ phase: 'preview', intent, plan, context });
                setInput('');
            }
        } catch (err) {
            if (cancelRef.current) return;
            setBarState({ phase: 'error', intent, error: String(err) });
        }
    }, [input, isIdle, wireApiCall, state.creditBalance, state.autoExecute, vocabRegistry]);

    const handleApprove = useCallback(async () => {
        if (barState.phase !== 'preview') return;
        const { intent, plan, context } = barState;

        const operationId = `op-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
        const operation: OperationEntry = {
            id: operationId,
            intent,
            status: 'running',
            steps: plan.steps,
            currentStep: 0,
            startedAt: Date.now(),
            stepErrors: [],
            format_version: OPERATION_FORMAT_VERSION,
        };

        dispatch({ type: 'ADD_OPERATION', operation });
        setBarState({ phase: 'executing', intent, plan, currentStep: 0 });

        // Merge widget values into plan steps before execution
        // Each widget has a `field` prop — always apply user-provided values
        const mergedSteps = plan.steps.map(step => {
            const merged = { ...step, args: { ...(step.args ?? {}) } };
            for (const widget of plan.ui_schema ?? []) {
                if ('field' in widget && widget.field) {
                    const widgetVal = widgetValues[widget.field];
                    if (widgetVal !== undefined) {
                        merged.args[widget.field] = widgetVal;
                    }
                }
            }
            return merged;
        });

        let lastResult: unknown = null;
        let aborted = false;
        const accumulatedErrors: NonNullable<OperationEntry['stepErrors']> = [];

        // EXECUTION PATH: Currently always local (node executes steps via vocabulary registry).
        // When the Wire platform has helper support (platform-managed ephemeral agents),
        // add a second execution path here:
        //
        // if (autoFulfill === 'helpers' && wireHelpersAvailable) {
        //   // Dispatch chain to Wire: POST /api/v1/wire/action/chain
        //   // with mode: 'trusted', chain definition from mergedSteps
        //   // Poll GET /api/v1/wire/action/chain/{chainId}/status every 5s
        //   // Handle async completion, cancellation, and errors
        //   // Requires: executeLlmStep on Wire server, async job queue,
        //   // helper pool infrastructure, chain status endpoint
        //   // See docs/plans/v2-alpha-sprints/sprint-4-auto-fulfill.md
        // }
        //
        // For now, all execution is local — the node has OpenRouter key + vocabulary registry.

        for (let i = 0; i < mergedSteps.length; i++) {
            // Cancel check — allows stopping mid-execution
            if (cancelRef.current) {
                dispatch({ type: 'COMPLETE_OPERATION', id: operationId, error: 'Cancelled by user' });
                setBarState({ phase: 'error', intent, error: 'Execution cancelled' });
                aborted = true;
                break;
            }

            const step = mergedSteps[i];
            setBarState(prev =>
                prev.phase === 'executing' ? { ...prev, currentStep: i } : prev,
            );
            dispatch({ type: 'UPDATE_OPERATION', id: operationId, updates: { currentStep: i } });

            try {
                // Minimum 500ms per step so the user can see progress
                const stepStart = Date.now();
                lastResult = await executeStep(step, context, dispatch, setMode, navigateView, operationId, vocabRegistry);
                const elapsed = Date.now() - stepStart;
                if (elapsed < 500) {
                    await new Promise(r => setTimeout(r, 500 - elapsed));
                }
            } catch (err) {
                const stepError = {
                    stepId: step.id,
                    command: step.command,
                    args: step.args,
                    error: String(err),
                };
                accumulatedErrors.push(stepError);
                dispatch({
                    type: 'UPDATE_OPERATION',
                    id: operationId,
                    updates: {
                        stepErrors: [...accumulatedErrors],
                    },
                });

                if (step.on_error === 'abort') {
                    dispatch({ type: 'COMPLETE_OPERATION', id: operationId, error: String(err) });
                    setBarState({ phase: 'error', intent, error: `Step "${step.description}" failed: ${String(err)}` });
                    aborted = true;
                    break;
                }
                // Default: continue to next step
            }
        }

        if (!aborted) {
            dispatch({ type: 'COMPLETE_OPERATION', id: operationId, result: lastResult });
            // Show errors in the complete banner if any steps failed (even with continue)
            const errorSuffix = accumulatedErrors.length > 0
                ? ` (${accumulatedErrors.length} step error${accumulatedErrors.length > 1 ? 's' : ''} — see Operations for details)`
                : '';
            setBarState({ phase: 'complete', intent: intent + errorSuffix, result: lastResult });

            // Sprint 2: Post-execution chain publishing
            // This is APPLICATION CODE using the wireApiCall context wrapper — NOT a plan step.
            // See TODO comment above BLOCKED_COMMANDS for the distinction.
            const shouldPublish = widgetValues['publish_chain'] === true;
            if (shouldPublish && accumulatedErrors.length === 0) {
                try {
                    const chainBody = buildChainDefinition(plan.steps, plan.ui_schema ?? []);
                    const title = intent.length > 200 ? intent.slice(0, 197) + '...' : intent;
                    await wireApiCall('POST', '/api/v1/contribute', {
                        type: 'action',
                        title,
                        body: chainBody,
                        pricing_mode: 'emergent',
                        topics: ['chain', 'user-plan'],
                    });
                    // Published successfully — could show notification here
                    console.info('Chain published to Wire:', title);
                } catch (pubErr) {
                    // Publish failure does NOT fail the operation — plan already executed successfully
                    console.warn('Failed to publish chain to Wire:', pubErr);
                }
            }

            // Auto-dismiss the intent bar banner after 30 seconds
            // (the operation stays in Operations tab — this only clears the inline banner)
            completeTimerRef.current = setTimeout(() => {
                setBarState({ phase: 'idle' });
                completeTimerRef.current = null;
            }, 30000);
        }
    }, [barState, dispatch, setMode, navigateView, vocabRegistry, widgetValues, wireApiCall]);

    // Keep ref in sync for auto-execute
    useEffect(() => { handleApproveRef.current = handleApprove; }, [handleApprove]);

    const handlePreviewCancel = useCallback(() => {
        setBarState({ phase: 'idle' });
        setWidgetValues({});
        setGatherProgress(null);
    }, []);

    const handleWidgetChange = useCallback((field: string, value: unknown) => {
        setWidgetValues(prev => ({ ...prev, [field]: value }));
    }, []);

    const handleRetry = useCallback(() => {
        if (barState.phase === 'error') {
            setInput(barState.intent);
        }
        setBarState({ phase: 'idle' });
        setWidgetValues({});
        setGatherProgress(null);
    }, [barState]);

    const handleCompleteClick = useCallback(() => {
        if (completeTimerRef.current) {
            clearTimeout(completeTimerRef.current);
            completeTimerRef.current = null;
        }
        // Navigate to operations to see result
        setMode('operations');
        setBarState({ phase: 'idle' });
    }, [setMode]);

    // --- Render ----------------------------------------------------------------

    return (
        <div className="intent-bar-wrapper">
            <form className="intent-bar" onSubmit={handleSubmit}>
                <input
                    ref={inputRef}
                    className="intent-bar-input"
                    type="text"
                    placeholder="What do you want to do?"
                    value={input}
                    onChange={(e) => setInput(e.target.value)}
                    disabled={!isIdle}
                />
                {isIdle ? (
                    <button className="intent-bar-submit" type="submit">Go</button>
                ) : (barState.phase === 'gathering' || barState.phase === 'planning') ? (
                    <button
                        className="intent-bar-submit intent-bar-cancel"
                        type="button"
                        onClick={handleCancel}
                    >
                        Cancel
                    </button>
                ) : null}
            </form>

            {/* Gathering transparency card */}
            {barState.phase === 'gathering' && gatherProgress && (
                <div className="intent-bar-panel intent-bar-gathering">
                    <div className="intent-bar-panel-header">
                        <span className="intent-bar-spinner" />
                        <span>Gathering context...</span>
                    </div>
                    <div className="intent-bar-gather-items">
                        <GatherItem label="Pyramids" done={gatherProgress.pyramids} />
                        <GatherItem label="Corpora" done={gatherProgress.corpora} />
                        <GatherItem label="Fleet status" done={gatherProgress.pulse} />
                        <GatherItem label="Agent roster" done={gatherProgress.roster} />
                    </div>
                </div>
            )}

            {/* Planning spinner */}
            {barState.phase === 'planning' && (
                <div className="intent-bar-panel intent-bar-planning">
                    <span className="intent-bar-spinner" />
                    <span>Planning...</span>
                </div>
            )}

            {/* Preview panel */}
            {barState.phase === 'preview' && (
                <div className="intent-bar-panel intent-bar-preview">
                    <div className="intent-bar-panel-header">
                        <span className="intent-bar-panel-title">Plan Preview</span>
                        <span className="intent-bar-panel-intent">{barState.intent}</span>
                    </div>
                    <div className="intent-bar-preview-steps">
                        {barState.plan.steps.map((step, i) => (
                            <div key={step.id} className="intent-bar-step">
                                <span className="intent-bar-step-num">{i + 1}</span>
                                <div className="intent-bar-step-content">
                                    <span className="intent-bar-step-desc">{step.description}</span>
                                    <details className="intent-bar-step-details">
                                        <summary>Details</summary>
                                        <pre className="intent-bar-step-technical">
                                            {step.command && `command: ${step.command}\nargs: ${JSON.stringify(step.args ?? {}, null, 2)}`}
                                            {step.navigate && `navigate: ${step.navigate.mode}\nprops: ${JSON.stringify(step.navigate.props ?? {}, null, 2)}`}
                                            {step.on_error && `\non_error: ${step.on_error}`}
                                        </pre>
                                    </details>
                                </div>
                            </div>
                        ))}
                    </div>
                    <div className="intent-bar-preview-widgets">
                        {barState.plan.ui_schema.map((widget, i) => (
                            <PlanWidget
                                key={widget.field ?? `widget-${i}`}
                                widget={widget}
                                value={widget.field ? widgetValues[widget.field] : undefined}
                                onChange={handleWidgetChange}
                                context={barState.context}
                                onApprove={handleApprove}
                                onCancel={handlePreviewCancel}
                            />
                        ))}
                    </div>
                </div>
            )}

            {/* Executing progress */}
            {barState.phase === 'executing' && (
                <div className="intent-bar-panel intent-bar-executing">
                    <span className="intent-bar-spinner" />
                    <span>
                        Executing step {barState.currentStep + 1} of {barState.plan.steps.length}
                        {barState.plan.steps[barState.currentStep] && (
                            <> — {barState.plan.steps[barState.currentStep].description}</>
                        )}
                    </span>
                </div>
            )}

            {/* Complete */}
            {barState.phase === 'complete' && (
                <div className="intent-bar-panel intent-bar-complete">
                    <span className="intent-bar-complete-icon">Done!</span>
                    <button
                        className="intent-bar-link"
                        type="button"
                        onClick={handleCompleteClick}
                    >
                        View result
                    </button>
                </div>
            )}

            {/* Error */}
            {barState.phase === 'error' && (
                <div className="intent-bar-panel intent-bar-error">
                    <span className="intent-bar-error-msg">{barState.error}</span>
                    <button
                        className="intent-bar-submit"
                        type="button"
                        onClick={handleRetry}
                    >
                        Retry
                    </button>
                </div>
            )}
        </div>
    );
}

// --- Sub-components ----------------------------------------------------------

function GatherItem({ label, done }: { label: string; done: boolean }) {
    return (
        <div className={`intent-bar-gather-item ${done ? 'intent-bar-gather-done' : ''}`}>
            <span className="intent-bar-gather-check">{done ? '\u2713' : '\u00B7'}</span>
            <span>{label}</span>
        </div>
    );
}
