import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AppShell } from "./components/AppShell";
import { AppProvider } from "./contexts/AppContext";
import { LoginScreen } from "./components/LoginScreen";
import { OnboardingWizard } from "./components/OnboardingWizard";

interface AuthState {
    access_token: string | null;
    email: string | null;
    node_id: string | null;
    user_id: string | null;
}

export default function App() {
    const [authState, setAuthState] = useState<AuthState | null>(null);
    const [checking, setChecking] = useState(true);
    const [onboarded, setOnboarded] = useState<boolean | null>(null);

    // Check current auth state + onboarding status on mount
    useEffect(() => {
        Promise.all([
            invoke<AuthState>("get_auth_state"),
            invoke<boolean>("is_onboarded"),
        ])
            .then(([state, isOnboarded]) => {
                setAuthState(state);
                setOnboarded(isOnboarded);
                setChecking(false);
                // Show the window now that the frontend is ready
                getCurrentWindow().show();
            })
            .catch(() => {
                setChecking(false);
                getCurrentWindow().show();
            });
    }, []);

    // Poll for auth state changes (deep link callback)
    useEffect(() => {
        if (authState?.access_token) return;

        const interval = setInterval(async () => {
            try {
                const state = await invoke<AuthState>("get_auth_state");
                if (state.access_token) {
                    setAuthState(state);
                }
            } catch { }
        }, 1500);

        return () => clearInterval(interval);
    }, [authState?.access_token]);

    const handleMagicLink = useCallback(async (email: string) => {
        await invoke("send_magic_link", { email });
    }, []);

    const handleVerifyLink = useCallback(async (magicLinkUrl: string, email: string) => {
        await invoke("verify_magic_link", { magicLinkUrl, email });
        const state = await invoke<AuthState>("get_auth_state");
        setAuthState(state);
    }, []);

    const handleVerifyOtp = useCallback(async (email: string, otpCode: string) => {
        await invoke("verify_otp", { email, otpCode });
        const state = await invoke<AuthState>("get_auth_state");
        setAuthState(state);
    }, []);

    const handleLogin = useCallback(async (email: string, password: string) => {
        await invoke("login", { email, password });
        const state = await invoke<AuthState>("get_auth_state");
        setAuthState(state);
    }, []);

    const handleLogout = useCallback(async () => {
        try {
            await invoke("logout");
            setAuthState(null);
        } catch (e) {
            console.error("Logout failed:", e);
        }
    }, []);

    const handleOnboardingComplete = useCallback(() => {
        setOnboarded(true);
    }, []);

    if (checking) {
        return (
            <div className="loading-screen">
                <div className="wire-logo-loading">W</div>
                <div className="tunnel-title">Wire Node</div>
                <div className="loading-spinner" />
            </div>
        );
    }

    if (!authState?.access_token) {
        return (
            <LoginScreen
                onMagicLink={handleMagicLink}
                onVerifyLink={handleVerifyLink}
                onVerifyOtp={handleVerifyOtp}
                onLogin={handleLogin}
            />
        );
    }

    // Show onboarding wizard for first-time users
    if (onboarded === false) {
        return (
            <OnboardingWizard
                onComplete={handleOnboardingComplete}
                defaultNodeName={authState.email?.split("@")[0] || "Wire Node"}
            />
        );
    }

    return (
        <AppProvider email={authState.email}>
            <AppShell onLogout={handleLogout} />
        </AppProvider>
    );
}
