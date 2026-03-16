import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

interface OnboardingWizardProps {
    onComplete: () => void;
    defaultNodeName?: string;
}

type Step = "welcome" | "folder" | "mesh" | "ready";

const STORAGE_OPTIONS = [
    { value: 10, label: "10 GB", desc: "Light contributor" },
    { value: 40, label: "40 GB", desc: "Recommended" },
    { value: 100, label: "100 GB", desc: "Power node" },
    { value: -1, label: "Custom", desc: "Enter your own" },
];

export function OnboardingWizard({ onComplete, defaultNodeName }: OnboardingWizardProps) {
    const [step, setStep] = useState<Step>("welcome");
    const [nodeName, setNodeName] = useState(defaultNodeName || "");
    const [selectedStorage, setSelectedStorage] = useState(40);
    const [customStorage, setCustomStorage] = useState("");
    const [meshHosting, setMeshHosting] = useState(false);
    const [saving, setSaving] = useState(false);
    const [error, setError] = useState("");

    // Folder linking state
    const [folderPath, setFolderPath] = useState("");
    const [corpusSlug, setCorpusSlug] = useState("");
    const [folderLinked, setFolderLinked] = useState(false);
    const [linkError, setLinkError] = useState("");

    const storageValue = selectedStorage === -1
        ? (parseInt(customStorage, 10) || 40)
        : selectedStorage;

    const handlePickFolder = useCallback(async () => {
        try {
            const dir = await open({ directory: true });
            if (dir) {
                setFolderPath(typeof dir === "string" ? dir : String(dir));
            }
        } catch (err) {
            console.error("Folder picker failed:", err);
        }
    }, []);

    const handleLinkFolder = useCallback(async () => {
        if (!folderPath || !corpusSlug.trim()) {
            setLinkError("Select a folder and enter a corpus slug");
            return;
        }
        setLinkError("");
        try {
            await invoke("link_folder", {
                folderPath,
                corpusSlug: corpusSlug.trim(),
            });
            setFolderLinked(true);
        } catch (err: any) {
            setLinkError(err?.toString() || "Failed to link folder");
        }
    }, [folderPath, corpusSlug]);

    const handleFinish = async () => {
        setSaving(true);
        setError("");
        try {
            await invoke("save_onboarding", {
                nodeName: nodeName.trim() || defaultNodeName || "Wire Node",
                storageCapGb: storageValue,
                meshHostingEnabled: meshHosting,
            });
            onComplete();
        } catch (err: any) {
            setError(err?.toString() || "Failed to save settings");
            setSaving(false);
        }
    };

    return (
        <div className="login-screen">
            <div className="login-card onboarding-card">
                {/* Step 1: Welcome + API Token / Name */}
                {step === "welcome" && (
                    <div className="onboarding-step">
                        <div className="wire-logo-login">W</div>
                        <h1 className="onboarding-title">Welcome to Wire Node</h1>
                        <p className="onboarding-desc">
                            You're about to turn this machine into a node in the
                            Wire network -- a decentralized document hosting mesh that
                            serves knowledge to consumers worldwide.
                        </p>
                        <div className="onboarding-benefit-list">
                            <div className="onboarding-benefit">
                                <span className="benefit-icon">[W]</span>
                                <div>
                                    <strong>Host documents</strong>
                                    <p>Your computer caches and serves Wire documents to nearby consumers</p>
                                </div>
                            </div>
                            <div className="onboarding-benefit">
                                <span className="benefit-icon">[C]</span>
                                <div>
                                    <strong>Earn credits</strong>
                                    <p>Get Wire credits based on pulls served and documents hosted</p>
                                </div>
                            </div>
                            <div className="onboarding-benefit">
                                <span className="benefit-icon">[~]</span>
                                <div>
                                    <strong>Runs quietly</strong>
                                    <p>Sits in your system tray, uses minimal resources</p>
                                </div>
                            </div>
                        </div>

                        <div className="form-group">
                            <label htmlFor="node-name">Node Name</label>
                            <input
                                id="node-name"
                                type="text"
                                value={nodeName}
                                onChange={(e) => setNodeName(e.target.value)}
                                placeholder={defaultNodeName || "My Wire Node"}
                            />
                            <span className="form-hint">
                                Visible on the network
                            </span>
                        </div>

                        <div className="form-group">
                            <label>Storage to allocate</label>
                            <div className="storage-options">
                                {STORAGE_OPTIONS.map((opt) => (
                                    <button
                                        key={opt.value}
                                        className={`storage-option ${selectedStorage === opt.value ? "active" : ""}`}
                                        onClick={() => setSelectedStorage(opt.value)}
                                    >
                                        <span className="storage-label">{opt.label}</span>
                                        <span className="storage-desc">{opt.desc}</span>
                                    </button>
                                ))}
                            </div>
                            {selectedStorage === -1 && (
                                <div className="custom-storage-input">
                                    <input
                                        type="number"
                                        value={customStorage}
                                        onChange={(e) => setCustomStorage(e.target.value)}
                                        placeholder="Enter GB"
                                        min="1"
                                        max="1000"
                                    />
                                    <span className="form-hint">GB</span>
                                </div>
                            )}
                        </div>

                        <button
                            className="login-button"
                            onClick={() => setStep("folder")}
                        >
                            Next: Link a Folder &rarr;
                        </button>
                    </div>
                )}

                {/* Step 2: Link First Folder */}
                {step === "folder" && (
                    <div className="onboarding-step">
                        <h2 className="onboarding-title">Link Your First Folder</h2>
                        <p className="onboarding-desc">
                            Connect a local folder to a Wire corpus. Documents in this folder
                            will be synced with the Wire network.
                        </p>

                        <div className="folder-picker-row">
                            <button
                                className="pick-folder-btn"
                                onClick={handlePickFolder}
                                type="button"
                            >
                                Choose Folder...
                            </button>
                            <span className="selected-path">
                                {folderPath
                                    ? (folderPath.length > 35 ? "..." + folderPath.slice(-32) : folderPath)
                                    : "No folder selected"}
                            </span>
                        </div>

                        <div className="form-group">
                            <label htmlFor="corpus-slug">Corpus Slug</label>
                            <input
                                id="corpus-slug"
                                type="text"
                                value={corpusSlug}
                                onChange={(e) => setCorpusSlug(e.target.value)}
                                placeholder="e.g. my-research"
                            />
                            <span className="form-hint">
                                The Wire corpus this folder maps to
                            </span>
                        </div>

                        {linkError && <div className="login-error">{linkError}</div>}

                        {folderLinked && (
                            <div className="onboarding-success">
                                Folder linked successfully
                            </div>
                        )}

                        {!folderLinked && (
                            <button
                                className="login-button secondary"
                                onClick={handleLinkFolder}
                                disabled={!folderPath || !corpusSlug.trim()}
                            >
                                Link Folder
                            </button>
                        )}

                        <div className="onboarding-nav">
                            <button
                                className="back-link"
                                onClick={() => setStep("welcome")}
                            >
                                &larr; Back
                            </button>
                            <button
                                className="login-button"
                                onClick={() => setStep("mesh")}
                            >
                                {folderLinked ? "Next" : "Skip"} &rarr;
                            </button>
                        </div>
                    </div>
                )}

                {/* Step 3: Mesh Hosting Opt-in */}
                {step === "mesh" && (
                    <div className="onboarding-step">
                        <h2 className="onboarding-title">Mesh Hosting</h2>
                        <p className="onboarding-desc">
                            Opt in to automatically discover and host high-demand documents
                            from the Wire network. Your node earns credits for every pull served.
                        </p>

                        <div className="mesh-option-cards">
                            <button
                                className={`mesh-option ${meshHosting ? "active" : ""}`}
                                onClick={() => setMeshHosting(true)}
                            >
                                <div className="mesh-option-title">Enable Mesh Hosting</div>
                                <div className="mesh-option-desc">
                                    Auto-host popular documents and earn credits passively.
                                    Uses up to {storageValue} GB of storage.
                                </div>
                            </button>
                            <button
                                className={`mesh-option ${!meshHosting ? "active" : ""}`}
                                onClick={() => setMeshHosting(false)}
                            >
                                <div className="mesh-option-title">Manual Only</div>
                                <div className="mesh-option-desc">
                                    Only sync documents from your linked folders.
                                    You can enable mesh hosting later in Settings.
                                </div>
                            </button>
                        </div>

                        <div className="onboarding-nav">
                            <button
                                className="back-link"
                                onClick={() => setStep("folder")}
                            >
                                &larr; Back
                            </button>
                            <button
                                className="login-button"
                                onClick={() => setStep("ready")}
                            >
                                Next &rarr;
                            </button>
                        </div>
                    </div>
                )}

                {/* Step 4: Summary + Launch */}
                {step === "ready" && (
                    <div className="onboarding-step">
                        <div style={{ fontSize: "2rem", textAlign: "center", marginBottom: "1rem", fontFamily: "monospace" }}>W</div>
                        <h2 className="onboarding-title">Ready to launch</h2>

                        <div className="onboarding-summary">
                            <div className="summary-row">
                                <span className="summary-label">Node name</span>
                                <span className="summary-value">{nodeName || defaultNodeName || "Wire Node"}</span>
                            </div>
                            <div className="summary-row">
                                <span className="summary-label">Storage</span>
                                <span className="summary-value">{storageValue} GB</span>
                            </div>
                            <div className="summary-row">
                                <span className="summary-label">Folder linked</span>
                                <span className="summary-value">{folderLinked ? `${corpusSlug}` : "None (skipped)"}</span>
                            </div>
                            <div className="summary-row">
                                <span className="summary-label">Mesh hosting</span>
                                <span className="summary-value">{meshHosting ? "Enabled" : "Manual only"}</span>
                            </div>
                        </div>

                        {error && <div className="login-error">{error}</div>}

                        <div className="onboarding-nav">
                            <button
                                className="back-link"
                                onClick={() => setStep("mesh")}
                            >
                                &larr; Back
                            </button>
                            <button
                                className="login-button"
                                onClick={handleFinish}
                                disabled={saving}
                            >
                                {saving ? "Starting..." : "Start Wire Node"}
                            </button>
                        </div>
                    </div>
                )}
            </div>
        </div>
    );
}
