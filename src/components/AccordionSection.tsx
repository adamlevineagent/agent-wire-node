import { useState, useRef, useEffect, useId, type ReactNode } from "react";

interface AccordionSectionProps {
    title: string;
    defaultOpen?: boolean;
    onToggle?: (open: boolean) => void;
    children: ReactNode;
}

export function AccordionSection({
    title,
    defaultOpen = false,
    onToggle,
    children,
}: AccordionSectionProps) {
    const [open, setOpen] = useState(defaultOpen);
    const [height, setHeight] = useState<number | null>(null);
    const contentRef = useRef<HTMLDivElement>(null);
    const id = useId();
    const headerId = `${id}-header`;
    const panelId = `${id}-panel`;

    const toggle = () => {
        setOpen((prev) => {
            const next = !prev;
            onToggle?.(next);
            return next;
        });
    };

    // ResizeObserver: recalculate height when children change size
    // (async-loaded content, confirmation dialogs, etc.)
    useEffect(() => {
        const el = contentRef.current;
        if (!el || !open) return;

        const update = () => setHeight(el.scrollHeight);
        update();

        const observer = new ResizeObserver(update);
        observer.observe(el);
        return () => observer.disconnect();
    }, [open, children]);

    return (
        <div className={`accordion-section ${open ? "accordion-section-open" : ""}`}>
            <button
                id={headerId}
                type="button"
                className="accordion-header"
                onClick={toggle}
                aria-expanded={open}
                aria-controls={panelId}
            >
                <span className={`accordion-chevron ${open ? "accordion-chevron-open" : ""}`}>
                    &#x25B8;
                </span>
                <span className="accordion-title">{title}</span>
            </button>
            <div
                id={panelId}
                role="region"
                aria-labelledby={headerId}
                className="accordion-content"
                style={{
                    maxHeight: open
                        ? height != null
                            ? `${height}px`
                            : "2000px"
                        : "0px",
                }}
            >
                <div ref={contentRef} className="accordion-content-inner">
                    {children}
                </div>
            </div>
        </div>
    );
}
