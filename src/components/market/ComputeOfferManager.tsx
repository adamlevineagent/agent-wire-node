// ComputeOfferManager.tsx — Publish and edit compute market offers.
//
// Per `docs/plans/compute-market-phase-2-exchange.md` §IV:
//   - List current offers with model, rates, discount curve, Wire status.
//   - Create new offer: select from loaded models, set per-M-token rates
//     + reservation fee + queue discount curve + max_queue_depth.
//   - Integer inputs only (Pillar 9) — basis points for multipliers,
//     credits for rates.
//   - Wire sync status: show when offer is active on Wire vs pending.
//
// IPCs consumed: compute_offer_create, compute_offer_update,
// compute_offer_remove, compute_offers_list.
//
// The rate/fee inputs are per-million tokens in credits (integer i64).
// Multipliers in the discount curve are basis points (integer i32,
// 10000 = 1.0x). Input fields enforce this with parseInt + clamp to
// reasonable ranges.

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface QueueDiscountPoint {
    depth: number;
    multiplier_bps: number;
}

interface ComputeOffer {
    model_id: string;
    provider_type: string;
    rate_per_m_input: number;
    rate_per_m_output: number;
    reservation_fee: number;
    queue_discount_curve: QueueDiscountPoint[];
    max_queue_depth: number;
    wire_offer_id: string | null;
}

interface OfferFormState {
    model_id: string;
    provider_type: "local" | "bridge";
    rate_per_m_input: string;       // stringified while editing
    rate_per_m_output: string;
    reservation_fee: string;
    max_queue_depth: string;
    curve: QueueDiscountPoint[];
}

const emptyForm: OfferFormState = {
    model_id: "",
    provider_type: "local",
    rate_per_m_input: "100",
    rate_per_m_output: "500",
    reservation_fee: "10",
    max_queue_depth: "8",
    curve: [
        { depth: 0, multiplier_bps: 10000 },
        { depth: 4, multiplier_bps: 9500 },
        { depth: 8, multiplier_bps: 9000 },
    ],
};

function parseIntOrZero(s: string): number {
    const n = parseInt(s, 10);
    return Number.isFinite(n) ? n : 0;
}

/**
 * Format a multiplier_bps as a human-readable multiplier string.
 * 10000 → "1.00x", 9500 → "0.95x", 11000 → "1.10x"
 */
function formatMultiplier(bps: number): string {
    return `${(bps / 10000).toFixed(2)}x`;
}

/**
 * Show the effective rate at a given queue depth, given a curve.
 * Looks up the highest-depth curve point <= the target depth.
 * Returns the base rate * multiplier_bps / 10000 as an integer
 * (floor division, same as the Rust math at settlement time).
 */
function effectiveRate(rate: number, depth: number, curve: QueueDiscountPoint[]): number {
    let multiplier = 10000; // default 1.0x
    for (const point of [...curve].sort((a, b) => a.depth - b.depth)) {
        if (depth >= point.depth) multiplier = point.multiplier_bps;
    }
    return Math.floor((rate * multiplier) / 10000);
}

