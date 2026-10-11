import { create } from "zustand";

import { api, type UpsertProviderPayload } from "../services/api";
import type { ProvidersState } from "../types";

interface ProvidersStore {
  state: ProvidersState | null;
  loading: boolean;
  error: string | null;
  load: () => Promise<void>;
  setApiKey: (providerId: string, key: string) => Promise<void>;
  upsert: (provider: UpsertProviderPayload) => Promise<void>;
  remove: (providerId: string) => Promise<void>;
  addModels: (providerId: string, keys: string[]) => Promise<void>;
  removeModel: (providerId: string, modelKey: string) => Promise<void>;
  setDefault: (providerId: string, modelKey: string) => Promise<void>;
  setFavorite: (
    providerId: string,
    modelKey: string,
    favorite: boolean,
  ) => Promise<void>;
  fetchModels: (providerId: string) => Promise<string[]>;
  clearError: () => void;
}

export const useProviders = create<ProvidersStore>((set) => ({
  state: null,
  loading: false,
  error: null,

  load: async () => {
    set({ loading: true });
    try {
      set({ state: await api.providersState(), loading: false, error: null });
    } catch (e) {
      set({ loading: false, error: String(e) });
    }
  },

  setApiKey: async (providerId, key) => {
    set({ state: await api.setApiKey(providerId, key) });
  },

  upsert: async (provider) => {
    set({ state: await api.upsertProvider(provider) });
  },

  remove: async (providerId) => {
    set({ state: await api.deleteProvider(providerId) });
  },

  addModels: async (providerId, keys) => {
    set({ state: await api.addModels(providerId, keys) });
  },

  removeModel: async (providerId, modelKey) => {
    set({ state: await api.removeModel(providerId, modelKey) });
  },

  setDefault: async (providerId, modelKey) => {
    set({ state: await api.setDefaultModel(providerId, modelKey) });
  },

  // 收藏只影响展示顺序，不影响正在用的模型 —— 所以这里**不去动**
  // defaultModelKey，只把后端返回的最新状态整体换掉。
  setFavorite: async (providerId, modelKey, favorite) => {
    set({ state: await api.setModelFavorite(providerId, modelKey, favorite) });
  },

  fetchModels: (providerId) => api.fetchModels(providerId),

  clearError: () => set({ error: null }),
}));

/**
 * 本地服务（Ollama 等）允许不带 API Key。
 *
 * 这段判断必须和 Rust 侧的 `is_keyless_provider` 保持一致：前端拿它做发送前的
 * 预检，后端拿它做真正的准入。两边不一致的话，要么误拦本地模型，
 * 要么让用户白等一次失败。
 */
export function isKeylessProvider(provider: { id: string; baseUrl: string }): boolean {
  const base = provider.baseUrl.toLowerCase();
  return (
    provider.id === "ollama" ||
    base.includes("localhost") ||
    base.includes("127.0.0.1") ||
    base.includes("0.0.0.0")
  );
}

/** 当前生效的服务商 / 模型（配置为空时退回第一个可用项）。 */
export function currentTarget(): {
  providerId: string;
  modelKey: string;
  /** 是否已经配了 API Key。 */
  hasKey: boolean;
  /** 是否**必须**有 Key 才能发（本地服务不需要）。 */
  needsKey: boolean;
  name: string;
} | null {
  const state = useProviders.getState().state;
  if (!state || state.providers.length === 0) return null;

  const provider =
    state.providers.find((p) => p.id === state.defaultProviderId) ??
    state.providers[0];

  const modelKey =
    state.defaultModelKey && provider.models.some((m) => m.key === state.defaultModelKey)
      ? state.defaultModelKey
      : (provider.models[0]?.key ?? "");

  if (!modelKey) return null;
  return {
    providerId: provider.id,
    modelKey,
    hasKey: provider.hasKey,
    needsKey: !provider.hasKey && !isKeylessProvider(provider),
    name: provider.name,
  };
}
