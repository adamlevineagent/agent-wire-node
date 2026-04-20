// ComputeOfferManager.tsx — Publish and edit compute market offers.
//
// Per `docs/plans/compute-market-phase-2-exchange.md` §IV:
//   - List current offers with model, rates, discount curve, Wire status.
//   - Create new offer: select from loaded models, set per-M-token rates
//     + reservation fee + queue discount curve + max_queue_depth.
//   - Integer inputs only (Pillar 9) — basis points for multipliers,
//     credits for rates.
//   - Wire sync status: show when offer is active on Wire vs pending.
//
// IPCs consumed: compute_offer_create, compute_offer_update,
// compute_offer_remove, compute_offers_list.

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface QueueDiscountPoint {
    depth: number;
    multiplier_bps: number;
}

interface ComputeOffer {
    model_id: string;
    provider_type: string;
    rate_per_m_input: number;
    rate_per_m_output: number;
    reservation_fee: number;
    queue_discount_curve: QueueDiscountPoint[];
    max_queue_depth: number;
    wire_offer_id: string | null;
}

interface OfferFormState {
    model_id: string;
    provider_type: "local" | "bridge";
    rate_per_m_input: string;       // stringified while editing
    rate_per_m_output: string;
    reservation_fee: string;
    max_queue_depth: string;
    curve: QueueDiscountPoint[];
}

const emptyForm: OfferFormState = {
    model_id: "",
    provider_type: "local",
    rate_per_m_input: "100",
    rate_per_m_output: "500",
    reservation_fee: "10",
    max_queue_depth: "8",
    curve: [
        { depth: 0, multiplier_bps: 10000 },
        { depth: 4, multiplier_bps: 9500 },
        { depth: 8, multiplier_bps: 9000 },
    ],
};

function parseIntOrZero(s: string): number {
    const n = parseInt(s, 10);
    return Number.isFinite(n) ? n : 0;
}

function formatMultiplier(bps: number): string {
    return `${(bps / 10000).toFixed(2)}×`;
}

/**
 * Effective rate at a given queue depth, given a curve.
 * Highest-depth curve point <= N wins. Floor division to match
 * the Rust settlement math (integer credits, Pillar 9).
 */
function effectiveRate(rate: number, depth: number, curve: QueueDiscountPoint[]): number {
    let multiplier = 10000;
    for (const point of [...curve].sort((a, b) => a.depth - b.depth)) {
        if (depth >= point.depth) multiplier = point.multiplier_bps;
    }
    return Math.floor((rate * multiplier) / 10000);
}

interface LocalModeStatus {
    enabled?: boolean;
    model?: string | null;
    available_models?: string[];
}

// Chronicle row shape we consume for mirror health.
interface MirrorHealthEvent {
    event_type: string;
    timestamp: string;
    metadata?: Record<string, unknown>;
}

// Wire's staleness threshold (queue_mirror_staleness_s economic_parameter
// at time of writing). We mirror it here for the freshness badge — yellow
// at half-threshold, red at full. If Wire tunes the threshold, update
// this constant to match. Not read dynamically because it's UX tuning,
// not a correctness signal.
const STALENESS_YELLOW_SECS = 45;
const STALENESS_RED_SECS = 90;

function formatAge(secs: number): string {
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
    if (secs < 86400) return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
    return `${Math.floor(secs / 86400)}d`;
}

/**
 * Mirror health indicator — renders a single badge describing the state
 * of the node's market-mirror task:
 *
 *   green "Pushed Ns ago"         — last push fresh (under YELLOW threshold)
 *   yellow "Pushed Ns ago"        — last push aging
 *   red "Stale — Ns since push"   — last push past RED threshold
 *                                   (matcher will reject)
 *   red "Mirror task panicked"    — supervisor caught a panic recently
 *   red "Mirror task exited"      — loop exited and didn't respawn
 *   red "Last push failed"        — most recent push errored
 *   gray "No pushes yet"          — fresh install, nothing to report
 *
 * Why surface this: prior to the supervisor + wall-clock seq fix, a
 * provider could go 54 hours without a push and look identical to a
 * healthy idle node from the operator's view. This badge makes the
 * liveness state visible so an operator doesn't have to dig into the
 * chronicle to know whether their mirror is functioning.
 *
 * Note: the Wire-side staleness CTE also accepts `last_heartbeat`
 * freshness (node heartbeat, 60s cadence) as an alternative — so a
 * stale mirror doesn't necessarily mean the node is unmatchable. This
 * badge is specifically about the mirror-push pathway, which is what
 * you want to know for "is my queue depth being reported to Wire?"
 */