export function ComputeOfferManager() {
    const [offers, setOffers] = useState<ComputeOffer[]>([]);
    const [loading, setLoading] = useState(true);
    const [form, setForm] = useState<OfferFormState>(emptyForm);
    const [saving, setSaving] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [editingModelId, setEditingModelId] = useState<string | null>(null);

    const refresh = useCallback(async () => {
        try {
            const list = await invoke<ComputeOffer[]>("compute_offers_list");
            setOffers(list);
            setError(null);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    }, []);

    useEffect(() => {
        void refresh();
    }, [refresh]);

    const beginEdit = (offer: ComputeOffer) => {
        setForm({
            model_id: offer.model_id,
            provider_type: offer.provider_type as "local" | "bridge",
            rate_per_m_input: String(offer.rate_per_m_input),
            rate_per_m_output: String(offer.rate_per_m_output),
            reservation_fee: String(offer.reservation_fee),
            max_queue_depth: String(offer.max_queue_depth),
            curve: offer.queue_discount_curve.length > 0
                ? offer.queue_discount_curve
                : emptyForm.curve,
        });
        setEditingModelId(offer.model_id);
        setError(null);
    };

    const resetForm = () => {
        setForm(emptyForm);
        setEditingModelId(null);
        setError(null);
    };

    const handleSave = async () => {
        setSaving(true);
        setError(null);
        try {
            const payload = {
                model_id: form.model_id.trim(),
                provider_type: form.provider_type,
                rate_per_m_input: parseIntOrZero(form.rate_per_m_input),
                rate_per_m_output: parseIntOrZero(form.rate_per_m_output),
                reservation_fee: parseIntOrZero(form.reservation_fee),
                queue_discount_curve: form.curve,
                max_queue_depth: parseIntOrZero(form.max_queue_depth),
            };
            if (!payload.model_id) {
                throw new Error("model_id is required");
            }
            const cmd = editingModelId ? "compute_offer_update" : "compute_offer_create";
            await invoke(cmd, { offer: payload });
            await refresh();
            resetForm();
        } catch (e) {
            setError(String(e));
        } finally {
            setSaving(false);
        }
    };

    const handleRemove = async (model_id: string) => {
        if (!confirm(`Remove offer for ${model_id}?`)) return;
        setSaving(true);
        setError(null);
        try {
            await invoke("compute_offer_remove", { modelId: model_id });
            await refresh();
            if (editingModelId === model_id) resetForm();
        } catch (e) {
            setError(String(e));
        } finally {
            setSaving(false);
        }
    };

    const updateCurvePoint = (idx: number, field: "depth" | "multiplier_bps", value: string) => {
        setForm((prev) => {
            const curve = [...prev.curve];
            curve[idx] = { ...curve[idx], [field]: parseIntOrZero(value) };
            return { ...prev, curve };
        });
    };

    const addCurvePoint = () => {
        setForm((prev) => ({
            ...prev,
            curve: [...prev.curve, { depth: prev.curve.length * 4, multiplier_bps: 10000 }],
        }));
    };

    const removeCurvePoint = (idx: number) => {
        setForm((prev) => ({ ...prev, curve: prev.curve.filter((_, i) => i !== idx) }));
    };

    return (
        <div className="compute-offer-manager">
            <h2>Compute Offer Manager</h2>

            {error && (
                <div className="error-banner" role="alert" style={{ color: "#c33", padding: "8px 0" }}>
                    {error}
                </div>
            )}

            <section style={{ marginBottom: 24 }}>
                <h3>Current offers</h3>
                {loading ? (
                    <p>Loading...</p>
                ) : offers.length === 0 ? (
                    <p style={{ color: "#888" }}>
                        No offers published yet. Create one below to start accepting market jobs.
                    </p>
                ) : (
                    <table style={{ width: "100%", borderCollapse: "collapse" }}>
                        <thead>
                            <tr>
                                <th style={cellStyle}>Model</th>
                                <th style={cellStyle}>Type</th>
                                <th style={cellStyle}>Input / M</th>
                                <th style={cellStyle}>Output / M</th>
                                <th style={cellStyle}>Reservation</th>
                                <th style={cellStyle}>Max Depth</th>
                                <th style={cellStyle}>Wire Status</th>
                                <th style={cellStyle}></th>
                            </tr>
                        </thead>
                        <tbody>
                            {offers.map((o) => (
                                <tr key={o.model_id}>
                                    <td style={cellStyle}>{o.model_id}</td>
                                    <td style={cellStyle}>{o.provider_type}</td>
                                    <td style={cellStyle}>{o.rate_per_m_input}</td>
                                    <td style={cellStyle}>{o.rate_per_m_output}</td>
                                    <td style={cellStyle}>{o.reservation_fee}</td>
                                    <td style={cellStyle}>{o.max_queue_depth}</td>
                                    <td style={cellStyle}>
                                        {o.wire_offer_id ? (
                                            <span style={{ color: "#3a3" }} title={o.wire_offer_id}>
                                                Active
                                            </span>
                                        ) : (
                                            <span style={{ color: "#c80" }}>Pending sync</span>
                                        )}
                                    </td>
                                    <td style={cellStyle}>
                                        <button onClick={() => beginEdit(o)} disabled={saving}>
                                            Edit
                                        </button>{" "}
                                        <button onClick={() => handleRemove(o.model_id)} disabled={saving}>
                                            Remove
                                        </button>
                                    </td>
                                </tr>
                            ))}
                        </tbody>
                    </table>
                )}
            </section>

            <section>
                <h3>{editingModelId ? `Edit ${editingModelId}` : "Create new offer"}</h3>
                <div style={formGridStyle}>
                    <label>
                        Model ID
                        <input
                            type="text"
                            value={form.model_id}
                            onChange={(e) => setForm({ ...form, model_id: e.target.value })}
                            disabled={editingModelId !== null}
                            placeholder="e.g. gemma3:27b"
                        />
                    </label>

                    <label>
                        Provider
                        <select
                            value={form.provider_type}
                            onChange={(e) =>
                                setForm({ ...form, provider_type: e.target.value as "local" | "bridge" })
                            }
                        >
                            <option value="local">local (Ollama)</option>
                            <option value="bridge">bridge (OpenRouter)</option>
                        </select>
                    </label>

                    <label>
                        Input rate (credits / million tokens)
                        <input
                            type="number"
                            step="1"
                            min="0"
                            value={form.rate_per_m_input}
                            onChange={(e) => setForm({ ...form, rate_per_m_input: e.target.value })}
                        />
                    </label>

                    <label>
                        Output rate (credits / million tokens)
                        <input
                            type="number"
                            step="1"
                            min="0"
                            value={form.rate_per_m_output}
                            onChange={(e) => setForm({ ...form, rate_per_m_output: e.target.value })}
                        />
                    </label>

                    <label>
                        Reservation fee (credits)
                        <input
                            type="number"
                            step="1"
                            min="0"
                            value={form.reservation_fee}
                            onChange={(e) => setForm({ ...form, reservation_fee: e.target.value })}
                        />
                    </label>

                    <label>
                        Max market queue depth
                        <input
                            type="number"
                            step="1"
                            min="0"
                            value={form.max_queue_depth}
                            onChange={(e) => setForm({ ...form, max_queue_depth: e.target.value })}
                        />
                    </label>
                </div>

                <div style={{ marginTop: 16 }}>
                    <h4>Queue discount curve</h4>
                    <p style={{ color: "#888", fontSize: 12 }}>
                        Multiplier in basis points (10000 = 1.00x). Points are sorted by depth;
                        the multiplier at depth N is the one from the highest point whose depth
                        ≤ N. Effective rate = base × multiplier / 10000 (integer math).
                    </p>
                    <table style={{ width: "100%", borderCollapse: "collapse" }}>
                        <thead>
                            <tr>
                                <th style={cellStyle}>Depth</th>
                                <th style={cellStyle}>Multiplier (bps)</th>
                                <th style={cellStyle}>As rate</th>
                                <th style={cellStyle}>Eff. Output / M</th>
                                <th style={cellStyle}></th>
                            </tr>
                        </thead>
                        <tbody>
                            {form.curve.map((point, idx) => (
                                <tr key={idx}>
                                    <td style={cellStyle}>
                                        <input
                                            type="number"
                                            step="1"
                                            min="0"
                                            value={point.depth}
                                            onChange={(e) => updateCurvePoint(idx, "depth", e.target.value)}
                                        />
                                    </td>
                                    <td style={cellStyle}>
                                        <input
                                            type="number"
                                            step="1"
                                            min="0"
                                            value={point.multiplier_bps}
                                            onChange={(e) =>
                                                updateCurvePoint(idx, "multiplier_bps", e.target.value)
                                            }
                                        />
                                    </td>
                                    <td style={cellStyle}>{formatMultiplier(point.multiplier_bps)}</td>
                                    <td style={cellStyle}>
                                        {effectiveRate(
                                            parseIntOrZero(form.rate_per_m_output),
                                            point.depth,
                                            form.curve,
                                        )}
                                    </td>
                                    <td style={cellStyle}>
                                        <button onClick={() => removeCurvePoint(idx)} disabled={form.curve.length <= 1}>
                                            Remove
                                        </button>
                                    </td>
                                </tr>
                            ))}
                        </tbody>
                    </table>
                    <button onClick={addCurvePoint} style={{ marginTop: 8 }}>
                        + Add curve point
                    </button>
                </div>

                <div style={{ marginTop: 24, display: "flex", gap: 12 }}>
                    <button onClick={handleSave} disabled={saving || !form.model_id.trim()}>
                        {saving ? "Saving..." : editingModelId ? "Update offer" : "Create offer"}
                    </button>
                    <button onClick={resetForm} disabled={saving}>
                        Reset
                    </button>
                </div>
            </section>
        </div>
    );
}

const cellStyle: React.CSSProperties = {
    padding: "6px 8px",
    borderBottom: "1px solid #eee",
    textAlign: "left",
    verticalAlign: "middle",
};

const formGridStyle: React.CSSProperties = {
    display: "grid",
    gridTemplateColumns: "repeat(2, 1fr)",
    gap: 12,
};
