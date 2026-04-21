import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useLocalMode, type OllamaProbeResult, type OllamaModelInfo, type ConfigHistoryEntry, type ExperimentalTerritory } from "../hooks/useLocalMode";
import { AccordionSection } from "./AccordionSection";
import { InferenceRoutingPanel } from "./settings/InferenceRoutingPanel";
import type { TaggedBuildEvent } from "../hooks/useBuildRowState";
import { invokeOrNull } from "../utils/invokeSafe";

// --- Types ------------------------------------------------------------------

interface WireNodeConfig {
    api_url: string;
    api_token: string;
    node_id: string;
    storage_cap_gb: number;
    mesh_hosting_enabled: boolean;
    auto_update_enabled: boolean;
    document_cache_dir: string;
    server_port: number;
    jwt_public_key: string;
    supabase_url: string;
    supabase_anon_key: string;
    tunnel_api_url: string;
}

interface HealthStatus {
    overall: string;
    checks: { name: string; status: string; message: string }[];
}

interface UpdateInfo {
    available: boolean;
    version?: string;
    body?: string;
}

type ComputeParticipationMode = "coordinator" | "hybrid" | "worker";

// Mirrors the Rust `ComputeParticipationPolicy` at src-tauri/src/pyramid/
// local_mode.rs. The 8 projectable booleans may be absent/undefined on
// the wire — the Rust side uses `#[serde(skip_serializing_if = "Option::is_none")]`
// so None serializes as "key omitted," which deserializes to `undefined`
// in JS (NOT `null`). The Rust side projects absent fields from `mode`
// at read time per DD-I. The UI always sends explicit booleans so
// there's no ambiguity when the operator has tuned a mode button; see
// `policyForMode` below.
interface ComputeParticipationPolicy {
    schema_type: "compute_participation_policy";
    mode: ComputeParticipationMode;
    allow_fleet_dispatch?: boolean;
    allow_fleet_serving?: boolean;
    allow_market_dispatch?: boolean;
    allow_market_visibility?: boolean;
    allow_storage_pulling?: boolean;
    allow_storage_hosting?: boolean;
    allow_relay_usage?: boolean;
    allow_relay_serving?: boolean;
    allow_serving_while_degraded: boolean;
    // Market-dispatch wall-clock. Not editable from Settings today —
    // the Inference Routing panel (separate plan) surfaces it readonly.
    // Declared here so round-trip through `policyForMode(...)` preserves
    // any value set by backend defaults or CLI edits.
    //
    // The two retired walker knobs (market_dispatch_threshold_queue_depth,
    // market_dispatch_eager) were removed in walker-re-plan-wire-2.1 Wave 5.
    // Legacy payloads that still carry them are silently absorbed by the
    // Rust deserializer (deny_unknown_fields removed for this struct).
    market_dispatch_max_wait_ms?: number;
}

// Conservative default matching the Rust `Default` impl + bundled
// default YAML: hybrid mode, fleet on, every market off until the
// operator opts in. `market_dispatch_max_wait_ms` mirrors Rust's
// `default_market_dispatch_max_wait_ms()` = 60_000.
const defaultComputeParticipationPolicy: ComputeParticipationPolicy = {
    schema_type: "compute_participation_policy",
    mode: "hybrid",
    allow_fleet_dispatch: true,
    allow_fleet_serving: true,
    allow_market_dispatch: false,
    allow_market_visibility: false,
    allow_storage_pulling: false,
    allow_storage_hosting: false,
    allow_relay_usage: false,
    allow_relay_serving: false,
    allow_serving_while_degraded: false,
    market_dispatch_max_wait_ms: 60_000,
};

const roleDescriptions: Record<ComputeParticipationMode, { label: string; description: string }> = {
    coordinator: {
        label: "Coordinator",
        description: "Dispatches work out (to fleet peers, network compute, storage, relay) but does not serve any inbound requests.",
    },
    hybrid: {
        label: "Hybrid",
        description: "Full participation — dispatches out AND serves across fleet, compute, storage, and relay networks.",
    },
    worker: {
        label: "Worker",
        description: "Serves inbound requests (fleet, compute, storage hosting, relay forwarding) but does not dispatch outward.",
    },
};

// Apply the DD-I mode projection across all 8 projectable booleans.
// Operator clicking a mode button wants "the canonical preset" — this
// sets all 8 explicitly, so the saved contribution has no projected
// fields and the Rust read path has no ambiguity. The operator can
// then un-tune any specific field from Settings detail if we add that
// UI later.
function policyForMode(
    mode: ComputeParticipationMode,
    prior: ComputeParticipationPolicy,
): ComputeParticipationPolicy {
    switch (mode) {
        case "coordinator":
            return {
                ...prior,
                mode,
                allow_fleet_dispatch: true,
                allow_fleet_serving: false,
                allow_market_dispatch: true,
                allow_market_visibility: false,
                allow_storage_pulling: true,
                allow_storage_hosting: false,
                allow_relay_usage: true,
                allow_relay_serving: false,
            };
        case "hybrid":
            return {
                ...prior,
                mode,
                allow_fleet_dispatch: true,
                allow_fleet_serving: true,
                allow_market_dispatch: true,
                allow_market_visibility: true,
                allow_storage_pulling: true,
                allow_storage_hosting: true,
                allow_relay_usage: true,
                allow_relay_serving: true,
            };
        case "worker":
            return {
                ...prior,
                mode,
                allow_fleet_dispatch: false,
                allow_fleet_serving: true,
                allow_market_dispatch: false,
                allow_market_visibility: true,
                allow_storage_pulling: false,
                allow_storage_hosting: true,
                allow_relay_usage: false,
                allow_relay_serving: true,
            };
    }
}

// --- Component --------------------------------------------------------------