function MirrorHealth() {
    const [state, setState] = useState<
        | { kind: "loading" }
        | { kind: "none" }
        | { kind: "pushed"; ageSecs: number }
        | { kind: "failed"; error: string; ageSecs: number }
        | { kind: "panicked"; message: string; ageSecs: number }
        | { kind: "exited"; ageSecs: number }
    >({ kind: "loading" });

    const refresh = useCallback(async () => {
        try {
            // Look back 24h — enough to catch the "silently stale since
            // Saturday" class of bug that motivated this work.
            const since = new Date(Date.now() - 24 * 60 * 60 * 1000).toISOString();
            // Query each lifecycle event type; keep the most recent
            // across all of them to decide the indicator state.
            const kinds: Array<MirrorHealthEvent["event_type"]> = [
                "queue_mirror_pushed",
                "queue_mirror_push_failed",
                "market_mirror_task_panicked",
                "market_mirror_task_exited",
            ];
            const results = await Promise.all(
                kinds.map((k) =>
                    invoke<MirrorHealthEvent[]>("get_compute_events", {
                        eventType: k,
                        after: since,
                        limit: 1,
                    }).catch(() => [] as MirrorHealthEvent[]),
                ),
            );
            // Flatten + pick the newest event. Event timestamps are ISO
            // strings; lexicographic compare works for same-timezone UTC.
            const all = results.flat();
            if (all.length === 0) {
                setState({ kind: "none" });
                return;
            }
            all.sort((a, b) => (a.timestamp < b.timestamp ? 1 : -1));
            const latest = all[0];
            const ageSecs = Math.max(
                0,
                Math.floor((Date.now() - new Date(latest.timestamp).getTime()) / 1000),
            );
            if (latest.event_type === "queue_mirror_pushed") {
                setState({ kind: "pushed", ageSecs });
            } else if (latest.event_type === "queue_mirror_push_failed") {
                const err = typeof latest.metadata?.error === "string"
                    ? latest.metadata.error
                    : "unknown error";
                setState({ kind: "failed", error: err, ageSecs });
            } else if (latest.event_type === "market_mirror_task_panicked") {
                const msg = typeof latest.metadata?.message === "string"
                    ? latest.metadata.message
                    : "panic";
                setState({ kind: "panicked", message: msg, ageSecs });
            } else {
                setState({ kind: "exited", ageSecs });
            }
        } catch {
            // Non-fatal — this component is pure observability, a read
            // failure shouldn't block the offer manager from rendering.
            setState({ kind: "none" });
        }
    }, []);

    useEffect(() => {
        void refresh();
        const handle = setInterval(() => void refresh(), 15000);
        return () => clearInterval(handle);
    }, [refresh]);

    if (state.kind === "loading") return null;
    if (state.kind === "none") {
        return (
            <div className="compute-mirror-health compute-mirror-health-neutral">
                Mirror: no pushes yet
            </div>
        );
    }
    if (state.kind === "pushed") {
        const tone =
            state.ageSecs > STALENESS_RED_SECS
                ? "red"
                : state.ageSecs > STALENESS_YELLOW_SECS
                  ? "yellow"
                  : "green";
        const label =
            tone === "red"
                ? `Mirror stale — last push ${formatAge(state.ageSecs)} ago (matcher may skip)`
                : `Mirror pushed ${formatAge(state.ageSecs)} ago`;
        return (
            <div
                className={`compute-mirror-health compute-mirror-health-${tone}`}
                title="Queue-mirror push liveness. Matcher accepts node heartbeat freshness as an alternative, so stale here doesn't necessarily mean unmatchable."
            >
                {label}
            </div>
        );
    }
    if (state.kind === "failed") {
        return (
            <div
                className="compute-mirror-health compute-mirror-health-red"
                title={state.error}
            >
                Last mirror push failed ({formatAge(state.ageSecs)} ago)
            </div>
        );
    }
    if (state.kind === "panicked") {
        return (
            <div
                className="compute-mirror-health compute-mirror-health-red"
                title={state.message}
            >
                Mirror task panicked ({formatAge(state.ageSecs)} ago) — supervisor respawned
            </div>
        );
    }
    return (
        <div className="compute-mirror-health compute-mirror-health-red">
            Mirror task exited ({formatAge(state.ageSecs)} ago) — restart node
        </div>
    );
}

/**
 * Delivery health indicator — the market_delivery.rs worker's status as
 * seen by the operator, parallel to MirrorHealth above.
 *
 * Reads the node's local chronicle for delivery events and renders the
 * most recent outcome. Pillar 42: every backend feature gets a frontend
 * surface so operators can test by feel, not by curl.
 *
 * Phase 3 rev 0.6.1 — two-leg P2P delivery. The worker now runs content
 * and settlement legs independently, each with its own attempt/succeed/
 * terminal event stream. Per spec line 404 we stay with ONE badge (end-
 * state focus; operators care delivered/failing/dead, not leg breakdowns
 * until triaging), but the label text describes the split outcome when
 * the legs diverge (content delivered + settlement dead, or vice versa).
 *
 * Back-compat: rev-0.5 rows emitted `market_result_delivered_to_wire`;
 * queries here UNION the old + new name so historical rows still count
 * as "delivered" for display purposes.
 *
 * State machine (badge tones):
 *   green   — "Both legs delivered Ns ago (N/24h)"
 *   blue    — "In flight: one leg delivered, waiting on the other"
 *   amber   — "Retrying — N transient leg attempt(s) failed"
 *   amber   — "Content delivered, settlement failed — <reason>"
 *   amber   — "Settlement delivered, content failed — <reason>"
 *   red     — "Delivery failed — <reason>" (both legs dead)
 *   red     — "Delivery task panicked (supervisor respawned)"
 *   red     — "Delivery task exited — restart node"
 *   info    — "Unknown privacy tier warning" (surfaced subtly when fresh)
 */
