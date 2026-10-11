import { create } from "zustand";

import { clampFontSize, clampOpacity } from "../lib/appearance";
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

/**
 * 把「外观」类设置写进根节点的 CSS 变量。
 *
 * 为什么走 CSS 变量而不是给组件传 props：字号和不透明度是**全局**的，
 * 而气泡在虚拟列表里是 `memo` 组件 —— 用 props 传会让每个气泡都多一个依赖，
 * 滚动时整片重渲染。写根变量只动一次样式，剩下的交给浏览器重算。
 *
 * 这里再 clamp 一次：后端已经是权威（见 storage/config.rs 的硬边界），
 * 但前端也要保证「拿到越界值不会把界面弄坏」—— 比如手工改过 providers.json
 * 的 `windowOpacity: 0`，那会直接得到一个全透明的窗口。
 */
export function applyAppearance(
  config: Pick<AppConfig, "windowOpacity" | "messageFontSize"> | null,
) {
  const root = document.documentElement;
  root.style.setProperty(
    "--win-opacity",
    String(clampOpacity(config?.windowOpacity ?? 1)),
  );
  root.style.setProperty(
    "--msg-font-size",
    `${clampFontSize(config?.messageFontSize ?? 13.5)}px`,
  );
}

/**
 * 拖动滑块时的**即时预览**：只改 CSS 变量，不落盘、也不进 store。
 *
 * 为什么要单独一个函数：`update_settings` 每条命令都带一次 fsync，
 * 所以滑块是「停手 350ms 才提交」的。不预览的话，用户拖动时窗口毫无变化，
 * 会以为滑块坏了。属性名只在这里出现一次，避免和 `applyAppearance` 写岔。
 */
export function previewAppearance(
  next: Partial<Pick<AppConfig, "windowOpacity" | "messageFontSize">>,
) {
  const root = document.documentElement;
  if (next.windowOpacity !== undefined) {
    root.style.setProperty("--win-opacity", String(clampOpacity(next.windowOpacity)));
  }
  if (next.messageFontSize !== undefined) {
    root.style.setProperty(
      "--msg-font-size",
      `${clampFontSize(next.messageFontSize)}px`,
    );
  }
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
  /** 重置设置（保留服务商与密钥）。 */
  resetSettings: () => Promise<void>;
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
      applyAppearance(config);
      set({ config, info, loaded: true, error: null });
    } catch (e) {
      set({ error: String(e), loaded: true });
    }
  },

  patch: async (patch) => {
    try {
      const config = await api.updateSettings(patch);
      applyTheme(config.theme);
      applyAppearance(config);
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

  /**
   * 重置设置：后端把偏好恢复成默认值（保留服务商与密钥），
   * 这里把返回的整份配置应用上，并同步主题与外观变量。
   *
   * 不同步的话会出现「设置重置了、但界面还是旧主题」——
   * 因为主题/透明度/字号是写在 DOM 上的，不是从 store 直接渲染的。
   */
  resetSettings: async () => {
    const config = await api.resetSettings();
    applyTheme(config.theme);
    applyAppearance(config);
    set({ config, error: null });
  },

  clearError: () => set({ error: null }),
}));
