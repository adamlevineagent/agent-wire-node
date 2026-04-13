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

export interface OllamaModelInfo {
  name: string;
  size_bytes: number;
  family: string | null;
  families: string[] | null;
  parameter_size: string | null;
  quantization_level: string | null;
  context_window: number | null;
  architecture: string | null;
  modified_at: string | null;
}

export interface LocalModeStatus {
  enabled: boolean;
  base_url: string | null;
  model: string | null;
  detected_context_limit: number | null;
  context_override: number | null;
  concurrency_override: number | null;
  available_models: string[];
  available_model_details: OllamaModelInfo[];
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
  available_model_details: OllamaModelInfo[];
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
  getModelDetails: (baseUrl: string, model: string) => Promise<OllamaModelInfo>;
  setContextOverride: (limit: number | null) => Promise<void>;
  setConcurrencyOverride: (concurrency: number | null) => Promise<void>;
  pullModel: (model: string) => Promise<void>;
  cancelPull: () => Promise<void>;
  deleteModel: (model: string) => Promise<void>;
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

  const getModelDetails = useCallback(async (baseUrl: string, model: string): Promise<OllamaModelInfo> => {
    return await invoke<OllamaModelInfo>("pyramid_get_model_details", { baseUrl, model });
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

  const setContextOverride = useCallback(async (limit: number | null) => {
    setLoading(true);
    setError(null);
    try {
      const next = await invoke<LocalModeStatus>("pyramid_set_context_override", { limit });
      setStatus(next);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const setConcurrencyOverride = useCallback(async (concurrency: number | null) => {
    setLoading(true);
    setError(null);
    try {
      const next = await invoke<LocalModeStatus>("pyramid_set_concurrency_override", { concurrency });
      setStatus(next);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const pullModel = useCallback(async (model: string) => {
    setLoading(true);
    setError(null);
    try {
      await invoke<void>("pyramid_ollama_pull_model", { model });
      // Pull completed — refresh model list to pick up the new model.
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, [refresh]);

  const cancelPull = useCallback(async () => {
    // Fire-and-forget — sets the cancellation flag on the backend.
    try {
      await invoke<void>("pyramid_ollama_cancel_pull");
    } catch {
      // Silently ignore cancel errors.
    }
  }, []);

  const deleteModel = useCallback(async (model: string) => {
    setLoading(true);
    setError(null);
    try {
      await invoke<void>("pyramid_ollama_delete_model", { model });
      // Refresh model list after deletion.
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, [refresh]);

  return { status, loading, error, refresh, enable, disable, probe, switchModel, getModelDetails, setContextOverride, setConcurrencyOverride, pullModel, cancelPull, deleteModel };
}
