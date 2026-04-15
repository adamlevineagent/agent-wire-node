// src/components/modes/ToolsMode.tsx — Phase 10: the full config
// contribution surface. Extends the three-tab shell shipped earlier
// with:
//
//   - My Tools: local config contributions grouped by schema_type +
//     pending agent proposals + existing Wire-published actions.
//   - Create: generative config wizard (schema picker → intent → draft
//     → render/refine → accept) backed by the Phase 9 IPC.
//   - Discover: placeholder for Phase 14's Wire config browser.
//
// Spec: docs/specs/config-contribution-and-wire-sharing.md → "Frontend:
//       ToolsMode.tsx" section, docs/specs/generative-config-pattern.md
//       → "IPC Contract" section, docs/plans/phase-10-workstream-prompt.md.

import { useCallback, useEffect, useMemo, useReducer, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import yaml from "js-yaml";
import { LOCAL_TOOLS } from "../../config/wire-actions";
import { useAppContext } from "../../contexts/AppContext";
import {
    takeToolsModePreset,
    TOOLS_MODE_PRESET_EVENT,
    type ToolsModePreset,
} from "../../utils/toolsModeBridge";
import { YamlConfigRenderer } from "../YamlConfigRenderer";
import { useYamlRendererSources } from "../../hooks/useYamlRendererSources";
import { PublishPreviewModal } from "../PublishPreviewModal";
import { ContributionDetailDrawer } from "../ContributionDetailDrawer";
import { MigrationPanel } from "../MigrationPanel";
import { QualityBadges } from "../QualityBadges";
import type { SchemaAnnotation } from "../../types/yamlRenderer";
import type {
    AcceptConfigResponse,
    ActiveConfigResponse,
    AutoUpdateSettingEntry,
    ConfigContribution,
    ConfigSchemaSummary,
    DiscoveryResult,
    GenerateConfigResponse,
    NeedsMigrationEntry,
    PullLatestResponse,
    Recommendation,
    RefineConfigResponse,
    WireUpdateEntry,
} from "../../types/configContributions";

// Phase 18d: add a fourth top-level tab for the schema migration
// surface. Order: My Tools, Needs Migration (only when there are
// flagged configs), Discover, Create. The badge on the tab label
// shows the count of flagged configs.
type ToolsTab = "my-tools" | "needs-migration" | "discover" | "create";

const TYPE_BADGE_COLORS: Record<string, string> = {
    action: "#3b82f6",
    chain: "#8b5cf6",
    skill: "#10b981",
    template: "#f59e0b",
};

interface WireTool {
    id: string;
    title: string;
    type: string;
    description: string;
    published: boolean;
    createdAt?: string;
}

// Cross-tab bridge — stored here so "Edit" from the My Tools drawer
// can switch the user into the Create tab pre-loaded with a draft.
interface CreateSeed {
    schemaType: string;
    slug: string | null;
    baseYaml: string;
    baseContributionId: string;
}

export function ToolsMode() {
    const [activeTab, setActiveTab] = useState<ToolsTab>("my-tools");
    const [createSeed, setCreateSeed] = useState<CreateSeed | null>(null);
    // Phase 15: preset bridge — a one-shot "please pick this schema
    // and jump to the intent step" signal from other parts of the
    // app (e.g. the DADBEAR Oversight "Set Default Norms" button).
    // Distinct from `createSeed` because the preset has no base YAML
    // or contribution id; it just drives a `pick-schema` dispatch.
    const [createPreset, setCreatePreset] = useState<ToolsModePreset | null>(
        null,
    );

    // Phase 18d: count of configs flagged needing migration. Drives the
    // tab badge AND the per-card "needs migration" chip in My Tools.
    // Refreshed when ToolsMode mounts and whenever a migration is
    // accepted/rejected. Stored as a Set of contribution_ids so the
    // My Tools panel can ask "is this contribution flagged?" in O(1).
    const [needsMigration, setNeedsMigration] = useState<Set<string>>(
        new Set(),
    );
    const [migrationRefreshToken, setMigrationRefreshToken] = useState(0);

    const openCreateFrom = useCallback((seed: CreateSeed) => {
        setCreateSeed(seed);
        setActiveTab("create");
    }, []);

    const clearSeed = useCallback(() => setCreateSeed(null), []);
    const clearPreset = useCallback(() => setCreatePreset(null), []);

    const bumpMigrationRefresh = useCallback(
        () => setMigrationRefreshToken((n) => n + 1),
        [],
    );

    // Phase 18d: fetch the flagged-config list at mount and on every
    // refresh bump so both the tab badge and the My Tools chip stay in
    // sync. Best-effort — failures log + leave the set empty so the
    // UI stays usable.
    useEffect(() => {
        let cancelled = false;
        invoke<NeedsMigrationEntry[]>("pyramid_list_configs_needing_migration")
            .then((rows) => {
                if (cancelled) return;
                const next = new Set<string>();
                for (const r of rows) {
                    next.add(r.contribution_id);
                }
                setNeedsMigration(next);
            })
            .catch((err) => {
                console.warn(
                    "[ToolsMode] needs migration fetch failed:",
                    err,
                );
            });
        return () => {
            cancelled = true;
        };
    }, [migrationRefreshToken]);

    // Phase 15: consume any preset queued by the bridge at mount time
    // AND subscribe to the custom event so presets queued while
    // ToolsMode is already mounted take effect immediately.
    useEffect(() => {
        const existing = takeToolsModePreset();
        if (existing) {
            setCreatePreset(existing);
            setActiveTab("create");
        }
        const handler = (e: Event) => {
            const detail = (e as CustomEvent<ToolsModePreset>).detail;
            if (detail) {
                setCreatePreset(detail);
                setActiveTab("create");
            }
        };
        window.addEventListener(TOOLS_MODE_PRESET_EVENT, handler);
        return () => {
            window.removeEventListener(TOOLS_MODE_PRESET_EVENT, handler);
        };
    }, []);

    const migrationCount = needsMigration.size;

    return (
        <div className="mode-container">
            <nav className="node-tabs">
                <button
                    className={`node-tab ${
                        activeTab === "my-tools" ? "node-tab-active" : ""
                    }`}
                    onClick={() => setActiveTab("my-tools")}
                >
                    My Tools
                </button>
                <button
                    className={`node-tab ${
                        activeTab === "needs-migration"
                            ? "node-tab-active"
                            : ""
                    }`}
                    onClick={() => setActiveTab("needs-migration")}
                    title="Configs flagged for LLM-assisted migration after a schema change"
                    style={{
                        display: "inline-flex",
                        alignItems: "center",
                        gap: 6,
                    }}
                >
                    Needs Migration
                    {migrationCount > 0 && (
                        <span
                            style={{
                                display: "inline-flex",
                                alignItems: "center",
                                justifyContent: "center",
                                minWidth: 18,
                                height: 18,
                                padding: "0 6px",
                                fontSize: 10,
                                fontWeight: 700,
                                color: "#1a1a2e",
                                background: "#f59e0b",
                                borderRadius: 9,
                            }}
                        >
                            {migrationCount}
                        </span>
                    )}
                </button>
                <button
                    className={`node-tab ${
                        activeTab === "discover" ? "node-tab-active" : ""
                    }`}
                    onClick={() => setActiveTab("discover")}
                >
                    Discover
                </button>
                <button
                    className={`node-tab ${
                        activeTab === "create" ? "node-tab-active" : ""
                    }`}
                    onClick={() => setActiveTab("create")}
                >
                    Create
                </button>
            </nav>

            <div className="node-tab-content">
                {activeTab === "my-tools" && (
                    <MyToolsPanel
                        onEdit={openCreateFrom}
                        flaggedContributionIds={needsMigration}
                    />
                )}
                {activeTab === "needs-migration" && (
                    <MigrationPanel
                        refreshToken={migrationRefreshToken}
                        onMigrationChanged={bumpMigrationRefresh}
                    />
                )}
                {activeTab === "discover" && <DiscoverPanel />}
                {activeTab === "create" && (
                    <CreatePanel
                        seed={createSeed}
                        onSeedConsumed={clearSeed}
                        preset={createPreset}
                        onPresetConsumed={clearPreset}
                    />
                )}
            </div>
        </div>
    );
}

// ─── My Tools ───────────────────────────────────────────────────────────────

interface MyToolsPanelProps {
    onEdit: (seed: CreateSeed) => void;
    /** Phase 18d: contribution_ids that are flagged needs_migration = 1.
     *  Each ConfigCard gets a "needs migration" chip when its active
     *  contribution_id is in this set. Allows discovery of the flag from
     *  the existing My Tools surface even if the user never opens the
     *  Needs Migration tab. */
    flaggedContributionIds?: Set<string>;
}

function MyToolsPanel({
    onEdit,
    flaggedContributionIds,
}: MyToolsPanelProps) {
    const { wireApiCall } = useAppContext();

    // Existing Wire-published actions (Sprint 3 behavior — keep as-is).
    const [wireTools, setWireTools] = useState<WireTool[]>([]);
    const [wireLoading, setWireLoading] = useState(true);
    const [wireError, setWireError] = useState<string | null>(null);

    // Phase 10: config contributions grouped by schema_type.
    const [schemas, setSchemas] = useState<ConfigSchemaSummary[]>([]);
    const [activeConfigs, setActiveConfigs] = useState<
        Record<string, ActiveConfigResponse | null>
    >({});
    const [configsLoading, setConfigsLoading] = useState(false);
    const [configsError, setConfigsError] = useState<string | null>(null);

    // Phase 10: pending agent proposals.
    const [proposals, setProposals] = useState<ConfigContribution[]>([]);
    const [proposalsLoading, setProposalsLoading] = useState(false);
    const [proposalsError, setProposalsError] = useState<string | null>(null);

    // Detail drawer + publish modal state.
    const [detailContribution, setDetailContribution] =
        useState<ConfigContribution | null>(null);
    const [detailInitialTab, setDetailInitialTab] = useState<
        "details" | "history"
    >("details");
    const [publishContributionId, setPublishContributionId] =
        useState<string | null>(null);
    const [publishSchemaType, setPublishSchemaType] = useState<string | undefined>(
        undefined,
    );

    // Phase 14: Wire update badges.
    const [wireUpdates, setWireUpdates] = useState<WireUpdateEntry[]>([]);
    const [selectedUpdate, setSelectedUpdate] =
        useState<WireUpdateEntry | null>(null);

    // Bump this to refresh config fetches after accept/reject/publish.
    const [refreshToken, setRefreshToken] = useState(0);

    const bumpRefresh = useCallback(() => setRefreshToken((n) => n + 1), []);

    // ── Fetch published Wire actions (existing) ─────────────────────────────

    useEffect(() => {
        let cancelled = false;
        setWireLoading(true);
        setWireError(null);

        wireApiCall("GET", "/api/v1/wire/my/contributions")
            .then((data: unknown) => {
                if (cancelled) return;
                const contributions = Array.isArray(data)
                    ? data
                    : (data as Record<string, unknown>)?.contributions ??
                      (data as Record<string, unknown>)?.data ??
                      [];
                const actions = (contributions as Array<Record<string, unknown>>)
                    .filter((c) => c.type === "action")
                    .map((c) => ({
                        id: String(c.id ?? c.uuid ?? ""),
                        title: String(c.title ?? "Untitled"),
                        type: String(c.type ?? "action"),
                        description: String(c.body ?? "").slice(0, 200),
                        published: true,
                        createdAt: c.created_at ? String(c.created_at) : undefined,
                    }));
                setWireTools(actions);
            })
            .catch((err: unknown) => {
                if (cancelled) return;
                console.warn("Failed to fetch Wire tools:", err);
                setWireError("Could not load published tools from the Wire.");
            })
            .finally(() => {
                if (!cancelled) setWireLoading(false);
            });

        return () => { cancelled = true; };
    }, [wireApiCall]);

    // ── Fetch schemas + active configs ──────────────────────────────────────

    useEffect(() => {
        let cancelled = false;
        setConfigsLoading(true);
        setConfigsError(null);

        (async () => {
            try {
                const schemaList = await invoke<ConfigSchemaSummary[]>(
                    "pyramid_config_schemas",
                );
                if (cancelled) return;
                setSchemas(schemaList);

                // Fetch active config per schema type in parallel.
                const pairs = await Promise.all(
                    schemaList.map(async (s) => {
                        try {
                            const active = await invoke<ActiveConfigResponse | null>(
                                "pyramid_active_config",
                                { schemaType: s.schema_type, slug: null },
                            );
                            return [s.schema_type, active] as const;
                        } catch (err) {
                            console.warn(
                                `[MyToolsPanel] active_config failed for ${s.schema_type}:`,
                                err,
                            );
                            return [s.schema_type, null] as const;
                        }
                    }),
                );
                if (cancelled) return;
                const map: Record<string, ActiveConfigResponse | null> = {};
                for (const [key, value] of pairs) map[key] = value;
                setActiveConfigs(map);
            } catch (err) {
                if (cancelled) return;
                setConfigsError(String(err));
            } finally {
                if (!cancelled) setConfigsLoading(false);
            }
        })();

        return () => { cancelled = true; };
    }, [refreshToken]);

    // ── Fetch pending proposals ─────────────────────────────────────────────

    useEffect(() => {
        let cancelled = false;
        setProposalsLoading(true);
        setProposalsError(null);

        invoke<ConfigContribution[]>("pyramid_pending_proposals", { slug: null })
            .then((rows) => {
                if (cancelled) return;
                setProposals(rows);
            })
            .catch((err) => {
                if (cancelled) return;
                setProposalsError(String(err));
            })
            .finally(() => {
                if (!cancelled) setProposalsLoading(false);
            });

        return () => { cancelled = true; };
    }, [refreshToken]);

    // ── Phase 14: fetch pending Wire updates ───────────────────────────────
    useEffect(() => {
        let cancelled = false;
        invoke<WireUpdateEntry[]>("pyramid_wire_update_available", {
            slug: null,
        })
            .then((rows) => {
                if (cancelled) return;
                setWireUpdates(rows);
            })
            .catch((err) => {
                if (cancelled) return;
                console.warn("[MyToolsPanel] wire updates failed:", err);
            });
        return () => {
            cancelled = true;
        };
    }, [refreshToken]);

    // ── Handlers ────────────────────────────────────────────────────────────

    const handleOpenDetail = useCallback(
        async (schemaType: string) => {
            try {
                // Load the full ConfigContribution row. Phase 4 exposes
                // `pyramid_active_config_contribution` which returns the
                // full row. We route through it so the drawer gets a
                // schema_type + contribution_id + all the metadata.
                const row = await invoke<ConfigContribution | null>(
                    "pyramid_active_config_contribution",
                    { schemaType, slug: null },
                );
                if (row) {
                    setDetailInitialTab("details");
                    setDetailContribution(row);
                }
            } catch (err) {
                console.warn("Failed to load contribution row:", err);
            }
        },
        [],
    );

    const handleOpenHistory = useCallback(
        async (schemaType: string) => {
            try {
                const row = await invoke<ConfigContribution | null>(
                    "pyramid_active_config_contribution",
                    { schemaType, slug: null },
                );
                if (row) {
                    setDetailInitialTab("history");
                    setDetailContribution(row);
                }
            } catch (err) {
                console.warn("Failed to load contribution row:", err);
            }
        },
        [],
    );

    const handlePublish = useCallback(
        async (schemaType: string) => {
            try {
                const row = await invoke<ConfigContribution | null>(
                    "pyramid_active_config_contribution",
                    { schemaType, slug: null },
                );
                if (row) {
                    setPublishContributionId(row.contribution_id);
                    setPublishSchemaType(row.schema_type);
                }
            } catch (err) {
                console.warn("Failed to resolve contribution for publish:", err);
            }
        },
        [],
    );

    const handleAcceptProposal = useCallback(
        async (proposal: ConfigContribution) => {
            try {
                await invoke("pyramid_accept_proposal", {
                    contributionId: proposal.contribution_id,
                });
                bumpRefresh();
            } catch (err) {
                alert(`Accept failed: ${String(err)}`);
            }
        },
        [bumpRefresh],
    );

    const handleRejectProposal = useCallback(
        async (proposal: ConfigContribution) => {
            const reason = window.prompt(
                `Reject proposal from ${proposal.created_by ?? "agent"}? Provide a reason (optional):`,
                "",
            );
            if (reason === null) return; // user cancelled
            try {
                await invoke("pyramid_reject_proposal", {
                    contributionId: proposal.contribution_id,
                    reason: reason.trim() ? reason.trim() : null,
                });
                bumpRefresh();
            } catch (err) {
                alert(`Reject failed: ${String(err)}`);
            }
        },
        [bumpRefresh],
    );

    const handleEdit = useCallback(
        (contribution: ConfigContribution) => {
            onEdit({
                schemaType: contribution.schema_type,
                slug: contribution.slug,
                baseYaml: contribution.yaml_content,
                baseContributionId: contribution.contribution_id,
            });
            setDetailContribution(null);
        },
        [onEdit],
    );

    const handleDrawerPublish = useCallback(
        (contribution: ConfigContribution) => {
            setPublishContributionId(contribution.contribution_id);
            setPublishSchemaType(contribution.schema_type);
        },
        [],
    );

    const publishClose = useCallback(() => {
        setPublishContributionId(null);
        setPublishSchemaType(undefined);
        bumpRefresh();
    }, [bumpRefresh]);

    // Fired by PublishPreviewModal after a successful publish. Closes
    // the detail drawer (if any) so its stale ConfigContribution row —
    // still showing `wire_contribution_id: null` from before the write
    // — is cleared. The next View click refetches the fresh row and
    // the "Published" badge lights up correctly.
    const handlePublishSuccess = useCallback(() => {
        setDetailContribution(null);
    }, []);

    // Merge local planner tools with Wire-published tools (existing).
    const legacyTools: Array<WireTool & { usageCount?: number }> = [
        ...LOCAL_TOOLS.map((t) => ({
            id: t.id,
            title: t.title,
            type: t.type,
            description: t.description,
            published: t.published,
            usageCount: t.usageCount,
            createdAt: undefined as string | undefined,
        })),
        ...wireTools,
    ];

    return (
        <div style={{ display: "flex", flexDirection: "column", gap: 24 }}>
            {/* ── Section A: My Configs ──────────────────────────────────── */}
            <section
                style={{ display: "flex", flexDirection: "column", gap: 12 }}
            >
                <SectionHeader
                    title="My Configs"
                    subtitle="Every knob that drives Wire Node behavior — provider routing, evidence policy, DADBEAR, and more — is a contribution you can view, refine, and share."
                />

                {configsLoading && (
                    <p style={{ color: "var(--text-secondary)", fontSize: 13 }}>
                        Loading configs…
                    </p>
                )}
                {configsError && (
                    <p style={{ color: "#fca5a5", fontSize: 13 }}>
                        {configsError}
                    </p>
                )}
                {!configsLoading && schemas.length === 0 && !configsError && (
                    <p style={{ color: "var(--text-secondary)", fontSize: 13 }}>
                        No schema types registered yet. Bundled defaults should
                        appear on first run.
                    </p>
                )}

                {schemas.map((schema) => {
                    const active = activeConfigs[schema.schema_type];
                    // Phase 14: find a matching update (if any) for this
                    // schema_type's active contribution.
                    const update = wireUpdates.find(
                        (u) =>
                            u.schema_type === schema.schema_type &&
                            (!active ||
                                u.local_contribution_id ===
                                    active.contribution_id),
                    );
                    // Phase 18d: surface the needs_migration chip when
                    // this card's active contribution is in the flagged
                    // set provided by ToolsMode.
                    const needsMigration =
                        active != null &&
                        flaggedContributionIds != null &&
                        flaggedContributionIds.has(active.contribution_id);
                    return (
                        <ConfigCard
                            key={schema.schema_type}
                            schema={schema}
                            active={active}
                            update={update}
                            needsMigration={needsMigration}
                            onView={() => handleOpenDetail(schema.schema_type)}
                            onHistory={() =>
                                handleOpenHistory(schema.schema_type)
                            }
                            onPublish={() => handlePublish(schema.schema_type)}
                            onOpenUpdate={() =>
                                update && setSelectedUpdate(update)
                            }
                        />
                    );
                })}
            </section>

            {/* ── Section B: Pending Proposals ────────────────────────────── */}
            <section
                style={{ display: "flex", flexDirection: "column", gap: 12 }}
            >
                <SectionHeader
                    title="Pending Proposals"
                    subtitle="Configs proposed by agents (via MCP) awaiting your review."
                />
                {proposalsLoading && (
                    <p style={{ color: "var(--text-secondary)", fontSize: 13 }}>
                        Loading proposals…
                    </p>
                )}
                {proposalsError && (
                    <p style={{ color: "#fca5a5", fontSize: 13 }}>
                        {proposalsError}
                    </p>
                )}
                {!proposalsLoading &&
                    !proposalsError &&
                    proposals.length === 0 && (
                        <p
                            style={{
                                color: "var(--text-secondary)",
                                fontSize: 13,
                                fontStyle: "italic",
                            }}
                        >
                            No pending proposals.
                        </p>
                    )}
                {proposals.map((p) => (
                    <ProposalCard
                        key={p.contribution_id}
                        proposal={p}
                        onAccept={() => handleAcceptProposal(p)}
                        onReject={() => handleRejectProposal(p)}
                    />
                ))}
            </section>

            {/* ── Section C: Published Wire Actions (existing) ─────────────── */}
            <section
                style={{ display: "flex", flexDirection: "column", gap: 12 }}
            >
                <SectionHeader
                    title="Published Wire Actions"
                    subtitle="Action contributions you have published to the Wire marketplace."
                />

                {wireLoading && (
                    <p style={{ color: "var(--text-secondary)", fontSize: 13 }}>
                        Loading tools…
                    </p>
                )}
                {wireError && (
                    <p
                        style={{
                            color: "var(--accent-warning, #f59e0b)",
                            fontSize: 13,
                        }}
                    >
                        {wireError}
                    </p>
                )}
                {!wireLoading &&
                    legacyTools.map((tool) => (
                        <LegacyToolCard key={tool.id} tool={tool} />
                    ))}
                {!wireLoading && legacyTools.length === 0 && (
                    <p style={{ color: "var(--text-secondary)" }}>
                        No published tools yet.
                    </p>
                )}
            </section>

            {/* Drawer + modal overlays */}
            <ContributionDetailDrawer
                contribution={detailContribution}
                initialTab={detailInitialTab}
                onClose={() => setDetailContribution(null)}
                onPublish={handleDrawerPublish}
                onEdit={handleEdit}
            />
            {publishContributionId && (
                <PublishPreviewModal
                    contributionId={publishContributionId}
                    schemaType={publishSchemaType}
                    onClose={publishClose}
                    onPublished={handlePublishSuccess}
                />
            )}
            {selectedUpdate && (
                <WireUpdateDrawer
                    update={selectedUpdate}
                    onClose={() => setSelectedUpdate(null)}
                    onPulled={() => {
                        setSelectedUpdate(null);
                        bumpRefresh();
                    }}
                />
            )}
        </div>
    );
}

// ── My Tools subcomponents ──────────────────────────────────────────────────

function SectionHeader({
    title,
    subtitle,
}: {
    title: string;
    subtitle?: string;
}) {
    return (
        <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
            <h3
                style={{
                    margin: 0,
                    fontSize: 14,
                    fontWeight: 700,
                    color: "var(--text-primary)",
                    textTransform: "uppercase",
                    letterSpacing: "0.04em",
                }}
            >
                {title}
            </h3>
            {subtitle && (
                <p
                    style={{
                        margin: 0,
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        lineHeight: 1.5,
                    }}
                >
                    {subtitle}
                </p>
            )}
        </div>
    );
}

function ConfigCard({
    schema,
    active,
    update,
    onView,
    onHistory,
    onPublish,
    onOpenUpdate,
    needsMigration,
}: {
    schema: ConfigSchemaSummary;
    active: ActiveConfigResponse | null | undefined;
    update?: WireUpdateEntry;
    onView: () => void;
    onHistory: () => void;
    onPublish: () => void;
    onOpenUpdate?: () => void;
    /** Phase 18d: render the "needs migration" chip when this card's
     *  active contribution is flagged. The user can click the chip to
     *  hop to the Needs Migration tab via the toolsModeBridge. */
    needsMigration?: boolean;
}) {
    const hasActive = !!active;
    const version = active?.version_chain_length ?? 0;
    return (
        <div
            style={{
                background: "var(--bg-secondary, #1a1a2e)",
                border: "1px solid var(--border-primary, #2a2a4a)",
                borderRadius: 8,
                padding: 16,
                display: "flex",
                flexDirection: "column",
                gap: 10,
            }}
        >
            <div
                style={{
                    display: "flex",
                    alignItems: "baseline",
                    gap: 10,
                    flexWrap: "wrap",
                }}
            >
                <span
                    style={{
                        fontSize: 14,
                        fontWeight: 600,
                        color: "var(--text-primary)",
                    }}
                >
                    {schema.display_name}
                </span>
                <span
                    style={{
                        fontSize: 10,
                        fontFamily: "var(--font-mono, monospace)",
                        color: "var(--text-secondary)",
                        opacity: 0.7,
                    }}
                >
                    {schema.schema_type}
                </span>
                {hasActive && (
                    <span
                        style={{
                            fontSize: 10,
                            padding: "2px 6px",
                            borderRadius: 4,
                            background: "rgba(16, 185, 129, 0.15)",
                            color: "#10b981",
                            fontWeight: 600,
                        }}
                    >
                        Active · v{version}
                    </span>
                )}
                {!hasActive && (
                    <span
                        style={{
                            fontSize: 10,
                            padding: "2px 6px",
                            borderRadius: 4,
                            background: "rgba(107, 114, 128, 0.15)",
                            color: "#9ca3af",
                            fontWeight: 600,
                        }}
                    >
                        No active config
                    </span>
                )}
                {update && (
                    <button
                        type="button"
                        onClick={onOpenUpdate}
                        style={{
                            fontSize: 10,
                            padding: "2px 8px",
                            borderRadius: 4,
                            background: "rgba(59, 130, 246, 0.18)",
                            color: "#60a5fa",
                            fontWeight: 600,
                            border: "1px solid rgba(59, 130, 246, 0.4)",
                            cursor: "pointer",
                        }}
                        title={`${update.chain_length_delta} version${update.chain_length_delta === 1 ? "" : "s"} ahead on the Wire`}
                    >
                        Update available ({update.chain_length_delta})
                    </button>
                )}
                {needsMigration && (
                    <span
                        style={{
                            fontSize: 10,
                            padding: "2px 8px",
                            borderRadius: 4,
                            background: "rgba(245, 158, 11, 0.18)",
                            color: "#f59e0b",
                            fontWeight: 600,
                            border: "1px solid rgba(245, 158, 11, 0.4)",
                            textTransform: "uppercase",
                            letterSpacing: "0.04em",
                        }}
                        title="The schema this config was written against has been refined — open the Needs Migration tab to review the LLM-assisted migration"
                    >
                        Migration needed
                    </span>
                )}
            </div>
            <p
                style={{
                    margin: 0,
                    fontSize: 12,
                    color: "var(--text-secondary)",
                    lineHeight: 1.5,
                }}
            >
                {schema.description}
            </p>
            {active?.triggering_note && (
                <p
                    style={{
                        margin: 0,
                        fontSize: 11,
                        color: "var(--text-secondary)",
                        fontStyle: "italic",
                        opacity: 0.85,
                    }}
                >
                    "{active.triggering_note}"
                </p>
            )}
            <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={onView}
                    disabled={!hasActive}
                    title={
                        hasActive
                            ? "View this config"
                            : "No active config to view"
                    }
                >
                    View
                </button>
                <button
                    type="button"
                    className="btn btn-ghost btn-small"
                    onClick={onHistory}
                    disabled={!hasActive}
                >
                    View History
                </button>
                <button
                    type="button"
                    className="btn btn-primary btn-small"
                    onClick={onPublish}
                    disabled={!hasActive}
                    title={
                        hasActive
                            ? "Publish the active config to the Wire"
                            : "Nothing to publish"
                    }
                >
                    Publish to Wire
                </button>
            </div>
        </div>
    );
}

function ProposalCard({
    proposal,
    onAccept,
    onReject,
}: {
    proposal: ConfigContribution;
    onAccept: () => void;
    onReject: () => void;
}) {
    return (
        <div
            style={{
                background: "rgba(167, 139, 250, 0.04)",
                border: "1px solid rgba(167, 139, 250, 0.2)",
                borderRadius: 8,
                padding: 14,
                display: "flex",
                flexDirection: "column",
                gap: 8,
            }}
        >
            <div
                style={{
                    display: "flex",
                    alignItems: "baseline",
                    gap: 8,
                    flexWrap: "wrap",
                }}
            >
                <span
                    style={{
                        fontSize: 13,
                        fontWeight: 600,
                        color: "var(--text-primary)",
                    }}
                >
                    {proposal.schema_type}
                </span>
                {proposal.slug && (
                    <code
                        style={{
                            fontSize: 11,
                            color: "var(--text-secondary)",
                        }}
                    >
                        {proposal.slug}
                    </code>
                )}
                <span
                    style={{
                        marginLeft: "auto",
                        fontSize: 11,
                        color: "var(--accent-purple, #a78bfa)",
                        fontWeight: 600,
                    }}
                >
                    from {proposal.created_by ?? "agent"}
                </span>
            </div>
            {proposal.triggering_note && (
                <p
                    style={{
                        margin: 0,
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        fontStyle: "italic",
                        lineHeight: 1.5,
                    }}
                >
                    "{proposal.triggering_note}"
                </p>
            )}
            <div style={{ display: "flex", gap: 6 }}>
                <button
                    type="button"
                    className="btn btn-primary btn-small"
                    onClick={onAccept}
                >
                    Accept
                </button>
                <button
                    type="button"
                    className="btn btn-ghost btn-small"
                    onClick={onReject}
                >
                    Reject
                </button>
            </div>
        </div>
    );
}

function WireUpdateDrawer({
    update,
    onClose,
    onPulled,
}: {
    update: WireUpdateEntry;
    onClose: () => void;
    onPulled: () => void;
}) {
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);

    const handlePull = useCallback(async () => {
        setBusy(true);
        setError(null);
        try {
            await invoke<PullLatestResponse>("pyramid_wire_pull_latest", {
                localContributionId: update.local_contribution_id,
                latestWireContributionId: update.latest_wire_contribution_id,
            });
            onPulled();
        } catch (err) {
            setError(String(err));
        } finally {
            setBusy(false);
        }
    }, [update, onPulled]);

    const handleDismiss = useCallback(async () => {
        setBusy(true);
        setError(null);
        try {
            await invoke("pyramid_wire_acknowledge_update", {
                localContributionId: update.local_contribution_id,
            });
            onClose();
        } catch (err) {
            setError(String(err));
        } finally {
            setBusy(false);
        }
    }, [update, onClose]);

    return (
        <div
            role="dialog"
            aria-label="Wire update"
            style={{
                position: "fixed",
                top: 0,
                right: 0,
                bottom: 0,
                width: 480,
                background: "var(--bg-primary, #0b0b1a)",
                borderLeft: "1px solid var(--border-primary, #2a2a4a)",
                boxShadow: "-8px 0 32px rgba(0, 0, 0, 0.4)",
                display: "flex",
                flexDirection: "column",
                zIndex: 400,
            }}
        >
            <div
                style={{
                    padding: 16,
                    borderBottom: "1px solid var(--border-primary, #2a2a4a)",
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "center",
                }}
            >
                <h3 style={{ margin: 0, fontSize: 14, color: "var(--text-primary)" }}>
                    Wire update for {update.schema_type}
                </h3>
                <button
                    type="button"
                    className="btn btn-ghost btn-small"
                    onClick={onClose}
                >
                    Close
                </button>
            </div>
            <div
                style={{
                    padding: 16,
                    overflowY: "auto",
                    display: "flex",
                    flexDirection: "column",
                    gap: 12,
                }}
            >
                <div
                    style={{
                        fontSize: 12,
                        color: "var(--text-secondary)",
                    }}
                >
                    {update.chain_length_delta}{" "}
                    {update.chain_length_delta === 1 ? "version" : "versions"} ahead on
                    the Wire
                    {update.slug && <> · scope: {update.slug}</>}
                </div>
                <div
                    style={{
                        fontSize: 11,
                        fontFamily: "var(--font-mono, monospace)",
                        color: "var(--text-secondary)",
                    }}
                >
                    Latest: {update.latest_wire_contribution_id}
                </div>
                {update.author_handles.length > 0 && (
                    <div
                        style={{
                            fontSize: 12,
                            color: "var(--text-secondary)",
                        }}
                    >
                        Authors:{" "}
                        {update.author_handles.join(", ")}
                    </div>
                )}
                {update.changes_summary && (
                    <div
                        style={{
                            padding: 10,
                            background: "rgba(59, 130, 246, 0.06)",
                            borderRadius: 6,
                            fontSize: 12,
                            color: "var(--text-primary)",
                            lineHeight: 1.5,
                            whiteSpace: "pre-wrap",
                        }}
                    >
                        {update.changes_summary}
                    </div>
                )}
                <div
                    style={{
                        fontSize: 11,
                        color: "var(--text-secondary)",
                    }}
                >
                    Checked at {new Date(update.checked_at).toLocaleString()}
                </div>
                {error && (
                    <div
                        style={{
                            padding: 10,
                            background: "rgba(239, 68, 68, 0.1)",
                            borderRadius: 6,
                            fontSize: 12,
                            color: "#fca5a5",
                        }}
                    >
                        {error}
                    </div>
                )}
            </div>
            <div
                style={{
                    padding: 16,
                    borderTop: "1px solid var(--border-primary, #2a2a4a)",
                    display: "flex",
                    gap: 8,
                }}
            >
                <button
                    type="button"
                    className="btn btn-ghost btn-small"
                    onClick={handleDismiss}
                    disabled={busy}
                >
                    Dismiss
                </button>
                <button
                    type="button"
                    className="btn btn-primary btn-small"
                    onClick={handlePull}
                    disabled={busy}
                >
                    {busy ? "Pulling…" : "Pull latest"}
                </button>
            </div>
        </div>
    );
}

