// Phase 15 — DADBEAR Oversight Page.
//
// Unified operator view assembling per-pyramid DADBEAR status,
// cost reconciliation, provider health, and orphan broadcasts.
// Composes components from earlier phases (CostRollupSection from
// Phase 13) plus new Phase 15 pieces (DadbearPyramidCard,
// ProviderHealthBanner, OrphanBroadcastsPanel, DadbearActivityDrawer).
// Spec: docs/specs/evidence-triage-and-dadbear.md Part 3 + Part 4.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppContext } from '../contexts/AppContext';
import { useDadbearOverviewV2 } from '../hooks/useDadbearOverviewV2';
import type { WorkItemOverviewRow } from '../hooks/useDadbearOverviewV2';
import { useProviderHealth } from '../hooks/useProviderHealth';
import { useOrphanBroadcasts } from '../hooks/useOrphanBroadcasts';
import { DadbearPyramidCard } from './DadbearPyramidCard';
import { ProviderHealthBanner } from './ProviderHealthBanner';
import { OrphanBroadcastsPanel } from './OrphanBroadcastsPanel';
import { DadbearActivityDrawer } from './DadbearActivityDrawer';
import { CostRollupSection } from './CostRollupSection';
import { requestToolsModePreset } from '../utils/toolsModeBridge';
// Phase 18c (L9): scope picker modal for the bulk pause/resume flow.
import {
    DadbearPauseScopeModal,
    type DadbearPauseScope,
} from './DadbearPauseScopeModal';

type PyramidFilter = 'all' | 'active' | 'paused' | 'breaker';

function currency(v: number): string {
    return `$${v.toFixed(2)}`;
}