function DeliveryHealth() {
    type FailedState = {
        kind: "failed";
        reason: string;
        ageSecs: number;
    };
    type PartialState = {
        kind: "content-only" | "settlement-only";
        reason: string;
        ageSecs: number;
    };
    type InFlightState = {
        kind: "in-flight";
        leg: "content" | "settlement";
        ageSecs: number;
    };
    const [state, setState] = useState<
        | { kind: "loading" }
        | { kind: "idle" }
        | { kind: "delivered"; ageSecs: number; count24h: number }
        | { kind: "retrying"; attemptFailedCount: number }
        | InFlightState
        | PartialState
        | FailedState
        | { kind: "panicked"; message: string; ageSecs: number }
        | { kind: "exited"; ageSecs: number }
        | { kind: "cas-lost"; ageSecs: number; reason: string }
    >({ kind: "loading" });

    // Secondary info badge for unknown privacy tier warnings. Rendered
    // alongside the primary badge when fresh so operators can see Q-PROTO-3
    // tier-mismatch noise without it stealing attention from delivery state.
    const [unknownTierWarn, setUnknownTierWarn] = useState<
        { ageSecs: number; tier: string } | null
    >(null);

    const refresh = useCallback(async () => {
        try {
            const since24h = new Date(Date.now() - 24 * 60 * 60 * 1000).toISOString();
            const sinceHr = new Date(Date.now() - 60 * 60 * 1000).toISOString();

            const fetchEvents = (eventType: string, after: string, limit: number) =>
                invoke<MirrorHealthEvent[]>("get_compute_events", {
                    eventType,
                    after,
                    limit,
                }).catch(() => [] as MirrorHealthEvent[]);

            // Parallel fetch of all event types we care about. Lifecycle
            // events have small limits (just need latest). The delivered +
            // retrying series use slightly larger windows for counts.
            //
            // Rev 0.6.1 adds per-leg events; we also keep rev-0.5 names in
            // the query set for back-compat (historical rows can still
            // contribute to the delivered count and to the "idle" check).
            const [
                deliveredNew,
                deliveredLegacy,
                contentLegOk,
                settlementLegOk,
                contentAttemptFailed,
                settlementAttemptFailed,
                attemptFailedLegacy,
                contentDeliveryFailed,
                settlementDeliveryFailed,
                rowDeliveryFailed,
                casLost,
                panicked,
                exited,
                unknownTier,
            ] = await Promise.all([
                fetchEvents("market_result_delivered", since24h, 50),
                fetchEvents("market_result_delivered_to_wire", since24h, 50),
                fetchEvents("market_content_leg_succeeded", since24h, 10),
                fetchEvents("market_settlement_leg_succeeded", since24h, 10),
                fetchEvents("market_content_delivery_attempt_failed", sinceHr, 50),
                fetchEvents("market_settlement_delivery_attempt_failed", sinceHr, 50),
                fetchEvents("market_result_delivery_attempt_failed", sinceHr, 50),
                fetchEvents("market_content_delivery_failed", since24h, 5),
                fetchEvents("market_settlement_delivery_failed", since24h, 5),
                fetchEvents("market_result_delivery_failed", since24h, 1),
                fetchEvents("market_result_delivery_cas_lost", sinceHr, 1),
                fetchEvents("market_delivery_task_panicked", sinceHr, 1),
                fetchEvents("market_delivery_task_exited", since24h, 1),
                fetchEvents("market_unknown_privacy_tier", sinceHr, 1),
            ]);

            const ageOf = (iso: string | undefined): number =>
                iso
                    ? Math.max(0, Math.floor((Date.now() - new Date(iso).getTime()) / 1000))
                    : Number.POSITIVE_INFINITY;

            // Surface unknown-privacy-tier warnings as a subtle secondary
            // badge if seen in the last hour. Not blocking — this is a
            // Q-PROTO-3 soft warn when Wire sends an unrecognized tier.
            if (unknownTier.length > 0) {
                const tier =
                    typeof unknownTier[0].metadata?.privacy_tier === "string"
                        ? unknownTier[0].metadata.privacy_tier
                        : typeof unknownTier[0].metadata?.tier === "string"
                          ? (unknownTier[0].metadata.tier as string)
                          : "unknown";
                setUnknownTierWarn({
                    ageSecs: ageOf(unknownTier[0].timestamp),
                    tier,
                });
            } else {
                setUnknownTierWarn(null);
            }

            // Precedence (primary badge):
            //  1. Lifecycle red states (panic/exit)
            //  2. Row-level terminal dead (both legs dead)
            //  3. Per-leg terminal split (one leg delivered, other dead)
            //  4. CAS-lost race
            //  5. Any attempt-failed (transient) → retrying
            //  6. In-flight (one leg ok, waiting on other, no terminal)
            //  7. Delivered (rev-0.6 + rev-0.5 UNION)
            //  8. Idle
            if (panicked.length > 0) {
                const msg =
                    typeof panicked[0].metadata?.message === "string"
                        ? panicked[0].metadata.message
                        : "panic";
                setState({ kind: "panicked", message: msg, ageSecs: ageOf(panicked[0].timestamp) });
                return;
            }
            // UNION both old + new delivered-row event names for back-compat.
            const deliveredUnion = [...deliveredNew, ...deliveredLegacy].sort((a, b) =>
                a.timestamp < b.timestamp ? 1 : -1,
            );
            // An "exited" event means the loop hit clean channel-close;
            // supervisor returned. If a "delivered" event fires more recently,
            // the supervisor came back up — show delivered. Otherwise, red.
            if (exited.length > 0) {
                const exitAge = ageOf(exited[0].timestamp);
                const lastDeliveryAge =
                    deliveredUnion.length > 0
                        ? ageOf(deliveredUnion[0].timestamp)
                        : Number.POSITIVE_INFINITY;
                if (exitAge < lastDeliveryAge) {
                    setState({ kind: "exited", ageSecs: exitAge });
                    return;
                }
            }

            const extractReason = (ev: MirrorHealthEvent | undefined): string => {
                if (!ev) return "unknown";
                const md = ev.metadata ?? {};
                if (typeof md.reason === "string") return md.reason;
                if (typeof md.final_error === "string") return md.final_error;
                if (typeof md.content_error === "string" && typeof md.settlement_error === "string")
                    return `content: ${md.content_error}; settlement: ${md.settlement_error}`;
                if (typeof md.content_error === "string") return md.content_error;
                if (typeof md.settlement_error === "string") return md.settlement_error;
                return "unknown";
            };

            // Row-level terminal — both legs dead. Recent (<5 min) wins
            // over everything below.
            if (rowDeliveryFailed.length > 0 && ageOf(rowDeliveryFailed[0].timestamp) < 300) {
                setState({
                    kind: "failed",
                    reason: extractReason(rowDeliveryFailed[0]),
                    ageSecs: ageOf(rowDeliveryFailed[0].timestamp),
                });
                return;
            }

            // Per-leg terminal split — one leg dead, the other succeeded.
            // Take the most recent terminal-leg event, correlate by job_id
            // with the opposite leg's success stream to decide the label.
            const latestContentDead = contentDeliveryFailed[0];
            const latestSettlementDead = settlementDeliveryFailed[0];
            const contentDeadAge = ageOf(latestContentDead?.timestamp);
            const settlementDeadAge = ageOf(latestSettlementDead?.timestamp);
            const jobIdOf = (ev: MirrorHealthEvent | undefined): string | null => {
                if (!ev) return null;
                const v = ev.metadata?.job_id;
                return typeof v === "string" ? v : null;
            };
            const contentOkJobIds = new Set(
                contentLegOk.map((e) => jobIdOf(e)).filter((v): v is string => v !== null),
            );
            const settlementOkJobIds = new Set(
                settlementLegOk.map((e) => jobIdOf(e)).filter((v): v is string => v !== null),
            );
            // Pick the more recent terminal-leg event — if that row has the
            // opposite leg succeeded, we're in a partial-delivery state.
            if (contentDeadAge < 300 || settlementDeadAge < 300) {
                if (settlementDeadAge <= contentDeadAge && latestSettlementDead) {
                    const jobId = jobIdOf(latestSettlementDead);
                    if (jobId && contentOkJobIds.has(jobId)) {
                        setState({
                            kind: "content-only",
                            reason: extractReason(latestSettlementDead),
                            ageSecs: settlementDeadAge,
                        });
                        return;
                    }
                }
                if (contentDeadAge < settlementDeadAge && latestContentDead) {
                    const jobId = jobIdOf(latestContentDead);
                    if (jobId && settlementOkJobIds.has(jobId)) {
                        setState({
                            kind: "settlement-only",
                            reason: extractReason(latestContentDead),
                            ageSecs: contentDeadAge,
                        });
                        return;
                    }
                }
            }

            // CAS-lost race — rare under per-leg model but possible.
            if (casLost.length > 0 && ageOf(casLost[0].timestamp) < 300) {
                const reason =
                    typeof casLost[0].metadata?.reason === "string"
                        ? casLost[0].metadata.reason
                        : "cas_lost";
                setState({
                    kind: "cas-lost",
                    reason,
                    ageSecs: ageOf(casLost[0].timestamp),
                });
                return;
            }

            const attemptFailedCount =
                contentAttemptFailed.length +
                settlementAttemptFailed.length +
                attemptFailedLegacy.length;
            if (attemptFailedCount > 0) {
                // Before declaring "retrying", check if one leg has
                // succeeded but the other hasn't terminated — that's
                // in-flight, not generic retry. In-flight wins because
                // it's a more specific signal for the operator.
                const mostRecentLegSuccess = [...contentLegOk, ...settlementLegOk].sort((a, b) =>
                    a.timestamp < b.timestamp ? 1 : -1,
                )[0];
                const mostRecentRowDone =
                    deliveredUnion[0]?.timestamp ?? rowDeliveryFailed[0]?.timestamp;
                if (
                    mostRecentLegSuccess &&
                    ageOf(mostRecentLegSuccess.timestamp) < 300 &&
                    (!mostRecentRowDone ||
                        mostRecentLegSuccess.timestamp > mostRecentRowDone)
                ) {
                    const leg = contentLegOk.includes(mostRecentLegSuccess)
                        ? "content"
                        : "settlement";
                    setState({
                        kind: "in-flight",
                        leg,
                        ageSecs: ageOf(mostRecentLegSuccess.timestamp),
                    });
                    return;
                }
                setState({
                    kind: "retrying",
                    attemptFailedCount,
                });
                return;
            }

            if (deliveredUnion.length > 0) {
                setState({
                    kind: "delivered",
                    ageSecs: ageOf(deliveredUnion[0].timestamp),
                    count24h: deliveredUnion.length,
                });
                return;
            }
            setState({ kind: "idle" });
        } catch {
            setState({ kind: "idle" });
        }
    }, []);

    useEffect(() => {
        void refresh();
        const handle = setInterval(() => void refresh(), 15000);
        return () => clearInterval(handle);
    }, [refresh]);

    if (state.kind === "loading") return null;

    const unknownTierBadge = unknownTierWarn ? (
        <div
            className="compute-mirror-health compute-mirror-health-neutral"
            title={`Wire sent a privacy tier this node doesn't recognize (tier: ${unknownTierWarn.tier}). Non-blocking; delivery still proceeds with the default handling.`}
        >
            Unknown privacy tier seen ({formatAge(unknownTierWarn.ageSecs)} ago)
        </div>
    ) : null;

    const withTierWarn = (primary: JSX.Element): JSX.Element => (
        <>
            {primary}
            {unknownTierBadge}
        </>
    );

    if (state.kind === "idle") {
        return withTierWarn(
            <div className="compute-mirror-health compute-mirror-health-neutral">
                Delivery: no jobs delivered yet
            </div>,
        );
    }
    if (state.kind === "delivered") {
        return withTierWarn(
            <div
                className="compute-mirror-health compute-mirror-health-green"
                title={`${state.count24h} delivery events in last 24h (rev-0.6 + rev-0.5 combined)`}
            >
                Both legs delivered {formatAge(state.ageSecs)} ago ({state.count24h}/24h)
            </div>,
        );
    }
    if (state.kind === "in-flight") {
        const waitingOn = state.leg === "content" ? "settlement" : "content";
        return withTierWarn(
            <div
                className="compute-mirror-health compute-mirror-health-neutral"
                title={`${state.leg} leg delivered ${formatAge(state.ageSecs)} ago; ${waitingOn} leg still in flight.`}
            >
                In flight — {state.leg} delivered, awaiting {waitingOn}
            </div>,
        );
    }
    if (state.kind === "retrying") {
        return withTierWarn(
            <div
                className="compute-mirror-health compute-mirror-health-yellow"
                title="Transient leg-attempt failures (5xx, network) in the last hour across content + settlement"
            >
                Delivery retrying — {state.attemptFailedCount} leg attempt(s) failed in last hour
            </div>,
        );
    }
    if (state.kind === "content-only") {
        return withTierWarn(
            <div
                className="compute-mirror-health compute-mirror-health-yellow"
                title={`Settlement leg terminal: ${state.reason}. Content was delivered to the requester ${formatAge(state.ageSecs)} ago.`}
            >
                Content delivered, settlement failed — {state.reason}
            </div>,
        );
    }
    if (state.kind === "settlement-only") {
        return withTierWarn(
            <div
                className="compute-mirror-health compute-mirror-health-yellow"
                title={`Content leg terminal: ${state.reason}. Settlement was delivered to Wire ${formatAge(state.ageSecs)} ago.`}
            >
                Settlement delivered, content failed — {state.reason}
            </div>,
        );
    }
    if (state.kind === "failed") {
        return withTierWarn(
            <div
                className="compute-mirror-health compute-mirror-health-red"
                title={state.reason}
            >
                Delivery failed ({formatAge(state.ageSecs)} ago) — {state.reason}
            </div>,
        );
    }
    if (state.kind === "cas-lost") {
        return withTierWarn(
            <div
                className="compute-mirror-health compute-mirror-health-red"
                title={`CAS race on ready→delivered transition: ${state.reason}. Rare under per-leg model; expected-ly transient.`}
            >
                Delivery CAS lost ({formatAge(state.ageSecs)} ago) — {state.reason}
            </div>,
        );
    }
    if (state.kind === "panicked") {
        return withTierWarn(
            <div
                className="compute-mirror-health compute-mirror-health-red"
                title={state.message}
            >
                Delivery task panicked ({formatAge(state.ageSecs)} ago) — supervisor respawned
            </div>,
        );
    }
    return withTierWarn(
        <div className="compute-mirror-health compute-mirror-health-red">
            Delivery task exited ({formatAge(state.ageSecs)} ago) — restart node
        </div>,
    );
}

