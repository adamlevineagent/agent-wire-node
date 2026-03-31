import { useEffect, useRef, ReactNode, useCallback } from 'react';

interface SlideOverPanelProps {
    open: boolean;
    onClose: () => void;
    children: ReactNode;
    width?: number;
    className?: string;
}

/**
 * Generic slide-over panel — fixed right-side drawer with escape-key close
 * and scroll-to-top on open. CSS pattern extracted from PyramidDetailDrawer.
 */
export function SlideOverPanel({ open, onClose, children, width = 400, className = '' }: SlideOverPanelProps) {
    const panelRef = useRef<HTMLDivElement>(null);

    // Escape key closes
    const handleKeyDown = useCallback((e: KeyboardEvent) => {
        if (e.key === 'Escape') onClose();
    }, [onClose]);

    // Track previous open state to only scroll on initial open, not re-renders
    const prevOpenRef = useRef(false);

    useEffect(() => {
        if (open) {
            document.addEventListener('keydown', handleKeyDown);
            // Scroll to top only on transition from closed → open
            if (!prevOpenRef.current && panelRef.current) {
                panelRef.current.scrollTop = 0;
            }
        }
        prevOpenRef.current = open;
        return () => document.removeEventListener('keydown', handleKeyDown);
    }, [open, handleKeyDown]);

    return (
        <div
            ref={panelRef}
            className={`slide-over-panel ${open ? '' : 'slide-over-panel-hidden'} ${className}`}
            style={{ width: `${width}px` }}
        >
            <div className="slide-over-panel-header">
                <button className="slide-over-panel-close" onClick={onClose} title="Close">
                    {'\u2715'}
                </button>
            </div>
            <div className="slide-over-panel-body">
                {children}
            </div>
        </div>
    );
}
