// ComputeNetworkStatus.tsx — Primary hero for the Market → Compute tab.
//
// The framing: the compute market is a cooperative compute pool. Value
// to the user is "my pyramids build fast because I'm in the network;
// when I'm idle, I help others." Credits are accounting — the ledger
// keeps the pool balanced, but the ledger is not the point. Every
// user-facing string here treats the surface as network connectivity,
// not as a trading venue.
//
// See `docs/plans/compute-market-invisibility-ux.md` for the full
// language contract and the six canonical states rendered below.
//
// Data plumbing (no new IPC — everything's already exposed):
//   - compute_market_get_state       → snapshot
//   - pyramid_get_local_mode_status  → local model state
//   - get_compute_summary            → week roll-up
//   - get_compute_events             → last-hour activity
//
// State derivation is a pure function so the six render branches
// dispatch off a single enum. No ambiguous composites.

import type { ComputeMarketStateSnapshot, LocalModeStatus, ComputeEvent } from "./types";

export type NetworkState =
    | "consumer"   // No model loaded → consumer-only membership invite
    | "paused"     // Model loaded but is_serving=false → ambient paused card
    | "active"     // Network actively using our GPU → "helping a build"
    | "ready"      // Connected + model loaded + helped recently, nothing active → ambient ready card
    | "quiet"      // Connected + model loaded, no recent activity → honest "quiet on the network"
    | "connected"; // Connected + model loaded + has recent activity snapshot → full contribution summary

interface Props {
    state: NetworkState;
    snapshot: ComputeMarketStateSnapshot | null;
    localMode: LocalModeStatus | null;
    recentEvents: ComputeEvent[];
    onToggleServing: () => void;
    togglingServing: boolean;
    onDismissConsumerInvite: () => void;
    consumerInviteDismissed: boolean;
}

/// Derive the network state from available data. Pure function so the
/// render logic stays a simple switch — see the state comments for
/// the exact trigger rules. Call with fresh-off-the-IPC data.
export function deriveNetworkState(
    snapshot: ComputeMarketStateSnapshot | null,
    localMode: LocalModeStatus | null,
    recentEvents: ComputeEvent[],
    consumerInviteDismissed: boolean,
): NetworkState {
    // Consumer-only invite: no model loaded and user hasn't dismissed
    // the invite for this session. Honor dismissal so the invite
    // doesn't nag after the operator has explicitly said "not now."
    const noModel = !localMode || !localMode.enabled || !localMode.model;
    if (noModel && !consumerInviteDismissed) {
        return "consumer";
    }

    // Paused: model loaded but operator has explicitly disabled
    // serving via the toggle (is_serving=false). Distinct from consumer
    // state — they've got hardware, they've chosen to pause.
    if (snapshot && !snapshot.is_serving && !noModel) {
        return "paused";
    }

    // Active: any job Queued or Executing in active_jobs right now.
    // This is the visible contribution moment — the network is using
    // our GPU.
    const active = snapshot ? Object.values(snapshot.active_jobs ?? {}) : [];
    if (active.length > 0) {
        return "active";
    }

    // Ready vs Quiet vs Connected: all three are "idle with model
    // loaded, serving on." Distinguish by recent activity:
    //   - recentEvents has 1+ entries    → connected (full summary)
    //   - recentEvents empty, some today → ready (ambient idle)
    //   - recentEvents empty + nothing   → quiet (network is empty)
    //
    // The distinction Ready vs Quiet is an honesty thing: "ready" says
    // "I'm here and available," "quiet" says "nobody's here." Both
    // are correct states on a fresh network.
    if (recentEvents.length > 0) {
        // Further distinguish connected vs ready: connected has enough
        // data to show the "last hour" summary; ready just says
        // "helped N earlier today."
        return recentEvents.length >= 3 ? "connected" : "ready";
    }

    return "quiet";
}

export function ComputeNetworkStatus(props: Props) {
    const { state } = props;

    switch (state) {
        case "consumer":
            return <ConsumerInvite {...props} />;
        case "paused":
            return <PausedCard {...props} />;
        case "active":
            return <ActiveCard {...props} />;
        case "ready":
            return <ReadyCard {...props} />;
        case "quiet":
            return <QuietCard {...props} />;
        case "connected":
            return <ConnectedCard {...props} />;
    }
}

// ════════════════════════════════════════════════════════════════════════
// State: consumer — no local model loaded
// ════════════════════════════════════════════════════════════════════════

