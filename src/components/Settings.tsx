import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocalMode, type OllamaProbeResult, type OllamaModelInfo } from "../hooks/useLocalMode";
import { AccordionSection } from "./AccordionSection";

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
        try {
            const [cfg, healthStatus, name] = await Promise.all([
                invoke<WireNodeConfig>("get_config"),
                invoke<HealthStatus>("get_health_status"),
                invoke<string>("get_node_name"),
            ]);
            setConfig(cfg);
            setHealth(healthStatus);
            setStorageCap(cfg.storage_cap_gb);
            setMeshHosting(cfg.mesh_hosting_enabled);
            setAutoUpdate(cfg.auto_update_enabled);
            setNodeName(name || "Wire Node");
        } catch (err) {
            console.error("Settings fetch error:", err);
        }
    }, []);

    useEffect(() => { fetchData(); }, [fetchData]);

    const handleSave = async () => {
        if (!config) return;
        setSaving(true);
        try {
            // Save via onboarding endpoint (which persists to disk)
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
                                        <div
                                            key={m.name}
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
