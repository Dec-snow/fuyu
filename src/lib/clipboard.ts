// 复制到剪贴板的**可测部分**。
//
// 这里只做两件前端该管的事：判断「这条内容值不值得给复制按钮」，
// 以及把「失败」变成一个**返回值**而不是异常 —— 上层要据此把按钮文字改成
// 「复制失败」并推提示。如果让它抛异常，调用点很容易写成
// `void copy()` 然后把失败吞掉，用户就会以为复制成功了。

import { api } from "../services/api";

/** 「已复制 / 复制失败」这两个提示保留多久。 */
export const COPY_FEEDBACK_MS = 1400;

export type CopyOutcome = "copied" | "failed" | "empty";

/**
 * 这条内容要不要提供复制入口。
 *
 * 纯空白不给：复制成功却粘出一片空白，比没有按钮更让人困惑。
 */
export function canCopy(text: string | null | undefined): boolean {
  return typeof text === "string" && text.trim().length > 0;
}

/**
 * 复制。**不抛异常**，失败以 `"failed"` 返回。
 *
 * 空/纯空白内容直接返回 `"empty"`，连 IPC 都不发。
 */
export async function copyText(text: string): Promise<CopyOutcome> {
  if (!canCopy(text)) return "empty";
  try {
    await api.copyToClipboard(text);
    return "copied";
  } catch {
    return "failed";
  }
}
