import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

// ─── Types ──────────────────────────────────────────────────────────

interface RelayMessage {
    id: string;
    sender_type: string;
    sender_id: string;
    target_type: string;
    target_id: string | null;
    subject: string | null;
    body: string;
    message_type: string;
    read_at: string | null;
    created_at: string;
    reply_to_id: string | null;
    metadata: Record<string, any> | null;
    status: string;
    resolved_at: string | null;
    dismissed_at: string | null;
}

// ─── Helpers ────────────────────────────────────────────────────────

function timeAgo(timestamp: string): string {
    const diff = Date.now() - new Date(timestamp).getTime();
    if (diff < 60_000) return "just now";
    if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
    if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
    return `${Math.floor(diff / 86_400_000)}d ago`;
}

function formatDate(timestamp: string): string {
    return new Date(timestamp).toLocaleDateString(undefined, {
        month: "short",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
    });
}

const typeIcons: Record<string, string> = {
    bug_report: "🐛",
    message: "💬",
    update: "📦",
    announcement: "📢",
};

const statusColors: Record<string, string> = {
    open: "status-open",
    investigating: "status-investigating",
    in_progress: "status-in-progress",
    resolved: "status-resolved",
    closed: "status-closed",
};

// ─── Component ──────────────────────────────────────────────────────

