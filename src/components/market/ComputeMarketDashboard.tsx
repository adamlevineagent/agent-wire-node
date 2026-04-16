// ComputeMarketDashboard.tsx — Top-level compute market page.
//
// Aggregates:
//   - is_serving toggle + observability summary from
//     `compute_market_get_state`.
//   - ComputeOfferManager (publish/edit/remove offers).
//   - ComputeMarketSurface (browse network pricing, read-only).
//
// Per `compute-market-phase-2-exchange.md` §IV + §III "compute_market_
// enable/disable": the `is_serving` toggle is the RUNTIME pause
// switch — distinct from the durable `compute_participation_policy.
// allow_market_visibility`. A node with allow_market_visibility=false
// AND is_serving=true still won't publish (policy gate wins). The
// UX should reflect both states.

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ComputeOfferManager } from "./ComputeOfferManager";
import { ComputeMarketSurface } from "./ComputeMarketSurface";

interface ComputeMarketStateSnapshot {
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

type Tab = "offers" | "surface";

export function ComputeMarketDashboard() {
    const [snapshot, setSnapshot] = useState<ComputeMarketStateSnapshot | null>(null);
    const [loading, setLoading] = useState(true);
    const [toggling, setToggling] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [tab, setTab] = useState<Tab>("offers");

    const refresh = useCallback(async () => {
        try {
            const snap = await invoke<ComputeMarketStateSnapshot>("compute_market_get_state");
            setSnapshot(snap);
            setError(null);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    }, []);

    useEffect(() => {
        void refresh();
        // Refresh every 5s to keep counters live. Cheap IPC, no Wire call.
        const handle = setInterval(() => void refresh(), 5000);
        return () => clearInterval(handle);
    }, [refresh]);

    const handleToggle = async () => {
        if (!snapshot) return;
        setToggling(true);
        setError(null);
        try {
            const cmd = snapshot.is_serving ? "compute_market_disable" : "compute_market_enable";
            await invoke(cmd);
            await refresh();
        } catch (e) {
            setError(String(e));
        } finally {
            setToggling(false);
        }
    };

    // Null-safe accessors — the initial render before the first IPC
    // returns has `snapshot === null`, and the `Record<string, unknown>`
    // fields can come back missing in degraded states. Falling back to
    // 0 everywhere prevents the "undefined" render bug on cold start.
    const isServing = snapshot?.is_serving ?? false;
    const offerCount = snapshot ? Object.keys(snapshot.offers ?? {}).length : 0;
    const activeJobCount = snapshot ? Object.keys(snapshot.active_jobs ?? {}).length : 0;
    const sessionJobs = snapshot?.session_jobs_completed ?? 0;
    const sessionCredits = snapshot?.session_credits_earned ?? 0;
    const lifetimeJobs = snapshot?.total_jobs_completed ?? 0;
    const lifetimeCredits = snapshot?.total_credits_earned ?? 0;

    return (
        <div className="compute-market-view">
            <header className="compute-market-hero">
                <div className="compute-market-hero-text">
                    <h2 className="compute-market-hero-title">Compute Market</h2>
                    <p className="compute-market-hero-sub">
                        Publish your GPU as a compute offer on the Wire. Market dispatches land
                        in the same queue as local + fleet work; settlement is Wire-side.
                    </p>
                </div>
                <button
                    className={`compute-market-toggle ${isServing ? "compute-market-toggle-on" : "compute-market-toggle-off"}`}
                    onClick={handleToggle}
                    disabled={toggling || loading}
                    title={
                        isServing
                            ? "Pause serving — stops pushing queue state to the Wire. Does not remove offers."
                            : "Start serving — resume pushing queue state. Requires allow_market_visibility on the participation policy."
                    }
                >
                    <span className="compute-market-toggle-dot" />
                    {toggling ? "…" : isServing ? "Pause serving" : "Start serving"}
                </button>
            </header>

            {error && (
                <div className="compute-market-error" role="alert">
                    {error}
                </div>
            )}

            <section className="compute-market-stats">
                <StatCard
                    label="Serving"
                    value={loading && !snapshot ? "…" : isServing ? "Yes" : "No"}
                    tone={isServing ? "positive" : "muted"}
                />
                <StatCard label="Offers" value={String(offerCount)} />
                <StatCard label="Active jobs" value={String(activeJobCount)} />
                <StatCard
                    label="Session credits"
                    value={formatCredits(sessionCredits)}
                    subtitle={`${sessionJobs} job${sessionJobs === 1 ? "" : "s"} · ${formatCredits(
                        lifetimeCredits,
                    )} lifetime (${lifetimeJobs})`}
                />
            </section>

            <nav className="compute-market-subtabs">
                <SubTab label="My offers" active={tab === "offers"} onClick={() => setTab("offers")} />
                <SubTab
                    label="Market surface"
                    active={tab === "surface"}
                    onClick={() => setTab("surface")}
                />
            </nav>

            <div className="compute-market-subtab-content">
                {tab === "offers" && <ComputeOfferManager />}
                {tab === "surface" && <ComputeMarketSurface />}
            </div>
        </div>
    );
}

interface StatCardProps {
    label: string;
    value: string;
    subtitle?: string;
    tone?: "default" | "positive" | "muted";
}

function StatCard({ label, value, subtitle, tone = "default" }: StatCardProps) {
    return (
        <div className={`compute-stat compute-stat-tone-${tone}`}>
            <div className="compute-stat-label">{label}</div>
            <div className="compute-stat-value">{value}</div>
            {subtitle && <div className="compute-stat-subtitle">{subtitle}</div>}
        </div>
    );
}

function SubTab({
    label,
    active,
    onClick,
}: {
    label: string;
    active: boolean;
    onClick: () => void;
}) {
    return (
        <button
            className={`compute-market-subtab ${active ? "compute-market-subtab-active" : ""}`}
            onClick={onClick}
        >
            {label}
        </button>
    );
}

/**
 * Format a credits i64 as a thousands-separated string.
 * Keeps integers exact (Pillar 9 — no float rounding on the
 * credit path).
 */
function formatCredits(n: number): string {
    if (!Number.isFinite(n)) return "0";
    return n.toLocaleString("en-US", { maximumFractionDigits: 0 });
}
