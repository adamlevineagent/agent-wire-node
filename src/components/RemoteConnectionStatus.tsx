// RemoteConnectionStatus.tsx — Wire identity and remote query status (WS-ONLINE-C)
//
// Shows: Wire identity status, tunnel status, remote query counts,
// and a manual tunnel URL input for testing remote queries.

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

interface RemoteConnectionStatusProps {
    tunnelUrl?: string | null;
    tunnelConnected?: boolean;
}

interface RemoteQueryStats {
    queries_served: number;
    queries_made: number;
    last_remote_query_at: string | null;
    unique_operators: number;
}

export function RemoteConnectionStatus({
    tunnelUrl,
    tunnelConnected,
}: RemoteConnectionStatusProps) {
    const [wireIdentityStatus, setWireIdentityStatus] = useState<
        "connected" | "expired" | "missing"
    >("missing");
    const [manualTunnelUrl, setManualTunnelUrl] = useState("");
    const [testResult, setTestResult] = useState<string | null>(null);
    const [testing, setTesting] = useState(false);
    const [queryStats, setQueryStats] = useState<RemoteQueryStats>({
        queries_served: 0,
        queries_made: 0,
        last_remote_query_at: null,
        unique_operators: 0,
    });

    // Poll for Wire identity status
    useEffect(() => {
        let cancelled = false;

        async function checkIdentity() {
            try {
                const status = await invoke<string>("get_wire_identity_status");
                if (!cancelled) {
                    if (status === "connected" || status === "expired" || status === "missing") {
                        setWireIdentityStatus(status as "connected" | "expired" | "missing");
                    }
                }
            } catch {
                // IPC command may not exist yet; default to missing
                if (!cancelled) {
                    setWireIdentityStatus("missing");
                }
            }
        }

        checkIdentity();
        const interval = setInterval(checkIdentity, 30000);
        return () => {
            cancelled = true;
            clearInterval(interval);
        };
    }, []);

    // Test remote connection to a manual tunnel URL
    const handleTestConnection = useCallback(async () => {
        if (!manualTunnelUrl.trim()) return;

        setTesting(true);
        setTestResult(null);

        try {
            const url = manualTunnelUrl.trim().replace(/\/$/, "");
            const response = await fetch(`${url}/health`, {
                method: "GET",
                signal: AbortSignal.timeout(10000),
            });

            if (response.ok) {
                const data = await response.json();
                setTestResult(
                    `Connected -- version ${data.version || "unknown"}, ${data.documents_cached || 0} documents cached`
                );
            } else {
                setTestResult(`Failed: HTTP ${response.status}`);
            }
        } catch (err: unknown) {
            const message = err instanceof Error ? err.message : String(err);
            setTestResult(`Connection failed: ${message}`);
        } finally {
            setTesting(false);
        }
    }, [manualTunnelUrl]);

    const identityIndicator =
        wireIdentityStatus === "connected"
            ? "[ON]"
            : wireIdentityStatus === "expired"
              ? "[!!]"
              : "[--]";
    const identityLabel =
        wireIdentityStatus === "connected"
            ? "Wire Identity Active"
            : wireIdentityStatus === "expired"
              ? "Wire Identity Expired"
              : "Wire Identity Not Set";
    const identityClass =
        wireIdentityStatus === "connected"
            ? "status-connected"
            : wireIdentityStatus === "expired"
              ? "status-warning"
              : "status-disconnected";

    const tunnelIndicator = tunnelConnected ? "[ON]" : "[OFF]";
    const tunnelClass = tunnelConnected
        ? "status-connected"
        : "status-disconnected";

    return (
        <div className="remote-connection-status">
            <h4>Remote Connections</h4>

            {/* Status Indicators */}
            <div className="remote-status-grid">
                <div className={`remote-status-item ${identityClass}`}>
                    <span className="status-indicator">{identityIndicator}</span>
                    <div className="status-detail">
                        <div className="status-label">{identityLabel}</div>
                    </div>
                </div>

                <div className={`remote-status-item ${tunnelClass}`}>
                    <span className="status-indicator">{tunnelIndicator}</span>
                    <div className="status-detail">
                        <div className="status-label">
                            {tunnelConnected ? "Tunnel Active" : "Tunnel Inactive"}
                        </div>
                        {tunnelUrl && (
                            <div className="status-value tunnel-url-display">
                                {tunnelUrl.replace("https://", "")}
                            </div>
                        )}
                    </div>
                </div>
            </div>

            {/* Query Counts */}
            <div className="remote-query-stats">
                <div className="stat-row">
                    <span className="stat-label">Queries Served</span>
                    <span className="stat-value">
                        {queryStats.queries_served.toLocaleString()}
                    </span>
                </div>
                <div className="stat-row">
                    <span className="stat-label">Queries Made</span>
                    <span className="stat-value">
                        {queryStats.queries_made.toLocaleString()}
                    </span>
                </div>
                <div className="stat-row">
                    <span className="stat-label">Unique Operators</span>
                    <span className="stat-value">
                        {queryStats.unique_operators}
                    </span>
                </div>
                {queryStats.last_remote_query_at && (
                    <div className="stat-row">
                        <span className="stat-label">Last Remote Query</span>
                        <span className="stat-value">
                            {new Date(queryStats.last_remote_query_at).toLocaleString()}
                        </span>
                    </div>
                )}
            </div>

            {/* Manual Tunnel URL for Testing */}
            <div className="remote-test-section">
                <label className="test-label">Test Remote Connection</label>
                <div className="test-input-row">
                    <input
                        type="text"
                        className="test-tunnel-input"
                        placeholder="https://node-id.tunnel.example.com"
                        value={manualTunnelUrl}
                        onChange={(e) => setManualTunnelUrl(e.target.value)}
                        onKeyDown={(e) => {
                            if (e.key === "Enter") handleTestConnection();
                        }}
                    />
                    <button
                        className="test-button"
                        onClick={handleTestConnection}
                        disabled={testing || !manualTunnelUrl.trim()}
                    >
                        {testing ? "Testing..." : "Test"}
                    </button>
                </div>
                {testResult && (
                    <div
                        className={`test-result ${testResult.startsWith("Connected") ? "test-success" : "test-failure"}`}
                    >
                        {testResult}
                    </div>
                )}
            </div>
        </div>
    );
}