export function ComputeOfferManager() {
    const [offers, setOffers] = useState<ComputeOffer[]>([]);
    const [loading, setLoading] = useState(true);
    const [form, setForm] = useState<OfferFormState>(emptyForm);
    const [saving, setSaving] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [editingModelId, setEditingModelId] = useState<string | null>(null);
    const [formOpen, setFormOpen] = useState(false);
    const [availableModels, setAvailableModels] = useState<string[]>([]);
    const [currentModel, setCurrentModel] = useState<string | null>(null);

    const refresh = useCallback(async () => {
        try {
            const list = await invoke<ComputeOffer[]>("compute_offers_list");
            setOffers(list);
            setError(null);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    }, []);

    const refreshLoadedModels = useCallback(async () => {
        try {
            const status = await invoke<LocalModeStatus>("pyramid_get_local_mode_status");
            setAvailableModels(status.available_models ?? []);
            setCurrentModel(status.model ?? null);
        } catch {
            // Non-fatal — model picker just falls back to free text entry.
            setAvailableModels([]);
            setCurrentModel(null);
        }
    }, []);

    useEffect(() => {
        void refresh();
        void refreshLoadedModels();
    }, [refresh, refreshLoadedModels]);

    // When the New Offer form opens with no model selected yet, default
    // to the currently-loaded model so the operator doesn't have to type
    // the slug by hand. Respects editing mode (where model_id is pinned).
    useEffect(() => {
        if (formOpen && !editingModelId && !form.model_id) {
            const picked = currentModel || availableModels[0] || "";
            if (picked) {
                setForm((prev) => ({ ...prev, model_id: picked }));
            }
        }
    }, [formOpen, editingModelId, form.model_id, currentModel, availableModels]);

    const beginEdit = (offer: ComputeOffer) => {
        setForm({
            model_id: offer.model_id,
            provider_type: offer.provider_type as "local" | "bridge",
            rate_per_m_input: String(offer.rate_per_m_input),
            rate_per_m_output: String(offer.rate_per_m_output),
            reservation_fee: String(offer.reservation_fee),
            max_queue_depth: String(offer.max_queue_depth),
            curve:
                offer.queue_discount_curve.length > 0
                    ? offer.queue_discount_curve
                    : emptyForm.curve,
        });
        setEditingModelId(offer.model_id);
        setFormOpen(true);
        setError(null);
    };

    const resetForm = () => {
        setForm(emptyForm);
        setEditingModelId(null);
        setFormOpen(false);
        setError(null);
    };

    const handleSave = async () => {
        setSaving(true);
        setError(null);
        try {
            // Wire-contract shape: OfferQueueDiscountPoint uses
            // {queue_depth, discount_bps}. Internal display math uses
            // {depth, multiplier_bps}. Translate at the IPC boundary.
            //   discount_bps = 10000 - multiplier_bps
            //   (10000 = no discount, 9500 = 5% off, 9000 = 10% off)
            const wireCurve = form.curve.map((p) => ({
                queue_depth: p.depth,
                discount_bps: Math.max(0, 10000 - p.multiplier_bps),
            }));
            const payload = {
                model_id: form.model_id.trim(),
                provider_type: form.provider_type,
                rate_per_m_input: parseIntOrZero(form.rate_per_m_input),
                rate_per_m_output: parseIntOrZero(form.rate_per_m_output),
                reservation_fee: parseIntOrZero(form.reservation_fee),
                queue_discount_curve: wireCurve,
                max_queue_depth: parseIntOrZero(form.max_queue_depth),
            };
            if (!payload.model_id) {
                throw new Error("model_id is required");
            }
            const cmd = editingModelId ? "compute_offer_update" : "compute_offer_create";
            await invoke(cmd, { offer: payload });
            await refresh();
            resetForm();
        } catch (e) {
            setError(String(e));
        } finally {
            setSaving(false);
        }
    };

    const handleRemove = async (model_id: string) => {
        if (!confirm(`Remove offer for ${model_id}? Active jobs continue; only new matches are blocked.`)) return;
        setSaving(true);
        setError(null);
        try {
            await invoke("compute_offer_remove", { modelId: model_id });
            await refresh();
            if (editingModelId === model_id) resetForm();
        } catch (e) {
            setError(String(e));
        } finally {
            setSaving(false);
        }
    };

    const updateCurvePoint = (
        idx: number,
        field: "depth" | "multiplier_bps",
        value: string,
    ) => {
        setForm((prev) => {
            const curve = [...prev.curve];
            curve[idx] = { ...curve[idx], [field]: parseIntOrZero(value) };
            return { ...prev, curve };
        });
    };

    const addCurvePoint = () => {
        setForm((prev) => ({
            ...prev,
            curve: [...prev.curve, { depth: prev.curve.length * 4, multiplier_bps: 10000 }],
        }));
    };

    const removeCurvePoint = (idx: number) => {
        setForm((prev) => ({
            ...prev,
            curve: prev.curve.filter((_, i) => i !== idx),
        }));
    };

    return (
        <div className="compute-offers-panel">
            {error && (
                <div className="compute-market-error" role="alert">
                    {error}
                </div>
            )}

            {/* Phase 3: two provider-path health indicators at the top of
                the offers panel. MirrorHealth surfaces the queue-mirror
                push loop (how Wire sees your queue depth); DeliveryHealth
                surfaces the result-callback loop (how Wire receives your
                inference results). Each refreshes every 15s against the
                local chronicle. */}
            <div className="compute-offers-health-row">
                <MirrorHealth />
                <DeliveryHealth />
            </div>

            <div className="compute-offers-header">
                <div className="compute-offers-header-text">
                    <h3 className="compute-section-title">Your offers</h3>
                    <p className="compute-section-sub">
                        Models you're publishing to the Wire. Each offer defines the rate you
                        charge, how the rate scales with queue depth, and the cap on concurrent
                        market jobs.
                    </p>
                </div>
                {!formOpen && (
                    <button
                        className="compute-primary-btn"
                        onClick={() => {
                            setForm(emptyForm);
                            setEditingModelId(null);
                            setFormOpen(true);
                            setError(null);
                        }}
                    >
                        + New offer
                    </button>
                )}
            </div>

            {loading ? (
                <div className="compute-empty">Loading…</div>
            ) : offers.length === 0 ? (
                <div className="compute-empty">
                    <div className="compute-empty-title">No offers published yet</div>
                    <div className="compute-empty-desc">
                        Create an offer to start accepting paid market jobs. You keep running
                        local and fleet work regardless — market dispatches just land in the
                        same queue with their own depth cap.
                    </div>
                </div>
            ) : (
                <div className="compute-offer-grid">
                    {offers.map((o) => (
                        <OfferCard
                            key={o.model_id}
                            offer={o}
                            onEdit={() => beginEdit(o)}
                            onRemove={() => handleRemove(o.model_id)}
                            disabled={saving}
                        />
                    ))}
                </div>
            )}

            {formOpen && (
                <div className="compute-form-panel">
                    <div className="compute-form-header">
                        <h4 className="compute-section-title">
                            {editingModelId ? `Edit offer — ${editingModelId}` : "New offer"}
                        </h4>
                        <button className="compute-ghost-btn" onClick={resetForm} disabled={saving}>
                            Cancel
                        </button>
                    </div>

                    <div className="compute-form-grid">
                        <label className="compute-field">
                            <span className="compute-field-label">Model ID</span>
                            {editingModelId !== null || availableModels.length === 0 ||
                             form.provider_type === "bridge" ? (
                                <input
                                    className="compute-input"
                                    type="text"
                                    value={form.model_id}
                                    onChange={(e) =>
                                        setForm({ ...form, model_id: e.target.value })
                                    }
                                    disabled={editingModelId !== null}
                                    placeholder="e.g. gemma3:27b"
                                />
                            ) : (
                                <select
                                    className="compute-input"
                                    value={form.model_id}
                                    onChange={(e) =>
                                        setForm({ ...form, model_id: e.target.value })
                                    }
                                >
                                    {!availableModels.includes(form.model_id) && form.model_id && (
                                        <option value={form.model_id}>{form.model_id} (not loaded)</option>
                                    )}
                                    {availableModels.map((m) => (
                                        <option key={m} value={m}>
                                            {m}{m === currentModel ? " (routing)" : ""}
                                        </option>
                                    ))}
                                </select>
                            )}
                            <span className="compute-field-hint">
                                {availableModels.length > 0 && form.provider_type === "local"
                                    ? `${availableModels.length} locally-loaded model${availableModels.length === 1 ? "" : "s"} detected. Pick one, or switch to bridge for OpenRouter slugs.`
                                    : "Must match a locally-loaded model (or an OpenRouter slug if provider is bridge)."}
                            </span>
                        </label>

                        <label className="compute-field">
                            <span className="compute-field-label">Provider</span>
                            <select
                                className="compute-input"
                                value={form.provider_type}
                                onChange={(e) =>
                                    setForm({
                                        ...form,
                                        provider_type: e.target.value as "local" | "bridge",
                                    })
                                }
                            >
                                <option value="local">Local (Ollama)</option>
                                <option value="bridge">Bridge (OpenRouter)</option>
                            </select>
                            <span className="compute-field-hint">
                                Local serves from your GPU; bridge proxies to OpenRouter (Phase 4).
                            </span>
                        </label>

                        <label className="compute-field">
                            <span className="compute-field-label">Input rate</span>
                            <div className="compute-input-with-suffix">
                                <input
                                    className="compute-input"
                                    type="number"
                                    step="1"
                                    min="0"
                                    value={form.rate_per_m_input}
                                    onChange={(e) =>
                                        setForm({ ...form, rate_per_m_input: e.target.value })
                                    }
                                />
                                <span className="compute-input-suffix">credits / M tokens</span>
                            </div>
                        </label>

                        <label className="compute-field">
                            <span className="compute-field-label">Output rate</span>
                            <div className="compute-input-with-suffix">
                                <input
                                    className="compute-input"
                                    type="number"
                                    step="1"
                                    min="0"
                                    value={form.rate_per_m_output}
                                    onChange={(e) =>
                                        setForm({ ...form, rate_per_m_output: e.target.value })
                                    }
                                />
                                <span className="compute-input-suffix">credits / M tokens</span>
                            </div>
                        </label>

                        <label className="compute-field">
                            <span className="compute-field-label">Reservation fee</span>
                            <div className="compute-input-with-suffix">
                                <input
                                    className="compute-input"
                                    type="number"
                                    step="1"
                                    min="0"
                                    value={form.reservation_fee}
                                    onChange={(e) =>
                                        setForm({ ...form, reservation_fee: e.target.value })
                                    }
                                />
                                <span className="compute-input-suffix">credits</span>
                            </div>
                            <span className="compute-field-hint">
                                Upfront deposit charged at match time, held until settle.
                            </span>
                        </label>

                        <label className="compute-field">
                            <span className="compute-field-label">Max market queue depth</span>
                            <div className="compute-input-with-suffix">
                                <input
                                    className="compute-input"
                                    type="number"
                                    step="1"
                                    min="0"
                                    value={form.max_queue_depth}
                                    onChange={(e) =>
                                        setForm({ ...form, max_queue_depth: e.target.value })
                                    }
                                />
                                <span className="compute-input-suffix">jobs</span>
                            </div>
                            <span className="compute-field-hint">
                                Beyond this, new market dispatches get rejected with 503 +
                                Retry-After so the Wire re-matches.
                            </span>
                        </label>
                    </div>

                    <div className="compute-curve-section">
                        <div className="compute-curve-header">
                            <h5 className="compute-curve-title">Queue discount curve</h5>
                            <p className="compute-curve-desc">
                                Multiplier in basis points (10000 = 1.00×). At depth N, the
                                multiplier from the highest point whose depth ≤ N wins.
                                Effective rate = base × multiplier / 10000.
                            </p>
                        </div>
                        <div className="compute-curve-table">
                            <div className="compute-curve-row compute-curve-head">
                                <div>Depth</div>
                                <div>Multiplier</div>
                                <div className="compute-curve-col-eff">As rate</div>
                                <div className="compute-curve-col-eff">Eff. output / M</div>
                                <div />
                            </div>
                            {form.curve.map((point, idx) => (
                                <div className="compute-curve-row" key={idx}>
                                    <div>
                                        <input
                                            className="compute-input compute-input-tight"
                                            type="number"
                                            step="1"
                                            min="0"
                                            value={point.depth}
                                            onChange={(e) =>
                                                updateCurvePoint(idx, "depth", e.target.value)
                                            }
                                        />
                                    </div>
                                    <div>
                                        <input
                                            className="compute-input compute-input-tight"
                                            type="number"
                                            step="100"
                                            min="0"
                                            value={point.multiplier_bps}
                                            onChange={(e) =>
                                                updateCurvePoint(
                                                    idx,
                                                    "multiplier_bps",
                                                    e.target.value,
                                                )
                                            }
                                        />
                                    </div>
                                    <div className="compute-curve-col-eff compute-mono">
                                        {formatMultiplier(point.multiplier_bps)}
                                    </div>
                                    <div className="compute-curve-col-eff compute-mono">
                                        {effectiveRate(
                                            parseIntOrZero(form.rate_per_m_output),
                                            point.depth,
                                            form.curve,
                                        )}
                                    </div>
                                    <div>
                                        <button
                                            className="compute-ghost-btn compute-ghost-btn-sm"
                                            onClick={() => removeCurvePoint(idx)}
                                            disabled={form.curve.length <= 1}
                                            title={
                                                form.curve.length <= 1
                                                    ? "At least one point required"
                                                    : "Remove curve point"
                                            }
                                        >
                                            ×
                                        </button>
                                    </div>
                                </div>
                            ))}
                        </div>
                        <button className="compute-ghost-btn compute-ghost-btn-sm" onClick={addCurvePoint}>
                            + Add curve point
                        </button>
                    </div>

                    <div className="compute-form-actions">
                        <button
                            className="compute-primary-btn"
                            onClick={handleSave}
                            disabled={saving || !form.model_id.trim()}
                        >
                            {saving
                                ? "Saving…"
                                : editingModelId
                                  ? "Update offer"
                                  : "Create offer"}
                        </button>
                        <button
                            className="compute-ghost-btn"
                            onClick={resetForm}
                            disabled={saving}
                        >
                            Discard
                        </button>
                    </div>
                </div>
            )}
        </div>
    );
}

interface OfferCardProps {
    offer: ComputeOffer;
    onEdit: () => void;
    onRemove: () => void;
    disabled: boolean;
}

function OfferCard({ offer, onEdit, onRemove, disabled }: OfferCardProps) {
    const wireStatus = offer.wire_offer_id ? "active" : "pending";
    return (
        <div className="compute-offer-card">
            <div className="compute-offer-card-header">
                <div className="compute-offer-card-model">
                    <span className="compute-offer-card-name">{offer.model_id}</span>
                    <span className="compute-offer-card-provider">{offer.provider_type}</span>
                </div>
                <span
                    className={`compute-offer-badge compute-offer-badge-${wireStatus}`}
                    title={
                        wireStatus === "active"
                            ? `Wire offer_id: ${offer.wire_offer_id}`
                            : "Not yet synced to the Wire"
                    }
                >
                    {wireStatus === "active" ? "Wire active" : "Pending sync"}
                </span>
            </div>

            <dl className="compute-offer-card-stats">
                <div className="compute-offer-stat">
                    <dt>Input</dt>
                    <dd className="compute-mono">{offer.rate_per_m_input}</dd>
                </div>
                <div className="compute-offer-stat">
                    <dt>Output</dt>
                    <dd className="compute-mono">{offer.rate_per_m_output}</dd>
                </div>
                <div className="compute-offer-stat">
                    <dt>Reservation</dt>
                    <dd className="compute-mono">{offer.reservation_fee}</dd>
                </div>
                <div className="compute-offer-stat">
                    <dt>Max depth</dt>
                    <dd className="compute-mono">{offer.max_queue_depth}</dd>
                </div>
            </dl>

            {offer.queue_discount_curve.length > 0 && (
                <div className="compute-offer-curve">
                    <div className="compute-offer-curve-label">Curve</div>
                    <div className="compute-offer-curve-points">
                        {offer.queue_discount_curve.map((p, i) => (
                            <span key={i} className="compute-offer-curve-point">
                                <span className="compute-offer-curve-depth">{p.depth}</span>
                                <span className="compute-offer-curve-sep">@</span>
                                <span className="compute-offer-curve-mul">
                                    {formatMultiplier(p.multiplier_bps)}
                                </span>
                            </span>
                        ))}
                    </div>
                </div>
            )}

            <div className="compute-offer-card-actions">
                <button className="compute-ghost-btn compute-ghost-btn-sm" onClick={onEdit} disabled={disabled}>
                    Edit
                </button>
                <button
                    className="compute-ghost-btn compute-ghost-btn-sm compute-ghost-btn-danger"
                    onClick={onRemove}
                    disabled={disabled}
                >
                    Remove
                </button>
            </div>
        </div>
    );
}
