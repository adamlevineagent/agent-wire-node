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

function formatMultiplier(bps: number): string {
    return `${(bps / 10000).toFixed(2)}×`;
}

/**
 * Effective rate at a given queue depth, given a curve.
 * Highest-depth curve point <= N wins. Floor division to match
 * the Rust settlement math (integer credits, Pillar 9).
 */
function effectiveRate(rate: number, depth: number, curve: QueueDiscountPoint[]): number {
    let multiplier = 10000;
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
    const [formOpen, setFormOpen] = useState(false);

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
            curve:
                offer.queue_discount_curve.length > 0
                    ? offer.queue_discount_curve
                    : emptyForm.curve,
        });
        setEditingModelId(offer.model_id);
        setFormOpen(true);
        setError(null);
    };

    const resetForm = () => {
        setForm(emptyForm);
        setEditingModelId(null);
        setFormOpen(false);
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
        if (!confirm(`Remove offer for ${model_id}? Active jobs continue; only new matches are blocked.`)) return;
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

    const updateCurvePoint = (
        idx: number,
        field: "depth" | "multiplier_bps",
        value: string,
    ) => {
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
        setForm((prev) => ({
            ...prev,
            curve: prev.curve.filter((_, i) => i !== idx),
        }));
    };

    return (
        <div className="compute-offers-panel">
            {error && (
                <div className="compute-market-error" role="alert">
                    {error}
                </div>
            )}

            <div className="compute-offers-header">
                <div className="compute-offers-header-text">
                    <h3 className="compute-section-title">Your offers</h3>
                    <p className="compute-section-sub">
                        Models you're publishing to the Wire. Each offer defines the rate you
                        charge, how the rate scales with queue depth, and the cap on concurrent
                        market jobs.
                    </p>
                </div>
                {!formOpen && (
                    <button
                        className="compute-primary-btn"
                        onClick={() => {
                            setForm(emptyForm);
                            setEditingModelId(null);
                            setFormOpen(true);
                            setError(null);
                        }}
                    >
                        + New offer
                    </button>
                )}
            </div>

            {loading ? (
                <div className="compute-empty">Loading…</div>
            ) : offers.length === 0 ? (
                <div className="compute-empty">
                    <div className="compute-empty-title">No offers published yet</div>
                    <div className="compute-empty-desc">
                        Create an offer to start accepting paid market jobs. You keep running
                        local and fleet work regardless — market dispatches just land in the
                        same queue with their own depth cap.
                    </div>
                </div>
            ) : (
                <div className="compute-offer-grid">
                    {offers.map((o) => (
                        <OfferCard
                            key={o.model_id}
                            offer={o}
                            onEdit={() => beginEdit(o)}
                            onRemove={() => handleRemove(o.model_id)}
                            disabled={saving}
                        />
                    ))}
                </div>
            )}

            {formOpen && (
                <div className="compute-form-panel">
                    <div className="compute-form-header">
                        <h4 className="compute-section-title">
                            {editingModelId ? `Edit offer — ${editingModelId}` : "New offer"}
                        </h4>
                        <button className="compute-ghost-btn" onClick={resetForm} disabled={saving}>
                            Cancel
                        </button>
                    </div>

                    <div className="compute-form-grid">
                        <label className="compute-field">
                            <span className="compute-field-label">Model ID</span>
                            <input
                                className="compute-input"
                                type="text"
                                value={form.model_id}
                                onChange={(e) => setForm({ ...form, model_id: e.target.value })}
                                disabled={editingModelId !== null}
                                placeholder="e.g. gemma3:27b"
                            />
                            <span className="compute-field-hint">
                                Must match a locally-loaded model (or an OpenRouter slug if
                                provider is bridge).
                            </span>
                        </label>

                        <label className="compute-field">
                            <span className="compute-field-label">Provider</span>
                            <select
                                className="compute-input"
                                value={form.provider_type}
                                onChange={(e) =>
                                    setForm({
                                        ...form,
                                        provider_type: e.target.value as "local" | "bridge",
                                    })
                                }
                            >
                                <option value="local">Local (Ollama)</option>
                                <option value="bridge">Bridge (OpenRouter)</option>
                            </select>
                            <span className="compute-field-hint">
                                Local serves from your GPU; bridge proxies to OpenRouter (Phase 4).
                            </span>
                        </label>

                        <label className="compute-field">
                            <span className="compute-field-label">Input rate</span>
                            <div className="compute-input-with-suffix">
                                <input
                                    className="compute-input"
                                    type="number"
                                    step="1"
                                    min="0"
                                    value={form.rate_per_m_input}
                                    onChange={(e) =>
                                        setForm({ ...form, rate_per_m_input: e.target.value })
                                    }
                                />
                                <span className="compute-input-suffix">credits / M tokens</span>
                            </div>
                        </label>

                        <label className="compute-field">
                            <span className="compute-field-label">Output rate</span>
                            <div className="compute-input-with-suffix">
                                <input
                                    className="compute-input"
                                    type="number"
                                    step="1"
                                    min="0"
                                    value={form.rate_per_m_output}
                                    onChange={(e) =>
                                        setForm({ ...form, rate_per_m_output: e.target.value })
                                    }
                                />
                                <span className="compute-input-suffix">credits / M tokens</span>
                            </div>
                        </label>

                        <label className="compute-field">
                            <span className="compute-field-label">Reservation fee</span>
                            <div className="compute-input-with-suffix">
                                <input
                                    className="compute-input"
                                    type="number"
                                    step="1"
                                    min="0"
                                    value={form.reservation_fee}
                                    onChange={(e) =>
                                        setForm({ ...form, reservation_fee: e.target.value })
                                    }
                                />
                                <span className="compute-input-suffix">credits</span>
                            </div>
                            <span className="compute-field-hint">
                                Upfront deposit charged at match time, held until settle.
                            </span>
                        </label>

                        <label className="compute-field">
                            <span className="compute-field-label">Max market queue depth</span>
                            <div className="compute-input-with-suffix">
                                <input
                                    className="compute-input"
                                    type="number"
                                    step="1"
                                    min="0"
                                    value={form.max_queue_depth}
                                    onChange={(e) =>
                                        setForm({ ...form, max_queue_depth: e.target.value })
                                    }
                                />
                                <span className="compute-input-suffix">jobs</span>
                            </div>
                            <span className="compute-field-hint">
                                Beyond this, new market dispatches get rejected with 503 +
                                Retry-After so the Wire re-matches.
                            </span>
                        </label>
                    </div>

                    <div className="compute-curve-section">
                        <div className="compute-curve-header">
                            <h5 className="compute-curve-title">Queue discount curve</h5>
                            <p className="compute-curve-desc">
                                Multiplier in basis points (10000 = 1.00×). At depth N, the
                                multiplier from the highest point whose depth ≤ N wins.
                                Effective rate = base × multiplier / 10000.
                            </p>
                        </div>
                        <div className="compute-curve-table">
                            <div className="compute-curve-row compute-curve-head">
                                <div>Depth</div>
                                <div>Multiplier</div>
                                <div className="compute-curve-col-eff">As rate</div>
                                <div className="compute-curve-col-eff">Eff. output / M</div>
                                <div />
                            </div>
                            {form.curve.map((point, idx) => (
                                <div className="compute-curve-row" key={idx}>
                                    <div>
                                        <input
                                            className="compute-input compute-input-tight"
                                            type="number"
                                            step="1"
                                            min="0"
                                            value={point.depth}
                                            onChange={(e) =>
                                                updateCurvePoint(idx, "depth", e.target.value)
                                            }
                                        />
                                    </div>
                                    <div>
                                        <input
                                            className="compute-input compute-input-tight"
                                            type="number"
                                            step="100"
                                            min="0"
                                            value={point.multiplier_bps}
                                            onChange={(e) =>
                                                updateCurvePoint(
                                                    idx,
                                                    "multiplier_bps",
                                                    e.target.value,
                                                )
                                            }
                                        />
                                    </div>
                                    <div className="compute-curve-col-eff compute-mono">
                                        {formatMultiplier(point.multiplier_bps)}
                                    </div>
                                    <div className="compute-curve-col-eff compute-mono">
                                        {effectiveRate(
                                            parseIntOrZero(form.rate_per_m_output),
                                            point.depth,
                                            form.curve,
                                        )}
                                    </div>
                                    <div>
                                        <button
                                            className="compute-ghost-btn compute-ghost-btn-sm"
                                            onClick={() => removeCurvePoint(idx)}
                                            disabled={form.curve.length <= 1}
                                            title={
                                                form.curve.length <= 1
                                                    ? "At least one point required"
                                                    : "Remove curve point"
                                            }
                                        >
                                            ×
                                        </button>
                                    </div>
                                </div>
                            ))}
                        </div>
                        <button className="compute-ghost-btn compute-ghost-btn-sm" onClick={addCurvePoint}>
                            + Add curve point
                        </button>
                    </div>

                    <div className="compute-form-actions">
                        <button
                            className="compute-primary-btn"
                            onClick={handleSave}
                            disabled={saving || !form.model_id.trim()}
                        >
                            {saving
                                ? "Saving…"
                                : editingModelId
                                  ? "Update offer"
                                  : "Create offer"}
                        </button>
                        <button
                            className="compute-ghost-btn"
                            onClick={resetForm}
                            disabled={saving}
                        >
                            Discard
                        </button>
                    </div>
                </div>
            )}
        </div>
    );
}

