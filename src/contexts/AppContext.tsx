import { createContext, useContext, useReducer, useCallback, useMemo, useRef, ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { CreditStats, SyncState } from '../components/Dashboard';
import type { OperationEntry } from '../types/planner';

// --- Types -------------------------------------------------------------------

export type Mode = 'pyramids' | 'knowledge' | 'tools' | 'fleet' | 'operations' | 'search' | 'compose' | 'dashboard' | 'identity' | 'settings';

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
    messageCount: number;
    activeMode: Mode;
    modeStacks: Record<Mode, ViewStackEntry[]>;
    // Sidebar live status fields
    pyramidCount: number;
    latestApexQuestion: string | null;
    fleetOnlineCount: number;
    taskCount: number;
    draftCount: number;
    docCount: number;
    corpusCount: number;
    lastSyncTime: string | null;
    activeOperations: OperationEntry[];
    autoExecute: boolean;
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
    | { type: 'SET_MESSAGE_COUNT'; count: number }
    | { type: 'SET_EMAIL'; email: string | null }
    | { type: 'SET_CREDIT_BALANCE'; balance: number }
    | { type: 'SET_PYRAMID_COUNT'; count: number; latestApexQuestion?: string | null }
    | { type: 'SET_FLEET_PULSE'; fleetOnlineCount: number; taskCount: number }
    | { type: 'SET_DRAFT_COUNT'; count: number }
    | { type: 'ADD_OPERATION'; operation: OperationEntry }
    | { type: 'UPDATE_OPERATION'; id: string; updates: Partial<OperationEntry> }
    | { type: 'COMPLETE_OPERATION'; id: string; result?: unknown; error?: string }
    | { type: 'DISMISS_OPERATION'; id: string }
    | { type: 'SET_AUTO_EXECUTE'; enabled: boolean };

// --- Initial State -----------------------------------------------------------

const ALL_MODES: Mode[] = ['pyramids', 'knowledge', 'tools', 'fleet', 'operations', 'search', 'compose', 'dashboard', 'identity', 'settings'];

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
    messageCount: 0,
    activeMode: 'pyramids',
    modeStacks: createInitialModeStacks(),
    pyramidCount: 0,
    latestApexQuestion: null,
    fleetOnlineCount: 0,
    taskCount: 0,
    draftCount: 0,
    docCount: 0,
    corpusCount: 0,
    lastSyncTime: null,
    activeOperations: [],
    autoExecute: false,
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
            return {
                ...state,
                syncState: action.syncState,
                docCount: action.syncState?.cached_documents?.length ?? 0,
                corpusCount: action.syncState?.linked_folders ? Object.keys(action.syncState.linked_folders).length : 0,
                lastSyncTime: action.syncState?.last_sync_at ?? null,
            };

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

        case 'SET_MESSAGE_COUNT':
            return { ...state, messageCount: action.count };

        case 'SET_EMAIL':
            return { ...state, email: action.email };

        case 'SET_CREDIT_BALANCE':
            return { ...state, creditBalance: action.balance };

        case 'SET_PYRAMID_COUNT':
            return {
                ...state,
                pyramidCount: action.count,
                latestApexQuestion: action.latestApexQuestion !== undefined ? action.latestApexQuestion : state.latestApexQuestion,
            };

        case 'SET_FLEET_PULSE':
            return {
                ...state,
                fleetOnlineCount: action.fleetOnlineCount,
                taskCount: action.taskCount,
            };

        case 'SET_DRAFT_COUNT':
            return { ...state, draftCount: action.count };

        case 'ADD_OPERATION':
            return { ...state, activeOperations: [...state.activeOperations, action.operation] };

        case 'UPDATE_OPERATION':
            return {
                ...state,
                activeOperations: state.activeOperations.map(op =>
                    op.id === action.id ? { ...op, ...action.updates } : op,
                ),
            };

        case 'COMPLETE_OPERATION':
            return {
                ...state,
                activeOperations: state.activeOperations.map(op =>
                    op.id === action.id
                        ? {
                            ...op,
                            status: action.error ? 'failed' as const : 'completed' as const,
                            result: action.result,
                            error: action.error,
                        }
                        : op,
                ),
            };

        case 'DISMISS_OPERATION':
            return {
                ...state,
                activeOperations: state.activeOperations.filter(op => op.id !== action.id),
            };

        case 'SET_AUTO_EXECUTE':
            return { ...state, autoExecute: action.enabled };

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
    /** Operator session auth — for dual-auth endpoints (handles, notifications, contributions/human, requests) */
    operatorApiCall: (method: string, path: string, body?: unknown) => Promise<unknown>;
    /** Wire agent auth (gne_live_* token) — for wire-scoped endpoints (pulse, query, entities, topics, roster, tasks, reputation, contribution/[id], my/earnings, mesh/*) */
    wireApiCall: (method: string, path: string, body?: unknown, headers?: Record<string, string>) => Promise<unknown>;
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

    const modeStacksRef = useRef(state.modeStacks);
    modeStacksRef.current = state.modeStacks;

    const currentView = useCallback((mode: Mode): ViewStackEntry => {
        const stack = modeStacksRef.current[mode];
        return stack[stack.length - 1];
    }, []);

    const operatorApiCall = useCallback(async (method: string, path: string, body?: unknown) => {
        return invoke('operator_api_call', { method, path, body: body ?? null });
    }, []);

    const wireApiCall = useCallback(async (method: string, path: string, body?: unknown, headers?: Record<string, string>) => {
        return invoke('wire_api_call', { method, path, body: body ?? null, headers: headers ?? null });
    }, []);

    const value = useMemo<AppContextValue>(() => ({
        state,
        dispatch,
        setMode,
        pushView,
        popView,
        navigateView,
        currentView,
        operatorApiCall,
        wireApiCall,
    }), [state, dispatch, setMode, pushView, popView, navigateView, currentView, operatorApiCall, wireApiCall]);

    return (
        <AppContext.Provider value={value}>
            {children}
        </AppContext.Provider>
    );
}
