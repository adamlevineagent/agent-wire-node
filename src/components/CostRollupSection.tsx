// Phase 13 — spend rollup section.
//
// Mounted on the CrossPyramidTimeline view in Phase 13 (Phase 15
// will move it to the DADBEAR Oversight page). Fetches the
// aggregated cost data from `pyramid_cost_rollup` and pivots it
// client-side into three views: by pyramid, by provider, by
// operation.

import { useEffect, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface CostBucket {
    slug: string;
    provider: string | null;
    operation: string;
    estimated: number;
    actual: number;
    call_count: number;
}

interface CostRollupResponse {
    total_estimated: number;
    total_actual: number;
    buckets: CostBucket[];
    from: string;
    to: string;
}

type Range = 'today' | 'week' | 'month';
type PivotBy = 'pyramid' | 'provider' | 'operation';

function formatCurrency(v: number) {
    return `$${v.toFixed(2)}`;
}

function pivotBuckets(
    buckets: CostBucket[],
    by: PivotBy,
): { key: string; estimated: number; actual: number; callCount: number }[] {
    const map = new Map<
        string,
        { key: string; estimated: number; actual: number; callCount: number }
    >();
    for (const b of buckets) {
        const key =
            by === 'pyramid'
                ? b.slug
                : by === 'provider'
                    ? b.provider ?? '(unknown)'
                    : b.operation;
        const entry = map.get(key) ?? {
            key,
            estimated: 0,
            actual: 0,
            callCount: 0,
        };
        entry.estimated += b.estimated;
        entry.actual += b.actual;
        entry.callCount += b.call_count;
        map.set(key, entry);
    }
    return Array.from(map.values()).sort((a, b) => b.estimated - a.estimated);
}

export function CostRollupSection() {
    const [range, setRange] = useState<Range>('today');
    const [data, setData] = useState<CostRollupResponse | null>(null);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [pivot, setPivot] = useState<PivotBy>('pyramid');

    const refresh = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
            const res = await invoke<CostRollupResponse>('pyramid_cost_rollup', {
                range,
                from: null,
                to: null,
            });
            setData(res);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    }, [range]);

    useEffect(() => {
        refresh();
    }, [refresh]);

    const pivoted = data ? pivotBuckets(data.buckets, pivot) : [];

    return (
        <div className="cost-rollup-section">
            <div className="cost-rollup-header">
                <h3>Spend Rollup</h3>
                <div className="cost-rollup-range-picker">
                    {(['today', 'week', 'month'] as Range[]).map(r => (
                        <button
                            key={r}
                            className={`cost-rollup-range ${r === range ? 'cost-rollup-range-active' : ''}`}
                            onClick={() => setRange(r)}
                        >
                            {r[0].toUpperCase() + r.slice(1)}
                        </button>
                    ))}
                </div>
            </div>

            {loading && <div className="cost-rollup-loading">Loading rollup…</div>}
            {error && <div className="cost-rollup-error">Error: {error}</div>}

            {data && !loading && (
                <>
                    <div className="cost-rollup-total">
                        Total: <strong>{formatCurrency(data.total_estimated)}</strong> est
                        {data.total_actual > 0 && (
                            <>
                                {' '}
                                / <strong>{formatCurrency(data.total_actual)}</strong> actual
                            </>
                        )}
                    </div>

                    <div className="cost-rollup-pivot-picker">
                        {(['pyramid', 'provider', 'operation'] as PivotBy[]).map(p => (
                            <button
                                key={p}
                                className={`cost-rollup-pivot ${p === pivot ? 'cost-rollup-pivot-active' : ''}`}
                                onClick={() => setPivot(p)}
                            >
                                By {p}
                            </button>
                        ))}
                    </div>

                    <div className="cost-rollup-buckets">
                        {pivoted.length === 0 ? (
                            <div className="cost-rollup-empty">
                                No cost data in this range yet.
                            </div>
                        ) : (
                            pivoted.map(bucket => (
                                <div key={bucket.key} className="cost-rollup-bucket">
                                    <span className="cost-rollup-bucket-key">{bucket.key}</span>
                                    <span className="cost-rollup-bucket-estimated">
                                        {formatCurrency(bucket.estimated)} est
                                    </span>
                                    {bucket.actual > 0 && (
                                        <span className="cost-rollup-bucket-actual">
                                            {formatCurrency(bucket.actual)} actual
                                        </span>
                                    )}
                                    <span className="cost-rollup-bucket-count">
                                        {bucket.callCount} calls
                                    </span>
                                </div>
                            ))
                        )}
                    </div>
                </>
            )}
        </div>
    );
}
