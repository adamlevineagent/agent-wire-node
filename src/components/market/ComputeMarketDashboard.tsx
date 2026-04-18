// ComputeMarketDashboard.tsx — Top-level Market → Compute surface.
//
// FRAMING (authoritative — see docs/plans/compute-market-invisibility-ux.md):
//
// The compute market is a cooperative compute pool. Under the hood there's
// real market machinery — rotator arm, prices, queue discounts, settlement.
// But to the operator, the value proposition is *network connectivity*:
// "my pyramids build fast because I'm in the network; when I'm idle, I
// help others." Credits are accounting; the ledger exists, but isn't the
// point.
//
// This component is now a thin shell:
//   1. Poll the three IPCs needed to derive the network state.
//   2. Render the ComputeNetworkStatus hero (6 possible card variants).
//   3. Wrap the existing trader-surface components (offer config,
//      market surface, policy matrix, raw ledger) in an AdvancedDrawer
//      that's default-closed.
//
// The "Compute Market" hero title and the stats grid that used to live
// here have been demoted to the Advanced drawer. Every string above
// the fold is now in cooperative/connectivity language per the plan
// doc's language contract.

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AdvancedDrawer } from "./AdvancedDrawer";
import {
    ComputeNetworkStatus,
    deriveNetworkState,
} from "./ComputeNetworkStatus";
import { ComputeOfferManager } from "./ComputeOfferManager";
import { ComputeMarketSurface } from "./ComputeMarketSurface";
import type {
    ComputeEvent,
    ComputeMarketStateSnapshot,
    LocalModeStatus,
} from "./types";

/// Refresh cadence for the snapshot + local-mode + recent-events
/// triple. 5 seconds matches the prior dashboard's cadence; none of
/// these IPCs hit the Wire, they're all local SQLite reads.
const REFRESH_INTERVAL_MS = 5_000;

/// Lookback for the "helped builds" activity indicator. An hour
/// strikes the right "recent enough to be meaningful, long enough to
/// register idle activity" balance.
const RECENT_EVENTS_WINDOW_MS = 60 * 60 * 1_000;

/// Session-scoped dismissal for the consumer-invite card. Keyed in
/// sessionStorage so the invite returns on the next session but stays
/// quiet for this one after the operator clicks "Not right now."
const CONSUMER_INVITE_DISMISSED_KEY = "wire.compute.consumer-invite-dismissed";

