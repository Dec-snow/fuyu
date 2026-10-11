import { useEffect, useRef, useState, type MouseEvent } from "react";

import { COPY_FEEDBACK_MS, canCopy, copyText, type CopyOutcome } from "../lib/clipboard";
import { useUi } from "../stores/ui";

/**
 * 复制按钮。
 *
 * 三个细节都是刻意为之：
 *
 * 1. **空内容直接不渲染** —— 复制成功却粘出一片空白，比没有按钮更让人困惑；
 * 2. **失败必须看得见**：按钮文字变成「复制失败」，同时推一条提示。
 *    复制最怕「以为成功、粘出来是旧的」；
 * 3. 提示显示期间给按钮加 class **强制它可见**：气泡上的复制按钮平时是
 *    悬停才显形的（`opacity: 0`），鼠标一移开，用户就看不到「已复制」了。
 */
export function CopyButton({
  text,
  className = "copy-button",
  idle = "复制",
  title = "复制",
}: {
  text: string;
  className?: string;
  idle?: string;
  title?: string;
}) {
  const [outcome, setOutcome] = useState<CopyOutcome | null>(null);
  const timer = useRef(0);

  // 组件在提示消失前被卸载（切会话、虚拟列表回收）时，别让定时器打到已卸载的组件。
  useEffect(() => () => window.clearTimeout(timer.current), []);

  if (!canCopy(text)) return null;

  const label = outcome === "copied" ? "已复制" : outcome === "failed" ? "复制失败" : idle;

  const onClick = (event: MouseEvent) => {
    // 气泡的双击编辑挂在父节点上，代码块也可能套在可点区域里。
    event.stopPropagation();
    void (async () => {
      const result = await copyText(text);
      setOutcome(result);
      if (result === "failed") {
        useUi
          .getState()
          .pushNotice("复制失败：系统剪贴板可能被其他程序占用，稍后再试");
      }
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setOutcome(null), COPY_FEEDBACK_MS);
    })();
  };

  return (
    <button
      type="button"
      className={`${className}${outcome ? ` ${outcome}` : ""}`}
      title={title}
      aria-label={title}
      onClick={onClick}
    >
      {label}
    </button>
  );
}
