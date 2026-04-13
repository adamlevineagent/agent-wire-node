import { useState, useRef, useId, type ReactNode } from "react";

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

    const handleKeyDown = (e: React.KeyboardEvent) => {
        if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            toggle();
        }
    };

    return (
        <div className={`accordion-section ${open ? "accordion-section-open" : ""}`}>
            <button
                id={headerId}
                type="button"
                className="accordion-header"
                onClick={toggle}
                onKeyDown={handleKeyDown}
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
                        ? contentRef.current
                            ? `${contentRef.current.scrollHeight}px`
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
