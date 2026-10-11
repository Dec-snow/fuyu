// 「全局提示条要显示什么」的纯逻辑。
//
// 从组件里抽出来是为了能测：这一批修复的起点就是**这几个错误通道一直只写不读**，
// 界面上什么都看不到。把「哪几路来源 → 显示成什么」做成纯函数之后，
// 可以直接断言「每一路都有对应的提示项」，而不必去搭一个 DOM 测试环境。

/** 提示条的数据来源（全部来自各个 store）。 */
export interface ToastSources {
  /** 聊天相关：没配模型、发送失败、会话载入中… */
  chatError: string | null;
  /** 设置读写失败。 */
  configError: string | null;
  /** 服务商 / 模型配置失败。 */
  providersError: string | null;
  /** 一次性通知：快捷键注册失败、图片超限等。 */
  notices: readonly string[];
}

export interface ToastItem {
  /** React key，同时用于去重。 */
  key: string;
  text: string;
  /** `notice` 会自动消失，`error` 留到用户手动关（往往需要照着改配置）。 */
  kind: "notice" | "error";
}

/**
 * 把各路来源摊平成一串要显示的提示。
 *
 * 顺序：先通知（瞬时信息），再错误（需要处理）。悬浮窗很矮，调用方通常会截断。
 */
export function collectToasts(sources: ToastSources): ToastItem[] {
  const items: ToastItem[] = [];

  sources.notices.forEach((text, index) => {
    const trimmed = text.trim();
    if (!trimmed) return;
    items.push({ key: `notice-${index}-${trimmed}`, text: trimmed, kind: "notice" });
  });

  if (sources.chatError?.trim()) {
    items.push({ key: "chat", text: sources.chatError.trim(), kind: "error" });
  }
  if (sources.configError?.trim()) {
    items.push({
      key: "config",
      text: `设置：${sources.configError.trim()}`,
      kind: "error",
    });
  }
  if (sources.providersError?.trim()) {
    items.push({
      key: "providers",
      text: `模型配置：${sources.providersError.trim()}`,
      kind: "error",
    });
  }

  return items;
}

/** 界面上一屏最多显示几条（悬浮窗高度有限）。 */
export const MAX_VISIBLE_TOASTS = 2;

/** 通知类提示自动收起的延时。错误不自动消失。 */
export const NOTICE_TTL_MS = 8000;