export function ComputeMarketDashboard() {
    const [snapshot, setSnapshot] = useState<ComputeMarketStateSnapshot | null>(null);
    const [localMode, setLocalMode] = useState<LocalModeStatus | null>(null);
    const [recentEvents, setRecentEvents] = useState<ComputeEvent[]>([]);
    const [toggling, setToggling] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [consumerDismissed, setConsumerDismissed] = useState<boolean>(() => {
        try {
            return sessionStorage.getItem(CONSUMER_INVITE_DISMISSED_KEY) === "1";
        } catch {
            return false;
        }
    });

    const refresh = useCallback(async () => {
        // Three IPCs in parallel. None hit the Wire — all local reads —
        // so there's no latency reason to serialize, and the surface
        // stays coherent if they all land at once.
        //
        // Note: we don't call `get_compute_summary` here. That query
        // returns event-counts + latency dimensions but not credit
        // totals. Credit roll-ups live on ComputeMarketState itself
        // (session_credits_earned, total_credits_earned). A real
        // week-scoped rollup would need a credit-scoped chronicle
        // query — deferred until we have the observability need.
        const since = new Date(Date.now() - RECENT_EVENTS_WINDOW_MS).toISOString();

        const [snapRes, localRes, eventsRes] = await Promise.allSettled([
            invoke<ComputeMarketStateSnapshot>("compute_market_get_state"),
            invoke<LocalModeStatus>("pyramid_get_local_mode_status"),
            invoke<ComputeEvent[]>("get_compute_events", {
                // chronicle filter: only provider-side accept events in
                // the last hour. Matches EVENT_MARKET_RECEIVED emissions
                // from spawn_market_worker — the "we helped a build"
                // signal we want to surface.
                eventType: "market_received",
                after: since,
                limit: 50,
            }),
        ]);

        let firstError: string | null = null;
        if (snapRes.status === "fulfilled") setSnapshot(snapRes.value);
        else firstError ??= String(snapRes.reason);
        if (localRes.status === "fulfilled") setLocalMode(localRes.value);
        // Local-mode read failures are recoverable — the hero falls
        // back to "no model" rendering. Don't surface them as top-level
        // errors; that'd make the error banner spam on fresh installs.
        if (eventsRes.status === "fulfilled") setRecentEvents(eventsRes.value ?? []);
        else setRecentEvents([]);

        setError(firstError);
    }, []);

    useEffect(() => {
        void refresh();
        const handle = setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
        return () => clearInterval(handle);
    }, [refresh]);

    const handleToggleServing = async () => {
        if (!snapshot) return;
        setToggling(true);
        setError(null);
        try {
            const wasServing = snapshot.is_serving;
            const cmd = wasServing ? "compute_market_disable" : "compute_market_enable";
            await invoke(cmd);

            // Cooperative-network UX: when the operator turns serving ON,
            // auto-publish a default offer for the routing model if no
            // offer exists yet. The "Contribute GPU when idle" toggle is
            // the operator intent — they don't need to also go to the
            // Advanced drawer and fill out a rate form. Power operators
            // who want custom rates can still edit via Advanced.
            //
            // Skip if: there's already any offer published, no routing
            // model is loaded, or local mode is disabled (bridge-only
            // operators can configure offers explicitly).
            if (!wasServing) {
                try {
                    const existing = await invoke<Array<{ model_id: string }>>("compute_offers_list");
                    if (existing.length === 0 && localMode?.enabled && localMode.model) {
                        await invoke("compute_offer_create", {
                            offer: {
                                model_id: localMode.model,
                                provider_type: "local",
                                // Modest defaults; power operators can customize
                                // via Advanced → offer edit. Values mirror the
                                // Advanced drawer's emptyForm defaults.
                                rate_per_m_input: 100,
                                rate_per_m_output: 500,
                                reservation_fee: 10,
                                queue_discount_curve: [],
                                max_queue_depth: 8,
                            },
                        });
                    }
                } catch (autoErr) {
                    // Auto-publish is a convenience. If it fails (Wire
                    // rejects, model-not-loaded race, etc.), don't roll
                    // back serving — the operator can still publish
                    // manually via Advanced → New offer.
                    console.warn("auto-publish default offer failed:", autoErr);
                }
            }

            await refresh();
        } catch (e) {
            setError(String(e));
        } finally {
            setToggling(false);
        }
    };

    const handleDismissConsumerInvite = () => {
        setConsumerDismissed(true);
        try {
            sessionStorage.setItem(CONSUMER_INVITE_DISMISSED_KEY, "1");
        } catch {
            /* private-mode browsers — no-op */
        }
    };

    const networkState = useMemo(
        () => deriveNetworkState(snapshot, localMode, recentEvents, consumerDismissed),
        [snapshot, localMode, recentEvents, consumerDismissed],
    );

    return (
        <div className="compute-market-view">
            <ComputeNetworkStatus
                state={networkState}
                snapshot={snapshot}
                localMode={localMode}
                recentEvents={recentEvents}
                onToggleServing={handleToggleServing}
                togglingServing={toggling}
                onDismissConsumerInvite={handleDismissConsumerInvite}
                consumerInviteDismissed={consumerDismissed}
            />

            {error && (
                <div className="compute-market-error" role="alert">
                    {error}
                </div>
            )}

            <AdvancedDrawer label="Advanced" hint="rates, offers, market inspector">
                <div className="compute-advanced-section">
                    <h4 className="compute-advanced-heading">Your offers</h4>
                    <ComputeOfferManager />
                </div>
                <div className="compute-advanced-section">
                    <h4 className="compute-advanced-heading">Market surface</h4>
                    <ComputeMarketSurface />
                </div>
            </AdvancedDrawer>
        </div>
    );
}