function LegacyToolCard({
    tool,
}: {
    tool: WireTool & { usageCount?: number };
}) {
    return (
        <div
            style={{
                background: "var(--bg-secondary, #1a1a2e)",
                border: "1px solid var(--border-primary, #2a2a4a)",
                borderRadius: 8,
                padding: 16,
            }}
        >
            <div
                style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 10,
                    marginBottom: 8,
                }}
            >
                <span
                    style={{
                        fontSize: 15,
                        fontWeight: 600,
                        color: "var(--text-primary, #e0e0e0)",
                    }}
                >
                    {tool.title}
                </span>
                <span
                    style={{
                        fontSize: 11,
                        fontWeight: 600,
                        textTransform: "uppercase",
                        letterSpacing: "0.05em",
                        padding: "2px 8px",
                        borderRadius: 4,
                        background: TYPE_BADGE_COLORS[tool.type] ?? "#6b7280",
                        color: "#fff",
                    }}
                >
                    {tool.type}
                </span>
                {tool.published && (
                    <span
                        style={{
                            fontSize: 11,
                            padding: "2px 6px",
                            borderRadius: 4,
                            background: "rgba(16, 185, 129, 0.15)",
                            color: "#10b981",
                        }}
                    >
                        Published
                    </span>
                )}
            </div>
            <p
                style={{
                    margin: 0,
                    fontSize: 13,
                    color: "var(--text-secondary, #a0a0b0)",
                    lineHeight: 1.5,
                }}
            >
                {tool.description}
            </p>
            {tool.createdAt && (
                <p
                    style={{
                        margin: "4px 0 0",
                        fontSize: 11,
                        color: "var(--text-tertiary, #6b7280)",
                    }}
                >
                    Published {new Date(tool.createdAt).toLocaleDateString()}
                </p>
            )}
        </div>
    );
}