function ConsumerInvite({ onDismissConsumerInvite }: Props) {
    return (
        <section className="compute-net-hero compute-net-consumer" aria-live="polite">
            <header className="compute-net-header">
                <span className="compute-net-icon">🌐</span>
                <span className="compute-net-title">Compute network</span>
                <span className="compute-net-status">Consumer member</span>
            </header>

            <p className="compute-net-body">
                You're in the network — your pyramids build with network help. Load a local
                model to also contribute compute when idle and keep your balance even.
            </p>

            <div className="compute-net-actions">
                <a className="compute-net-primary" href="#/settings/local-mode">
                    Load a local model
                </a>
                <button className="compute-net-secondary" onClick={onDismissConsumerInvite}>
                    Not right now
                </button>
            </div>
        </section>
    );
}

// ════════════════════════════════════════════════════════════════════════
// State: paused — model loaded, serving disabled by operator
// ════════════════════════════════════════════════════════════════════════

function PausedCard({ snapshot, localMode, onToggleServing, togglingServing }: Props) {
    return (
        <section className="compute-net-hero compute-net-paused">
            <header className="compute-net-header">
                <span className="compute-net-icon">🌐</span>
                <span className="compute-net-title">Compute network</span>
                <span className="compute-net-status">Paused</span>
            </header>

            <ServingToggle
                on={false}
                model={localMode?.model ?? null}
                busy={togglingServing}
                onToggle={onToggleServing}
            />

            <p className="compute-net-body">
                You're still connected as a consumer — your builds still get network help
                (paid from balance). Turn contribution back on to help others and keep
                your balance even.
            </p>

            <Balance snapshot={snapshot} />
        </section>
    );
}

// ════════════════════════════════════════════════════════════════════════
// State: active — network actively using our GPU
// ════════════════════════════════════════════════════════════════════════

function ActiveCard({ snapshot, onToggleServing, togglingServing }: Props) {
    const activeJobs = snapshot ? Object.values(snapshot.active_jobs ?? {}) : [];
    const jobCount = activeJobs.length;

    return (
        <section className="compute-net-hero compute-net-active" aria-live="polite">
            <header className="compute-net-header">
                <span className="compute-net-icon compute-net-icon-pulse">🌐</span>
                <span className="compute-net-title">Compute network</span>
                <span className="compute-net-status compute-net-status-active">
                    Helping {jobCount === 1 ? "a build" : `${jobCount} builds`}
                </span>
            </header>

            <p className="compute-net-body">
                Network is using your GPU right now.
            </p>

            <div className="compute-net-actions">
                <button
                    className="compute-net-secondary"
                    onClick={onToggleServing}
                    disabled={togglingServing}
                    title="Pause contribution — the current job finishes, no new ones start."
                >
                    {togglingServing ? "…" : "Pause contribution"}
                </button>
            </div>
        </section>
    );
}

// ════════════════════════════════════════════════════════════════════════
// State: ready — connected, recent-ish activity, currently idle
// ════════════════════════════════════════════════════════════════════════

function ReadyCard({ localMode, recentEvents, onToggleServing, togglingServing }: Props) {
    const helpedCount = recentEvents.length;
    return (
        <section className="compute-net-hero compute-net-ready">
            <header className="compute-net-header">
                <span className="compute-net-icon">🌐</span>
                <span className="compute-net-title">Compute network</span>
                <span className="compute-net-status">Ready</span>
            </header>

            <ServingToggle
                on={true}
                model={localMode?.model ?? null}
                busy={togglingServing}
                onToggle={onToggleServing}
            />

            <p className="compute-net-body compute-net-body-dim">
                Quiet on the network right now.
                {helpedCount > 0 && (
                    <>
                        {" "}Helped {helpedCount} build{helpedCount === 1 ? "" : "s"} earlier today.
                    </>
                )}
            </p>
        </section>
    );
}

// ════════════════════════════════════════════════════════════════════════
// State: quiet — connected, no recent activity, nobody's on the network
// ════════════════════════════════════════════════════════════════════════

function QuietCard({ localMode, onToggleServing, togglingServing }: Props) {
    return (
        <section className="compute-net-hero compute-net-quiet">
            <header className="compute-net-header">
                <span className="compute-net-icon compute-net-icon-dim">🌐</span>
                <span className="compute-net-title">Compute network</span>
                <span className="compute-net-status compute-net-status-dim">Quiet</span>
            </header>

            <ServingToggle
                on={true}
                model={localMode?.model ?? null}
                busy={togglingServing}
                onToggle={onToggleServing}
            />

            <p className="compute-net-body compute-net-body-dim">
                No active builds on the network right now. Your builds will run local-speed
                until others join.
            </p>
        </section>
    );
}

// ════════════════════════════════════════════════════════════════════════
// State: connected — the default, with recent activity + week summary
// ════════════════════════════════════════════════════════════════════════

