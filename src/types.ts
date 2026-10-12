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

/**
 * 一个人设（角色预设）。
 *
 * 注意名字：这个文件里已经有一个 `Role`（消息角色 user/assistant/system/tool），
 * 两者完全不是一回事 —— 所以这里叫 `RolePreset`，别叫 `Role`。
 */
export interface RolePreset {
  id: string;
  name: string;
  prompt: string;
  /** 内置模板：可以改内容，但不能删（删了就找不回来了）。 */
  builtin: boolean;
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
  /**
   * 悬浮窗**背景**的不透明度（0.35 ~ 1.0）。
   *
   * 只影响背景，不影响文字：调低不会让字变糊，否则「调透明」就等于
   * 「调得看不清」。
   */
  windowOpacity: number;
  /** 正文字号（px，11 ~ 18）。影响聊天内容、输入框这些「要读的文字」。 */
  messageFontSize: number;
  /** 角色预设（内置 + 自建）。 */
  roles: RolePreset[];
  /** 用户的「开机自启」选择；系统里的真实状态可能被任务管理器改掉。 */
  autostart: boolean;
  /**
   * 长期记忆开关。
   *
   * 打开后每次提问都会把相关的记忆一并发给模型服务商，所以默认关闭。
   */
  memoryEnabled: boolean;
  /** 一问多答上次用过的对比目标（面板里随时可改）。 */
  compareTargets: CompareTarget[];
}

/** 一问多答里的一个目标。 */
export interface CompareTarget {
  providerId: string;
  modelKey: string;
}

/** 「采用这条」的结果：写进会话的那两条消息。 */
export interface AdoptedTurn {
  userMessageId: string;
  assistantMessageId: string;
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
  /** 这个会话用的人设 id（`null` = 用全局系统提示词）。 */
  roleId: string | null;
}

/** 一条诊断日志。 */
export interface LogEntry {
  at: number;
  /** 来源：`chat` / `config` / `provider` / `db` / `tool`。 */
  source: string;
  message: string;
}

/** 「设置 → 数据 → 诊断信息」要显示的全部内容。 */
export interface Diagnostics {
  version: string;
  os: string;
  arch: string;
  dbPath: string;
  dbBytes: number;
  dbVersion: number;
  conversations: number;
  messages: number;
  configPath: string;
  configBytes: number;
  providers: number;
  webSearchConfigured: boolean;
  alwaysAllowedTools: string[];
  logs: LogEntry[];
}

/** 一天的用量汇总（画趋势图用）。 */
export interface UsageDay {
  /** `YYYY-MM-DD`（本地时间）。 */
  day: string;
  inputTokens: number;
  outputTokens: number;
  messages: number;
  /** 这一天里**有价格**的模型的花费合计（元）。 */
  cost: number;
  /**
   * 这一天是否含有「价格表里没有」的模型。
   * 有的话 `cost` 是**偏低的** —— 界面必须说明，不能把偏低的数字当成全部。
   */
  hasUnpriced: boolean;
}

export interface UsageReport {
  days: UsageDay[];
  totalInputTokens: number;
  totalOutputTokens: number;
  totalMessages: number;
  estimatedCost: number;
  /** 价格表里认不出来的模型。 */
  unpricedModels: string[];
  pricedModels: number;
}

/** 知识库里的一篇文档。 */
export interface KbDocument {
  id: string;
  title: string;
  /** 来源：文件名，或「粘贴的文本」。 */
  source: string;
  chars: number;
  chunkCount: number;
  addedAt: number;
}

/** 一次知识库检索的命中。 */
export interface KbHit {
  chunkId: string;
  documentId: string;
  title: string;
  /** 文档里的第几片（从 1 开始）。 */
  ordinal: number;
  text: string;
  score: number;
}

export interface KbStats {
  documents: number;
  chunks: number;
  terms: number;
  chars: number;
}

/** 一条长期记忆。 */
export interface Memory {
  id: string;
  text: string;
  /** `tool`（模型自己记的）/ `user`（手动添加）。 */
  source: string;
  createdAt: number;
  lastUsedAt: number | null;
  /** 被注入过几次。 */
  usedCount: number;
}

export interface MemoryStats {
  total: number;
  limit: number;
  /** 被用过至少一次的有几条。 */
  used: number;
}

/** 收录一条记忆的结果。 */
export interface MemoryAddOutcome {
  memory: Memory;
  /** 是不是合并进了已有的一条（而不是新增）。 */
  merged: boolean;
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