// ─── Create tab (generative config wizard) ──────────────────────────────────

type CreateStep =
    | "schema-picker"
    | "intent"
    | "generating"
    | "edit"
    | "refining"
    | "accepted";

interface CreateState {
    step: CreateStep;
    schemas: ConfigSchemaSummary[];
    schemasLoading: boolean;
    schemasError: string | null;
    selectedSchema: ConfigSchemaSummary | null;
    slug: string | null;
    intent: string;
    draftContributionId: string | null;
    rawYaml: string;
    values: Record<string, unknown>;
    annotation: SchemaAnnotation | null;
    annotationLoading: boolean;
    version: number;
    triggeringNote: string | null;
    accepted: AcceptConfigResponse | null;
    error: string | null;
}

const initialCreateState: CreateState = {
    step: "schema-picker",
    schemas: [],
    schemasLoading: false,
    schemasError: null,
    selectedSchema: null,
    slug: null,
    intent: "",
    draftContributionId: null,
    rawYaml: "",
    values: {},
    annotation: null,
    annotationLoading: false,
    version: 0,
    triggeringNote: null,
    accepted: null,
    error: null,
};

type CreateAction =
    | { type: "load-schemas-start" }
    | { type: "load-schemas-success"; schemas: ConfigSchemaSummary[] }
    | { type: "load-schemas-error"; error: string }
    | {
          type: "pick-schema";
          schema: ConfigSchemaSummary | null;
          slug?: string | null;
      }
    | { type: "set-intent"; intent: string }
    | { type: "generate-start" }
    | { type: "generate-success"; response: GenerateConfigResponse }
    | { type: "generate-error"; error: string }
    | { type: "annotation-start" }
    | { type: "annotation-success"; annotation: SchemaAnnotation | null }
    | { type: "change-field"; path: string; value: unknown }
    | { type: "refine-start" }
    | { type: "refine-success"; response: RefineConfigResponse; note: string }
    | { type: "refine-error"; error: string }
    | { type: "accept-start" }
    | { type: "accept-success"; response: AcceptConfigResponse }
    | { type: "accept-error"; error: string }
    | { type: "reset" }
    | {
          type: "seed-from-existing";
          schema: ConfigSchemaSummary;
          slug: string | null;
          rawYaml: string;
          values: Record<string, unknown>;
          baseContributionId: string;
      };

