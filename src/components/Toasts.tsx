import { useEffect, useMemo } from "react";

import { collectToasts, MAX_VISIBLE_TOASTS, NOTICE_TTL_MS } from "../lib/toasts";
import { useChat } from "../stores/chat";
import { useConfig } from "../stores/config";
import { useProviders } from "../stores/providers";
import { useUi } from "../stores/ui";

/**
 * 全局提示条：把「只写不读」的几路错误/通知真正显示出来。
 *
 * 为什么需要它：`useChat.error`、`useConfig.error`、`useProviders.error`、
 * `useUi.notices` 一直都在被写入，却**没有任何组件读过它们**。后果是相当多
 * 的失败是完全静默的：
 *
 * - 刚装好、还没配模型就按回车 → 输入框清空了，没有气泡、没有转圈、
 *   也没有任何文字，看起来就是「这软件坏了」；
 * - 粘贴一张后端不接受的图片 → 图没了、字也没了，一句提示都没有；
 * - 快捷键被别的软件占用 → 通知写进了 store，然后被丢掉。
 *
 * 放在窗口顶部：悬浮窗里底部被输入框占着。
 */
export function Toasts() {
  const chatError = useChat((s) => s.error);
  const clearChatError = useChat((s) => s.clearError);
  const configError = useConfig((s) => s.error);
  const clearConfigError = useConfig((s) => s.clearError);
  const providersError = useProviders((s) => s.error);
  const clearProvidersError = useProviders((s) => s.clearError);
  const notices = useUi((s) => s.notices);
  const setNotices = useUi((s) => s.setNotices);

  const items = useMemo(
    () => collectToasts({ chatError, configError, providersError, notices }),
    [chatError, configError, providersError, notices],
  );

  // 通知类会自动收起；错误留着 —— 用户往往需要照着它去改配置，
  // 自己消失会让人来不及看。
  useEffect(() => {
    if (notices.length === 0) return;
    const timer = window.setTimeout(() => setNotices([]), NOTICE_TTL_MS);
    return () => window.clearTimeout(timer);
  }, [notices, setNotices]);

  // 提示条会占掉本来就紧张的窗口高度。
  const shown = items.slice(0, MAX_VISIBLE_TOASTS);
  if (shown.length === 0) return null;

  const dismiss = (key: string) => {
    if (key === "chat") clearChatError();
    else if (key === "config") clearConfigError();
    else if (key === "providers") clearProvidersError();
    else if (key.startsWith("notice-")) {
      // key 里带下标前缀，用它精确定位到那一条。
      const index = Number(key.split("-")[1]);
      setNotices(notices.filter((_, i) => i !== index));
    }
  };

  return (
    <div className="toast-stack" role="status" aria-live="polite">
      {shown.map((item) => (
        <div className="toast" key={item.key}>
          <span className="toast-text">{item.text}</span>
          <button
            className="toast-close"
            title="关闭"
            onClick={() => dismiss(item.key)}
          >
            ×
          </button>
        </div>
      ))}
      {items.length > shown.length ? (
        <div className="toast-more">还有 {items.length - shown.length} 条提示…</div>
      ) : null}
    </div>
  );
}
