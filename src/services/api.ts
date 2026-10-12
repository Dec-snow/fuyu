// 所有 `invoke` 调用的唯一入口。
//
// 好处：命令名、参数名、返回类型只在这里出现一次；页面组件里不出现任何
// 字符串形式的命令名，改名时编译期就能发现。

import { invoke } from "@tauri-apps/api/core";

import type {
  AdoptedTurn,
  AppConfig,
  AppInfo,
  CompareTarget,
  Conversation,
  Diagnostics,
  KbDocument,
  KbHit,
  KbStats,
  Memory,
  SkillPreset,
  MemoryAddOutcome,
  MemoryStats,
  MessageHit,
  ProvidersState,
  RolePreset,
  StoredMessage,
  UsageReport,
} from "../types";

export interface ConversationDetail {
  conversation: Conversation;
  messages: StoredMessage[];
}

export interface SettingsPatch {
  theme?: AppConfig["theme"];
  systemPrompt?: string;
  shortcut?: string;
  hideOnBlur?: boolean;
  rememberWindowPosition?: boolean;
  quickActionEnabled?: boolean;
  /** 长期记忆开关。 */
  memoryEnabled?: boolean;
  /** 一问多答的对比目标（整表覆写）。 */
  compareTargets?: CompareTarget[];
  /** 技能列表（整表覆写）。 */
  skills?: SkillPreset[];
  webSearchEndpoint?: string;
  proxyUrl?: string;
  useSystemProxy?: boolean;
  temperature?: number;
  maxTokens?: number;
  defaultProviderId?: string;
  defaultModelKey?: string;
  contextBudgetTokens?: number;
  /** 整表覆写「永久允许」的工具名（撤销某一项时传剩下的那些）。 */
  alwaysAllowedTools?: string[];
  /** 窗口背景不透明度；后端会收进合法区间（见 storage/config.rs）。 */
  windowOpacity?: number;
  /** 正文字号 px；后端会收进合法区间。 */
  messageFontSize?: number;
  /** 角色预设（整表覆写）。 */
  roles?: RolePreset[];
}

export interface UpsertProviderPayload {
  id: string;
  name: string;
  baseUrl: string;
  models: { key: string; favorite: boolean }[];
  isPreset: boolean;
}

