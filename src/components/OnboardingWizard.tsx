import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface OnboardingWizardProps {
    onComplete: () => void;
    defaultNodeName?: string;
}

type Step = "welcome" | "mesh" | "ready";

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

    const storageValue = selectedStorage === -1
        ? (parseInt(customStorage, 10) || 40)
        : selectedStorage;

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
                            Wire Node builds knowledge pyramids from your local files
                            and connects them to the Wire intelligence network.
                        </p>
                        <div className="onboarding-benefit-list">
                            <div className="onboarding-benefit">
                                <span className="benefit-icon">[P]</span>
                                <div>
                                    <strong>Build knowledge pyramids</strong>
                                    <p>Turn any codebase or document folder into layered, queryable intelligence</p>
                                </div>
                            </div>
                            <div className="onboarding-benefit">
                                <span className="benefit-icon">[W]</span>
                                <div>
                                    <strong>Publish to the Wire</strong>
                                    <p>Share pyramids and contributions on the Wire network and earn credits</p>
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
                            onClick={() => setStep("mesh")}
                        >
                            Next &rarr;
                        </button>
                    </div>
                )}

                {/* Step 2: Mesh Hosting Opt-in */}
                {step === "mesh" && (
                    <div className="onboarding-step">
                        <h2 className="onboarding-title">Mesh Hosting</h2>
                        <p className="onboarding-desc">
                            Reserve storage for hosting Wire content on this node.
                            When mesh hosting is enabled, your node can cache and serve
                            pyramids and contributions to agents on the network.
                            Hosted content is managed from the Fleet tab.
                        </p>

                        <div className="mesh-option-cards">
                            <button
                                className={`mesh-option ${meshHosting ? "active" : ""}`}
                                onClick={() => setMeshHosting(true)}
                            >
                                <div className="mesh-option-title">Enable Mesh Hosting</div>
                                <div className="mesh-option-desc">
                                    Reserve up to {storageValue} GB for hosting Wire documents.
                                    You can manage hosted content from the Fleet tab.
                                </div>
                            </button>
                            <button
                                className={`mesh-option ${!meshHosting ? "active" : ""}`}
                                onClick={() => setMeshHosting(false)}
                            >
                                <div className="mesh-option-title">Skip for Now</div>
                                <div className="mesh-option-desc">
                                    Only use local pyramids and linked folders.
                                    You can enable mesh hosting later in Settings.
                                </div>
                            </button>
                        </div>

                        <div className="onboarding-nav">
                            <button
                                className="back-link"
                                onClick={() => setStep("welcome")}
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

                {/* Step 3: Summary + Launch */}
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
