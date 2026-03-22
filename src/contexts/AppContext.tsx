import { createContext, useContext, useReducer, useCallback, ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { CreditStats, SyncState } from '../components/Dashboard';

// --- Types -------------------------------------------------------------------

export type Mode = 'dashboard' | 'pyramids' | 'search' | 'warroom' | 'compose' | 'agents' | 'node' | 'activity' | 'identity' | 'settings';

export interface ViewStackEntry {
    view: string;
    props: Record<string, unknown>;
}

export interface TunnelStatusData {
    tunnel_id: string | null;
    tunnel_url: string | null;
    status: string | { Error: string };
}

export interface AppState {
    operatorId: string | null;
    operatorSessionToken: string | null;
    email: string | null;
    creditBalance: number;
    tunnelStatus: TunnelStatusData | null;
    credits: CreditStats | null;
    syncState: SyncState | null;
    notificationCount: number;
    activeMode: Mode;
    modeStacks: Record<Mode, ViewStackEntry[]>;
}

// --- Actions -----------------------------------------------------------------

type AppAction =
    | { type: 'SET_MODE'; mode: Mode }
    | { type: 'PUSH_VIEW'; mode: Mode; entry: ViewStackEntry }
    | { type: 'POP_VIEW'; mode: Mode }
    | { type: 'NAVIGATE_VIEW'; mode: Mode; entry: ViewStackEntry }
    | { type: 'SET_CREDITS'; credits: CreditStats | null }
    | { type: 'SET_SYNC_STATE'; syncState: SyncState | null }
    | { type: 'SET_TUNNEL_STATUS'; tunnelStatus: TunnelStatusData | null }
    | { type: 'SET_OPERATOR_SESSION'; operatorId: string | null; operatorSessionToken: string | null }
    | { type: 'SET_NOTIFICATION_COUNT'; count: number }
    | { type: 'SET_EMAIL'; email: string | null };

// --- Initial State -----------------------------------------------------------

const ALL_MODES: Mode[] = ['dashboard', 'pyramids', 'search', 'warroom', 'compose', 'agents', 'node', 'activity', 'identity', 'settings'];

function createInitialModeStacks(): Record<Mode, ViewStackEntry[]> {
    const stacks: Record<string, ViewStackEntry[]> = {};
    for (const mode of ALL_MODES) {
        stacks[mode] = [{ view: 'root', props: {} }];
    }
    return stacks as Record<Mode, ViewStackEntry[]>;
}

const initialState: AppState = {
    operatorId: null,
    operatorSessionToken: null,
    email: null,
    creditBalance: 0,
    tunnelStatus: null,
    credits: null,
    syncState: null,
    notificationCount: 0,
    activeMode: 'pyramids',
    modeStacks: createInitialModeStacks(),
};

// --- Reducer -----------------------------------------------------------------

function appReducer(state: AppState, action: AppAction): AppState {
    switch (action.type) {
        case 'SET_MODE':
            return { ...state, activeMode: action.mode };

        case 'PUSH_VIEW': {
            const stack = [...state.modeStacks[action.mode], action.entry];
            return {
                ...state,
                modeStacks: { ...state.modeStacks, [action.mode]: stack },
            };
        }

        case 'POP_VIEW': {
            const stack = state.modeStacks[action.mode];
            if (stack.length <= 1) return state; // don't pop root
            return {
                ...state,
                modeStacks: { ...state.modeStacks, [action.mode]: stack.slice(0, -1) },
            };
        }

        case 'NAVIGATE_VIEW': {
            // Replace entire stack with root + new entry
            return {
                ...state,
                modeStacks: {
                    ...state.modeStacks,
                    [action.mode]: [{ view: 'root', props: {} }, action.entry],
                },
            };
        }

        case 'SET_CREDITS':
            return {
                ...state,
                credits: action.credits,
                creditBalance: action.credits?.server_credit_balance ?? action.credits?.credits_earned ?? 0,
            };

        case 'SET_SYNC_STATE':
            return { ...state, syncState: action.syncState };

        case 'SET_TUNNEL_STATUS':
            return { ...state, tunnelStatus: action.tunnelStatus };

        case 'SET_OPERATOR_SESSION':
            return {
                ...state,
                operatorId: action.operatorId,
                operatorSessionToken: action.operatorSessionToken,
            };

        case 'SET_NOTIFICATION_COUNT':
            return { ...state, notificationCount: action.count };

        case 'SET_EMAIL':
            return { ...state, email: action.email };

        default:
            return state;
    }
}

// --- Context -----------------------------------------------------------------

interface AppContextValue {
    state: AppState;
    dispatch: React.Dispatch<AppAction>;
    setMode: (mode: Mode) => void;
    pushView: (mode: Mode, view: string, props?: Record<string, unknown>) => void;
    popView: (mode: Mode) => void;
    navigateView: (mode: Mode, view: string, props?: Record<string, unknown>) => void;
    currentView: (mode: Mode) => ViewStackEntry;
    operatorApiCall: (method: string, path: string, body?: unknown) => Promise<unknown>;
}

const AppContext = createContext<AppContextValue | null>(null);

export function useAppContext(): AppContextValue {
    const ctx = useContext(AppContext);
    if (!ctx) throw new Error('useAppContext must be used within AppProvider');
    return ctx;
}

// --- Provider ----------------------------------------------------------------

interface AppProviderProps {
    children: ReactNode;
    email?: string | null;
}

export function AppProvider({ children, email }: AppProviderProps) {
    const [state, dispatch] = useReducer(appReducer, {
        ...initialState,
        email: email ?? null,
    });

    const setMode = useCallback((mode: Mode) => {
        dispatch({ type: 'SET_MODE', mode });
    }, []);

    const pushView = useCallback((mode: Mode, view: string, props: Record<string, unknown> = {}) => {
        dispatch({ type: 'PUSH_VIEW', mode, entry: { view, props } });
    }, []);

    const popView = useCallback((mode: Mode) => {
        dispatch({ type: 'POP_VIEW', mode });
    }, []);

    const navigateView = useCallback((mode: Mode, view: string, props: Record<string, unknown> = {}) => {
        dispatch({ type: 'NAVIGATE_VIEW', mode, entry: { view, props } });
    }, []);

    const currentView = useCallback((mode: Mode): ViewStackEntry => {
        const stack = state.modeStacks[mode];
        return stack[stack.length - 1];
    }, [state.modeStacks]);

    const operatorApiCall = useCallback(async (method: string, path: string, body?: unknown) => {
        return invoke('operator_api_call', { method, path, body: body ?? null });
    }, []);

    return (
        <AppContext.Provider value={{
            state,
            dispatch,
            setMode,
            pushView,
            popView,
            navigateView,
            currentView,
            operatorApiCall,
        }}>
            {children}
        </AppContext.Provider>
    );
}
