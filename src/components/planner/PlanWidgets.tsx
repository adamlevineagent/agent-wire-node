import type { PlannerContext, WidgetSchema } from '../../types/planner';

// --- Individual widget components ---

interface BaseWidgetProps {
    widget: WidgetSchema;
    value: unknown;
    onChange: (field: string, value: unknown) => void;
    context: PlannerContext;
}

function CorpusSelector({ widget, value, onChange, context }: BaseWidgetProps) {
    const field = widget.field ?? '';
    return (
        <div className="plan-widget plan-widget-corpus">
            {widget.label && <label className="plan-widget-label">{widget.label}</label>}
            <select
                className="plan-widget-select"
                value={(value as string) ?? ''}
                onChange={(e) => onChange(field, e.target.value)}
            >
                <option value="">Select corpus...</option>
                {context.corpora.map((c) => (
                    <option key={c.slug} value={c.slug}>
                        {c.slug} — {c.path} ({c.doc_count} docs)
                    </option>
                ))}
            </select>
        </div>
    );
}

function TextInputWidget({ widget, value, onChange }: BaseWidgetProps) {
    const field = widget.field ?? '';
    return (
        <div className="plan-widget plan-widget-text">
            {widget.label && <label className="plan-widget-label">{widget.label}</label>}
            <input
                className="plan-widget-input"
                type="text"
                placeholder={widget.placeholder ?? ''}
                value={(value as string) ?? ''}
                onChange={(e) => onChange(field, e.target.value)}
            />
        </div>
    );
}

function CostPreview({ widget, context }: BaseWidgetProps) {
    const amount = widget.amount ?? null;
    const breakdown = widget.breakdown as Record<string, unknown> | undefined;
    const hasQueryCosts = breakdown && 'query' in breakdown;
    const hasActionCosts = breakdown && 'actions' in breakdown;

    return (
        <div className="plan-widget plan-widget-cost">
            {widget.label && <label className="plan-widget-label">{widget.label}</label>}
            <div className="plan-widget-cost-body">
                {amount !== null && (
                    <div className="plan-widget-cost-total">
                        <span className="plan-widget-cost-amount">
                            {amount.toLocaleString()} credits
                        </span>
                        <span className="plan-widget-cost-estimated">estimated</span>
                    </div>
                )}
                {hasQueryCosts && (
                    <div className="plan-widget-cost-line">
                        Queries: <span className="plan-widget-cost-dynamic">dynamic (governor-adjusted)</span>
                    </div>
                )}
                {hasActionCosts && (
                    <div className="plan-widget-cost-line">
                        Actions: {String(breakdown.actions)} credits
                    </div>
                )}
                <div className="plan-widget-cost-balance">
                    Balance: {context.balance.toLocaleString()} credits
                </div>
            </div>
        </div>
    );
}

function ToggleWidget({ widget, value, onChange }: BaseWidgetProps) {
    const field = widget.field ?? '';
    const checked = typeof value === 'boolean' ? value : (widget.default ?? false);
    return (
        <div className="plan-widget plan-widget-toggle">
            <label className="plan-widget-toggle-row">
                <input
                    type="checkbox"
                    checked={checked}
                    onChange={(e) => onChange(field, e.target.checked)}
                />
                <span className="plan-widget-toggle-label">{widget.label ?? field}</span>
            </label>
        </div>
    );
}

function SelectWidget({ widget, value, onChange }: BaseWidgetProps) {
    const field = widget.field ?? '';
    const options = widget.options ?? [];
    return (
        <div className="plan-widget plan-widget-select">
            {widget.label && <label className="plan-widget-label">{widget.label}</label>}
            <select
                className="plan-widget-select"
                value={(value as string) ?? ''}
                onChange={(e) => onChange(field, e.target.value)}
            >
                <option value="">Select...</option>
                {options.map((opt) => (
                    <option key={opt.value} value={opt.value}>
                        {opt.label}
                    </option>
                ))}
            </select>
        </div>
    );
}

function AgentSelector({ widget, value, onChange, context }: BaseWidgetProps) {
    const field = widget.field ?? '';
    return (
        <div className="plan-widget plan-widget-agent">
            {widget.label && <label className="plan-widget-label">{widget.label}</label>}
            <select
                className="plan-widget-select"
                value={(value as string) ?? ''}
                onChange={(e) => onChange(field, e.target.value)}
            >
                <option value="">Select agent...</option>
                {context.agents.map((a) => (
                    <option
                        key={a.id}
                        value={a.id}
                        className={a.status === 'online' ? 'plan-widget-agent-online' : ''}
                    >
                        {a.status === 'online' ? '\u25CF ' : '\u25CB '}{a.name} ({a.status})
                    </option>
                ))}
            </select>
        </div>
    );
}

interface ConfirmationWidgetProps extends BaseWidgetProps {
    onApprove: () => void;
    onCancel: () => void;
}

function ConfirmationWidget({ widget, onApprove, onCancel }: ConfirmationWidgetProps) {
    return (
        <div className="plan-widget plan-widget-confirmation">
            {widget.summary && (
                <div className="plan-widget-confirmation-summary">{widget.summary}</div>
            )}
            {widget.details && (
                <div className="plan-widget-confirmation-details">{widget.details}</div>
            )}
            <div className="plan-widget-confirmation-actions">
                <button
                    className="plan-widget-btn plan-widget-btn-approve"
                    onClick={onApprove}
                >
                    Approve
                </button>
                <button
                    className="plan-widget-btn plan-widget-btn-cancel"
                    onClick={onCancel}
                >
                    Cancel
                </button>
            </div>
        </div>
    );
}

// --- Dispatcher ---

interface PlanWidgetProps {
    widget: WidgetSchema;
    value: unknown;
    onChange: (field: string, value: unknown) => void;
    context: PlannerContext;
    onApprove?: () => void;
    onCancel?: () => void;
}

export function PlanWidget({
    widget,
    value,
    onChange,
    context,
    onApprove,
    onCancel,
}: PlanWidgetProps) {
    switch (widget.type) {
        case 'corpus_selector':
            return <CorpusSelector widget={widget} value={value} onChange={onChange} context={context} />;
        case 'text_input':
            return <TextInputWidget widget={widget} value={value} onChange={onChange} context={context} />;
        case 'cost_preview':
            return <CostPreview widget={widget} value={value} onChange={onChange} context={context} />;
        case 'select':
            return <SelectWidget widget={widget} value={value} onChange={onChange} context={context} />;
        case 'toggle':
        case 'checkbox':
            return <ToggleWidget widget={widget} value={value} onChange={onChange} context={context} />;
        case 'agent_selector':
            return <AgentSelector widget={widget} value={value} onChange={onChange} context={context} />;
        case 'confirmation':
            return (
                <ConfirmationWidget
                    widget={widget}
                    value={value}
                    onChange={onChange}
                    context={context}
                    onApprove={onApprove ?? (() => {})}
                    onCancel={onCancel ?? (() => {})}
                />
            );
        default:
            // Visible fallback so unknown widget types don't silently disappear
            return (
                <div className="plan-widget plan-widget-unknown">
                    <span className="plan-widget-label">Unknown widget: {widget.type}</span>
                </div>
            );
    }
}
