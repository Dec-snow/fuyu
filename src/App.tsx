import { useEffect } from "react";

import { Composer } from "./components/Composer";
import { MessageList } from "./components/MessageList";
import { SettingsPanel } from "./components/SettingsPanel";
import { Sidebar } from "./components/Sidebar";
import { TitleBar } from "./components/TitleBar";
import { Toasts } from "./components/Toasts";
import { api } from "./services/api";
import { useConfig } from "./stores/config";
import { useUi } from "./stores/ui";

export default function App() {
  const expanded = useUi((s) => s.expanded);
  const sidebarHidden = useUi((s) => s.sidebarHidden);
  const settingsOpen = useUi((s) => s.settingsOpen);
  const phase = useUi((s) => s.phase);
  const hideOnBlur = useConfig((s) => s.config?.hideOnBlur ?? true);
  const configLoaded = useConfig((s) => s.loaded);

  // 动画阶段写到 <html> 上，CSS 只认 data-phase，不再关心 React 状态。
  useEffect(() => {
    document.documentElement.dataset.phase = phase;
  }, [phase]);

  // 失焦自动隐藏。
  //
  // 三个例外：
  // 1. 刚被程序化唤出（blurGuardUntil）；
  // 2. 设置面板开着 —— 用户很可能要去浏览器里复制 API Key，这时候把窗口
  //    藏起来会让人根本没法把 Key 粘进来；
  // 3. 用户自己关掉了这个选项。
  useEffect(() => {
    // 配置还没回来之前**不要**注册这个监听。
    //
    // 为什么：`config` 为 null 时上面只能按默认值取 `true`，于是「用户明明
    // 关掉了失焦自动隐藏」在启动的头一两百毫秒里仍然生效 —— 窗口刚被
    // `show_window` 显示出来，一次失焦就把它藏了。症状是「窗口一闪就没了」，
    // 而且**只在启动瞬间偶发**，靠手工几乎复现不了（这个 bug 就是实测时
    // 观察到「启动后窗口不可见」才顺出来的）。
    if (!configLoaded || !hideOnBlur) return;

    let timer = 0;
    const onBlur = () => {
      if (Date.now() < useUi.getState().blurGuardUntil) return;
      if (useUi.getState().settingsOpen) return;
      timer = window.setTimeout(() => {
        if (useUi.getState().settingsOpen) return;
        void api.hideWindow();
      }, 180);
    };
    const onFocus = () => window.clearTimeout(timer);

    window.addEventListener("blur", onBlur);
    window.addEventListener("focus", onFocus);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("focus", onFocus);
    };
  }, [hideOnBlur, configLoaded]);

  // Esc 收起设置面板 / 悬浮窗。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;

      // 设置面板开着就关面板 —— 哪怕焦点在输入框里（用户就是想退出设置）。
      if (useUi.getState().settingsOpen) {
        useUi.getState().closeSettings();
        return;
      }

      // 焦点在输入框里时，Esc 的意思是「取消这次输入」，不是「把窗口藏起来」。
      // 消息编辑框虽然自己 stopPropagation 了，这里再兜一层：
      // 悬浮窗被 Esc 藏掉时，用户正在写的草稿会一起消失。
      const target = event.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable)
      ) {
        return;
      }

      if (!useUi.getState().expanded) {
        void api.hideWindow();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  return (
    <div className={`app ${expanded ? "expanded" : "floating"}`}>
      <TitleBar />
      <Toasts />
      <div className="app-body">
        {expanded && !sidebarHidden ? <Sidebar /> : null}
        <main className="app-main">
          <MessageList />
          <Composer />
        </main>
      </div>
      {settingsOpen ? <SettingsPanel /> : null}
    </div>
  );
}