function createReducer(state: CreateState, action: CreateAction): CreateState {
    switch (action.type) {
        case "load-schemas-start":
            return { ...state, schemasLoading: true, schemasError: null };
        case "load-schemas-success":
            return {
                ...state,
                schemasLoading: false,
                schemas: action.schemas,
            };
        case "load-schemas-error":
            return {
                ...state,
                schemasLoading: false,
                schemasError: action.error,
            };
        case "pick-schema":
            return {
                ...state,
                selectedSchema: action.schema,
                slug: action.slug ?? null,
                step: action.schema ? "intent" : "schema-picker",
                intent: "",
                error: null,
            };
        case "set-intent":
            return { ...state, intent: action.intent };
        case "generate-start":
            return { ...state, step: "generating", error: null };
        case "generate-success": {
            const values = safeYamlParse(action.response.yaml_content);
            return {
                ...state,
                step: "edit",
                draftContributionId: action.response.contribution_id,
                rawYaml: action.response.yaml_content,
                values,
                version: action.response.version,
                triggeringNote: state.intent,
                error: null,
            };
        }
        case "generate-error":
            return { ...state, step: "intent", error: action.error };
        case "annotation-start":
            return { ...state, annotationLoading: true };
        case "annotation-success":
            return {
                ...state,
                annotationLoading: false,
                annotation: action.annotation,
            };
        case "change-field":
            return {
                ...state,
                values: writePath(state.values, action.path, action.value),
            };
        case "refine-start":
            return { ...state, step: "refining", error: null };
        case "refine-success": {
            const values = safeYamlParse(action.response.yaml_content);
            return {
                ...state,
                step: "edit",
                draftContributionId: action.response.new_contribution_id,
                rawYaml: action.response.yaml_content,
                values,
                version: action.response.version,
                // The refined version's provenance is the refinement
                // note the user just submitted — NOT the original
                // intent. Mirror the backend's supersession chain so
                // the renderer's version info shows the right note.
                triggeringNote: action.note,
                error: null,
            };
        }
        case "refine-error":
            return { ...state, step: "edit", error: action.error };
        case "accept-start":
            return { ...state, error: null };
        case "accept-success":
            return { ...state, step: "accepted", accepted: action.response };
        case "accept-error":
            return { ...state, error: action.error };
        case "reset":
            return {
                ...initialCreateState,
                schemas: state.schemas,
                schemasLoading: false,
            };
        case "seed-from-existing":
            return {
                ...initialCreateState,
                schemas: state.schemas,
                selectedSchema: action.schema,
                slug: action.slug,
                step: "edit",
                draftContributionId: action.baseContributionId,
                rawYaml: action.rawYaml,
                values: action.values,
                version: 0,
                triggeringNote: "Seeded from existing contribution",
            };
        default:
            return state;
    }
}

interface CreatePanelProps {
    seed: CreateSeed | null;
    onSeedConsumed: () => void;
    // Phase 15: one-shot preset that picks a schema and jumps to the
    // intent step without a seeded draft. Used by the DADBEAR
    // Oversight "Set Default Norms" button.
    preset?: ToolsModePreset | null;
    onPresetConsumed?: () => void;
}

