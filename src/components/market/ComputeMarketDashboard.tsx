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

    const offerCount = snapshot ? Object.keys(snapshot.offers).length : 0;
    const activeJobCount = snapshot ? Object.keys(snapshot.active_jobs).length : 0;

    return (
        <div className="compute-market-dashboard" style={{ padding: 16 }}>
            <h1>Compute Market</h1>
            <p style={{ color: "#888", fontSize: 13 }}>
                Publish your GPU as a compute offer on the Wire. Market dispatches land
                in the same queue as local + fleet work; settlement is Wire-side.
            </p>

            {error && (
                <div role="alert" style={{ color: "#c33", padding: "8px 0" }}>
                    {error}
                </div>
            )}

            {loading && !snapshot ? (
                <p>Loading market state...</p>
            ) : snapshot ? (
                <section
                    style={{
                        border: "1px solid #ddd",
                        borderRadius: 6,
                        padding: 16,
                        marginBottom: 16,
                        display: "grid",
                        gridTemplateColumns: "repeat(4, 1fr) auto",
                        gap: 12,
                        alignItems: "center",
                    }}
                >
                    <Stat label="Serving" value={snapshot.is_serving ? "Yes" : "No"} emphasis={snapshot.is_serving} />
                    <Stat label="Offers" value={String(offerCount)} />
                    <Stat label="Active jobs" value={String(activeJobCount)} />
                    <Stat
                        label="Session credits"
                        value={String(snapshot.session_credits_earned)}
                        subtitle={`${snapshot.total_credits_earned} lifetime`}
                    />
                    <button
                        onClick={handleToggle}
                        disabled={toggling}
                        style={{
                            background: snapshot.is_serving ? "#c80" : "#3a3",
                            color: "white",
                            padding: "8px 16px",
                            border: "none",
                            borderRadius: 4,
                            cursor: toggling ? "wait" : "pointer",
                        }}
                    >
                        {toggling
                            ? "..."
                            : snapshot.is_serving
                              ? "Pause serving"
                              : "Start serving"}
                    </button>
                </section>
            ) : null}

            <nav style={{ borderBottom: "1px solid #ddd", marginBottom: 16 }}>
                <TabButton label="My offers" active={tab === "offers"} onClick={() => setTab("offers")} />
                <TabButton
                    label="Market surface"
                    active={tab === "surface"}
                    onClick={() => setTab("surface")}
                />
            </nav>

            {tab === "offers" && <ComputeOfferManager />}
            {tab === "surface" && <ComputeMarketSurface />}
        </div>
    );
}

function Stat({
    label,
    value,
    subtitle,
    emphasis,
}: {
    label: string;
    value: string;
    subtitle?: string;
    emphasis?: boolean;
}) {
    return (
        <div>
            <div style={{ color: "#888", fontSize: 11, textTransform: "uppercase" }}>{label}</div>
            <div
                style={{
                    fontSize: 20,
                    fontWeight: 600,
                    color: emphasis ? "#3a3" : "#222",
                }}
            >
                {value}
            </div>
            {subtitle && <div style={{ color: "#888", fontSize: 11 }}>{subtitle}</div>}
        </div>
    );
}

function TabButton({
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
            onClick={onClick}
            style={{
                background: "transparent",
                border: "none",
                padding: "8px 16px",
                borderBottom: active ? "2px solid #3a6ea5" : "2px solid transparent",
                fontWeight: active ? 600 : 400,
                cursor: "pointer",
                color: active ? "#3a6ea5" : "#555",
            }}
        >
            {label}
        </button>
    );
}
