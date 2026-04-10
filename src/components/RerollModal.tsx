// Phase 13 — reroll with notes modal.
//
// Shared across `PyramidBuildViz` (node / per-call reroll) and
// `CrossPyramidTimeline` (cross-pyramid reroll). The parent
// component owns the open/close state and passes in the target
// descriptor; this component handles the form, the empty-note
// confirmation flow, and the backend call.
//
// The backend accepts empty notes — the UX friction (confirm
// prompt) and the rate-limit warning banner are the primary
// anti-slot-machine levers per the spec.

import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

export type RerollTarget =
    | { type: 'node'; nodeId: string; stepName: string }
    | { type: 'cache'; cacheKey: string; stepName: string };

interface RerollModalProps {
    slug: string;
    target: RerollTarget;
    currentContent: string | null;
    onClose: () => void;
    onRerolled?: (result: RerollResult) => void;
}

export interface RerollResult {
    new_cache_entry_id: number;
    manifest_id: number | null;
    new_content: unknown;
    downstream_invalidated: number;
    rate_limit_warning: boolean;
}

export function RerollModal({
    slug,
    target,
    currentContent,
    onClose,
    onRerolled,
}: RerollModalProps) {
    const [note, setNote] = useState('');
    const [submitting, setSubmitting] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [confirmingEmpty, setConfirmingEmpty] = useState(false);
    const [result, setResult] = useState<RerollResult | null>(null);

    const targetLabel =
        target.type === 'node'
            ? `Node ${target.nodeId}`
            : `Cache entry ${target.cacheKey.slice(0, 12)}…`;

    async function doSubmit() {
        setSubmitting(true);
        setError(null);
        try {
            const args: Record<string, unknown> = {
                slug,
                note,
                forceFresh: true,
            };
            if (target.type === 'node') {
                args.nodeId = target.nodeId;
                args.cacheKey = null;
            } else {
                args.cacheKey = target.cacheKey;
                args.nodeId = null;
            }
            const r = await invoke<RerollResult>('pyramid_reroll_node', args);
            setResult(r);
            if (onRerolled) onRerolled(r);
        } catch (e) {
            setError(String(e));
        } finally {
            setSubmitting(false);
        }
    }

    function handleSubmit() {
        if (note.trim().length === 0 && !confirmingEmpty) {
            setConfirmingEmpty(true);
            return;
        }
        doSubmit();
    }

    function renderBody() {
        if (result) {
            return (
                <div className="reroll-modal-result">
                    <p>
                        <strong>Rerolled successfully.</strong>
                    </p>
                    <ul className="reroll-modal-summary">
                        <li>New cache entry id: {result.new_cache_entry_id}</li>
                        {result.manifest_id !== null && (
                            <li>Change manifest id: {result.manifest_id}</li>
                        )}
                        <li>
                            Downstream entries invalidated:{' '}
                            {result.downstream_invalidated}
                        </li>
                    </ul>
                    {result.rate_limit_warning && (
                        <div className="reroll-modal-warning">
                            You&apos;ve rerolled this node multiple times in the last 10
                            minutes. Providing specific feedback usually produces
                            better results than additional attempts.
                        </div>
                    )}
                    <div className="reroll-modal-actions">
                        <button className="btn btn-primary" onClick={onClose}>
                            Close
                        </button>
                    </div>
                </div>
            );
        }

        return (
            <>
                {currentContent && (
                    <div className="reroll-modal-preview">
                        <div className="reroll-modal-preview-label">Current output</div>
                        <pre className="reroll-modal-preview-text">
                            {currentContent.slice(0, 2000)}
                            {currentContent.length > 2000 ? '…' : ''}
                        </pre>
                    </div>
                )}

                <label className="reroll-modal-label">
                    Why reroll?{' '}
                    <span className="reroll-modal-hint">
                        (strongly encouraged — blank rerolls just re-roll the dice)
                    </span>
                </label>
                <textarea
                    className="reroll-modal-textarea"
                    value={note}
                    onChange={e => setNote(e.target.value)}
                    placeholder="What's wrong with the current output? What should the new version address?"
                    rows={5}
                    disabled={submitting}
                />

                {confirmingEmpty && note.trim().length === 0 && (
                    <div className="reroll-modal-confirm">
                        Rerolling without feedback will just re-run the LLM with
                        different randomness. Continue anyway?
                    </div>
                )}

                {error && <div className="reroll-modal-error">Error: {error}</div>}

                <div className="reroll-modal-actions">
                    <button className="btn btn-secondary" onClick={onClose} disabled={submitting}>
                        Cancel
                    </button>
                    <button
                        className="btn btn-primary"
                        onClick={handleSubmit}
                        disabled={submitting}
                    >
                        {submitting
                            ? 'Rerolling…'
                            : confirmingEmpty && note.trim().length === 0
                                ? 'Reroll anyway'
                                : 'Reroll'}
                    </button>
                </div>
            </>
        );
    }

    return (
        <div className="reroll-modal-backdrop" onClick={onClose}>
            <div className="reroll-modal" onClick={e => e.stopPropagation()}>
                <div className="reroll-modal-header">
                    <h3>Reroll: {target.stepName}</h3>
                    <div className="reroll-modal-target">{targetLabel}</div>
                    <button className="reroll-modal-close" onClick={onClose} aria-label="Close">
                        ×
                    </button>
                </div>

                <div className="reroll-modal-body">{renderBody()}</div>
            </div>
        </div>
    );
}