function CreatePanel({
    seed,
    onSeedConsumed,
    preset,
    onPresetConsumed,
}: CreatePanelProps) {
    const [state, dispatch] = useReducer(createReducer, initialCreateState);

    // ── Load schemas on mount ───────────────────────────────────────────────

    useEffect(() => {
        let cancelled = false;
        dispatch({ type: "load-schemas-start" });
        invoke<ConfigSchemaSummary[]>("pyramid_config_schemas")
            .then((list) => {
                if (cancelled) return;
                dispatch({ type: "load-schemas-success", schemas: list });
            })
            .catch((err) => {
                if (cancelled) return;
                dispatch({ type: "load-schemas-error", error: String(err) });
            });
        return () => { cancelled = true; };
    }, []);

    // ── Fetch annotation whenever the selected schema changes ──────────────

    useEffect(() => {
        if (!state.selectedSchema) return;
        let cancelled = false;
        dispatch({ type: "annotation-start" });
        invoke<SchemaAnnotation | null>("pyramid_get_schema_annotation", {
            schemaType: state.selectedSchema.schema_type,
        })
            .then((ann) => {
                if (cancelled) return;
                dispatch({ type: "annotation-success", annotation: ann });
            })
            .catch((err) => {
                if (cancelled) return;
                console.warn(
                    "[CreatePanel] annotation fetch failed:",
                    err,
                );
                dispatch({ type: "annotation-success", annotation: null });
            });
        return () => { cancelled = true; };
    }, [state.selectedSchema?.schema_type]);

    // ── Consume seed from My Tools "Edit" button ───────────────────────────

    useEffect(() => {
        if (!seed || state.schemas.length === 0) return;
        const schema = state.schemas.find(
            (s) => s.schema_type === seed.schemaType,
        );
        if (!schema) return;
        const values = safeYamlParse(seed.baseYaml);
        dispatch({
            type: "seed-from-existing",
            schema,
            slug: seed.slug,
            rawYaml: seed.baseYaml,
            values,
            baseContributionId: seed.baseContributionId,
        });
        onSeedConsumed();
    }, [seed, state.schemas, onSeedConsumed]);

    // ── Phase 15: Consume preset (schema-only pre-selection) ──────────────
    //
    // The DADBEAR Oversight "Set Default Norms" button dispatches a
    // preset { schemaType: 'dadbear_norms', slug: null }. We jump
    // straight to the intent step so the user can write a norm
    // adjustment directly without picking a schema from the list.
    useEffect(() => {
        if (!preset || state.schemas.length === 0) return;
        const schema = state.schemas.find(
            (s) => s.schema_type === preset.schemaType,
        );
        if (!schema) return;
        dispatch({ type: "pick-schema", schema, slug: preset.slug ?? null });
        onPresetConsumed?.();
    }, [preset, state.schemas, onPresetConsumed]);

    // ── Renderer sources hook (dynamic options + cost estimates) ───────────

    const rendererDeps = useYamlRendererSources(state.annotation, state.values);

    // ── Action handlers ────────────────────────────────────────────────────

    const handlePickSchema = useCallback(
        (schema: ConfigSchemaSummary) => {
            dispatch({ type: "pick-schema", schema, slug: null });
        },
        [],
    );

    const handleBackToPicker = useCallback(() => {
        dispatch({ type: "pick-schema", schema: null });
    }, []);

    const handleGenerate = useCallback(async () => {
        if (!state.selectedSchema) return;
        const trimmed = state.intent.trim();
        if (trimmed.length === 0) return;
        dispatch({ type: "generate-start" });
        try {
            const response = await invoke<GenerateConfigResponse>(
                "pyramid_generate_config",
                {
                    schemaType: state.selectedSchema.schema_type,
                    slug: state.slug,
                    intent: trimmed,
                },
            );
            dispatch({ type: "generate-success", response });
        } catch (err) {
            dispatch({ type: "generate-error", error: String(err) });
        }
    }, [state.selectedSchema, state.intent, state.slug]);

    const handleFieldChange = useCallback((path: string, value: unknown) => {
        dispatch({ type: "change-field", path, value });
    }, []);

    const handleRefine = useCallback(
        async (note: string) => {
            if (!state.draftContributionId || !state.selectedSchema) return;
            const trimmed = note.trim();
            if (trimmed.length === 0) return; // backend also enforces
            dispatch({ type: "refine-start" });
            try {
                // Backend expects current_yaml as a String per the Phase 9
                // IPC. Re-serialize the current value tree so refinements
                // reflect any inline edits the user made.
                const currentYaml = safeYamlStringify(state.values);
                const response = await invoke<RefineConfigResponse>(
                    "pyramid_refine_config",
                    {
                        contributionId: state.draftContributionId,
                        currentYaml,
                        note: trimmed,
                    },
                );
                dispatch({ type: "refine-success", response, note: trimmed });
            } catch (err) {
                dispatch({ type: "refine-error", error: String(err) });
            }
        },
        [state.draftContributionId, state.selectedSchema, state.values],
    );

    const handleAccept = useCallback(async () => {
        if (!state.selectedSchema) return;
        dispatch({ type: "accept-start" });
        try {
            // Phase 9 `pyramid_accept_config` takes an optional `yaml`
            // payload (inline values) OR promotes the latest draft for
            // the (schema_type, slug) pair. Passing values gives us the
            // inline path so any edits the user made land intact.
            const response = await invoke<AcceptConfigResponse>(
                "pyramid_accept_config",
                {
                    schemaType: state.selectedSchema.schema_type,
                    slug: state.slug,
                    yaml: state.values,
                    triggeringNote: state.triggeringNote,
                },
            );
            dispatch({ type: "accept-success", response });
        } catch (err) {
            dispatch({ type: "accept-error", error: String(err) });
        }
    }, [
        state.selectedSchema,
        state.slug,
        state.values,
        state.triggeringNote,
    ]);

    const handleReset = useCallback(() => {
        dispatch({ type: "reset" });
    }, []);

    // ── Render ──────────────────────────────────────────────────────────────

    return (
        <div
            style={{
                display: "flex",
                flexDirection: "column",
                gap: 16,
                padding: "8px 0",
            }}
        >
            {/* Step 1: Schema picker */}
            {state.step === "schema-picker" && (
                <>
                    <SectionHeader
                        title="Create a config"
                        subtitle="Pick a config type to generate. Intelligence turns your intent into a working YAML you can refine with notes."
                    />
                    {state.schemasLoading && (
                        <p
                            style={{
                                color: "var(--text-secondary)",
                                fontSize: 13,
                            }}
                        >
                            Loading schemas…
                        </p>
                    )}
                    {state.schemasError && (
                        <p style={{ color: "#fca5a5", fontSize: 13 }}>
                            {state.schemasError}
                        </p>
                    )}
                    {!state.schemasLoading && state.schemas.length === 0 && (
                        <p
                            style={{
                                color: "var(--text-secondary)",
                                fontSize: 13,
                            }}
                        >
                            No schemas registered. Bundled defaults should
                            seed on first run.
                        </p>
                    )}
                    <div
                        style={{
                            display: "grid",
                            gridTemplateColumns:
                                "repeat(auto-fill, minmax(260px, 1fr))",
                            gap: 12,
                        }}
                    >
                        {state.schemas.map((schema) => (
                            <button
                                key={schema.schema_type}
                                type="button"
                                onClick={() => handlePickSchema(schema)}
                                disabled={!schema.has_generation_skill}
                                style={{
                                    textAlign: "left",
                                    background:
                                        "var(--bg-secondary, #1a1a2e)",
                                    border:
                                        "1px solid var(--border-primary, #2a2a4a)",
                                    borderRadius: 8,
                                    padding: 16,
                                    cursor: schema.has_generation_skill
                                        ? "pointer"
                                        : "not-allowed",
                                    color: "var(--text-primary)",
                                    display: "flex",
                                    flexDirection: "column",
                                    gap: 8,
                                    opacity: schema.has_generation_skill
                                        ? 1
                                        : 0.5,
                                    transition: "border-color 0.15s ease",
                                }}
                                title={
                                    schema.has_generation_skill
                                        ? `Generate a ${schema.schema_type} config`
                                        : "No generation skill available for this schema"
                                }
                            >
                                <div
                                    style={{
                                        fontSize: 14,
                                        fontWeight: 600,
                                    }}
                                >
                                    {schema.display_name}
                                </div>
                                <div
                                    style={{
                                        fontSize: 10,
                                        fontFamily:
                                            "var(--font-mono, monospace)",
                                        color: "var(--text-secondary)",
                                        opacity: 0.7,
                                    }}
                                >
                                    {schema.schema_type}
                                </div>
                                <div
                                    style={{
                                        fontSize: 12,
                                        color: "var(--text-secondary)",
                                        lineHeight: 1.5,
                                    }}
                                >
                                    {schema.description}
                                </div>
                                {!schema.has_generation_skill && (
                                    <div
                                        style={{
                                            fontSize: 11,
                                            color: "#fbbf24",
                                            marginTop: 4,
                                        }}
                                    >
                                        No generation skill registered
                                    </div>
                                )}
                            </button>
                        ))}
                    </div>
                </>
            )}

            {/* Step 2: Intent entry */}
            {state.step === "intent" && state.selectedSchema && (
                <>
                    <SectionHeader
                        title={`Describe your ${state.selectedSchema.display_name}`}
                    />
                    <p
                        style={{
                            color: "var(--text-secondary)",
                            fontSize: 13,
                            lineHeight: 1.6,
                            margin: 0,
                        }}
                    >
                        {state.selectedSchema.description}
                    </p>
                    <textarea
                        value={state.intent}
                        onChange={(e) =>
                            dispatch({
                                type: "set-intent",
                                intent: e.target.value,
                            })
                        }
                        placeholder="Describe what you want. The more specific, the better. Example: 'Keep costs low, only maintain pyramids with active agent queries, run everything on local compute.'"
                        rows={5}
                        autoFocus
                        style={{
                            padding: "10px 12px",
                            background: "var(--bg-card)",
                            color: "var(--text-primary)",
                            border: "1px solid var(--glass-border)",
                            borderRadius: "var(--radius-sm)",
                            fontSize: 13,
                            lineHeight: 1.5,
                            resize: "vertical",
                            minHeight: 100,
                            fontFamily: "inherit",
                        }}
                    />
                    {state.error && (
                        <p style={{ color: "#fca5a5", fontSize: 12 }}>
                            {state.error}
                        </p>
                    )}
                    <div style={{ display: "flex", gap: 8 }}>
                        <button
                            type="button"
                            className="btn btn-primary"
                            onClick={handleGenerate}
                            disabled={state.intent.trim().length === 0}
                        >
                            Generate
                        </button>
                        <button
                            type="button"
                            className="btn btn-ghost"
                            onClick={handleBackToPicker}
                        >
                            Back
                        </button>
                    </div>
                </>
            )}

            {/* Step 3: Generating (loading state) */}
            {state.step === "generating" && (
                <div
                    style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: 8,
                        padding: "40px 20px",
                        alignItems: "center",
                    }}
                >
                    <div
                        style={{
                            fontSize: 14,
                            color: "var(--text-secondary)",
                        }}
                    >
                        Generating config…
                    </div>
                    <div
                        style={{
                            fontSize: 11,
                            color: "var(--text-secondary)",
                            opacity: 0.6,
                            textAlign: "center",
                            maxWidth: 420,
                        }}
                    >
                        Intelligence is turning your intent into a YAML
                        document. This can take 10-30 seconds.
                    </div>
                </div>
            )}

            {/* Step 4: Render + refine */}
            {state.step === "edit" && state.selectedSchema && (
                <>
                    <div
                        style={{
                            display: "flex",
                            gap: 8,
                            alignItems: "baseline",
                            flexWrap: "wrap",
                        }}
                    >
                        <SectionHeader
                            title={`Refine ${state.selectedSchema.display_name}`}
                        />
                        <span
                            style={{
                                marginLeft: "auto",
                                fontSize: 11,
                                color: "var(--text-secondary)",
                                fontFamily: "var(--font-mono, monospace)",
                            }}
                        >
                            draft v{state.version}
                        </span>
                    </div>
                    {state.error && (
                        <div
                            style={{
                                padding: "10px 12px",
                                background: "rgba(239, 68, 68, 0.08)",
                                border: "1px solid rgba(239, 68, 68, 0.25)",
                                borderRadius: 6,
                                color: "#fca5a5",
                                fontSize: 13,
                            }}
                        >
                            {state.error}
                        </div>
                    )}
                    {state.annotationLoading && (
                        <p
                            style={{
                                color: "var(--text-secondary)",
                                fontSize: 12,
                            }}
                        >
                            Loading schema annotation…
                        </p>
                    )}
                    {!state.annotationLoading && state.annotation && (
                        <YamlConfigRenderer
                            schema={state.annotation}
                            values={state.values}
                            onChange={handleFieldChange}
                            onAccept={handleAccept}
                            onNotes={handleRefine}
                            optionSources={rendererDeps.optionSources}
                            costEstimates={rendererDeps.costEstimates}
                            versionInfo={{
                                version: state.version,
                                totalVersions: state.version,
                                triggeringNote:
                                    state.triggeringNote ?? undefined,
                            }}
                        />
                    )}
                    {!state.annotationLoading && !state.annotation && (
                        <>
                            <p
                                style={{
                                    color: "var(--text-secondary)",
                                    fontSize: 12,
                                }}
                            >
                                No UI schema annotation registered for this
                                type. Raw YAML — accept as-is or refine with
                                a note.
                            </p>
                            <pre
                                style={{
                                    margin: 0,
                                    padding: "10px 12px",
                                    background: "rgba(0,0,0,0.35)",
                                    border: "1px solid rgba(255,255,255,0.08)",
                                    borderRadius: 6,
                                    maxHeight: 400,
                                    overflow: "auto",
                                    fontSize: 11,
                                    fontFamily:
                                        "var(--font-mono, monospace)",
                                    color: "var(--text-primary)",
                                    whiteSpace: "pre-wrap",
                                    wordBreak: "break-word",
                                }}
                            >
                                {state.rawYaml}
                            </pre>
                            <FallbackActions
                                onAccept={handleAccept}
                                onRefine={handleRefine}
                            />
                        </>
                    )}
                    <div style={{ display: "flex", gap: 8, marginTop: 4 }}>
                        <button
                            type="button"
                            className="btn btn-ghost btn-small"
                            onClick={handleBackToPicker}
                        >
                            Cancel & pick another
                        </button>
                    </div>
                </>
            )}

            {/* Refining in progress */}
            {state.step === "refining" && (
                <div
                    style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: 8,
                        padding: "40px 20px",
                        alignItems: "center",
                    }}
                >
                    <div
                        style={{
                            fontSize: 14,
                            color: "var(--text-secondary)",
                        }}
                    >
                        Applying notes…
                    </div>
                </div>
            )}

            {/* Step 5: Accepted */}
            {state.step === "accepted" && state.accepted && (
                <div
                    style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: 12,
                        padding: "20px 16px",
                        background: "rgba(16, 185, 129, 0.06)",
                        border: "1px solid rgba(16, 185, 129, 0.25)",
                        borderRadius: 8,
                    }}
                >
                    <div
                        style={{
                            color: "#10b981",
                            fontSize: 15,
                            fontWeight: 600,
                        }}
                    >
                        Config accepted · version {state.accepted.version}
                    </div>
                    <p
                        style={{
                            margin: 0,
                            fontSize: 12,
                            color: "var(--text-secondary)",
                            lineHeight: 1.5,
                        }}
                    >
                        This config is now active. Operational sync:{" "}
                        <code>
                            {state.accepted.sync_result.operational_table}
                        </code>
                        {state.accepted.sync_result.reload_triggered.length >
                            0 && (
                            <>
                                . Reloads triggered:{" "}
                                {state.accepted.sync_result.reload_triggered.join(
                                    ", ",
                                )}
                                .
                            </>
                        )}
                    </p>
                    <div style={{ display: "flex", gap: 8 }}>
                        <button
                            type="button"
                            className="btn btn-primary"
                            onClick={handleReset}
                        >
                            Create another
                        </button>
                    </div>
                </div>
            )}
        </div>
    );
}

