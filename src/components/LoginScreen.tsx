import { useState } from "react";

interface LoginScreenProps {
    onMagicLink: (email: string) => Promise<void>;
    onVerifyLink: (magicLinkUrl: string, email: string) => Promise<void>;
    onVerifyOtp: (email: string, otpCode: string) => Promise<void>;
    onLogin: (email: string, password: string) => Promise<void>;
}

type Step = "email" | "waiting";
type AuthMode = "magic" | "password";

export function LoginScreen({ onMagicLink, onVerifyLink, onVerifyOtp, onLogin }: LoginScreenProps) {
    const [step, setStep] = useState<Step>("email");
    const [email, setEmail] = useState("");
    const [password, setPassword] = useState("");
    const [linkUrl, setLinkUrl] = useState("");
    const [otpCode, setOtpCode] = useState("");
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

    const handleOtpSubmit = async (e: React.FormEvent) => {
        e.preventDefault();
        setError("");
        setLoading(true);
        try {
            await onVerifyOtp(email, otpCode.trim());
        } catch (err: any) {
            setError(err?.toString() || "Code verification failed");
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
                            Check your email for a verification code
                        </div>

                        <p className="login-waiting-text">
                            We sent a 6-digit code to <strong>{email}</strong>.
                            Enter it below to sign in.
                        </p>

                        <form onSubmit={handleOtpSubmit} className="otp-form">
                            <div className="form-group">
                                <input
                                    id="otp-code"
                                    type="text"
                                    inputMode="numeric"
                                    pattern="[0-9]*"
                                    maxLength={6}
                                    value={otpCode}
                                    onChange={(e) => setOtpCode(e.target.value.replace(/\D/g, ""))}
                                    placeholder="000000"
                                    required
                                    autoFocus
                                    style={{
                                        textAlign: "center",
                                        fontSize: "1.75rem",
                                        letterSpacing: "0.5em",
                                        fontFamily: "monospace",
                                    }}
                                />
                            </div>

                            {error && <div className="login-error">{error}</div>}

                            <button
                                type="submit"
                                className="login-button"
                                disabled={loading || otpCode.length < 6}
                            >
                                {loading ? "Verifying..." : "Verify Code"}
                            </button>
                        </form>

                        <div className="login-pulse-container" style={{ marginTop: "1rem" }}>
                            <div className="login-pulse" />
                            <span>Or click the link in your email...</span>
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
                                setOtpCode("");
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