export function Messages() {
    const [messages, setMessages] = useState<RelayMessage[]>([]);
    const [archivedMessages, setArchivedMessages] = useState<RelayMessage[]>([]);
    const [loading, setLoading] = useState(true);
    const [composing, setComposing] = useState<"bug_report" | "message" | null>(null);
    const [body, setBody] = useState("");
    const [subject, setSubject] = useState("");
    const [sending, setSending] = useState(false);
    const [expandedThread, setExpandedThread] = useState<string | null>(null);
    const [showArchived, setShowArchived] = useState(false);

    const fetchMessages = useCallback(async () => {
        try {
            const msgs = await invoke<RelayMessage[]>("get_messages");
            setMessages(msgs);
        } catch (err) {
            console.error("Failed to fetch messages:", err);
        } finally {
            setLoading(false);
        }
    }, []);

    const fetchArchived = useCallback(async () => {
        try {
            const msgs = await invoke<RelayMessage[]>("get_archived_messages");
            setArchivedMessages(msgs);
        } catch (err) {
            console.error("Failed to fetch archived messages:", err);
        }
    }, []);

    useEffect(() => {
        fetchMessages();
        const interval = setInterval(fetchMessages, 30_000);
        return () => clearInterval(interval);
    }, [fetchMessages]);

    // Load archived when toggle is opened
    useEffect(() => {
        if (showArchived && archivedMessages.length === 0) {
            fetchArchived();
        }
    }, [showArchived, archivedMessages.length, fetchArchived]);

    const handleSend = async () => {
        if (!body.trim() || !composing) return;
        setSending(true);
        try {
            await invoke("send_message", {
                body: body.trim(),
                messageType: composing,
                subject: subject.trim() || null,
            });
            setComposing(null);
            setBody("");
            setSubject("");
            fetchMessages();
        } catch (err) {
            console.error("Send failed:", err);
        } finally {
            setSending(false);
        }
    };

    const handleMarkRead = async (id: string) => {
        try {
            await invoke("mark_message_read", { messageId: id });
            setMessages((prev) =>
                prev.map((m) => (m.id === id ? { ...m, read_at: new Date().toISOString() } : m))
            );
        } catch (err) {
            console.error("Mark read failed:", err);
        }
    };

    const handleDismiss = async (id: string) => {
        try {
            await invoke("dismiss_message", { messageId: id });
            // Remove from active messages
            setMessages((prev) => prev.filter((m) => m.id !== id));
            // If archived view is open, refresh it
            if (showArchived) fetchArchived();
        } catch (err) {
            console.error("Dismiss failed:", err);
        }
    };

    // Group messages into threads
    const rootMessages = messages.filter((m) => !m.reply_to_id);
    const replies = messages.filter((m) => m.reply_to_id);
    const threadMap = new Map<string, RelayMessage[]>();
    for (const r of replies) {
        const existing = threadMap.get(r.reply_to_id!) || [];
        existing.push(r);
        threadMap.set(r.reply_to_id!, existing);
    }

    // Same for archived
    const archivedRoots = archivedMessages.filter((m) => !m.reply_to_id);
    const archivedReplies = archivedMessages.filter((m) => m.reply_to_id);
    const archivedThreadMap = new Map<string, RelayMessage[]>();
    for (const r of archivedReplies) {
        const existing = archivedThreadMap.get(r.reply_to_id!) || [];
        existing.push(r);
        archivedThreadMap.set(r.reply_to_id!, existing);
    }

    const unreadCount = messages.filter((m) => !m.read_at && m.sender_type === "admin").length;

    const isDismissable = (msg: RelayMessage) =>
        msg.status === "resolved" || msg.status === "closed";

    if (loading) {
        return (
            <div className="messages-loading">
                <div className="loading-pulse">Loading messages…</div>
            </div>
        );
    }

    const renderMessageCard = (
        msg: RelayMessage,
        thread: RelayMessage[],
        opts?: { archived?: boolean }
    ) => {
        const isExpanded = expandedThread === msg.id;
        const isFromAdmin = msg.sender_type === "admin";
        const isUnread = !msg.read_at && isFromAdmin;
        const canDismiss = isDismissable(msg) && !opts?.archived;
        const isResolved = msg.status === "resolved" || msg.status === "closed";

        return (
            <div
                key={msg.id}
                className={`message-card ${isUnread ? "message-unread" : ""} ${opts?.archived ? "message-archived" : ""} ${isResolved && !opts?.archived ? "message-resolved" : ""}`}
            >
                <div className="message-card-header" onClick={() => thread.length > 0 && setExpandedThread(isExpanded ? null : msg.id)}>
                    <div className="message-card-left">
                        <span className="message-icon">{typeIcons[msg.message_type] || "💬"}</span>
                        <div className="message-card-info">
                            <div className="message-card-top-row">
                                <span className={`message-type-badge message-type-${msg.message_type}`}>
                                    {msg.message_type.replace("_", " ")}
                                </span>
                                {msg.status && msg.status !== "open" && (
                                    <span className={`message-status-pill ${statusColors[msg.status] || ""}`}>
                                        {msg.status.replace("_", " ")}
                                    </span>
                                )}
                                <span className="message-from">
                                    {isFromAdmin ? "from Wire" : "you"}
                                </span>
                            </div>
                            {msg.subject && (
                                <div className="message-subject">{msg.subject}</div>
                            )}
                            <div className="message-body-preview">
                                {msg.body.length > 120 ? msg.body.slice(0, 120) + "…" : msg.body}
                            </div>
                        </div>
                    </div>
                    <div className="message-card-right">
                        <span className="message-time" title={formatDate(msg.created_at)}>
                            {timeAgo(msg.created_at)}
                        </span>
                        {thread.length > 0 && (
                            <span className="thread-count">
                                {thread.length} {thread.length === 1 ? "reply" : "replies"}
                            </span>
                        )}
                        {isUnread && (
                            <button
                                className="mark-read-btn"
                                onClick={(e) => {
                                    e.stopPropagation();
                                    handleMarkRead(msg.id);
                                }}
                            >
                                ✓ Read
                            </button>
                        )}
                        {canDismiss && (
                            <button
                                className="dismiss-btn"
                                onClick={(e) => {
                                    e.stopPropagation();
                                    handleDismiss(msg.id);
                                }}
                                title="Remove from inbox"
                            >
                                ✓ Dismiss
                            </button>
                        )}
                    </div>
                </div>

                {/* Thread replies */}
                {isExpanded && thread.length > 0 && (
                    <div className="thread-replies">
                        {thread
                            .sort((a, b) => new Date(a.created_at).getTime() - new Date(b.created_at).getTime())
                            .map((reply) => (
                                <div key={reply.id} className={`thread-reply ${reply.sender_type === "admin" ? "reply-admin" : "reply-operator"}`}>
                                    <div className="reply-meta">
                                        <span className="reply-from">
                                            {reply.sender_type === "admin" ? "Wire" : "You"}
                                        </span>
                                        <span className="reply-time">{timeAgo(reply.created_at)}</span>
                                    </div>
                                    <div className="reply-body">{reply.body}</div>
                                </div>
                            ))}
                    </div>
                )}
            </div>
        );
    };

    return (
        <div className="messages-panel">
            {/* Header */}
            <div className="messages-header">
                <div className="messages-title">
                    📬 Inbox
                    {unreadCount > 0 && (
                        <span className="unread-badge">{unreadCount}</span>
                    )}
                </div>
                <div className="messages-actions">
                    <button
                        className="compose-btn compose-bug"
                        onClick={() => setComposing(composing === "bug_report" ? null : "bug_report")}
                    >
                        {composing === "bug_report" ? "✕ Cancel" : "🐛 Report Bug"}
                    </button>
                    <button
                        className="compose-btn compose-msg"
                        onClick={() => setComposing(composing === "message" ? null : "message")}
                    >
                        {composing === "message" ? "✕ Cancel" : "💬 Message"}
                    </button>
                </div>
            </div>

            {/* Compose */}
            {composing && (
                <div className={`compose-panel ${composing === "bug_report" ? "compose-bug-panel" : ""}`}>
                    <div className="compose-header">
                        {composing === "bug_report" ? "🐛 Report a Bug" : "💬 Send a Message"}
                    </div>
                    <input
                        type="text"
                        value={subject}
                        onChange={(e) => setSubject(e.target.value)}
                        placeholder={composing === "bug_report" ? "Brief description of the issue" : "Subject (optional)"}
                        className="compose-input"
                    />
                    <textarea
                        value={body}
                        onChange={(e) => setBody(e.target.value)}
                        placeholder={
                            composing === "bug_report"
                                ? "What happened? What were you doing when it occurred?"
                                : "Your message…"
                        }
                        rows={4}
                        className="compose-textarea"
                    />
                    {composing === "bug_report" && (
                        <div className="compose-diagnostics-note">
                            🔧 System diagnostics (health, version, OS) will be automatically included
                        </div>
                    )}
                    <button
                        className="send-btn"
                        onClick={handleSend}
                        disabled={sending || !body.trim()}
                    >
                        {sending ? "Sending…" : composing === "bug_report" ? "Submit Bug Report" : "Send Message"}
                    </button>
                </div>
            )}

            {/* Active message list */}
            <div className="messages-list">
                {rootMessages.length === 0 && replies.length === 0 ? (
                    <div className="messages-empty">
                        <div className="messages-empty-icon">📬</div>
                        <p className="messages-empty-title">Your inbox is empty</p>
                        <p className="messages-empty-desc">
                            We'll reach out here with updates and announcements.
                            You can report issues or send us a note anytime.
                        </p>
                    </div>
                ) : (
                    rootMessages.map((msg) => {
                        const thread = threadMap.get(msg.id) || [];
                        return renderMessageCard(msg, thread);
                    })
                )}
            </div>

            {/* Archived toggle */}
            <div className="archived-section">
                <button
                    className="archived-toggle"
                    onClick={() => {
                        setShowArchived(!showArchived);
                        if (!showArchived) fetchArchived();
                    }}
                >
                    {showArchived ? "▲ Hide archived" : `▼ Show archived (${archivedRoots.length || "…"})`}
                </button>

                {showArchived && (
                    <div className="archived-list">
                        {archivedRoots.length === 0 ? (
                            <div className="archived-empty">No archived messages</div>
                        ) : (
                            archivedRoots.map((msg) => {
                                const thread = archivedThreadMap.get(msg.id) || [];
                                return renderMessageCard(msg, thread, { archived: true });
                            })
                        )}
                    </div>
                )}
            </div>
        </div>
    );
}