/**
 * Manual Accept / Notes buttons shown when no schema annotation exists.
 * The YamlConfigRenderer normally owns these buttons, but in the
 * fallback path we need our own pair.
 */
function FallbackActions({
    onAccept,
    onRefine,
}: {
    onAccept: () => void;
    onRefine: (note: string) => void;
}) {
    const [notesOpen, setNotesOpen] = useState(false);
    const [note, setNote] = useState("");
    return (
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            <div style={{ display: "flex", gap: 8 }}>
                <button
                    type="button"
                    className="btn btn-primary"
                    onClick={onAccept}
                >
                    Accept
                </button>
                <button
                    type="button"
                    className="btn btn-secondary"
                    onClick={() => setNotesOpen((o) => !o)}
                >
                    {notesOpen ? "Cancel Notes" : "Notes"}
                </button>
            </div>
            {notesOpen && (
                <div
                    style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: 6,
                    }}
                >
                    <textarea
                        value={note}
                        onChange={(e) => setNote(e.target.value)}
                        placeholder="Describe what to refine…"
                        rows={3}
                        style={{
                            padding: "8px 10px",
                            background: "var(--bg-card)",
                            color: "var(--text-primary)",
                            border: "1px solid var(--glass-border)",
                            borderRadius: "var(--radius-sm)",
                            fontSize: 13,
                            lineHeight: 1.5,
                            resize: "vertical",
                            minHeight: 70,
                            fontFamily: "inherit",
                        }}
                    />
                    <button
                        type="button"
                        className="btn btn-primary btn-small"
                        disabled={note.trim().length === 0}
                        onClick={() => {
                            const trimmed = note.trim();
                            if (!trimmed) return;
                            onRefine(trimmed);
                            setNote("");
                            setNotesOpen(false);
                        }}
                    >
                        Submit Notes
                    </button>
                </div>
            )}
        </div>
    );
}

// ─── Discover tab ────────────────────────────────────────────────────────────
//
// Phase 14: Wire discovery + ranking + recommendations surface.
// Replaces the Phase 10 placeholder with a real search UI backed by
// `pyramid_wire_discover` + `pyramid_wire_recommendations` +
// `pyramid_pull_wire_config`. Renders results with the shared
// QualityBadges component and wires a detail drawer for preview +
// pull. Auto-update toggles live in a modal reachable from the tab
// header.

type DiscoverSortBy = "score" | "rating" | "adoption" | "fresh" | "chain_length";