export const api = {
  // ---- 设置与信息
  appInfo: () => invoke<AppInfo>("app_info"),
  getConfig: () => invoke<AppConfig>("get_config"),
  updateSettings: (patch: SettingsPatch) =>
    invoke<AppConfig>("update_settings", { patch }),
  applyProxy: () => invoke<void>("apply_proxy"),

  // ---- 服务商
  providersState: () => invoke<ProvidersState>("get_providers_state"),
  upsertProvider: (provider: UpsertProviderPayload) =>
    invoke<ProvidersState>("upsert_provider", { provider }),
  deleteProvider: (providerId: string) =>
    invoke<ProvidersState>("delete_provider", { providerId }),
  addModels: (providerId: string, modelKeys: string[]) =>
    invoke<ProvidersState>("add_models", { providerId, modelKeys }),
  removeModel: (providerId: string, modelKey: string) =>
    invoke<ProvidersState>("remove_model", { providerId, modelKey }),
  setDefaultModel: (providerId: string, modelKey: string) =>
    invoke<ProvidersState>("set_default_model", { providerId, modelKey }),
  setModelFavorite: (providerId: string, modelKey: string, favorite: boolean) =>
    invoke<ProvidersState>("set_model_favorite", {
      providerId,
      modelKey,
      favorite,
    }),
  setApiKey: (providerId: string, apiKey: string) =>
    invoke<ProvidersState>("set_api_key", { providerId, apiKey }),
  fetchModels: (providerId: string) =>
    invoke<string[]>("fetch_models", { providerId }),
  testProvider: (providerId: string, modelKey: string) =>
    invoke<string>("test_provider", { providerId, modelKey }),

  // ---- 对话
  chatSend: (request: {
    providerId: string;
    modelKey: string;
    /**
     * 历史消息。图片**不**在这里传：带上 `id`，由后端按 id 从库里取图，
     * 否则一个带图的多轮会话每次提问都要在 IPC 上搬几 MB 的 base64。
     */
    messages: { id?: string | null; role: string; content: string }[];
    systemPrompt?: string | null;
    conversationId?: string | null;
    requestId: string;
    temperature?: number | null;
    maxTokens?: number | null;
    enableTools?: boolean;
    approvalSessionId?: string | null;
    /** 本次使用的技能（一次性任务指令 + 工具白名单）。 */
    skillId?: string | null;
  }) => invoke<string>("chat_send", { request }),
  chatCancel: (requestId: string) =>
    invoke<boolean>("chat_cancel", { requestId }),
  /**
   * 把一问多答里选中的那一问一答写进会话。
   *
   * 对比本身**不落库**（请求带 `conversationId: null`），只有「采用」这一步
   * 才产生真正的消息 —— 对比是草稿纸。
   */
  adoptAnswer: (request: {
    conversationId: string;
    question: string;
    answer: string;
    providerId: string;
    modelKey: string;
  }) => invoke<AdoptedTurn>("adopt_answer", { request }),
  chatApproveTool: (
    requestId: string,
    callId: string,
    approved: boolean,
    remember: "once" | "session" | "always",
  ) =>
    invoke<boolean>("chat_approve_tool", {
      requestId,
      callId,
      approved,
      remember,
    }),

  // ---- 剪贴板
  //
  // 走 Rust 而不是 `navigator.clipboard`：后者的失败模式在悬浮窗里不可靠
  // （见 `src-tauri/src/commands/clipboard.rs` 的注释）。
  copyToClipboard: (text: string) =>
    invoke<void>("copy_to_clipboard", { text }),

  // ---- 备份
  //
  // 导出由后端写文件并返回路径（不弹「另存为」对话框：那要额外插件与权限）；
  // 导入由前端读文件内容后传进来（WebView 里拿不到文件的真实路径）。
  exportHistory: () =>
    invoke<{
      path: string;
      bytes: number;
      conversations: number;
      messages: number;
    }>("export_history"),
  importHistory: (json: string) =>
    invoke<{ imported: number; skipped: number; messages: number }>(
      "import_history",
      { json },
    ),

  // ---- 诊断与数据管理
  diagnostics: () => invoke<Diagnostics>("diagnostics"),
  clearLogs: () => invoke<number>("clear_logs"),
  /** 重置设置（保留服务商与密钥），返回重置后的整份配置。 */
  resetSettings: () => invoke<AppConfig>("reset_settings"),
  /** 删除所有服务商的 API Key，返回删掉了几个。 */
  deleteAllApiKeys: () => invoke<number>("delete_all_api_keys"),

  // ---- 开机自启
  //
  // 插件只在 Rust 侧使用（见 `src-tauri/src/commands/autostart.rs`），
  // 前端调的是下面这两个命令 —— 不用给前端加依赖、也不用放开插件权限。
  setAutostart: (enabled: boolean) =>
    invoke<AppConfig>("set_autostart", { enabled }),
  /** 读系统里的**真实**状态（用户可能在任务管理器里关掉了）。 */
  autostartStatus: () => invoke<boolean>("autostart_status"),
  /** 这一次是不是「开机自启带 --minimized」启动的。 */
  startedMinimized: () => invoke<boolean>("started_minimized"),

  // ---- 用量与费用
  //
  // 估算在 Rust 侧做（内置价格表 + 认不出来就如实列进 unpricedModels）。
  usageReport: (days?: number) =>
    invoke<UsageReport>("usage_report", { days: days ?? null }),

  // ---- 文档解析
  //
  // docx / xlsx / pptx 是 ZIP + XML，前端解要引一堆库；交给 Rust 提取文字，
  // 而且大小与 zip bomb 上限能在那边强制执行（前端限制拦不住直接调命令的人）。
  extractDocument: (fileName: string, payload: string) =>
    invoke<{
      fileName: string;
      kind: string;
      text: string;
      chars: number;
      truncated: boolean;
    }>("extract_document", { fileName, payload }),

  // ---- 本地知识库
  //
  // 检索在 Rust 侧做（自定义分词 + BM25）。这里只负责搬运与展示。
  kbList: () => invoke<KbDocument[]>("kb_list"),
  kbStats: () => invoke<KbStats>("kb_stats"),
  kbAddText: (title: string, source: string | null, text: string) =>
    invoke<KbDocument>("kb_add_text", { title, source, text }),
  /** Office 文档：前端读成 data URL，后端提取文字后入库。 */
  kbAddDocument: (fileName: string, payload: string) =>
    invoke<KbDocument>("kb_add_document", { fileName, payload }),
  kbDelete: (id: string) => invoke<void>("kb_delete", { id }),
  kbSearch: (query: string, limit?: number) =>
    invoke<KbHit[]>("kb_search", { query, limit: limit ?? null }),

  // ---- 长期记忆
  memoryList: () => invoke<Memory[]>("memory_list"),
  memoryStats: () => invoke<MemoryStats>("memory_stats"),
  memoryAdd: (text: string) => invoke<MemoryAddOutcome>("memory_add", { text }),
  memoryDelete: (id: string) => invoke<void>("memory_delete", { id }),
  /** 试搜：看看某句话会挑中哪几条记忆。 */
  memoryRelevant: (query: string) => invoke<Memory[]>("memory_relevant", { query }),

  // ---- 历史
  listConversations: (limit?: number) =>
    invoke<Conversation[]>("list_conversations", { limit: limit ?? null }),
  getConversation: (id: string) =>
    invoke<ConversationDetail>("get_conversation", { id }),
  createConversation: (
    providerId: string,
    modelKey: string,
    title?: string | null,
  ) =>
    invoke<Conversation>("create_conversation", {
      providerId,
      modelKey,
      title: title ?? null,
    }),
  renameConversation: (id: string, title: string) =>
    invoke<void>("rename_conversation", { id, title }),
  setConversationPinned: (id: string, pinned: boolean) =>
    invoke<void>("set_conversation_pinned", { id, pinned }),
  setConversationRole: (id: string, roleId: string | null) =>
    invoke<void>("set_conversation_role", { id, roleId }),
  deleteConversation: (id: string) =>
    invoke<void>("delete_conversation", { id }),
  clearAllHistory: () => invoke<void>("clear_all_history"),
  saveUserMessage: (conversationId: string, content: string, images: string[] = []) =>
    invoke<string>("save_user_message", { conversationId, content, images }),
  updateMessage: (id: string, content: string) =>
    invoke<void>("update_message", { id, content }),
  deleteMessage: (id: string) => invoke<void>("delete_message", { id }),
  messagesBefore: (conversationId: string, messageId: string) =>
    invoke<StoredMessage[]>("messages_before", { conversationId, messageId }),
  searchMessages: (query: string, limit?: number) =>
    invoke<MessageHit[]>("search_messages", { query, limit: limit ?? null }),
  tokenTotals: () =>
    invoke<{ inputTokens: number; outputTokens: number }>("token_totals"),

  // ---- 窗口
  showWindow: () => invoke<void>("show_window"),
  hideWindow: () => invoke<void>("hide_window"),
  finishHide: () => invoke<void>("finish_hide"),
  toggleWindow: () => invoke<void>("toggle_window"),
  setWindowMode: (expanded: boolean) =>
    invoke<void>("set_window_mode", { expanded }),
  saveWindowPosition: () => invoke<void>("save_window_position"),
  getShortcut: () => invoke<string>("get_shortcut"),
  setShortcut: (shortcut: string) =>
    invoke<string>("set_shortcut", { shortcut }),
  setQuickActionShortcut: (shortcut: string) =>
    invoke<string>("set_quick_action_shortcut", { shortcut }),
  reapplyShortcuts: () => invoke<void>("reapply_shortcuts"),
};
