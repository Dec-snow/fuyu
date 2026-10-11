import { useEffect, useRef } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { api } from "../services/api";
import { useChat } from "../stores/chat";
import { useConfig } from "../stores/config";
import { useUi } from "../stores/ui";

/**
 * 无边框窗口的自绘标题栏。
 *
 * 拖动必须调用 `startDragging()`（走系统的窗口拖动），而不是自己在 mousemove
 * 里算位移 —— 后者在缩放比例不为 100% 的显示器上会漂移。
 */
export function TitleBar() {
  const expanded = useUi((s) => s.expanded);
  const setExpanded = useUi((s) => s.setExpanded);
  const openSettings = useUi((s) => s.openSettings);
  const toggleSidebar = useUi((s) => s.toggleSidebar);
  const sidebarHidden = useUi((s) => s.sidebarHidden);
  const newConversation = useChat((s) => s.newConversation);

  /** 这次按下是不是真的进了「拖窗口」流程。 */
  const dragging = useRef(false);

  const onDragStart = (event: React.MouseEvent) => {
    if (event.buttons !== 1) return;
    // 按钮上不要启动拖动。子元素的 `stopPropagation` 挂在 `onClick` 上，
    // 拦不住 `mousedown` —— 按 ☰ / ⚙ / ✕ 时手抖两三个像素就会变成拖窗口：
    // 那一次点击丢失（要再点一次），窗口还会跟着跳一下。
    if ((event.target as HTMLElement).closest("button")) return;
    dragging.current = true;
    void getCurrentWindow().startDragging();
  };

  // 「记住窗口位置」只在**真的拖过窗口之后**才写一次。
  //
  // 之前是挂在 App 的 `onMouseUp` 上：在应用里随便点一下按钮、
  // 拖一下滑块、选一段文字都会触发 `save_window_position`，而它会重写整个
  // 配置文件（临时文件 + fsync + rename）——一分钟能写出几十次 fsync。
  useEffect(() => {
    const onMouseUp = () => {
      if (!dragging.current) return;
      dragging.current = false;
      if (useConfig.getState().config?.rememberWindowPosition) {
        void api.saveWindowPosition();
      }
    };
    window.addEventListener("mouseup", onMouseUp);
    return () => window.removeEventListener("mouseup", onMouseUp);
  }, []);

  const switchMode = async (next: boolean) => {
    setExpanded(next);
    await api.setWindowMode(next);
  };

  return (
    <header className="titlebar" onMouseDown={onDragStart}>
      {expanded ? (
        <button
          className="icon-button"
          title={sidebarHidden ? "显示历史" : "隐藏历史"}
          onClick={(e) => {
            e.stopPropagation();
            toggleSidebar();
          }}
        >
          ☰
        </button>
      ) : null}

      <span className="brand">浮语</span>

      {expanded ? (
        <button
          className="ghost-button"
          onClick={(e) => {
            e.stopPropagation();
            void newConversation();
          }}
        >
          + 新对话
        </button>
      ) : null}

      <div className="titlebar-spacer" />

      <button
        className="icon-button"
        title={expanded ? "收起为悬浮窗" : "展开完整模式"}
        onClick={(e) => {
          e.stopPropagation();
          void switchMode(!expanded);
        }}
      >
        {expanded ? "⤡" : "⤢"}
      </button>
      <button
        className="icon-button"
        title="设置"
        onClick={(e) => {
          e.stopPropagation();
          openSettings();
        }}
      >
        ⚙
      </button>
      <button
        className="icon-button"
        title="隐藏（Alt+Space 唤回）"
        onClick={(e) => {
          e.stopPropagation();
          void api.hideWindow();
        }}
      >
        ✕
      </button>
    </header>
  );
}