function DiscoverPanel() {
    const [schemas, setSchemas] = useState<ConfigSchemaSummary[]>([]);
    const [schemaType, setSchemaType] = useState<string>("");
    const [query, setQuery] = useState("");
    const [tags, setTags] = useState("");
    const [sortBy, setSortBy] = useState<DiscoverSortBy>("score");
    const [results, setResults] = useState<DiscoveryResult[]>([]);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [recommendations, setRecommendations] = useState<Recommendation[]>([]);
    const [recSlug, setRecSlug] = useState<string>("");
    const [slugs, setSlugs] = useState<string[]>([]);
    const [selectedResult, setSelectedResult] = useState<DiscoveryResult | null>(
        null,
    );
    const [pullBusy, setPullBusy] = useState(false);
    const [pullMessage, setPullMessage] = useState<string | null>(null);
    const [showAutoUpdate, setShowAutoUpdate] = useState(false);

    // Phase 18a (L2): credential preview modal state. Populated by
    // the `pyramid_preview_pull_contribution` IPC before a pull is
    // committed. When `missing_credentials` is non-empty the user
    // sees a warning and can cancel, set credentials first, or
    // pull anyway.
    const [pendingPull, setPendingPull] = useState<{
        wire_contribution_id: string;
        activate: boolean;
        required_credentials: string[];
        missing_credentials: string[];
        title: string;
    } | null>(null);

    // ── Load schema list + slug list on mount.
    useEffect(() => {
        (async () => {
            try {
                const list = await invoke<ConfigSchemaSummary[]>(
                    "pyramid_config_schemas",
                );
                setSchemas(list);
                if (list.length > 0 && !schemaType) {
                    setSchemaType(list[0].schema_type);
                }
            } catch (err) {
                console.warn("Failed to load schemas:", err);
            }
            try {
                const s = await invoke<Array<{ slug: string }> | string[]>(
                    "pyramid_list_slugs",
                );
                const normalized = Array.isArray(s)
                    ? (s as unknown[]).map((entry) =>
                          typeof entry === "string"
                              ? entry
                              : typeof entry === "object" && entry !== null && "slug" in entry
                              ? String((entry as { slug: unknown }).slug)
                              : "",
                      ).filter(Boolean)
                    : [];
                setSlugs(normalized);
            } catch (err) {
                console.warn("Failed to load slugs:", err);
            }
        })();
    }, []); // eslint-disable-line react-hooks/exhaustive-deps

    // ── Fetch recommendations when slug + schema_type selected.
    useEffect(() => {
        if (!recSlug || !schemaType) {
            setRecommendations([]);
            return;
        }
        let cancelled = false;
        invoke<Recommendation[]>("pyramid_wire_recommendations", {
            slug: recSlug,
            schemaType,
            limit: 5,
        })
            .then((r) => {
                if (!cancelled) setRecommendations(r);
            })
            .catch((err) => {
                if (!cancelled) {
                    console.warn("Failed to fetch recommendations:", err);
                    setRecommendations([]);
                }
            });
        return () => {
            cancelled = true;
        };
    }, [recSlug, schemaType]);

    const runSearch = useCallback(async () => {
        if (!schemaType) return;
        setLoading(true);
        setError(null);
        try {
            const tagList = tags
                .split(",")
                .map((t) => t.trim())
                .filter(Boolean);
            const r = await invoke<DiscoveryResult[]>("pyramid_wire_discover", {
                schemaType,
                query: query.trim() ? query.trim() : null,
                tags: tagList.length > 0 ? tagList : null,
                limit: 20,
                sortBy,
            });
            setResults(r);
        } catch (err) {
            setError(String(err));
            setResults([]);
        } finally {
            setLoading(false);
        }
    }, [schemaType, query, tags, sortBy]);

    // Phase 18a (L2): inner pull that runs after the user has seen
    // any credential warnings. Separated from `handlePull` so the
    // preview modal can call it on the "Pull anyway" path without
    // re-running the preview round trip.
    const executePull = useCallback(
        async (wire_contribution_id: string, activate: boolean) => {
            if (pullBusy) return;
            setPullBusy(true);
            setPullMessage(null);
            try {
                const outcome = await invoke<PullLatestResponse>(
                    "pyramid_pull_wire_config",
                    {
                        wireContributionId: wire_contribution_id,
                        slug: recSlug || null,
                        activate,
                    },
                );
                setPullMessage(
                    outcome.activated
                        ? `Pulled & activated — new contribution_id ${outcome.new_local_contribution_id.slice(0, 8)}…`
                        : `Pulled as proposal — review in My Tools (id ${outcome.new_local_contribution_id.slice(0, 8)}…)`,
                );
                setSelectedResult(null);
            } catch (err) {
                const message = String(err);
                if (message.includes("credential")) {
                    setPullMessage(
                        `Pull refused — ${message}. Add the missing credentials in Settings → Credentials, then retry.`,
                    );
                } else {
                    setPullMessage(`Pull failed: ${message}`);
                }
            } finally {
                setPullBusy(false);
            }
        },
        [pullBusy, recSlug],
    );

    const handlePull = useCallback(
        async (wire_contribution_id: string, activate: boolean) => {
            if (pullBusy) return;
            // Phase 18a (L2): preview the contribution first to scan
            // for missing credential references. If anything is
            // missing, show a confirmation modal listing the keys
            // before committing to the actual pull.
            setPullMessage(null);
            try {
                const preview = await invoke<{
                    yaml: string;
                    schema_type: string | null;
                    title: string;
                    description: string;
                    required_credentials: string[];
                    missing_credentials: string[];
                }>("pyramid_preview_pull_contribution", {
                    wireContributionId: wire_contribution_id,
                });
                if (preview.missing_credentials.length === 0) {
                    // No missing credentials — pull immediately.
                    await executePull(wire_contribution_id, activate);
                } else {
                    // Stage the modal so the user can decide.
                    setPendingPull({
                        wire_contribution_id,
                        activate,
                        required_credentials: preview.required_credentials,
                        missing_credentials: preview.missing_credentials,
                        title: preview.title || preview.schema_type || "Wire contribution",
                    });
                }
            } catch (err) {
                // Preview failed (network or auth) — surface the
                // error without falling through to a blind pull.
                setPullMessage(`Preview failed: ${String(err)}`);
            }
        },
        [pullBusy, executePull],
    );

    return (
        <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
            {/* ── Header with auto-update button ─────────────────────────── */}
            <div
                style={{
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "flex-start",
                    gap: 12,
                }}
            >
                <SectionHeader
                    title="Discover on the Wire"
                    subtitle="Search Wire-published contributions, ranked by quality, adoption, and freshness. Pull matching configs into your local contribution store."
                />
                <button
                    type="button"
                    className="btn btn-ghost btn-small"
                    onClick={() => setShowAutoUpdate(true)}
                >
                    Auto-update settings
                </button>
            </div>

            {/* ── Search bar ────────────────────────────────────────────── */}
            <div
                style={{
                    display: "grid",
                    gridTemplateColumns: "160px 1fr 200px 120px auto",
                    gap: 8,
                    alignItems: "center",
                }}
            >
                <select
                    value={schemaType}
                    onChange={(e) => setSchemaType(e.target.value)}
                    className="settings-input"
                    style={{ padding: "6px 8px", fontSize: 12 }}
                >
                    {schemas.map((s) => (
                        <option key={s.schema_type} value={s.schema_type}>
                            {s.display_name}
                        </option>
                    ))}
                </select>
                <input
                    type="text"
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                    onKeyDown={(e) => {
                        if (e.key === "Enter") runSearch();
                    }}
                    placeholder="Search text (optional)"
                    className="settings-input"
                    style={{ padding: "6px 8px", fontSize: 12 }}
                />
                <input
                    type="text"
                    value={tags}
                    onChange={(e) => setTags(e.target.value)}
                    onKeyDown={(e) => {
                        if (e.key === "Enter") runSearch();
                    }}
                    placeholder="tags (comma-separated)"
                    className="settings-input"
                    style={{ padding: "6px 8px", fontSize: 12 }}
                />
                <select
                    value={sortBy}
                    onChange={(e) => setSortBy(e.target.value as DiscoverSortBy)}
                    className="settings-input"
                    style={{ padding: "6px 8px", fontSize: 12 }}
                >
                    <option value="score">Score</option>
                    <option value="rating">Rating</option>
                    <option value="adoption">Adoption</option>
                    <option value="fresh">Freshest</option>
                    <option value="chain_length">Chain length</option>
                </select>
                <button
                    type="button"
                    className="btn btn-primary btn-small"
                    onClick={runSearch}
                    disabled={loading || !schemaType}
                >
                    {loading ? "Searching…" : "Search"}
                </button>
            </div>

            {/* ── Pyramid selector for recommendations ──────────────────── */}
            {slugs.length > 0 && (
                <div
                    style={{
                        display: "flex",
                        alignItems: "center",
                        gap: 8,
                        fontSize: 12,
                        color: "var(--text-secondary)",
                    }}
                >
                    <span>Recommend for pyramid:</span>
                    <select
                        value={recSlug}
                        onChange={(e) => setRecSlug(e.target.value)}
                        className="settings-input"
                        style={{ padding: "4px 8px", fontSize: 12, maxWidth: 220 }}
                    >
                        <option value="">— none —</option>
                        {slugs.map((s) => (
                            <option key={s} value={s}>
                                {s}
                            </option>
                        ))}
                    </select>
                </div>
            )}

            {/* ── Pull outcome banner ───────────────────────────────────── */}
            {pullMessage && (
                <div
                    style={{
                        padding: "8px 12px",
                        borderRadius: 6,
                        background: "rgba(59, 130, 246, 0.1)",
                        border: "1px solid rgba(59, 130, 246, 0.2)",
                        fontSize: 12,
                        color: "var(--text-primary)",
                    }}
                >
                    {pullMessage}
                </div>
            )}

            {/* ── Phase 18a (L2): credential preview confirmation ────────── */}
            {pendingPull && (
                <div
                    role="dialog"
                    aria-modal="true"
                    style={{
                        position: "fixed",
                        inset: 0,
                        background: "rgba(0, 0, 0, 0.6)",
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "center",
                        zIndex: 1000,
                    }}
                    onClick={() => setPendingPull(null)}
                >
                    <div
                        onClick={(e) => e.stopPropagation()}
                        style={{
                            background: "var(--bg-elevated, #1a1a1a)",
                            border: "1px solid rgba(248, 113, 113, 0.4)",
                            borderRadius: 8,
                            padding: 20,
                            maxWidth: 540,
                            width: "calc(100% - 40px)",
                            display: "flex",
                            flexDirection: "column",
                            gap: 12,
                        }}
                    >
                        <div
                            style={{
                                fontSize: 14,
                                fontWeight: 600,
                                color: "#fca5a5",
                            }}
                        >
                            Missing credentials
                        </div>
                        <p style={{ fontSize: 12, lineHeight: 1.5, margin: 0 }}>
                            <strong>{pendingPull.title}</strong> requires credentials you
                            haven't set yet:
                        </p>
                        <ul
                            style={{
                                margin: 0,
                                padding: "0 0 0 18px",
                                fontSize: 12,
                                color: "#fdba74",
                            }}
                        >
                            {pendingPull.missing_credentials.map((key) => (
                                <li key={key}>
                                    <code>{key}</code>
                                </li>
                            ))}
                        </ul>
                        {pendingPull.required_credentials.length >
                            pendingPull.missing_credentials.length && (
                            <p
                                style={{
                                    margin: 0,
                                    fontSize: 11,
                                    color: "var(--text-secondary)",
                                }}
                            >
                                Other credentials this contribution references are already
                                set:{" "}
                                {pendingPull.required_credentials
                                    .filter(
                                        (k) =>
                                            !pendingPull.missing_credentials.includes(k),
                                    )
                                    .join(", ")}
                            </p>
                        )}
                        <p
                            style={{
                                margin: 0,
                                fontSize: 12,
                                lineHeight: 1.5,
                                color: "var(--text-secondary)",
                            }}
                        >
                            Set them in Settings → Credentials, or pull anyway. The
                            contribution will be inactive until the credentials exist.
                        </p>
                        <div
                            style={{
                                display: "flex",
                                gap: 8,
                                justifyContent: "flex-end",
                                marginTop: 4,
                            }}
                        >
                            <button
                                type="button"
                                className="compose-btn"
                                onClick={() => setPendingPull(null)}
                            >
                                Cancel
                            </button>
                            <button
                                type="button"
                                className="save-btn"
                                onClick={async () => {
                                    const pull = pendingPull;
                                    setPendingPull(null);
                                    await executePull(
                                        pull.wire_contribution_id,
                                        pull.activate,
                                    );
                                }}
                            >
                                Pull anyway
                            </button>
                        </div>
                    </div>
                </div>
            )}

            {/* ── Recommendations banner ────────────────────────────────── */}
            {recommendations.length > 0 && (
                <section
                    style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: 8,
                    }}
                >
                    <SectionHeader
                        title="Recommended for this pyramid"
                        subtitle={`Based on ${recSlug} — source type overlap + tier routing similarity.`}
                    />
                    {recommendations.map((r) => (
                        <RecommendationCard
                            key={r.wire_contribution_id}
                            rec={r}
                            onView={() =>
                                setSelectedResult({
                                    wire_contribution_id: r.wire_contribution_id,
                                    title: r.title,
                                    description: r.description,
                                    tags: [],
                                    author_handle: null,
                                    rating: null,
                                    adoption_count: 0,
                                    open_rebuttals: 0,
                                    chain_length: 0,
                                    freshness_days: 0,
                                    score: r.score,
                                    rationale: r.rationale,
                                    schema_type: schemaType,
                                })
                            }
                        />
                    ))}
                </section>
            )}

            {/* ── Main results list ─────────────────────────────────────── */}
            {error && (
                <p style={{ color: "#fca5a5", fontSize: 13 }}>{error}</p>
            )}
            {!loading && results.length === 0 && !error && (
                <p
                    style={{
                        color: "var(--text-secondary)",
                        fontSize: 12,
                        fontStyle: "italic",
                    }}
                >
                    No results. The Wire's discovery endpoint may not be live yet — try
                    different search terms, or check back once server-side search ships.
                </p>
            )}
            {results.map((r) => (
                <DiscoveryResultCard
                    key={r.wire_contribution_id}
                    result={r}
                    onView={() => setSelectedResult(r)}
                />
            ))}

            {/* ── Detail drawer ─────────────────────────────────────────── */}
            {selectedResult && (
                <DiscoveryDetailDrawer
                    result={selectedResult}
                    onClose={() => setSelectedResult(null)}
                    onPull={(activate) =>
                        handlePull(selectedResult.wire_contribution_id, activate)
                    }
                    pullBusy={pullBusy}
                />
            )}

            {/* ── Auto-update toggles modal ──────────────────────────────── */}
            {showAutoUpdate && (
                <AutoUpdateSettingsModal
                    schemas={schemas}
                    onClose={() => setShowAutoUpdate(false)}
                />
            )}
        </div>
    );
}

function DiscoveryResultCard({
    result,
    onView,
}: {
    result: DiscoveryResult;
    onView: () => void;
}) {
    return (
        <div
            style={{
                background: "var(--bg-secondary, #1a1a2e)",
                border: "1px solid var(--border-primary, #2a2a4a)",
                borderRadius: 8,
                padding: 14,
                display: "flex",
                flexDirection: "column",
                gap: 8,
            }}
        >
            <div
                style={{
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "baseline",
                    gap: 8,
                }}
            >
                <div
                    style={{
                        fontSize: 14,
                        fontWeight: 600,
                        color: "var(--text-primary)",
                    }}
                >
                    {result.title || result.wire_contribution_id.slice(0, 12)}
                </div>
                <div
                    style={{
                        fontSize: 11,
                        fontFamily: "var(--font-mono, monospace)",
                        color: "var(--text-secondary)",
                    }}
                >
                    score {(result.score * 100).toFixed(0)}
                </div>
            </div>
            {result.author_handle && (
                <div
                    style={{
                        fontSize: 11,
                        color: "var(--text-secondary)",
                    }}
                >
                    by {result.author_handle}
                </div>
            )}
            <QualityBadges
                rating={result.rating ?? undefined}
                adoptionCount={result.adoption_count}
                openRebuttals={result.open_rebuttals}
                chainLength={result.chain_length}
                freshnessDays={result.freshness_days}
            />
            {result.description && (
                <p
                    style={{
                        margin: 0,
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        lineHeight: 1.5,
                    }}
                >
                    {result.description}
                </p>
            )}
            {result.rationale && (
                <p
                    style={{
                        margin: 0,
                        fontSize: 11,
                        color: "var(--accent-purple, #a78bfa)",
                        fontStyle: "italic",
                    }}
                >
                    {result.rationale}
                </p>
            )}
            <div style={{ display: "flex", gap: 6 }}>
                <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={onView}
                >
                    View details
                </button>
            </div>
        </div>
    );
}

