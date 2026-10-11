// 与 Rust 侧一一对应的类型声明。
//
// 手写而不是自动生成：类型不多，且手写能让「Rust 里改名了、前端忘了改」
// 立刻在 `tsc` 阶段暴露，而不是运行时才发现字段是 undefined。

export type Role = "user" | "assistant" | "system";

export interface ChatMessage {
  role: Role;
  content: string;
}

export interface ModelEntry {
  key: string;
  favorite: boolean;
}

export interface ProviderView {
  id: string;
  name: string;
  baseUrl: string;
  models: ModelEntry[];
  isPreset: boolean;
  /** 只有「有没有 Key」，永远不会返回 Key 本身。 */
  hasKey: boolean;
}

export interface ProvidersState {
  providers: ProviderView[];
  defaultProviderId: string | null;
  defaultModelKey: string | null;
}

export interface AppConfig {
  providers: ProviderView[];
  defaultProviderId: string | null;
  defaultModelKey: string | null;
  systemPrompt: string;
  theme: "system" | "light" | "dark";
  shortcut: string;
  hideOnBlur: boolean;
  rememberWindowPosition: boolean;
  /** 划词助手开关（Alt+Q）。 */
  quickActionEnabled: boolean;
  quickActionShortcut: string;
  /** SearXNG 搜索端点；留空表示不启用联网搜索。 */
  webSearchEndpoint: string | null;
  windowX: number | null;
  windowY: number | null;
  expandedWidth: number;
  expandedHeight: number;
  expanded: boolean;
  proxyUrl: string | null;
  useSystemProxy: boolean;
  temperature: number;
  maxTokens: number | null;
  /** 发请求时的上下文预算（token）。超出就从最旧的消息开始丢。 */
  contextBudgetTokens: number;
  /**
   * 被「永久允许」的工具名（在审批卡上勾选「永久」时写入）。
   *
   * 这些工具以后不再弹审批卡；数据外发型工具（联网抓取）后端不允许永久放行，
   * 所以它永远不会出现在这里。
   */
  alwaysAllowedTools: string[];
}

export interface Conversation {
  id: string;
  title: string;
  providerId: string;
  modelKey: string;
  systemPrompt: string | null;
  pinned: boolean;
  createdAt: number;
  updatedAt: number;
  messageCount: number;
  /** 较早历史的滚动摘要（空串表示还没有）。 */
  summary: string;
  /** 摘要已覆盖前面多少条消息。 */
  summaryMessageCount: number;
}

export interface StoredMessage {
  id: string;
  conversationId: string;
  role: Role;
  content: string;
  reasoning: string | null;
  providerId: string | null;
  modelKey: string | null;
  inputTokens: number | null;
  outputTokens: number | null;
  createdAt: number;
  /** 终态；用户消息永远是 ok。失败与取消也会落库，作为「重试」的锚点。 */
  status: "ok" | "cancelled" | "error";
  /** 仅当 status === "error" 时有值。 */
  error: AppError | null;
  /** 图片的 data URL（只有用户消息可能有）。 */
  images: string[];
}

export interface MessageHit {
  messageId: string;
  conversationId: string;
  conversationTitle: string;
  role: Role;
  snippet: string;
  createdAt: number;
}

export interface AppInfo {
  version: string;
  name: string;
  dbPath: string;
  configPath: string;
}

/** Rust 的 `AppError` 序列化形态。 */
export interface AppError {
  code: string;
  message: string;
  retryable: boolean;
}

export function isAppError(value: unknown): value is AppError {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as AppError).message === "string" &&
    typeof (value as AppError).code === "string"
  );
}

/** Rust `ChatEvent`：`#[serde(tag = "type", rename_all = "snake_case")]`。 */
export type ChatEvent =
  | { type: "start"; requestId: string }
  | { type: "delta"; requestId: string; text: string }
  | { type: "reasoning_delta"; requestId: string; text: string }
  | {
      type: "context_trimmed";
      requestId: string;
      droppedMessages: number;
      /** 改用摘要替代的条数（信息没丢，与 droppedMessages 不同）。 */
      summarizedMessages: number;
      estimatedTokens: number;
    }
  | {
      type: "summary_updated";
      requestId: string;
      conversationId: string;
      summary: string;
      coveredMessages: number;
    }
  | {
      type: "usage";
      requestId: string;
      inputTokens: number | null;
      outputTokens: number | null;
    }
  | { type: "finish"; requestId: string; reason: string | null }
  | { type: "cancelled"; requestId: string }
  | {
      type: "error";
      requestId: string;
      code: string;
      message: string;
      retryable: boolean;
    }
  | {
      type: "tool_start";
      requestId: string;
      callId: string;
      name: string;
      arguments: string;
    }
  | {
      type: "tool_end";
      requestId: string;
      callId: string;
      name: string;
      result: string;
    }
  | {
      type: "tool_error";
      requestId: string;
      callId: string;
      name: string;
      message: string;
    }
  | {
      type: "tool_pending";
      requestId: string;
      callId: string;
      name: string;
      summary: string;
      canRemember: boolean;
    }
  | {
      type: "tool_rejected";
      requestId: string;
      callId: string;
      name: string;
    };

/** 后端给每个事件套了单调递增的 `seq`，可用于去重与排序。
 *
 * 用映射类型而不是 `ChatEvent & { seq: number }`：后者虽然也能用，但
 * 通过交叉类型做判别式收窄在部分 TS 版本上不生效，映射类型能保证结果
 * 仍然是一个规规矩矩的可收窄联合。 */
export type SequencedChatEvent = {
  [K in ChatEvent["type"]]: Extract<ChatEvent, { type: K }> & { seq: number };
}[ChatEvent["type"]];

/** UI 层的一条消息（比 `StoredMessage` 多了流式状态）。 */
export interface UiMessage {
  id: string;
  role: Role;
  content: string;
  reasoning: string;
  /** 正在接收增量。 */
  streaming: boolean;
  /** 出错信息（若有）。 */
  error: AppError | null;
  /** 是否被用户取消。 */
  cancelled: boolean;
  inputTokens: number | null;
  outputTokens: number | null;
  createdAt: number;
  /**
   * 因上下文预算被省略的历史条数。
   *
   * 大于 0 时界面上要给个提示 —— 静默丢弃历史会让用户以为模型「忘了」
   * 前面说过的话。
   */
  trimmed: number;
  /** 被**摘要替代**（而非丢弃）的历史条数。 */
  summarized: number;
  /** 本条消息关联的工具调用时间线（agent 循环时填充）。 */
  tools: ToolTrace[];
  /** 用户消息携带的图片（data URL）。 */
  images: string[];
}

/** 一次工具调用的时间线片段。 */
export interface ToolTrace {
  callId: string;
  name: string;
  status: "pending" | "running" | "ok" | "error" | "rejected";
  /** pending 时的参数摘要。 */
  summary?: string;
  /** ok / error 时的结果或错误文案。 */
  result?: string;
  /** pending 时是否允许「会话 / 永久」记住。 */
  canRemember?: boolean;
}