function ConnectedCard({
    snapshot,
    localMode,
    recentEvents,
    onToggleServing,
    togglingServing,
}: Props) {
    return (
        <section className="compute-net-hero compute-net-connected">
            <header className="compute-net-header">
                <span className="compute-net-icon">🌐</span>
                <span className="compute-net-title">Compute network</span>
                <span className="compute-net-status">Connected</span>
            </header>

            <ServingToggle
                on={true}
                model={localMode?.model ?? null}
                busy={togglingServing}
                onToggle={onToggleServing}
            />

            <div className="compute-net-divider" />

            <ContributionSummary recentEvents={recentEvents} snapshot={snapshot} />
        </section>
    );
}

// ════════════════════════════════════════════════════════════════════════
// Shared sub-components
// ════════════════════════════════════════════════════════════════════════

interface ServingToggleProps {
    on: boolean;
    model: string | null;
    busy: boolean;
    onToggle: () => void;
}

/// The primary action on the Ready/Quiet/Paused/Connected cards.
/// Framing: "contribute GPU when idle" — not "earn credits."
function ServingToggle({ on, model, busy, onToggle }: ServingToggleProps) {
    return (
        <div className="compute-net-toggle-row">
            <div className="compute-net-toggle-label">
                <div className="compute-net-toggle-primary">Contribute GPU when idle</div>
                {model && (
                    <div className="compute-net-toggle-sub">
                        Model served: <span className="compute-net-model">{model}</span>
                    </div>
                )}
            </div>
            <button
                className={`compute-net-toggle ${on ? "compute-net-toggle-on" : "compute-net-toggle-off"}`}
                onClick={onToggle}
                disabled={busy}
                aria-pressed={on}
                title={
                    on
                        ? "Pause contribution — current job finishes, no new ones start."
                        : "Resume contribution — start helping builds again."
                }
            >
                <span className="compute-net-toggle-dot" />
                <span className="compute-net-toggle-text">
                    {busy ? "…" : on ? "ON" : "OFF"}
                </span>
            </button>
        </div>
    );
}

/// The "Last hour / Session / All-time" rows. Only shown on the
/// Connected card.
///
/// Currently provider-side only — how much I've contributed to the
/// network. The mirror requester-side numbers (my pyramids built
/// using N network GPUs) are surfaced on the Builds tab via
/// EVENT_BUILD_NETWORK_CONTRIBUTION rather than here, so the hero
/// stays a single-concept surface ("am I contributing").
function ContributionSummary({
    recentEvents,
    snapshot,
}: {
    recentEvents: ComputeEvent[];
    snapshot: ComputeMarketStateSnapshot | null;
}) {
    const helpedLastHour = recentEvents.length;
    const sessionJobs = snapshot?.session_jobs_completed ?? 0;
    const sessionCredits = snapshot?.session_credits_earned ?? 0;
    const lifetimeJobs = snapshot?.total_jobs_completed ?? 0;
    const lifetimeCredits = snapshot?.total_credits_earned ?? 0;

    return (
        <div className="compute-net-summary">
            <div className="compute-net-summary-block">
                <div className="compute-net-summary-label">Last hour</div>
                <div className="compute-net-summary-line">
                    Helped · {helpedLastHour} build{helpedLastHour === 1 ? "" : "s"}
                </div>
            </div>

            {sessionJobs > 0 && (
                <div className="compute-net-summary-block">
                    <div className="compute-net-summary-label">This session</div>
                    <div className="compute-net-summary-line">
                        Contributed {formatCredits(sessionCredits)} ·{" "}
                        {sessionJobs} build{sessionJobs === 1 ? "" : "s"} helped
                    </div>
                </div>
            )}

            {lifetimeJobs > 0 && lifetimeJobs !== sessionJobs && (
                <div className="compute-net-summary-block">
                    <div className="compute-net-summary-label">All-time</div>
                    <div className="compute-net-summary-line">
                        Contributed {formatCredits(lifetimeCredits)} ·{" "}
                        {lifetimeJobs} build{lifetimeJobs === 1 ? "" : "s"} helped
                    </div>
                </div>
            )}
        </div>
    );
}

/// Small unobtrusive balance line — like a battery meter, not a
/// stock ticker. Shown in Paused state only where the framing is
/// "you're still a consumer and your balance is available for use."
function Balance({ snapshot }: { snapshot: ComputeMarketStateSnapshot | null }) {
    const balance = snapshot?.session_credits_earned ?? 0;
    if (balance === 0) return null;
    return (
        <div className="compute-net-balance-line">
            Balance · {formatCredits(balance)}
        </div>
    );
}

/// Thousands-separated credits display. Pillar 9 — keep integers
/// exact; never round on the credit path.
function formatCredits(n: number): string {
    if (!Number.isFinite(n)) return "0";
    return Math.trunc(n).toLocaleString("en-US");
}
