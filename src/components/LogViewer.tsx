import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

export function LogViewer() {
    const [logs, setLogs] = useState<string[]>([]);
    const [autoRefresh, setAutoRefresh] = useState(true);
    const [filter, setFilter] = useState("");

    const fetchLogs = async () => {
        try {
            const lines: string[] = await invoke("get_logs");
            setLogs(lines);
        } catch (err) {
            console.error("Failed to fetch logs:", err);
        }
    };

    useEffect(() => {
        fetchLogs();
        if (autoRefresh) {
            const interval = setInterval(fetchLogs, 2000);
            return () => clearInterval(interval);
        }
    }, [autoRefresh]);

    const filtered = filter
        ? logs.filter((l) => l.toLowerCase().includes(filter.toLowerCase()))
        : logs;

    return (
        <div className="log-viewer">
            <div className="log-toolbar">
                <input
                    type="text"
                    value={filter}
                    onChange={(e) => setFilter(e.target.value)}
                    placeholder="Filter logs..."
                    className="log-filter-input"
                />
                <label className="log-auto-refresh">
                    <input
                        type="checkbox"
                        checked={autoRefresh}
                        onChange={(e) => setAutoRefresh(e.target.checked)}
                    />
                    Auto-refresh
                </label>
                <button className="log-refresh-btn" onClick={fetchLogs}>
                    Refresh
                </button>
            </div>
            <div className="log-container">
                {filtered.length === 0 ? (
                    <div className="log-empty">No logs yet</div>
                ) : (
                    filtered.map((line, i) => (
                        <div
                            key={i}
                            className={`log-line ${
                                line.includes("ERROR") ? "log-error" :
                                line.includes("WARN") ? "log-warn" :
                                line.includes("INFO") ? "log-info" :
                                "log-debug"
                            }`}
                        >
                            {line}
                        </div>
                    ))
                )}
            </div>
        </div>
    );
}
