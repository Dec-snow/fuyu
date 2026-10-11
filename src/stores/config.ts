import { create } from "zustand";

import { api, type SettingsPatch } from "../services/api";
import type { AppConfig, AppInfo } from "../types";

/** 把 `system` 解析成 light / dark 并写到 <html data-theme>。 */
export function applyTheme(theme: AppConfig["theme"]) {
  const prefersDark =
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches;
  const resolved = theme === "system" ? (prefersDark ? "dark" : "light") : theme;
  document.documentElement.dataset.theme = resolved;
}

interface ConfigState {
  config: AppConfig | null;
  info: AppInfo | null;
  loaded: boolean;
  error: string | null;
  load: () => Promise<void>;
  patch: (patch: SettingsPatch) => Promise<void>;
  setShortcut: (shortcut: string) => Promise<void>;
  setQuickActionShortcut: (shortcut: string) => Promise<void>;
  clearError: () => void;
}

export const useConfig = create<ConfigState>((set) => ({
  config: null,
  info: null,
  loaded: false,
  error: null,

  load: async () => {
    try {
      const [config, info] = await Promise.all([api.getConfig(), api.appInfo()]);
      applyTheme(config.theme);
      set({ config, info, loaded: true, error: null });
    } catch (e) {
      set({ error: String(e), loaded: true });
    }
  },

  patch: async (patch) => {
    try {
      const config = await api.updateSettings(patch);
      applyTheme(config.theme);
      set({ config, error: null });
      // 代理改动需要后端重建 HTTP 客户端，否则要重启才生效。
      if (patch.proxyUrl !== undefined || patch.useSystemProxy !== undefined) {
        await api.applyProxy();
      }
    } catch (e) {
      set({ error: String(e) });
      throw e;
    }
  },

  setShortcut: async (shortcut) => {
    // 先让后端试着注册；抢不到键位就会抛错，此时配置不变。
    await api.setShortcut(shortcut);
    const config = await api.getConfig();
    set({ config });
  },

  setQuickActionShortcut: async (shortcut) => {
    await api.setQuickActionShortcut(shortcut);
    const config = await api.getConfig();
    set({ config });
  },

  clearError: () => set({ error: null }),
}));