interface OfferCardProps {
    offer: ComputeOffer;
    onEdit: () => void;
    onRemove: () => void;
    disabled: boolean;
}

function OfferCard({ offer, onEdit, onRemove, disabled }: OfferCardProps) {
    const wireStatus = offer.wire_offer_id ? "active" : "pending";
    return (
        <div className="compute-offer-card">
            <div className="compute-offer-card-header">
                <div className="compute-offer-card-model">
                    <span className="compute-offer-card-name">{offer.model_id}</span>
                    <span className="compute-offer-card-provider">{offer.provider_type}</span>
                </div>
                <span
                    className={`compute-offer-badge compute-offer-badge-${wireStatus}`}
                    title={
                        wireStatus === "active"
                            ? `Wire offer_id: ${offer.wire_offer_id}`
                            : "Not yet synced to the Wire"
                    }
                >
                    {wireStatus === "active" ? "Wire active" : "Pending sync"}
                </span>
            </div>

            <dl className="compute-offer-card-stats">
                <div className="compute-offer-stat">
                    <dt>Input</dt>
                    <dd className="compute-mono">{offer.rate_per_m_input}</dd>
                </div>
                <div className="compute-offer-stat">
                    <dt>Output</dt>
                    <dd className="compute-mono">{offer.rate_per_m_output}</dd>
                </div>
                <div className="compute-offer-stat">
                    <dt>Reservation</dt>
                    <dd className="compute-mono">{offer.reservation_fee}</dd>
                </div>
                <div className="compute-offer-stat">
                    <dt>Max depth</dt>
                    <dd className="compute-mono">{offer.max_queue_depth}</dd>
                </div>
            </dl>

            {offer.queue_discount_curve.length > 0 && (
                <div className="compute-offer-curve">
                    <div className="compute-offer-curve-label">Curve</div>
                    <div className="compute-offer-curve-points">
                        {offer.queue_discount_curve.map((p, i) => (
                            <span key={i} className="compute-offer-curve-point">
                                <span className="compute-offer-curve-depth">{p.depth}</span>
                                <span className="compute-offer-curve-sep">@</span>
                                <span className="compute-offer-curve-mul">
                                    {formatMultiplier(p.multiplier_bps)}
                                </span>
                            </span>
                        ))}
                    </div>
                </div>
            )}

            <div className="compute-offer-card-actions">
                <button className="compute-ghost-btn compute-ghost-btn-sm" onClick={onEdit} disabled={disabled}>
                    Edit
                </button>
                <button
                    className="compute-ghost-btn compute-ghost-btn-sm compute-ghost-btn-danger"
                    onClick={onRemove}
                    disabled={disabled}
                >
                    Remove
                </button>
            </div>
        </div>
    );
}