function RecommendationCard({
    rec,
    onView,
}: {
    rec: Recommendation;
    onView: () => void;
}) {
    return (
        <div
            style={{
                background: "rgba(167, 139, 250, 0.06)",
                border: "1px solid rgba(167, 139, 250, 0.18)",
                borderRadius: 8,
                padding: 12,
                display: "flex",
                flexDirection: "column",
                gap: 6,
            }}
        >
            <div
                style={{
                    fontSize: 13,
                    fontWeight: 600,
                    color: "var(--text-primary)",
                }}
            >
                {rec.title || rec.wire_contribution_id.slice(0, 12)}
            </div>
            {rec.description && (
                <p
                    style={{
                        margin: 0,
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        lineHeight: 1.5,
                    }}
                >
                    {rec.description}
                </p>
            )}
            <p
                style={{
                    margin: 0,
                    fontSize: 11,
                    color: "var(--accent-purple, #a78bfa)",
                    fontStyle: "italic",
                }}
            >
                {rec.rationale}
            </p>
            <div style={{ display: "flex", gap: 6 }}>
                <button
                    type="button"
                    className="btn btn-ghost btn-small"
                    onClick={onView}
                >
                    View details
                </button>
            </div>
        </div>
    );
}

function DiscoveryDetailDrawer({
    result,
    onClose,
    onPull,
    pullBusy,
}: {
    result: DiscoveryResult;
    onClose: () => void;
    onPull: (activate: boolean) => void;
    pullBusy: boolean;
}) {
    return (
        <div
            role="dialog"
            aria-label="Contribution details"
            style={{
                position: "fixed",
                top: 0,
                right: 0,
                bottom: 0,
                width: 480,
                background: "var(--bg-primary, #0b0b1a)",
                borderLeft: "1px solid var(--border-primary, #2a2a4a)",
                boxShadow: "-8px 0 32px rgba(0, 0, 0, 0.4)",
                display: "flex",
                flexDirection: "column",
                zIndex: 400,
            }}
        >
            <div
                style={{
                    padding: 16,
                    borderBottom: "1px solid var(--border-primary, #2a2a4a)",
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "center",
                }}
            >
                <h3 style={{ margin: 0, fontSize: 14, color: "var(--text-primary)" }}>
                    {result.title || "Untitled"}
                </h3>
                <button
                    type="button"
                    className="btn btn-ghost btn-small"
                    onClick={onClose}
                >
                    Close
                </button>
            </div>
            <div
                style={{
                    padding: 16,
                    overflowY: "auto",
                    display: "flex",
                    flexDirection: "column",
                    gap: 12,
                }}
            >
                <div style={{ fontSize: 11, color: "var(--text-secondary)" }}>
                    {result.author_handle && <>by {result.author_handle} · </>}
                    <span
                        style={{
                            fontFamily: "var(--font-mono, monospace)",
                        }}
                    >
                        {result.wire_contribution_id}
                    </span>
                </div>
                <QualityBadges
                    rating={result.rating ?? undefined}
                    adoptionCount={result.adoption_count}
                    openRebuttals={result.open_rebuttals}
                    chainLength={result.chain_length}
                    freshnessDays={result.freshness_days}
                />
                {result.description && (
                    <p
                        style={{
                            margin: 0,
                            fontSize: 13,
                            color: "var(--text-primary)",
                            lineHeight: 1.6,
                        }}
                    >
                        {result.description}
                    </p>
                )}
                {result.rationale && (
                    <div
                        style={{
                            padding: 10,
                            background: "rgba(167, 139, 250, 0.06)",
                            borderRadius: 6,
                            fontSize: 12,
                            color: "var(--accent-purple, #a78bfa)",
                        }}
                    >
                        {result.rationale}
                    </div>
                )}
                {result.tags.length > 0 && (
                    <div
                        style={{
                            display: "flex",
                            flexWrap: "wrap",
                            gap: 4,
                        }}
                    >
                        {result.tags.map((t) => (
                            <span
                                key={t}
                                style={{
                                    padding: "2px 6px",
                                    borderRadius: 4,
                                    fontSize: 10,
                                    background: "rgba(255, 255, 255, 0.05)",
                                    color: "var(--text-secondary)",
                                }}
                            >
                                {t}
                            </span>
                        ))}
                    </div>
                )}
                <div
                    style={{
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        lineHeight: 1.5,
                    }}
                >
                    Score: {(result.score * 100).toFixed(0)} / 100
                    {result.schema_type && <> · {result.schema_type}</>}
                </div>
            </div>
            <div
                style={{
                    padding: 16,
                    borderTop: "1px solid var(--border-primary, #2a2a4a)",
                    display: "flex",
                    gap: 8,
                }}
            >
                <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={() => onPull(false)}
                    disabled={pullBusy}
                    title="Pull into your contribution store as a proposal"
                >
                    {pullBusy ? "Pulling…" : "Pull as proposal"}
                </button>
                <button
                    type="button"
                    className="btn btn-primary btn-small"
                    onClick={() => onPull(true)}
                    disabled={pullBusy}
                    title="Pull and activate immediately"
                >
                    {pullBusy ? "Pulling…" : "Pull and activate"}
                </button>
            </div>
        </div>
    );
}

function AutoUpdateSettingsModal({
    schemas,
    onClose,
}: {
    schemas: ConfigSchemaSummary[];
    onClose: () => void;
}) {
    const [settings, setSettings] = useState<Record<string, boolean>>({});
    const [loading, setLoading] = useState(true);
    const [savingKey, setSavingKey] = useState<string | null>(null);

    useEffect(() => {
        let cancelled = false;
        invoke<AutoUpdateSettingEntry[]>("pyramid_wire_auto_update_status")
            .then((rows) => {
                if (cancelled) return;
                const map: Record<string, boolean> = {};
                for (const r of rows) map[r.schema_type] = r.enabled;
                setSettings(map);
            })
            .catch((err) => console.warn("auto_update_status failed:", err))
            .finally(() => {
                if (!cancelled) setLoading(false);
            });
        return () => {
            cancelled = true;
        };
    }, []);

    const handleToggle = useCallback(
        async (schemaType: string, enabled: boolean) => {
            setSavingKey(schemaType);
            try {
                await invoke("pyramid_wire_auto_update_toggle", {
                    schemaType,
                    enabled,
                });
                setSettings((prev) => ({ ...prev, [schemaType]: enabled }));
            } catch (err) {
                alert(`Failed to toggle auto-update: ${String(err)}`);
            } finally {
                setSavingKey(null);
            }
        },
        [],
    );

    return (
        <div
            role="dialog"
            aria-label="Auto-update settings"
            style={{
                position: "fixed",
                inset: 0,
                background: "rgba(0, 0, 0, 0.6)",
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                zIndex: 500,
            }}
            onClick={onClose}
        >
            <div
                onClick={(e) => e.stopPropagation()}
                style={{
                    background: "var(--bg-primary, #0b0b1a)",
                    border: "1px solid var(--border-primary, #2a2a4a)",
                    borderRadius: 12,
                    padding: 24,
                    maxWidth: 560,
                    width: "90%",
                    maxHeight: "80vh",
                    overflow: "auto",
                    display: "flex",
                    flexDirection: "column",
                    gap: 12,
                }}
            >
                <h3 style={{ margin: 0, color: "var(--text-primary)" }}>
                    Auto-update from Wire
                </h3>
                <div
                    style={{
                        padding: 10,
                        background: "rgba(245, 158, 11, 0.1)",
                        borderRadius: 6,
                        border: "1px solid rgba(245, 158, 11, 0.3)",
                        fontSize: 12,
                        color: "#fcd34d",
                        lineHeight: 1.5,
                    }}
                >
                    Auto-update pulls new versions without prompting. Contributions
                    that reference new credentials will always require manual review.
                </div>
                {loading && (
                    <p style={{ color: "var(--text-secondary)", fontSize: 13 }}>
                        Loading…
                    </p>
                )}
                {!loading && schemas.length === 0 && (
                    <p style={{ color: "var(--text-secondary)", fontSize: 13 }}>
                        No schema types registered yet.
                    </p>
                )}
                {schemas.map((s) => {
                    const enabled = settings[s.schema_type] ?? false;
                    return (
                        <label
                            key={s.schema_type}
                            style={{
                                display: "flex",
                                alignItems: "center",
                                gap: 10,
                                padding: "8px 10px",
                                background: "var(--bg-secondary, #1a1a2e)",
                                borderRadius: 6,
                                cursor: savingKey === s.schema_type ? "wait" : "pointer",
                                opacity: savingKey === s.schema_type ? 0.6 : 1,
                            }}
                        >
                            <input
                                type="checkbox"
                                checked={enabled}
                                disabled={savingKey === s.schema_type}
                                onChange={(e) =>
                                    handleToggle(s.schema_type, e.target.checked)
                                }
                            />
                            <div style={{ flex: 1 }}>
                                <div
                                    style={{
                                        fontSize: 13,
                                        fontWeight: 600,
                                        color: "var(--text-primary)",
                                    }}
                                >
                                    {s.display_name}
                                </div>
                                <div
                                    style={{
                                        fontSize: 11,
                                        color: "var(--text-secondary)",
                                    }}
                                >
                                    {s.schema_type}
                                </div>
                            </div>
                        </label>
                    );
                })}
                <div
                    style={{
                        display: "flex",
                        justifyContent: "flex-end",
                        marginTop: 8,
                    }}
                >
                    <button
                        type="button"
                        className="btn btn-primary btn-small"
                        onClick={onClose}
                    >
                        Done
                    </button>
                </div>
            </div>
        </div>
    );
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/**
 * Parse a YAML document body into a plain object. On failure returns
 * an empty object — the renderer handles that gracefully.
 */
function safeYamlParse(body: string): Record<string, unknown> {
    try {
        const parsed = yaml.load(body);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
            return parsed as Record<string, unknown>;
        }
        return {};
    } catch (err) {
        console.warn("[ToolsMode] YAML parse failed:", err);
        return {};
    }
}

/**
 * Serialize a values object to a YAML document. Uses js-yaml defaults
 * except we set `lineWidth: -1` to avoid arbitrary folding so the
 * backend substituter's token count matches what the frontend sees.
 */
function safeYamlStringify(values: Record<string, unknown>): string {
    try {
        return yaml.dump(values, { lineWidth: -1, noRefs: true });
    } catch (err) {
        console.warn("[ToolsMode] YAML stringify failed:", err);
        return "";
    }
}

/**
 * Immutably write a dotted path into a nested record. Intermediate
 * segments that don't exist are created as plain objects. Mirrors the
 * reader in YamlConfigRenderer.
 */
function writePath(
    root: Record<string, unknown>,
    path: string,
    value: unknown,
): Record<string, unknown> {
    const parts = path.split(".");
    if (parts.length === 0) return root;
    const next: Record<string, unknown> = { ...root };
    let cursor: Record<string, unknown> = next;
    for (let i = 0; i < parts.length - 1; i++) {
        const key = parts[i];
        const existing = cursor[key];
        const clone: Record<string, unknown> =
            existing && typeof existing === "object" && !Array.isArray(existing)
                ? { ...(existing as Record<string, unknown>) }
                : {};
        cursor[key] = clone;
        cursor = clone;
    }
    cursor[parts[parts.length - 1]] = value;
    return next;
}
