import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

// --- Types ------------------------------------------------------------------

interface WireNodeConfig {
    api_url: string;
    api_token: string;
    node_id: string;
    storage_cap_gb: number;
    mesh_hosting_enabled: boolean;
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
    const [apiUrl, setApiUrl] = useState("");
    const [apiToken, setApiToken] = useState("");
    const [health, setHealth] = useState<HealthStatus | null>(null);
    const [saving, setSaving] = useState(false);
    const [saved, setSaved] = useState(false);
    const [autoUpdate, setAutoUpdate] = useState(false);
    const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
    const [checking, setChecking] = useState(false);
    const [installing, setInstalling] = useState(false);

    const fetchData = useCallback(async () => {
        try {
            const [cfg, healthStatus] = await Promise.all([
                invoke<WireNodeConfig>("get_config"),
                invoke<HealthStatus>("get_health_status"),
            ]);
            setConfig(cfg);
            setHealth(healthStatus);
            setStorageCap(cfg.storage_cap_gb);
            setMeshHosting(cfg.mesh_hosting_enabled);
            setApiUrl(cfg.api_url);
            setApiToken(cfg.api_token);
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
                nodeName: config.node_id || "Wire Node",
                storageCapGb: storageCap,
                meshHostingEnabled: meshHosting,
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

            {/* API Configuration */}
            <div className="settings-section">
                <div className="settings-section-header">API Configuration</div>
                <div className="form-group">
                    <label htmlFor="api-url">Wire API URL</label>
                    <input
                        id="api-url"
                        type="text"
                        value={apiUrl}
                        onChange={(e) => setApiUrl(e.target.value)}
                        placeholder="https://newsbleach.com"
                        className="settings-input"
                    />
                </div>
                <div className="form-group">
                    <label htmlFor="api-token">API Token</label>
                    <input
                        id="api-token"
                        type="password"
                        value={apiToken}
                        onChange={(e) => setApiToken(e.target.value)}
                        placeholder="Your Wire API token"
                        className="settings-input"
                    />
                </div>
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
