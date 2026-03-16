import { useState } from "react";

interface LoginScreenProps {
    onMagicLink: (email: string) => Promise<void>;
    onVerifyLink: (magicLinkUrl: string, email: string) => Promise<void>;
    onLogin: (email: string, password: string) => Promise<void>;
}

type Step = "email" | "waiting";
type AuthMode = "magic" | "password";

export function LoginScreen({ onMagicLink, onVerifyLink, onLogin }: LoginScreenProps) {
    const [step, setStep] = useState<Step>("email");
    const [email, setEmail] = useState("");
    const [password, setPassword] = useState("");
    const [linkUrl, setLinkUrl] = useState("");
    const [error, setError] = useState("");
    const [loading, setLoading] = useState(false);
    const [showManualPaste, setShowManualPaste] = useState(false);
    const [authMode, setAuthMode] = useState<AuthMode>("magic");

    const handleSendLink = async (e: React.FormEvent) => {
        e.preventDefault();
        setError("");
        setLoading(true);
        try {
            await onMagicLink(email);
            setStep("waiting");
        } catch (err: any) {
            setError(err?.toString() || "Failed to send magic link");
        } finally {
            setLoading(false);
        }
    };

    const handlePasswordLogin = async (e: React.FormEvent) => {
        e.preventDefault();
        setError("");
        setLoading(true);
        try {
            await onLogin(email, password);
        } catch (err: any) {
            setError(err?.toString() || "Login failed");
        } finally {
            setLoading(false);
        }
    };

    const handleVerify = async (e: React.FormEvent) => {
        e.preventDefault();
        setError("");
        setLoading(true);
        try {
            await onVerifyLink(linkUrl.trim(), email);
        } catch (err: any) {
            setError(err?.toString() || "Verification failed");
        } finally {
            setLoading(false);
        }
    };

    return (
        <div className="login-screen">
            <div className="login-card">
                <div className="login-header">
                    <div className="wire-logo-login">W</div>
                    <h1>Wire Node</h1>
                    <p className="login-subtitle">
                        Host documents, earn credits, power the Wire
                    </p>
                </div>

                {step === "email" ? (
                    <>
                        {/* Auth mode toggle */}
                        <div className="auth-mode-toggle">
                            <button
                                className={`auth-mode-btn ${authMode === "magic" ? "active" : ""}`}
                                onClick={() => setAuthMode("magic")}
                                type="button"
                            >
                                Magic Link
                            </button>
                            <button
                                className={`auth-mode-btn ${authMode === "password" ? "active" : ""}`}
                                onClick={() => setAuthMode("password")}
                                type="button"
                            >
                                Password
                            </button>
                        </div>

                        {authMode === "magic" ? (
                            <form onSubmit={handleSendLink} className="login-form">
                                <div className="form-group">
                                    <label htmlFor="email">Email</label>
                                    <input
                                        id="email"
                                        type="email"
                                        value={email}
                                        onChange={(e) => setEmail(e.target.value)}
                                        placeholder="your@email.com"
                                        required
                                        autoFocus
                                    />
                                </div>

                                {error && <div className="login-error">{error}</div>}

                                <button
                                    type="submit"
                                    className="login-button"
                                    disabled={loading}
                                >
                                    {loading ? "Sending..." : "Send Magic Link"}
                                </button>

                                <p className="login-footer">
                                    We'll email you a link -- click it and you're in.
                                    Deep links use <code>agentwire://</code>
                                </p>
                            </form>
                        ) : (
                            <form onSubmit={handlePasswordLogin} className="login-form">
                                <div className="form-group">
                                    <label htmlFor="email-pw">Email</label>
                                    <input
                                        id="email-pw"
                                        type="email"
                                        value={email}
                                        onChange={(e) => setEmail(e.target.value)}
                                        placeholder="your@email.com"
                                        required
                                        autoFocus
                                    />
                                </div>

                                <div className="form-group">
                                    <label htmlFor="password">Password</label>
                                    <input
                                        id="password"
                                        type="password"
                                        value={password}
                                        onChange={(e) => setPassword(e.target.value)}
                                        placeholder="Your password"
                                        required
                                    />
                                </div>

                                {error && <div className="login-error">{error}</div>}

                                <button
                                    type="submit"
                                    className="login-button"
                                    disabled={loading}
                                >
                                    {loading ? "Logging in..." : "Log In"}
                                </button>
                            </form>
                        )}
                    </>
                ) : (
                    <div className="login-form">
                        <div className="login-success">
                            Check your email and click the link
                        </div>

                        <p className="login-waiting-text">
                            Click the button in the email we just sent to <strong>{email}</strong>.
                            This app will log you in automatically via <code>agentwire://</code> deep link.
                        </p>

                        <div className="login-pulse-container">
                            <div className="login-pulse" />
                            <span>Waiting for you to click the link...</span>
                        </div>

                        {/* Manual paste fallback for Linux / edge cases */}
                        <button
                            type="button"
                            className="trouble-link"
                            onClick={() => setShowManualPaste(!showManualPaste)}
                        >
                            {showManualPaste ? "Hide manual login" : "Having trouble?"}
                        </button>

                        {showManualPaste && (
                            <form onSubmit={handleVerify} className="manual-paste-form">
                                <div className="form-group">
                                    <label htmlFor="magic-link">
                                        Right-click the email button, copy link, then paste here
                                    </label>
                                    <input
                                        id="magic-link"
                                        type="text"
                                        value={linkUrl}
                                        onChange={(e) => setLinkUrl(e.target.value)}
                                        placeholder="https://...supabase.../auth/v1/verify?token=..."
                                        required
                                        style={{ fontSize: "0.75rem" }}
                                    />
                                </div>

                                {error && <div className="login-error">{error}</div>}

                                <button
                                    type="submit"
                                    className="login-button"
                                    disabled={loading || linkUrl.length < 20}
                                >
                                    {loading ? "Verifying..." : "Verify Link"}
                                </button>
                            </form>
                        )}

                        <button
                            type="button"
                            className="back-link"
                            onClick={() => {
                                setStep("email");
                                setLinkUrl("");
                                setError("");
                                setShowManualPaste(false);
                            }}
                        >
                            &larr; Use a different email
                        </button>
                    </div>
                )}
            </div>
        </div>
    );
}
