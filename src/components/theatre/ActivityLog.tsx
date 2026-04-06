import { useEffect, useRef } from 'react';
import type { LogEntry } from './types';

interface ActivityLogProps {
    log: LogEntry[];
    collapsed?: boolean;
}

export function ActivityLog({ log, collapsed = false }: ActivityLogProps) {
    const scrollRef = useRef<HTMLDivElement>(null);

    useEffect(() => {
        if (scrollRef.current) {
            scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
        }
    }, [log]);

    if (log.length === 0) return null;

    return (
        <div className={`theatre-log ${collapsed ? 'theatre-log-collapsed' : ''}`}>
            <div className="theatre-log-header">Activity</div>
            <div className="theatre-log-scroll" ref={scrollRef}>
                {log.map((entry, i) => (
                    <div key={i} className="theatre-log-entry">
                        <span className="theatre-log-time">
                            {Math.floor(entry.elapsed_secs / 60)}:{String(Math.floor(entry.elapsed_secs % 60)).padStart(2, '0')}
                        </span>
                        <span className="theatre-log-msg">{entry.message}</span>
                    </div>
                ))}
            </div>
        </div>
    );
}