export function DadbearOversightPage() {
    const { setMode } = useAppContext();
    const {
        data: overview,
        loading: overviewLoading,
        error: overviewError,
        refetch: refetchOverview,
    } = useDadbearOverviewV2(10_000);

    const {
        data: providerHealth,
        loading: providerLoading,
        error: providerError,
        acknowledge: acknowledgeProvider,
    } = useProviderHealth(30_000);

    const {
        data: orphans,
        loading: orphansLoading,
        error: orphansError,
        acknowledge: acknowledgeOrphan,
    } = useOrphanBroadcasts(60_000, false);

    const [filter, setFilter] = useState<PyramidFilter>('all');
    const [activityDrawerSlug, setActivityDrawerSlug] = useState<string | null>(
        null,
    );
    const [busyGlobal, setBusyGlobal] = useState(false);
    const [globalError, setGlobalError] = useState<string | null>(null);
    const [toast, setToast] = useState<string | null>(null);
    // Phase 18c (L9): scope picker modal for pause / resume. Single
    // state field — null when closed, "pause"/"resume" when open in
    // the corresponding action mode.
    const [scopeModalAction, setScopeModalAction] = useState<
        'pause' | 'resume' | null
    >(null);
    // Hold the pending toast-clear timeout so we can clear it on a
    // subsequent toast or on unmount. Without this, a queued timeout
    // from a previous toast can step on the next one, and a toast
    // fired right before unmount will call setState on an unmounted
    // component (React warning, not a crash — still worth fixing).
    const toastTimeoutRef = useRef<number | null>(null);

    const showToast = useCallback((msg: string) => {
        if (toastTimeoutRef.current !== null) {
            window.clearTimeout(toastTimeoutRef.current);
        }
        setToast(msg);
        toastTimeoutRef.current = window.setTimeout(() => {
            setToast(null);
            toastTimeoutRef.current = null;
        }, 4000);
    }, []);

    useEffect(() => {
        return () => {
            if (toastTimeoutRef.current !== null) {
                window.clearTimeout(toastTimeoutRef.current);
                toastTimeoutRef.current = null;
            }
        };
    }, []);

    // Phase 18c (L9): handlers receive (scope, scope_value) from the
    // scope picker modal and forward them to the existing IPCs which
    // already accept those parameters. The modal does the count
    // preview and validation; these handlers just dispatch.
    const handlePauseWithScope = useCallback(
        async (scope: DadbearPauseScope, scopeValue: string | null) => {
            setScopeModalAction(null);
            setBusyGlobal(true);
            setGlobalError(null);
            try {
                const resp = await invoke<{ affected: number }>(
                    'pyramid_freeze_all',
                    { scope, scopeValue },
                );
                await refetchOverview();
                showToast(
                    `Froze DADBEAR on ${resp.affected} pyramid${resp.affected === 1 ? '' : 's'}`,
                );
            } catch (e) {
                setGlobalError(String(e));
            } finally {
                setBusyGlobal(false);
            }
        },
        [refetchOverview, showToast],
    );

    const handleResumeWithScope = useCallback(
        async (scope: DadbearPauseScope, scopeValue: string | null) => {
            setScopeModalAction(null);
            setBusyGlobal(true);
            setGlobalError(null);
            try {
                const resp = await invoke<{ affected: number }>(
                    'pyramid_unfreeze_all',
                    { scope, scopeValue },
                );
                await refetchOverview();
                showToast(
                    `Unfroze DADBEAR on ${resp.affected} pyramid${resp.affected === 1 ? '' : 's'}`,
                );
            } catch (e) {
                setGlobalError(String(e));
            } finally {
                setBusyGlobal(false);
            }
        },
        [refetchOverview, showToast],
    );

    const handleSetDefaultNorms = useCallback(() => {
        // Queue the preset + switch to ToolsMode. The ToolsMode
        // bridge consumes the preset on mount / event and dispatches
        // a pick-schema for dadbear_norms, skipping the schema
        // picker and jumping straight to the intent step.
        requestToolsModePreset({
            schemaType: 'dadbear_norms',
            slug: null,
        });
        setMode('tools');
    }, [setMode]);

    const handleConfigurePyramid = useCallback(
        (slug: string) => {
            // Per-pyramid "Configure" opens the same flow but with
            // the pyramid slug bound. The user ends up editing a
            // pyramid-scoped dadbear_norms contribution.
            requestToolsModePreset({
                schemaType: 'dadbear_norms',
                slug,
            });
            setMode('tools');
        },
        [setMode],
    );

    // Work-item-centric filter: derived_status from holds projection.
    const filteredPyramids = useMemo(() => {
        if (!overview) return [];
        switch (filter) {
            case 'active':
                return overview.pyramids.filter((p) => p.derived_status === 'active');
            case 'paused':
                return overview.pyramids.filter(
                    (p) => p.derived_status === 'paused' || p.derived_status === 'held',
                );
            case 'breaker':
                return overview.pyramids.filter((p) => p.derived_status === 'breaker');
            default:
                return overview.pyramids;
        }
    }, [overview, filter]);

    const unacknowledgedOrphanCount = orphans.filter(
        (o) => !o.acknowledged_at,
    ).length;

    return (
        <div className="dadbear-oversight-page">
            <header className="dadbear-oversight-header">
                <h2>DADBEAR Oversight</h2>
                <div className="dadbear-oversight-subtitle">
                    Per-pyramid auto-update status, cost reconciliation, and
                    leak detection.
                </div>
            </header>

            {unacknowledgedOrphanCount > 0 && (
                <div
                    className="dadbear-oversight-leak-banner"
                    onClick={() => {
                        const el = document.getElementById(
                            'orphan-broadcasts',
                        );
                        if (el) {
                            el.scrollIntoView({ behavior: 'smooth' });
                        }
                    }}
                >
                    Orphan broadcasts detected — {unacknowledgedOrphanCount}{' '}
                    unreviewed. Potential credential leak.
                </div>
            )}

            {toast && <div className="dadbear-oversight-toast">{toast}</div>}
            {globalError && (
                <div className="dadbear-oversight-error">{globalError}</div>
            )}

            <section className="dadbear-oversight-globals">
                <div className="dadbear-oversight-global-controls">
                    {/* Phase 18c (L9): "Pause..." / "Resume..." (with
                        ellipsis) signals the scope picker modal opens. */}
                    <button
                        className="btn btn-danger"
                        disabled={busyGlobal}
                        onClick={() => setScopeModalAction('pause')}
                    >
                        Pause...
                    </button>
                    <button
                        className="btn btn-primary"
                        disabled={busyGlobal}
                        onClick={() => setScopeModalAction('resume')}
                    >
                        Resume...
                    </button>
                    <button
                        className="btn btn-secondary"
                        onClick={handleSetDefaultNorms}
                    >
                        Set Default Norms
                    </button>
                </div>
                {overview && (
                    <div className="dadbear-oversight-totals">
                        <span>
                            Active: <strong>{overview.totals.active_count}</strong>
                        </span>
                        <span>
                            Paused: <strong>{overview.totals.paused_count}</strong>
                        </span>
                        <span>
                            Breaker: <strong>{overview.totals.breaker_count}</strong>
                        </span>
                        <span>
                            Compiled:{' '}
                            <strong>{overview.totals.total_compiled}</strong>
                        </span>
                        <span>
                            Dispatched:{' '}
                            <strong>{overview.totals.total_dispatched}</strong>
                        </span>
                        <span>
                            Blocked:{' '}
                            <strong>{overview.totals.total_blocked}</strong>
                        </span>
                        <span>
                            24h cost:{' '}
                            <strong>
                                {currency(overview.totals.total_cost_24h_usd)}
                            </strong>
                        </span>
                    </div>
                )}
            </section>

            <section className="dadbear-oversight-pyramids">
                <div className="dadbear-oversight-pyramids-header">
                    <h3>Per-Pyramid Status</h3>
                    <div className="dadbear-oversight-filter">
                        {(['all', 'active', 'paused', 'breaker'] as PyramidFilter[]).map(
                            (f) => (
                                <button
                                    key={f}
                                    className={`dadbear-oversight-filter-btn ${
                                        f === filter
                                            ? 'dadbear-oversight-filter-btn-active'
                                            : ''
                                    }`}
                                    onClick={() => setFilter(f)}
                                >
                                    {f[0].toUpperCase() + f.slice(1)}
                                </button>
                            ),
                        )}
                    </div>
                </div>

                {overviewLoading && !overview && (
                    <div className="dadbear-oversight-loading">
                        Loading oversight…
                    </div>
                )}
                {overviewError && (
                    <div className="dadbear-oversight-error">
                        {overviewError}
                    </div>
                )}

                {overview && filteredPyramids.length === 0 && (
                    <div className="dadbear-oversight-empty">
                        {filter === 'all'
                            ? 'No pyramids with DADBEAR configuration.'
                            : `No ${filter} pyramids.`}
                    </div>
                )}

                <div className="dadbear-oversight-card-grid">
                    {filteredPyramids.map((row) => (
                        <DadbearPyramidCard
                            key={row.slug}
                            row={row}
                            onViewActivity={setActivityDrawerSlug}
                            onConfigure={handleConfigurePyramid}
                            onMutated={refetchOverview}
                        />
                    ))}
                </div>
            </section>

            <CostRollupSection />

            <ProviderHealthBanner
                data={providerHealth}
                loading={providerLoading}
                error={providerError}
                onAcknowledge={acknowledgeProvider}
            />

            <OrphanBroadcastsPanel
                data={orphans}
                loading={orphansLoading}
                error={orphansError}
                onAcknowledge={acknowledgeOrphan}
            />

            {activityDrawerSlug && (
                <DadbearActivityDrawer
                    slug={activityDrawerSlug}
                    onClose={() => setActivityDrawerSlug(null)}
                />
            )}

            {/* Phase 18c (L9): scope picker modal for pause / resume.
                Lives at the page level so the backdrop can cover the
                full oversight view. */}
            {scopeModalAction && (
                <DadbearPauseScopeModal
                    action={scopeModalAction}
                    onCancel={() => setScopeModalAction(null)}
                    onConfirm={
                        scopeModalAction === 'pause'
                            ? handlePauseWithScope
                            : handleResumeWithScope
                    }
                />
            )}
        </div>
    );
}
