// src/hooks/useLocalMode.ts — Phase 18a (L1) Local Mode toggle hook.
//
// Wraps the three Phase 18a IPC commands plus the optional probe
// helper so the Settings.tsx Local LLM section stays compact:
//
//   pyramid_get_local_mode_status
//   pyramid_enable_local_mode
//   pyramid_disable_local_mode
//   pyramid_probe_ollama
//
// The hook owns the in-flight loading flag, the most recent error
// string, and a one-shot probe API for the "Test connection" button.
// Consumers re-render when status, loading, or error changes.

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface LocalModeStatus {
  enabled: boolean;
  base_url: string | null;
  model: string | null;
  detected_context_limit: number | null;
  available_models: string[];
  reachable: boolean;
  reachability_error: string | null;
  ollama_provider_id: string;
  prior_tier_routing_contribution_id: string | null;
  prior_build_strategy_contribution_id: string | null;
}

export interface OllamaProbeResult {
  reachable: boolean;
  reachability_error: string | null;
  available_models: string[];
}

export interface UseLocalModeResult {
  status: LocalModeStatus | null;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  enable: (baseUrl: string, model: string | null) => Promise<void>;
  disable: () => Promise<void>;
  probe: (baseUrl: string) => Promise<OllamaProbeResult>;
  switchModel: (model: string) => Promise<void>;
}

export function useLocalMode(): UseLocalModeResult {
  const [status, setStatus] = useState<LocalModeStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const fresh = await invoke<LocalModeStatus>("pyramid_get_local_mode_status");
      setStatus(fresh);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const enable = useCallback(
    async (baseUrl: string, model: string | null) => {
      setLoading(true);
      setError(null);
      try {
        const next = await invoke<LocalModeStatus>("pyramid_enable_local_mode", {
          baseUrl,
          model,
        });
        setStatus(next);
      } catch (err) {
        setError(String(err));
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  const disable = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const next = await invoke<LocalModeStatus>("pyramid_disable_local_mode");
      setStatus(next);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const probe = useCallback(async (baseUrl: string): Promise<OllamaProbeResult> => {
    return await invoke<OllamaProbeResult>("pyramid_probe_ollama", { baseUrl });
  }, []);

  const switchModel = useCallback(async (model: string) => {
    setLoading(true);
    setError(null);
    try {
      const next = await invoke<LocalModeStatus>("pyramid_switch_local_model", { model });
      setStatus(next);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  return { status, loading, error, refresh, enable, disable, probe, switchModel };
}
