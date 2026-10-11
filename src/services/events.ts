// Rust → 前端的流式事件桥。
//
// 整个应用只有**一个** Tauri 事件监听器（在 main.tsx 里启动），事件到这里
// 之后同步分发给订阅者。这样做的两个理由：
//   1. 每个组件各自 listen 会产生 N 个 IPC 监听器，增量事件高频到达时开销明显；
//   2. 去重、乱序、清理逻辑集中一处，不会各写一遍。
//
// 去重逻辑本身在 `lib/event-gate.ts`：那是一个不依赖 Tauri 的纯类，可以单测。

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { ChatEventGate } from "../lib/event-gate";
import type { SequencedChatEvent } from "../types";

export const CHAT_EVENT = "chat-event";
export const WINDOW_SHOWN = "window-shown";
export const WINDOW_PREPARE_HIDE = "window-prepare-hide";
export const SHORTCUT_ERROR = "shortcut-error";
export const QUICK_ACTION = "quick-action";
export const QUICK_ACTION_ERROR = "quick-action-error";

/** 终态之后隔多久才忘掉这条请求（见 `ChatEventGate` 的注释）。 */
const FORGET_TERMINAL_AFTER_MS = 5000;

type ChatHandler = (event: SequencedChatEvent) => void;

const chatHandlers = new Set<ChatHandler>();
/** 唯一的去重闸门实例。 */
const gate = new ChatEventGate(FORGET_TERMINAL_AFTER_MS);

export async function startEventBridge(): Promise<UnlistenFn> {
  return listen<SequencedChatEvent>(CHAT_EVENT, ({ payload }) => {
    if (!gate.accept(payload)) return;
    chatHandlers.forEach((handler) => handler(payload));
  });
}

export function onChatEvent(handler: ChatHandler): () => void {
  chatHandlers.add(handler);
  return () => {
    chatHandlers.delete(handler);
  };
}

export async function onWindowShown(handler: () => void): Promise<UnlistenFn> {
  return listen(WINDOW_SHOWN, () => handler());
}

export async function onWindowPrepareHide(
  handler: () => void,
): Promise<UnlistenFn> {
  return listen(WINDOW_PREPARE_HIDE, () => handler());
}

export async function onShortcutError(
  handler: (messages: string[]) => void,
): Promise<UnlistenFn> {
  return listen<string[]>(SHORTCUT_ERROR, ({ payload }) => handler(payload));
}

/** 划词取词成功：拿到选中文本。 */
export async function onQuickAction(
  handler: (text: string) => void,
): Promise<UnlistenFn> {
  return listen<string>(QUICK_ACTION, ({ payload }) => handler(payload));
}

/** 划词取词失败（没选中 / UAC 窗口等）。 */
export async function onQuickActionError(
  handler: (message: string) => void,
): Promise<UnlistenFn> {
  return listen<string>(QUICK_ACTION_ERROR, ({ payload }) => handler(payload));
}
