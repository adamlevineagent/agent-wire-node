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

    // Test remote connection via IPC — renderer cannot fetch arbitrary URLs
    const handleTestConnection = useCallback(async () => {
        if (!manualTunnelUrl.trim()) return;

        setTesting(true);
        setTestResult(null);

        try {
            const data = await invoke<{ version?: string; documents_cached?: number }>(
                "test_remote_connection",
                { url: manualTunnelUrl.trim() }
            );
            setTestResult(
                `Connected -- version ${data.version || "unknown"}, ${data.documents_cached || 0} documents cached`
            );
        } catch (err: unknown) {
            const message = err instanceof Error ? err.message : String(err);
            setTestResult(message);
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
