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
import { YamlConfigRenderer } from "../YamlConfigRenderer";
import { useYamlRendererSources } from "../../hooks/useYamlRendererSources";
import { PublishPreviewModal } from "../PublishPreviewModal";
import { ContributionDetailDrawer } from "../ContributionDetailDrawer";
import type { SchemaAnnotation } from "../../types/yamlRenderer";
import type {
    AcceptConfigResponse,
    ActiveConfigResponse,
    ConfigContribution,
    ConfigSchemaSummary,
    GenerateConfigResponse,
    RefineConfigResponse,
} from "../../types/configContributions";

type ToolsTab = "my-tools" | "discover" | "create";

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

    const openCreateFrom = useCallback((seed: CreateSeed) => {
        setCreateSeed(seed);
        setActiveTab("create");
    }, []);

    const clearSeed = useCallback(() => setCreateSeed(null), []);

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
                    <MyToolsPanel onEdit={openCreateFrom} />
                )}
                {activeTab === "discover" && <DiscoverPanel />}
                {activeTab === "create" && (
                    <CreatePanel seed={createSeed} onSeedConsumed={clearSeed} />
                )}
            </div>
        </div>
    );
}

// ─── My Tools ───────────────────────────────────────────────────────────────

interface MyToolsPanelProps {
    onEdit: (seed: CreateSeed) => void;
}

function MyToolsPanel({ onEdit }: MyToolsPanelProps) {
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
                    return (
                        <ConfigCard
                            key={schema.schema_type}
                            schema={schema}
                            active={active}
                            onView={() => handleOpenDetail(schema.schema_type)}
                            onHistory={() =>
                                handleOpenHistory(schema.schema_type)
                            }
                            onPublish={() => handlePublish(schema.schema_type)}
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
    onView,
    onHistory,
    onPublish,
}: {
    schema: ConfigSchemaSummary;
    active: ActiveConfigResponse | null | undefined;
    onView: () => void;
    onHistory: () => void;
    onPublish: () => void;
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
}

function CreatePanel({ seed, onSeedConsumed }: CreatePanelProps) {
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

function DiscoverPanel() {
    return (
        <div
            style={{
                padding: "24px 16px",
                display: "flex",
                flexDirection: "column",
                gap: 12,
                background: "rgba(139, 92, 246, 0.04)",
                border: "1px solid rgba(139, 92, 246, 0.18)",
                borderRadius: 8,
            }}
        >
            <div
                style={{
                    fontSize: 14,
                    fontWeight: 600,
                    color: "var(--text-primary)",
                }}
            >
                Wire discovery — coming in Phase 14
            </div>
            <p
                style={{
                    margin: 0,
                    fontSize: 13,
                    color: "var(--text-secondary)",
                    lineHeight: 1.6,
                }}
            >
                Browse and pull config contributions from the Wire. This tab
                will search the marketplace by schema type and tags, preview
                configs via the same YAML renderer, and pull them into your
                local contribution store as pending proposals for your
                review.
            </p>
            <p
                style={{
                    margin: 0,
                    fontSize: 12,
                    color: "var(--text-secondary)",
                    opacity: 0.75,
                    lineHeight: 1.6,
                }}
            >
                Phase 14 ships the ranking, recommendations, supersession
                notifications, and quality badges. The underlying IPC
                (<code>pyramid_search_wire_configs</code> and{" "}
                <code>pyramid_pull_wire_config</code>) is not yet registered
                in the node — no interactive surface to wire against yet.
            </p>
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
