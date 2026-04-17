// AdvancedDrawer.tsx — Collapsible expert surface
//
// The compute market's primary tab is framed as a cooperative network —
// the hero surface stays in capability/contribution language, never
// "earn" or "sell" or "market." But the mechanism underneath is real
// market machinery: rates, offers, queue discount curves, settlement
// paths, the full market surface. Power operators + anyone debugging
// needs all of that visible.
//
// The AdvancedDrawer is the bargain: one click surfaces the full
// mechanism. Default-closed, so the tester-mode experience stays
// clean. No hand-holding copy inside — this is the expert view, it
// can look like one.

import { useState } from "react";

interface AdvancedDrawerProps {
    /// Collapsed-state label. Default: "Advanced"; callers can
    /// override for specific sub-sections (e.g. "Advanced — rates,
    /// offers, market inspector" on the compute market).
    label?: string;
    /// Short one-line hint shown alongside the label when collapsed.
    hint?: string;
    /// Whether to expand by default. Only pass `true` when an operator
    /// has an expressed reason to be in advanced (URL param, persisted
    /// preference) — never as a UI default.
    defaultOpen?: boolean;
    children: React.ReactNode;
}

export function AdvancedDrawer({
    label = "Advanced",
    hint,
    defaultOpen = false,
    children,
}: AdvancedDrawerProps) {
    const [open, setOpen] = useState(defaultOpen);

    return (
        <div className={`advanced-drawer ${open ? "advanced-drawer-open" : "advanced-drawer-closed"}`}>
            <button
                className="advanced-drawer-toggle"
                onClick={() => setOpen(!open)}
                aria-expanded={open}
            >
                <span className={`advanced-drawer-caret ${open ? "advanced-drawer-caret-open" : ""}`}>
                    ▸
                </span>
                <span className="advanced-drawer-label">{label}</span>
                {hint && !open && <span className="advanced-drawer-hint">— {hint}</span>}
            </button>

            {open && <div className="advanced-drawer-body">{children}</div>}
        </div>
    );
}