export function Settings() {
    const [config, setConfig] = useState<WireNodeConfig | null>(null);
    const [storageCap, setStorageCap] = useState(10);
    const [meshHosting, setMeshHosting] = useState(false);
    const [health, setHealth] = useState<HealthStatus | null>(null);
    const [saving, setSaving] = useState(false);
    const [saved, setSaved] = useState(false);
    const [autoUpdate, setAutoUpdate] = useState(false);
    const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
    const [checking, setChecking] = useState(false);
    const [installing, setInstalling] = useState(false);
    const [nodeName, setNodeName] = useState("Wire Node");
    const [computePolicy, setComputePolicy] = useState<ComputeParticipationPolicy>(defaultComputeParticipationPolicy);
    const [computePolicyLoaded, setComputePolicyLoaded] = useState(false);
    const [computePolicyUnavailable, setComputePolicyUnavailable] = useState(false);
    const [computePolicySaving, setComputePolicySaving] = useState(false);
    const [computePolicySaved, setComputePolicySaved] = useState(false);

    // --- Phase 18a (L1): Local Mode toggle state -------------------------
    //
    // The hook owns the IPC round trips; this component owns the
    // user-editable form state (URL, model picker, probe results) and
    // the disable confirmation guard. The hook's `status` is the
    // source of truth for the toggle state — when the toggle is on,
    // URL/model fields are read-only and reflect the saved values.
    const localMode = useLocalMode();
    const [localUrl, setLocalUrl] = useState("http://localhost:11434/v1");
    const [localModelChoice, setLocalModelChoice] = useState<string>("");
    const [probeResult, setProbeResult] = useState<OllamaProbeResult | null>(null);
    const [probeBusy, setProbeBusy] = useState(false);
    const [confirmingDisable, setConfirmingDisable] = useState(false);
    const [detailsCache, setDetailsCache] = useState<Record<string, OllamaModelInfo>>({});
    const [detailsLoading, setDetailsLoading] = useState<Record<string, boolean>>({});

    // Phase 3: Context and concurrency override form state
    const [contextInput, setContextInput] = useState<string>("");
    const [concurrencyInput, setConcurrencyInput] = useState<number>(1);

    // Phase 4: Pull model state
    const [pullModelInput, setPullModelInput] = useState("");
    const [pulling, setPulling] = useState(false);
    const [pullProgress, setPullProgress] = useState<{
        model: string;
        status: string;
        completedBytes: number | null;
        totalBytes: number | null;
    } | null>(null);
    // Phase 4: Delete model confirmation state — holds the model name
    // being confirmed for deletion, or null when no confirmation is active.
    const [deletingModel, setDeletingModel] = useState<string | null>(null);

    // Phase 5: Config history state
    const [configHistory, setConfigHistory] = useState<ConfigHistoryEntry[]>([]);
    const [historyLoaded, setHistoryLoaded] = useState(false);
    const [confirmingRollback, setConfirmingRollback] = useState<string | null>(null);

    // Phase 6: Experimental territory state
    const [territory, setTerritory] = useState<ExperimentalTerritory | null>(null);
    const [territoryLoaded, setTerritoryLoaded] = useState(false);
    const [territorySaving, setTerritorySaving] = useState(false);
    const [territorySaved, setTerritorySaved] = useState(false);

    // Sync local form state with the hook's status whenever it
    // refreshes — so the URL and dropdown reflect the persisted
    // ollama_base_url / ollama_model from the state row.
    useEffect(() => {
        if (localMode.status?.base_url) {
            setLocalUrl(localMode.status.base_url);
        }
        if (localMode.status?.model) {
            setLocalModelChoice(localMode.status.model);
        }
    }, [localMode.status]);

    // Reset history + territory loaded flags when status changes (model switch,
    // enable/disable) so the next accordion open fetches fresh data.
    useEffect(() => {
        setHistoryLoaded(false);
        setTerritoryLoaded(false);
    }, [localMode.status?.model, localMode.status?.enabled]);

    // Sync context/concurrency override inputs from status
    useEffect(() => {
        if (localMode.status?.context_override != null) {
            setContextInput(String(localMode.status.context_override));
        } else {
            setContextInput("");
        }
        setConcurrencyInput(localMode.status?.concurrency_override ?? 1);
    }, [localMode.status?.context_override, localMode.status?.concurrency_override]);

    // Dismiss the disable confirmation dialog whenever the enabled
    // state actually changes (e.g. the disable IPC succeeded).
    useEffect(() => {
        setConfirmingDisable(false);
    }, [localMode.status?.enabled]);

    const handleProbe = useCallback(async () => {
        setProbeBusy(true);
        setProbeResult(null);
        try {
            const result = await localMode.probe(localUrl);
            setProbeResult(result);
            // If the probe found models and the user hasn't picked
            // one yet, pre-select the first.
            if (
                result.reachable &&
                result.available_models.length > 0 &&
                !localModelChoice
            ) {
                setLocalModelChoice(result.available_models[0]);
            }
        } catch (err) {
            setProbeResult({
                reachable: false,
                reachability_error: String(err),
                available_models: [],
                available_model_details: [],
            });
        } finally {
            setProbeBusy(false);
        }
    }, [localMode, localUrl, localModelChoice]);

    // Auto-probe on mount: fires once when status has loaded,
    // local mode is off, and a base_url was previously configured.
    useEffect(() => {
        if (
            localMode.status &&
            !localMode.status.enabled &&
            localMode.status.base_url &&
            !probeResult
        ) {
            handleProbe();
        }
    }, [localMode.status]); // eslint-disable-line react-hooks/exhaustive-deps

    // Phase 4: Subscribe to Ollama pull progress events from the
    // BuildEventBus. The backend emits TaggedBuildEvent with slug
    // "__ollama__" and kind.type "ollama_pull" for pull progress.
    useEffect(() => {
        let unlisten: UnlistenFn | null = null;
        let active = true;

        (async () => {
            try {
                unlisten = await listen<TaggedBuildEvent>("cross-build-event", (ev) => {
                    if (!active) return;
                    const payload = ev.payload;
                    if (!payload || payload.slug !== "__ollama__") return;
                    const kind = payload.kind;
                    if (kind.type !== "ollama_pull") return;

                    // Extract pull progress fields from the event.
                    const model = (kind as Record<string, unknown>).model as string;
                    const status = (kind as Record<string, unknown>).status as string;
                    const completedBytes = (kind as Record<string, unknown>).completed_bytes as number | null;
                    const totalBytes = (kind as Record<string, unknown>).total_bytes as number | null;

                    if (status === "success") {
                        // Pull completed — clear progress and refresh model list.
                        setPullProgress(null);
                        setPulling(false);
                        // Trigger a probe refresh to pick up the new model.
                        handleProbe();
                    } else {
                        setPullProgress({ model, status, completedBytes, totalBytes });
                    }
                });
            } catch (e) {
                console.warn("Settings: listen for pull events failed", e);
            }
        })();

        return () => {
            active = false;
            if (unlisten) unlisten();
        };
    }, [handleProbe]);

    const handleEnableLocalMode = useCallback(async () => {
        // Need a model selection — fall back to the probe's first
        // result if the dropdown is empty.
        let model: string | null = localModelChoice || null;
        if (!model && probeResult && probeResult.available_models.length > 0) {
            model = probeResult.available_models[0];
        }
        await localMode.enable(localUrl, model);
    }, [localMode, localUrl, localModelChoice, probeResult]);

    const handleDisableLocalMode = useCallback(async () => {
        if (!confirmingDisable) {
            // First click arms the confirmation; second click commits.
            setConfirmingDisable(true);
            return;
        }
        setConfirmingDisable(false);
        await localMode.disable();
    }, [localMode, confirmingDisable]);

    // Phase 4: Pull model handler
    const handlePullModel = useCallback(async () => {
        const model = pullModelInput.trim();
        if (!model) return;
        setPulling(true);
        setPullProgress({ model, status: "starting pull...", completedBytes: null, totalBytes: null });
        try {
            await localMode.pullModel(model);
            // pullModel calls refresh() internally on success.
            // The event listener handles clearing progress on "success" event,
            // but if the IPC returns before we get the final event (unlikely),
            // clean up here too.
            setPullProgress(null);
            setPulling(false);
            setPullModelInput("");
            // Re-probe to pick up new model in the list.
            handleProbe();
        } catch {
            // Error is surfaced via localMode.error
            setPullProgress(null);
            setPulling(false);
        }
    }, [pullModelInput, localMode, handleProbe]);

    const handleCancelPull = useCallback(async () => {
        await localMode.cancelPull();
        setPullProgress(null);
        setPulling(false);
    }, [localMode]);

    // Phase 4: Delete model handler
    const handleDeleteModel = useCallback(async (model: string) => {
        setDeletingModel(null);
        try {
            await localMode.deleteModel(model);
            // deleteModel calls refresh() internally. Re-probe to update card list.
            handleProbe();
        } catch {
            // Error is surfaced via localMode.error
        }
    }, [localMode, handleProbe]);

    // Phase 5: Fetch config history on demand (when accordion opens).
    const fetchConfigHistory = useCallback(async () => {
        try {
            const history = await localMode.getConfigHistory("tier_routing", 20);
            setConfigHistory(history);
            setHistoryLoaded(true);
        } catch {
            // Silently fail — history is secondary info
            setHistoryLoaded(true);
        }
    }, [localMode]);

    // Phase 5: Rollback handler — confirms, then rolls back and refreshes the list.
    const handleRollback = useCallback(async (contributionId: string) => {
        setConfirmingRollback(null);
        await localMode.rollbackConfig(contributionId);
        // Refresh history list after rollback creates a new entry.
        await fetchConfigHistory();
    }, [localMode, fetchConfigHistory]);

    // Phase 6: Dimension metadata for the territory UI.
    const TERRITORY_DIMENSIONS: { key: string; label: string; description: string }[] = [
        { key: "model_selection", label: "Model Selection", description: "Which Ollama model is used for builds" },
        { key: "context_limit", label: "Context Limit", description: "Token context window size for the model" },
        { key: "concurrency", label: "Concurrency", description: "Number of parallel build workers" },
        // Future dimensions (compute market):
        // { key: "pricing", label: "Pricing", description: "Cost constraints for inference" },
        // { key: "job_acceptance", label: "Job Acceptance", description: "Which jobs to accept from the mesh" },
        // { key: "scheduling", label: "Scheduling", description: "When builds are allowed to run" },
    ];

    // Phase 6: Fetch territory on demand (when accordion opens).
    const fetchTerritory = useCallback(async () => {
        try {
            const t = await localMode.getExperimentalTerritory();
            setTerritory(t);
        } catch {
            // If no territory exists yet, initialize with all dimensions locked.
            const defaultDimensions: ExperimentalTerritory["dimensions"] = {};
            for (const dim of TERRITORY_DIMENSIONS) {
                defaultDimensions[dim.key] = { status: "locked", bounds: null };
            }
            setTerritory({ schema_type: "experimental_territory", dimensions: defaultDimensions });
        }
        setTerritoryLoaded(true);
    }, [localMode]);

    // Phase 6: Update a single dimension's status in the local territory state.
    const setDimensionStatus = useCallback((dimKey: string, status: "locked" | "experimental" | "experimental_within_bounds") => {
        setTerritory((prev) => {
            if (!prev) return prev;
            const dim = prev.dimensions[dimKey] || { status: "locked", bounds: null };
            return {
                ...prev,
                dimensions: {
                    ...prev.dimensions,
                    [dimKey]: {
                        ...dim,
                        status,
                        bounds: status === "experimental_within_bounds" ? (dim.bounds || { min: undefined, max: undefined }) : null,
                    },
                },
            };
        });
        setTerritorySaved(false);
    }, []);

    // Phase 6: Update a dimension's bounds in the local territory state.
    const setDimensionBounds = useCallback((dimKey: string, field: "min" | "max", value: string) => {
        setTerritory((prev) => {
            if (!prev) return prev;
            const dim = prev.dimensions[dimKey];
            if (!dim) return prev;
            const parsed = value === "" ? undefined : parseInt(value, 10);
            return {
                ...prev,
                dimensions: {
                    ...prev.dimensions,
                    [dimKey]: {
                        ...dim,
                        bounds: {
                            ...(dim.bounds || {}),
                            [field]: isNaN(parsed as number) ? undefined : parsed,
                        },
                    },
                },
            };
        });
        setTerritorySaved(false);
    }, []);

    // Phase 6: Save territory to backend.
    const handleSaveTerritory = useCallback(async () => {
        if (!territory) return;
        setTerritorySaving(true);
        try {
            await localMode.setExperimentalTerritory(territory);
            setTerritorySaved(true);
            setTimeout(() => setTerritorySaved(false), 2000);
        } catch {
            // Error is surfaced via localMode.error
        } finally {
            setTerritorySaving(false);
        }
    }, [territory, localMode]);

    // Phase 5: Format a created_at timestamp as relative time or short date.
    const formatHistoryTime = (iso: string): string => {
        try {
            // SQLite datetime('now') produces "YYYY-MM-DD HH:MM:SS" (space, no T, no Z).
            // Normalize to ISO 8601 for reliable cross-engine parsing.
            const normalized = iso.includes("T") ? iso : iso.replace(" ", "T") + "Z";
            const date = new Date(normalized);
            const now = new Date();
            const diffMs = now.getTime() - date.getTime();
            const diffSec = Math.floor(diffMs / 1000);
            const diffMin = Math.floor(diffSec / 60);
            const diffHr = Math.floor(diffMin / 60);
            const diffDay = Math.floor(diffHr / 24);

            if (diffSec < 60) return "just now";
            if (diffMin < 60) return `${diffMin}m ago`;
            if (diffHr < 24) return `${diffHr}h ago`;
            if (diffDay < 7) return `${diffDay}d ago`;

            // Fall back to short date
            return date.toLocaleDateString(undefined, {
                month: "short",
                day: "numeric",
                hour: "2-digit",
                minute: "2-digit",
            });
        } catch {
            return iso;
        }
    };

    // Format bytes as human-readable (MB/GB).
    const formatBytes = (bytes: number): string => {
        if (bytes >= 1_000_000_000) {
            return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
        }
        return `${(bytes / 1_048_576).toFixed(0)} MB`;
    };

    // Lazy-load context window for a model via /api/show when not
    // already known. Caches results in component state.
    const loadModelDetails = useCallback(async (modelName: string) => {
        if (detailsCache[modelName] || detailsLoading[modelName]) return;
        const baseUrl = localMode.status?.base_url || localUrl;
        setDetailsLoading((prev) => ({ ...prev, [modelName]: true }));
        try {
            const details = await localMode.getModelDetails(baseUrl, modelName);
            setDetailsCache((prev) => ({ ...prev, [modelName]: details }));
        } catch {
            // Silently fail — card still shows "..." for context
        } finally {
            setDetailsLoading((prev) => ({ ...prev, [modelName]: false }));
        }
    }, [detailsCache, detailsLoading, localMode, localUrl]);

    // The list of models: prefer available_model_details from status/probe
    // when present; fall back to constructing minimal OllamaModelInfo from
    // the string list. Merge in any cached details from lazy loading.
    const availableModelDetails: OllamaModelInfo[] = (() => {
        let details: OllamaModelInfo[] = [];

        // Prefer rich details from status or probe
        if (localMode.status?.enabled && localMode.status.available_model_details?.length > 0) {
            details = localMode.status.available_model_details;
        } else if (probeResult && probeResult.available_model_details?.length > 0) {
            details = probeResult.available_model_details;
        } else {
            // Fallback: construct minimal objects from string list
            const names: string[] =
                (localMode.status?.enabled && localMode.status.available_models.length > 0)
                    ? localMode.status.available_models
                    : (probeResult && probeResult.available_models.length > 0)
                        ? probeResult.available_models
                        : [];
            details = names.map((name) => ({
                name,
                size_bytes: 0,
                family: null,
                families: null,
                parameter_size: null,
                quantization_level: null,
                context_window: null,
                architecture: null,
                modified_at: null,
            }));
        }

        // Merge in any lazily-loaded details (context_window, architecture)
        return details.map((m) => {
            const cached = detailsCache[m.name];
            if (!cached) return m;
            return {
                ...m,
                context_window: cached.context_window ?? m.context_window,
                architecture: cached.architecture ?? m.architecture,
                parameter_size: cached.parameter_size ?? m.parameter_size,
                quantization_level: cached.quantization_level ?? m.quantization_level,
                size_bytes: cached.size_bytes || m.size_bytes,
                family: cached.family ?? m.family,
                families: cached.families ?? m.families,
            };
        });
    })();

    // Keep a flat string list for backward compat with enable logic
    const availableModels: string[] = availableModelDetails.map((m) => m.name);

    const fetchData = useCallback(async () => {
        // Each invoke runs independently; a single failure must not hide the
        // siblings. invokeOrNull resolves to null on failure so the Promise.all
        // cannot reject. Each setter is guarded on a non-null value.
        const [cfg, healthStatus, name, policy] = await Promise.all([
            invokeOrNull<WireNodeConfig>("get_config"),
            invokeOrNull<HealthStatus>("get_health_status"),
            invokeOrNull<string>("get_node_name"),
            invokeOrNull<ComputeParticipationPolicy>("pyramid_get_compute_participation_policy"),
        ]);
        if (cfg) {
            setConfig(cfg);
            setStorageCap(cfg.storage_cap_gb);
            setMeshHosting(cfg.mesh_hosting_enabled);
            setAutoUpdate(cfg.auto_update_enabled);
        }
        if (healthStatus) setHealth(healthStatus);
        if (name) setNodeName(name || "Wire Node");
        if (policy) {
            setComputePolicy(policy);
            setComputePolicyLoaded(true);
            setComputePolicyUnavailable(false);
        } else {
            setComputePolicyUnavailable(true);
        }
    }, []);

    useEffect(() => { fetchData(); }, [fetchData]);

    const handleSave = async () => {
        // Save is always valid even if the initial get_config fetch failed —
        // save_onboarding takes only local state (nodeName, storageCap,
        // meshHosting, autoUpdate), and the backend writes to disk AND
        // updates its own in-memory config. No dependency on the frontend's
        // `config` object.
        setSaving(true);
        try {
            await invoke("save_onboarding", {
                nodeName: nodeName,
                storageCapGb: storageCap,
                meshHostingEnabled: meshHosting,
                autoUpdateEnabled: autoUpdate,
            });
            setSaved(true);
            setTimeout(() => setSaved(false), 2000);
        } catch (err) {
            console.error("Save failed:", err);
        } finally {
            setSaving(false);
        }
    };

    const handleCheckUpdate = async () => {
        setChecking(true);
        try {
            const info = await invoke<UpdateInfo>("check_for_update");
            setUpdateInfo(info);
        } catch (err) {
            console.error("Update check failed:", err);
        } finally {
            setChecking(false);
        }
    };

    const handleInstallUpdate = async () => {
        setInstalling(true);
        try {
            await invoke("install_update");
        } catch (err) {
            console.error("Update install failed:", err);
            setInstalling(false);
        }
    };

    const handleSelectComputeMode = useCallback(async (mode: ComputeParticipationMode) => {
        const next = policyForMode(mode, computePolicy);
        setComputePolicySaving(true);
        try {
            await invoke("pyramid_set_compute_participation_policy", { policy: next });
            setComputePolicy(next);
            setComputePolicySaved(true);
            setTimeout(() => setComputePolicySaved(false), 2000);
        } catch (err) {
            console.error("Compute participation policy save failed:", err);
        } finally {
            setComputePolicySaving(false);
        }
    }, [computePolicy]);

    const statusIcon: Record<string, string> = {
        ok: "[OK]",
        warning: "[!!]",
        error: "[XX]",
    };

    return (
        <div className="settings-panel">
            {/* Health Status */}
            {health && (
                <div className={`health-panel health-${health.overall}`}>
                    <div className="health-header">
                        <span className="health-indicator">
                            {health.overall === "healthy" ? "[OK]" : health.overall === "warning" ? "[!!]" : "[XX]"}
                        </span>
                        <span className="health-label">
                            {health.overall === "healthy" ? "All systems nominal" : health.overall === "warning" ? "Attention needed" : "Issues detected"}
                        </span>
                    </div>
                    <div className="health-checks">
                        {health.checks.map((check) => (
                            <div key={check.name} className={`health-check health-check-${check.status}`}>
                                <span>{statusIcon[check.status] || "?"}</span>
                                <span className="health-check-name">{check.name}</span>
                                <span className="health-check-msg">{check.message}</span>
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Node Info */}
            {config && (
                <div className="settings-section">
                    <div className="settings-section-header">Node Information</div>
                    <div className="node-info-grid">
                        <div className="node-info-item">
                            <span className="node-info-label">Node ID</span>
                            <span className="node-info-value">{config.node_id || "Not registered"}</span>
                        </div>
                        <div className="node-info-item">
                            <span className="node-info-label">Server Port</span>
                            <span className="node-info-value">{config.server_port}</span>
                        </div>
                        <div className="node-info-item">
                            <span className="node-info-label">Cache Directory</span>
                            <span className="node-info-value node-info-path" title={config.document_cache_dir}>
                                {config.document_cache_dir.length > 40
                                    ? "..." + config.document_cache_dir.slice(-37)
                                    : config.document_cache_dir}
                            </span>
                        </div>
                    </div>
                </div>
            )}

            {/* Storage Cap */}
            <div className="settings-section">
                <div className="settings-section-header">Storage Cap</div>
                <p className="settings-section-desc">
                    Maximum disk space this node will use for caching and hosting documents.
                </p>
                <div className="storage-slider-row">
                    <input
                        type="range"
                        min={1}
                        max={100}
                        value={storageCap}
                        onChange={(e) => setStorageCap(parseInt(e.target.value))}
                        className="storage-slider"
                    />
                    <span className="storage-value">{storageCap} GB</span>
                </div>
                <div className="storage-presets">
                    {[1, 5, 10, 25, 50, 100].map((v) => (
                        <button
                            key={v}
                            className={`storage-preset ${storageCap === v ? "active" : ""}`}
                            onClick={() => setStorageCap(v)}
                        >
                            {v} GB
                        </button>
                    ))}
                </div>
            </div>

            {/* Mesh Hosting Toggle */}
            <div className="settings-section">
                <div className="settings-section-header">Mesh Hosting</div>
                <p className="settings-section-desc">
                    When enabled, your node will automatically discover and host high-demand
                    documents from the Wire network, earning credits for pulls served.
                </p>
                <label className="settings-toggle">
                    <input
                        type="checkbox"
                        checked={meshHosting}
                        onChange={(e) => setMeshHosting(e.target.checked)}
                    />
                    <span>Enable mesh hosting</span>
                </label>
            </div>

            <div className="settings-section">
                <div className="settings-section-header">Fleet Participation</div>
                <p className="settings-section-desc">
                    Declare how this node should participate in private fleet compute. This first
                    slice stores durable operator intent only; dispatch behavior will be derived
                    from it in later phases.
                </p>
                <div style={{ display: "grid", gap: 10, marginTop: 12 }}>
                    {(["coordinator", "hybrid", "worker"] as ComputeParticipationMode[]).map((mode) => {
                        const role = roleDescriptions[mode];
                        const selected = computePolicy.mode === mode;
                        return (
                            <button
                                key={mode}
                                type="button"
                                className={`storage-preset ${selected ? "active" : ""}`}
                                onClick={() => handleSelectComputeMode(mode)}
                                disabled={!computePolicyLoaded || computePolicySaving}
                                style={{
                                    textAlign: "left",
                                    width: "100%",
                                    padding: "12px 14px",
                                    display: "flex",
                                    flexDirection: "column",
                                    gap: 4,
                                }}
                            >
                                <span style={{ display: "flex", justifyContent: "space-between", gap: 12 }}>
                                    <span>{role.label}</span>
                                    <span style={{ opacity: 0.7, fontSize: 11 }}>
                                        dispatch {mode === "worker" ? "off" : "on"} · serve {mode === "coordinator" ? "off" : "on"}
                                    </span>
                                </span>
                                <span style={{ fontSize: 12, opacity: 0.85, whiteSpace: "normal" }}>
                                    {role.description}
                                </span>
                            </button>
                        );
                    })}
                </div>
                <div style={{ marginTop: 10, fontSize: 12, color: "var(--text-secondary)" }}>
                    {computePolicySaving
                        ? "Saving participation policy…"
                        : computePolicySaved
                            ? "Participation policy saved."
                            : computePolicyLoaded
                                ? `Current mode: ${roleDescriptions[computePolicy.mode].label}.`
                                : computePolicyUnavailable
                                    ? "Participation policy unavailable — check backend."
                                    : "Loading participation policy…"}
                </div>
            </div>

            {/* --- Wave 4 (walker-re-plan-wire-2.1 §8): Inference Routing - */}
            <InferenceRoutingPanel />

            {/* --- Phase 18a (L1): Local LLM (Ollama) -------------------- */}
            <div className="settings-section">
                <div className="settings-section-header">Local LLM (Ollama)</div>
                <p className="settings-section-desc">
                    Route all tiers through a local Ollama instance. When enabled,
                    every build uses local models instead of cloud providers.
                </p>

                <label className="settings-toggle">
                    <input
                        type="checkbox"
                        checked={localMode.status?.enabled ?? false}
                        disabled={localMode.loading}
                        aria-label="Use local models (Ollama)"
                        onChange={async (e) => {
                            if (e.target.checked) {
                                await handleEnableLocalMode();
                            } else {
                                await handleDisableLocalMode();
                            }
                        }}
                    />
                    <span>
                        Use local models (Ollama)
                        {localMode.loading && (
                            <span style={{ marginLeft: 8, opacity: 0.7 }}>
                                working…
                            </span>
                        )}
                    </span>
                </label>

                {/* URL field — read-only when toggle is on */}
                <div style={{ marginTop: 12, display: "flex", flexDirection: "column", gap: 6 }}>
                    <label
                        htmlFor="ollama-base-url"
                        style={{
                            fontSize: 11,
                            color: "var(--text-secondary)",
                            textTransform: "uppercase",
                            letterSpacing: 0.5,
                        }}
                    >
                        Base URL
                    </label>
                    <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                        <input
                            id="ollama-base-url"
                            type="text"
                            value={localUrl}
                            onChange={(e) => setLocalUrl(e.target.value)}
                            disabled={localMode.status?.enabled || localMode.loading}
                            className="settings-input"
                            placeholder="http://localhost:11434/v1"
                            style={{ flex: 1, padding: "6px 8px", fontSize: 12 }}
                        />
                        <button
                            type="button"
                            className="compose-btn"
                            onClick={handleProbe}
                            disabled={
                                probeBusy ||
                                localMode.loading
                            }
                            title="Reach Ollama at the URL above and list available models"
                        >
                            {probeBusy
                                ? "Testing…"
                                : localMode.status?.enabled
                                    ? "Refresh models"
                                    : "Test connection"}
                        </button>
                    </div>
                    {!localUrl.startsWith("http://") && !localUrl.startsWith("https://") && (
                        <span style={{ color: "#f87171", fontSize: 11 }}>
                            URL must start with http:// or https://
                        </span>
                    )}
                    {(() => {
                        try {
                            const host = new URL(localUrl).hostname;
                            const isLocal = host === "localhost" || host === "127.0.0.1" || host === "::1";
                            if (!isLocal) {
                                return (
                                    <div
                                        style={{
                                            marginTop: 8,
                                            padding: "8px 12px",
                                            borderRadius: 6,
                                            background: "rgba(251, 146, 60, 0.1)",
                                            border: "1px solid rgba(251, 146, 60, 0.3)",
                                            fontSize: 12,
                                            color: "#fdba74",
                                        }}
                                    >
                                        You are pointing at a remote server. All prompts and
                                        build data will be sent there. Ollama does not use
                                        authentication.
                                    </div>
                                );
                            }
                        } catch {
                            // invalid URL — the protocol check above handles this
                        }
                        return null;
                    })()}
                </div>

                {/* Model cards */}
                <div style={{ marginTop: 12 }}>
                    <AccordionSection title="Models" defaultOpen={true}>
                        {availableModelDetails.length === 0 ? (
                            <div className="model-card-empty">
                                {probeResult
                                    ? "No models found \u2014 pull a model with `ollama pull` first"
                                    : "Click Test connection to populate"}
                            </div>
                        ) : (
                            <div className={`model-card-list ${localMode.loading ? "model-card-list-loading" : ""}`}>
                                {availableModelDetails.map((m) => {
                                    const isActive =
                                        localModelChoice === m.name ||
                                        (localMode.status?.enabled && localMode.status.model === m.name);
                                    return (
                                        <div key={m.name}>
                                            <div
                                                className={`model-card ${isActive ? "model-card-active" : ""} ${localMode.loading ? "model-card-disabled" : ""}`}
                                                role="button"
                                                tabIndex={0}
                                                aria-pressed={isActive}
                                                onClick={async () => {
                                                    if (localMode.loading) return;
                                                    if (localMode.status?.enabled) {
                                                        await localMode.switchModel(m.name);
                                                    } else {
                                                        setLocalModelChoice(m.name);
                                                    }
                                                    // Lazy-load context window if missing
                                                    if (m.context_window == null && !detailsCache[m.name]) {
                                                        loadModelDetails(m.name);
                                                    }
                                                }}
                                                onKeyDown={(e) => {
                                                    if (e.key === "Enter" || e.key === " ") {
                                                        e.preventDefault();
                                                        (e.currentTarget as HTMLElement).click();
                                                    }
                                                }}
                                            >
                                                <div className="model-card-top-row">
                                                    <span className="model-card-name">{m.name}</span>
                                                    <div className="model-card-badges">
                                                        {isActive && (
                                                            <span className="model-card-badge model-card-badge-active">Active</span>
                                                        )}
                                                        {m.parameter_size && (
                                                            <span className="model-card-badge">{m.parameter_size}</span>
                                                        )}
                                                        {m.quantization_level && (
                                                            <span className="model-card-badge">{m.quantization_level}</span>
                                                        )}
                                                    </div>
                                                    {/* Phase 4: Delete button — hidden on active model */}
                                                    {!isActive && (
                                                        <button
                                                            type="button"
                                                            className="model-card-delete"
                                                            title={`Delete ${m.name}`}
                                                            onClick={(e) => {
                                                                e.stopPropagation();
                                                                setDeletingModel(m.name);
                                                            }}
                                                            disabled={localMode.loading}
                                                        >
                                                            &times;
                                                        </button>
                                                    )}
                                                </div>
                                                {(m.size_bytes > 0 || m.context_window != null) && (
                                                    <div className="model-card-size">
                                                        {m.size_bytes > 0 && formatBytes(m.size_bytes)}
                                                        {m.size_bytes > 0 && m.context_window != null && " \u00b7 "}
                                                        {m.context_window != null
                                                            ? `${Math.round(m.context_window / 1000)}K ctx`
                                                            : detailsLoading[m.name]
                                                                ? "\u2026"
                                                                : null}
                                                    </div>
                                                )}
                                            </div>
                                            {/* Phase 4: Delete confirmation */}
                                            {deletingModel === m.name && (
                                                <div className="model-delete-confirm">
                                                    <div className="model-delete-confirm-text">
                                                        Delete <strong>{m.name}</strong>? This removes model files from Ollama.
                                                    </div>
                                                    <div className="model-delete-confirm-warning">
                                                        If a build is in progress, deleting may cause it to fail.
                                                    </div>
                                                    <div className="model-delete-confirm-actions">
                                                        <button
                                                            type="button"
                                                            className="compose-btn"
                                                            onClick={(e) => {
                                                                e.stopPropagation();
                                                                setDeletingModel(null);
                                                            }}
                                                        >
                                                            Cancel
                                                        </button>
                                                        <button
                                                            type="button"
                                                            className="model-delete-confirm-btn"
                                                            disabled={localMode.loading}
                                                            onClick={(e) => {
                                                                e.stopPropagation();
                                                                handleDeleteModel(m.name);
                                                            }}
                                                        >
                                                            Delete
                                                        </button>
                                                    </div>
                                                </div>
                                            )}
                                        </div>
                                    );
                                })}
                            </div>
                        )}
                    </AccordionSection>
                </div>

                {/* Phase 3: Context Window Override */}
                {localMode.status?.enabled && (
                    <div style={{ marginTop: 12 }}>
                        <AccordionSection title="Context Window">
                            <div className="context-override-section">
                                <div className="override-status-line">
                                    <span style={{ fontSize: 12, color: "var(--text-secondary)" }}>
                                        Detected:{" "}
                                        {localMode.status.detected_context_limit
                                            ? `${Math.round(localMode.status.detected_context_limit / 1000)}K tokens`
                                            : "unknown"}
                                    </span>
                                    {localMode.status.context_override != null && (
                                        <span style={{ fontSize: 12, color: "var(--accent-cyan)" }}>
                                            Override: {Math.round(localMode.status.context_override / 1000)}K tokens
                                        </span>
                                    )}
                                </div>
                                <div className="override-effective-line">
                                    <span style={{ fontSize: 11, color: "var(--text-muted)" }}>
                                        Effective context:{" "}
                                        <strong style={{ color: "var(--text-primary)" }}>
                                            {(() => {
                                                const effective = localMode.status.context_override ?? localMode.status.detected_context_limit;
                                                return effective ? `${Math.round(effective / 1000)}K tokens` : "unknown";
                                            })()}
                                        </strong>
                                    </span>
                                </div>
                                <div className="override-input-row">
                                    <input
                                        type="number"
                                        className="settings-input override-input"
                                        value={contextInput}
                                        onChange={(e) => setContextInput(e.target.value)}
                                        placeholder={
                                            localMode.status.detected_context_limit
                                                ? String(localMode.status.detected_context_limit)
                                                : "e.g. 32768"
                                        }
                                        min={1024}
                                        step={1024}
                                    />
                                    <button
                                        type="button"
                                        className="compose-btn"
                                        disabled={localMode.loading || !contextInput}
                                        onClick={async () => {
                                            const val = parseInt(contextInput, 10);
                                            if (!isNaN(val) && val > 0) {
                                                await localMode.setContextOverride(val);
                                            }
                                        }}
                                    >
                                        Apply
                                    </button>
                                    {localMode.status.context_override != null && (
                                        <button
                                            type="button"
                                            className="override-reset-btn"
                                            disabled={localMode.loading}
                                            onClick={async () => {
                                                await localMode.setContextOverride(null);
                                            }}
                                        >
                                            Reset to auto-detect
                                        </button>
                                    )}
                                </div>
                                {(() => {
                                    const val = parseInt(contextInput, 10);
                                    const detected = localMode.status?.detected_context_limit;
                                    if (!isNaN(val) && detected && val > detected) {
                                        return (
                                            <div className="override-warning-banner">
                                                Model may not support this context length — use at your own risk
                                            </div>
                                        );
                                    }
                                    return null;
                                })()}
                            </div>
                        </AccordionSection>
                    </div>
                )}

                {/* Phase 3: Concurrency Override */}
                {localMode.status?.enabled && (
                    <div style={{ marginTop: 12 }}>
                        <AccordionSection title="Concurrency">
                            <div className="concurrency-section">
                                <div className="override-input-row">
                                    <input
                                        type="number"
                                        className="settings-input override-input"
                                        value={concurrencyInput}
                                        onChange={(e) => {
                                            const v = parseInt(e.target.value, 10);
                                            if (!isNaN(v)) setConcurrencyInput(Math.max(1, Math.min(12, v)));
                                        }}
                                        min={1}
                                        max={12}
                                    />
                                    <button
                                        type="button"
                                        className="compose-btn"
                                        disabled={localMode.loading}
                                        onClick={async () => {
                                            await localMode.setConcurrencyOverride(concurrencyInput);
                                        }}
                                    >
                                        Apply
                                    </button>
                                    {localMode.status.concurrency_override != null && localMode.status.concurrency_override !== 1 && (
                                        <button
                                            type="button"
                                            className="override-reset-btn"
                                            disabled={localMode.loading}
                                            onClick={async () => {
                                                await localMode.setConcurrencyOverride(null);
                                            }}
                                        >
                                            Reset to default (1)
                                        </button>
                                    )}
                                </div>
                                <div className="override-warning-banner">
                                    Most home users should leave this on 1 to prevent issues.
                                </div>
                                {concurrencyInput > 1 && (
                                    <div className="override-warning-banner override-warning-elevated">
                                        Higher concurrency increases memory pressure on your GPU.
                                    </div>
                                )}
                            </div>
                        </AccordionSection>
                    </div>
                )}

                {/* Phase 4: Pull Model */}
                <div style={{ marginTop: 12 }}>
                    <AccordionSection title="Pull Model">
                        <div className="pull-section">
                            <div className="pull-input-row">
                                <input
                                    type="text"
                                    className="settings-input pull-input"
                                    value={pullModelInput}
                                    onChange={(e) => setPullModelInput(e.target.value)}
                                    placeholder="e.g. llama3.2:latest"
                                    disabled={pulling}
                                    onKeyDown={(e) => {
                                        if (e.key === "Enter" && !pulling && pullModelInput.trim()) {
                                            handlePullModel();
                                        }
                                    }}
                                />
                                <button
                                    type="button"
                                    className="compose-btn"
                                    disabled={pulling || localMode.loading || !pullModelInput.trim()}
                                    onClick={handlePullModel}
                                >
                                    Pull Model
                                </button>
                                {pulling && (
                                    <button
                                        type="button"
                                        className="pull-cancel-btn"
                                        onClick={handleCancelPull}
                                    >
                                        Cancel
                                    </button>
                                )}
                            </div>
                            <a
                                href="https://ollama.com/library"
                                target="_blank"
                                rel="noopener noreferrer"
                                className="pull-browse-link"
                            >
                                Browse Ollama Library
                            </a>
                            {pullProgress && (
                                <div className="pull-progress-area">
                                    <div className="pull-status">
                                        {pullProgress.status}
                                        {pullProgress.completedBytes != null && pullProgress.totalBytes != null && pullProgress.totalBytes > 0 && (
                                            <span className="pull-status-bytes">
                                                {" "}{formatBytes(pullProgress.completedBytes)} / {formatBytes(pullProgress.totalBytes)}
                                            </span>
                                        )}
                                    </div>
                                    {pullProgress.totalBytes != null && pullProgress.totalBytes > 0 && (
                                        <div className="pull-progress">
                                            <div
                                                className="pull-progress-bar"
                                                style={{
                                                    width: `${Math.min(100, Math.round(((pullProgress.completedBytes ?? 0) / pullProgress.totalBytes) * 100))}%`,
                                                }}
                                            />
                                        </div>
                                    )}
                                </div>
                            )}
                        </div>
                    </AccordionSection>
                </div>

                {/* Phase 5: Configuration History — always visible when
                   the Ollama panel is loaded so users can see history even
                   after disabling local mode. The empty state inside the
                   accordion handles "no changes yet." Prior condition
                   (enabled || configHistory.length > 0) hid the accordion
                   on fresh page loads when local mode was disabled but
                   history existed in the DB, because configHistory starts
                   as [] before the lazy fetch fires. */}
                {localMode.status != null && (
                    <div style={{ marginTop: 12 }}>
                        <AccordionSection
                            title="Configuration History"
                            onToggle={(open) => {
                                if (open && !historyLoaded) {
                                    fetchConfigHistory();
                                }
                            }}
                        >
                            <div className="config-history-list">
                                {!historyLoaded ? (
                                    <div className="config-history-loading">Loading history...</div>
                                ) : configHistory.length === 0 ? (
                                    <div className="config-history-empty">No configuration changes recorded yet.</div>
                                ) : (
                                    <>
                                        {configHistory.map((entry) => (
                                            <div
                                                key={entry.contribution_id}
                                                className={`config-history-entry ${entry.is_active ? "config-history-entry-active" : ""}`}
                                            >
                                                <div className="config-history-entry-row">
                                                    <div className="config-history-entry-info">
                                                        <span className="config-history-timestamp">
                                                            {formatHistoryTime(entry.created_at)}
                                                        </span>
                                                        {entry.triggering_note && (
                                                            <span className="config-history-note">
                                                                {entry.triggering_note}
                                                            </span>
                                                        )}
                                                    </div>
                                                    <div className="config-history-entry-actions">
                                                        {entry.created_by && (
                                                            <span className="config-history-badge">
                                                                {entry.created_by}
                                                            </span>
                                                        )}
                                                        {entry.is_active && (
                                                            <span className="config-history-badge config-history-badge-active">
                                                                Active
                                                            </span>
                                                        )}
                                                        {!entry.is_active && (
                                                            <button
                                                                type="button"
                                                                className="config-history-rollback-btn"
                                                                disabled={localMode.status?.enabled || localMode.loading}
                                                                title={
                                                                    localMode.status?.enabled
                                                                        ? "Disable local mode first"
                                                                        : `Roll back to this version`
                                                                }
                                                                onClick={() => setConfirmingRollback(entry.contribution_id)}
                                                            >
                                                                Rollback
                                                            </button>
                                                        )}
                                                    </div>
                                                </div>
                                                {/* Rollback confirmation inline */}
                                                {confirmingRollback === entry.contribution_id && (
                                                    <div className="config-history-confirm">
                                                        <span className="config-history-confirm-text">
                                                            Roll back tier routing to the version from{" "}
                                                            {formatHistoryTime(entry.created_at)}?
                                                        </span>
                                                        <div className="config-history-confirm-actions">
                                                            <button
                                                                type="button"
                                                                className="compose-btn"
                                                                onClick={() => setConfirmingRollback(null)}
                                                            >
                                                                Cancel
                                                            </button>
                                                            <button
                                                                type="button"
                                                                className="config-history-confirm-btn"
                                                                disabled={localMode.loading}
                                                                onClick={() => handleRollback(entry.contribution_id)}
                                                            >
                                                                Confirm rollback
                                                            </button>
                                                        </div>
                                                    </div>
                                                )}
                                            </div>
                                        ))}
                                        {configHistory.length >= 20 && (
                                            <div className="config-history-truncated">
                                                Showing most recent 20 entries
                                            </div>
                                        )}
                                    </>
                                )}
                            </div>
                        </AccordionSection>
                    </div>
                )}

                {/* Phase 6: Experimental Territory — only visible when local mode is enabled */}
                {localMode.status?.enabled && (
                    <div style={{ marginTop: 12 }}>
                        <AccordionSection
                            title="Optimization Territory"
                            onToggle={(open) => {
                                if (open && !territoryLoaded) {
                                    fetchTerritory();
                                }
                            }}
                        >
                            <div className="territory-section">
                                <p className="territory-explainer">
                                    When the steward arrives, it will only optimize dimensions
                                    you've marked as experimental. Locked dimensions are never
                                    touched.
                                </p>
                                {!territoryLoaded ? (
                                    <div className="territory-loading">Loading territory...</div>
                                ) : territory ? (
                                    <>
                                        {TERRITORY_DIMENSIONS.map((dim) => {
                                            const dimState = territory.dimensions[dim.key] || { status: "locked", bounds: null };
                                            return (
                                                <div key={dim.key} className="territory-dimension">
                                                    <div className="territory-dimension-header">
                                                        <span className="territory-dimension-name">
                                                            <span className="territory-dimension-icon">
                                                                {dimState.status === "locked"
                                                                    ? "[x]"
                                                                    : dimState.status === "experimental"
                                                                        ? "[ ]"
                                                                        : "[~]"}
                                                            </span>
                                                            {dim.label}
                                                        </span>
                                                        <span className="territory-dimension-desc">{dim.description}</span>
                                                    </div>
                                                    <div className="territory-status-selector">
                                                        <button
                                                            type="button"
                                                            className={`territory-status-option ${dimState.status === "locked" ? "territory-status-option-active" : ""}`}
                                                            onClick={() => setDimensionStatus(dim.key, "locked")}
                                                        >
                                                            Locked
                                                        </button>
                                                        <button
                                                            type="button"
                                                            className={`territory-status-option ${dimState.status === "experimental" ? "territory-status-option-active" : ""}`}
                                                            onClick={() => setDimensionStatus(dim.key, "experimental")}
                                                        >
                                                            Experimental
                                                        </button>
                                                        <button
                                                            type="button"
                                                            className={`territory-status-option ${dimState.status === "experimental_within_bounds" ? "territory-status-option-active" : ""}`}
                                                            onClick={() => setDimensionStatus(dim.key, "experimental_within_bounds")}
                                                        >
                                                            Bounded
                                                        </button>
                                                    </div>
                                                    {dimState.status === "experimental_within_bounds" && dim.key !== "model_selection" && (
                                                        <div className="territory-bounds">
                                                            <label className="territory-bounds-label">
                                                                Min
                                                                <input
                                                                    type="number"
                                                                    className="settings-input territory-bounds-input"
                                                                    value={dimState.bounds?.min ?? ""}
                                                                    onChange={(e) => setDimensionBounds(dim.key, "min", e.target.value)}
                                                                    placeholder="none"
                                                                />
                                                            </label>
                                                            <label className="territory-bounds-label">
                                                                Max
                                                                <input
                                                                    type="number"
                                                                    className="settings-input territory-bounds-input"
                                                                    value={dimState.bounds?.max ?? ""}
                                                                    onChange={(e) => setDimensionBounds(dim.key, "max", e.target.value)}
                                                                    placeholder="none"
                                                                />
                                                            </label>
                                                        </div>
                                                    )}
                                                </div>
                                            );
                                        })}
                                        <button
                                            type="button"
                                            className={`territory-save-btn ${territorySaved ? "territory-save-btn-saved" : ""}`}
                                            disabled={territorySaving || localMode.loading}
                                            onClick={handleSaveTerritory}
                                        >
                                            {territorySaved ? "Saved" : territorySaving ? "Saving..." : "Save Territory"}
                                        </button>
                                    </>
                                ) : null}
                            </div>
                        </AccordionSection>
                    </div>
                )}

                {/* Status line */}
                <div style={{ marginTop: 12, fontSize: 12 }}>
                    {localMode.status?.enabled && localMode.status.reachable ? (
                        <span style={{ color: "#4ade80" }}>
                            ✓ Enabled — routing all tiers through{" "}
                            <strong>{localMode.status.model ?? "?"}</strong> on{" "}
                            <strong>{localMode.status.base_url ?? "?"}</strong>
                            {(localMode.status.context_override ?? localMode.status.detected_context_limit) && (
                                <>
                                    {" "}
                                    · context limit{" "}
                                    {Math.round(
                                        (localMode.status.context_override ?? localMode.status.detected_context_limit!) / 1000,
                                    )}
                                    K tokens
                                </>
                            )}
                        </span>
                    ) : localMode.status?.enabled && !localMode.status.reachable ? (
                        <span style={{ color: "#f87171" }}>
                            ✗ Cannot reach Ollama at{" "}
                            <strong>{localMode.status.base_url ?? "?"}</strong>:{" "}
                            {localMode.status.reachability_error ?? "unknown error"}
                        </span>
                    ) : probeResult && probeResult.reachable ? (
                        <span style={{ color: "#4ade80" }}>
                            ✓ Reachable — {probeResult.available_models.length}{" "}
                            model{probeResult.available_models.length === 1 ? "" : "s"} available
                        </span>
                    ) : probeResult && !probeResult.reachable ? (
                        <span style={{ color: "#f87171" }}>
                            ✗ Cannot reach Ollama:{" "}
                            {probeResult.reachability_error ?? "unknown error"}
                        </span>
                    ) : (
                        <span style={{ color: "var(--text-secondary)" }}>
                            Disabled — builds use cloud providers (OpenRouter)
                        </span>
                    )}
                </div>

                {/* Warning banner when enabled */}
                {localMode.status?.enabled && (
                    <div
                        style={{
                            marginTop: 12,
                            padding: "8px 12px",
                            borderRadius: 6,
                            background: "rgba(251, 146, 60, 0.1)",
                            border: "1px solid rgba(251, 146, 60, 0.3)",
                            fontSize: 12,
                            color: "#fdba74",
                        }}
                    >
                        {(() => {
                            const c = localMode.status?.concurrency_override ?? 1;
                            if (c <= 1) {
                                return "Builds run entirely on your machine with concurrency 1. Adjust in the Concurrency section above if your hardware supports it.";
                            }
                            return `Concurrency set to ${c}. Builds run entirely on your machine with ${c} parallel workers.`;
                        })()}
                    </div>
                )}

                {/* Confirm disable */}
                {confirmingDisable && (
                    <div
                        style={{
                            marginTop: 12,
                            padding: "8px 12px",
                            borderRadius: 6,
                            background: "rgba(248, 113, 113, 0.1)",
                            border: "1px solid rgba(248, 113, 113, 0.3)",
                            fontSize: 12,
                            color: "#fca5a5",
                            display: "flex",
                            justifyContent: "space-between",
                            alignItems: "center",
                            gap: 8,
                        }}
                    >
                        <span>
                            Disable local mode? This will restore your previous tier
                            routing.
                        </span>
                        <div style={{ display: "flex", gap: 6 }}>
                            <button
                                type="button"
                                className="compose-btn"
                                onClick={() => setConfirmingDisable(false)}
                            >
                                Cancel
                            </button>
                            <button
                                type="button"
                                className="save-btn"
                                onClick={async () => {
                                    setConfirmingDisable(false);
                                    await localMode.disable();
                                }}
                            >
                                Yes, disable
                            </button>
                        </div>
                    </div>
                )}

                {/* Error surface */}
                {localMode.error && !localMode.loading && (
                    <div
                        style={{
                            marginTop: 12,
                            padding: "8px 12px",
                            borderRadius: 6,
                            background: "rgba(248, 113, 113, 0.1)",
                            border: "1px solid rgba(248, 113, 113, 0.3)",
                            fontSize: 12,
                            color: "#fca5a5",
                        }}
                    >
                        {localMode.error}
                    </div>
                )}
            </div>

            {/* Auto-Update */}
            <div className="settings-section">
                <div className="settings-section-header">Auto-Update</div>
                <p className="settings-section-desc">
                    When enabled, Wire can push updates to your node automatically.
                    Updates are code-signed for security.
                </p>
                <label className="settings-toggle">
                    <input
                        type="checkbox"
                        checked={autoUpdate}
                        onChange={(e) => setAutoUpdate(e.target.checked)}
                    />
                    <span>Enable auto-update</span>
                </label>

                <div className="update-actions">
                    <button
                        className="compose-btn"
                        onClick={handleCheckUpdate}
                        disabled={checking}
                    >
                        {checking ? "Checking..." : "Check for Updates"}
                    </button>
                </div>

                {updateInfo && updateInfo.available && (
                    <div className="update-banner">
                        <div className="update-banner-header">
                            <span>Version {updateInfo.version} available</span>
                        </div>
                        {updateInfo.body && (
                            <p className="update-notes">{updateInfo.body}</p>
                        )}
                        <button
                            className="save-btn"
                            onClick={handleInstallUpdate}
                            disabled={installing}
                        >
                            {installing ? "Installing... (app will restart)" : "Install & Restart"}
                        </button>
                    </div>
                )}

                {updateInfo && !updateInfo.available && (
                    <div className="update-current">
                        You're running the latest version
                    </div>
                )}
            </div>

            {/* Save */}
            <button
                className={`save-btn ${saved ? "save-success" : ""}`}
                onClick={handleSave}
                disabled={saving}
            >
                {saved ? "Saved" : saving ? "Saving..." : "Save Settings"}
            </button>
        </div>
    );
}
