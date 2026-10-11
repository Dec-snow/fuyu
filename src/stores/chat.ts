import { create } from "zustand";

import {
  dropConversation,
  makeAssistantPlaceholder,
  makeUserMessage,
  reduceChatEvent,
  toUiMessages,
  visibleMessages,
  type ChatStreamState,
  type StreamRoute,
} from "../lib/chat-state";
import { isTerminalEvent } from "../lib/event-gate";
import { api } from "../services/api";
import { onChatEvent } from "../services/events";
import type {
  AppError,
  Conversation,
  SequencedChatEvent,
  UiMessage,
} from "../types";
import { useConfig } from "./config";
import { currentTarget } from "./providers";

/**
 * 单次请求允许携带的消息条数上限。
 *
 * 必须和后端 `validate_messages` 里的 `MAX_CHAT_MESSAGES` 保持一致：
 * 前端拿它做发送前的预检，免得「先删旧回答、再发一个注定被拒的请求」。
 */
const MAX_CHAT_MESSAGES = 200;

/**
 * 会话列表最多能拉多少条。
 *
 * 必须和后端 `list_conversations` 里的 `clamp(1, 1000)` 一致：
 * 要得再多也只会拿到 1000 条，界面上得如实说明「更早的请用搜索」。
 */
export const CONVERSATION_LIMIT_MAX = 1000;

function newRequestId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `req_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`;
}

function localId(prefix: string): string {
  return `${prefix}_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
}

function omit<T>(record: Record<string, T>, key: string): Record<string, T> {
  const next = { ...record };
  delete next[key];
  return next;
}

function normalizeError(value: unknown): AppError {
  if (typeof value === "object" && value !== null && "message" in value) {
    const v = value as Partial<AppError>;
    return {
      code: v.code ?? "internal",
      message: v.message ?? String(value),
      retryable: v.retryable ?? false,
    };
  }
  return { code: "internal", message: String(value), retryable: false };
}

interface SubmitParams {
  /**
   * 需要新落库的用户消息。
   *
   * 重新生成时**不传** —— 那条用户消息已经在库里了，再存一次就是重复。
   */
  newUserText?: string;
  /** 这次要新落库的图片（data URL）；只有和 `newUserText` 一起给才有意义。 */
  newImages?: string[];
  /** 发给模型的历史，必须已经包含最后那条用户消息。 */
  history: { id?: string | null; role: string; content: string }[];
}

interface ChatStore extends ChatStreamState {
  conversations: Conversation[];
  loadingMessages: boolean;
  /** 全局错误（未配置模型、落库失败等）。气泡级的错误在 `UiMessage.error` 上。 */
  error: string | null;
  /** 是否在请求里带上 `tools`（agent 工具开关）。 */
  toolsEnabled: boolean;
  /** 等待用户裁决的敏感工具审批卡片。 */
  pendingApprovals: PendingApproval[];
  /** 当前会话的滚动摘要（较早历史的压缩结果，空串表示没有）。 */
  activeSummary: string;
  /** 摘要已覆盖前面多少条消息。 */
  activeSummaryCount: number;
  /**
   * 从搜索结果跳进来时要高亮的那条消息。
   *
   * 命中项本来就带 `messageId`，但点击时只切换了会话 —— 用户落在会话最底部，
   * 完全不知道命中的是哪一条。这里让界面滚过去并短暂描边。
   */
  highlightMessageId: string | null;
  setHighlight: (id: string | null) => void;
  /**
   * 会话列表一次拉多少条。
   *
   * 后端默认给 200 条并且最多 1000 条，所以第 201 个会话原本是**够不着**的 ——
   * 侧栏既没有分页也没有「加载更多」。这里把它做成可增长的：点一次加 200。
   */
  conversationLimit: number;
  loadMoreConversations: () => Promise<void>;
  /**
   * 同步的「正在提交」闸门。
   *
   * `streamingId` 要等到几次 IPC（建会话 / 落库）之后才设得上，那段时间里
   * 第二次提交会被放进来：两个请求抢同一个 `live[convId]` 槽位，先发的那条
   * 事件全部被丢弃（界面上永远看不到），但**两条回答都写进了数据库**。
   */
  submitting: boolean;

  clearError: () => void;

  bootstrap: () => Promise<void>;
  refreshConversations: () => Promise<void>;
  selectConversation: (id: string) => Promise<void>;
  newConversation: () => Promise<void>;
  renameConversation: (id: string, title: string) => Promise<void>;
  togglePinned: (id: string) => Promise<void>;
  deleteConversation: (id: string) => Promise<void>;
  send: (text: string, images?: string[]) => Promise<boolean>;
  deleteMessage: (messageId: string) => Promise<void>;
  regenerate: () => Promise<void>;
  retry: (messageId: string) => Promise<void>;
  editAndResend: (messageId: string, newContent: string) => Promise<void>;
  cancel: () => Promise<void>;
  setToolsEnabled: (enabled: boolean) => void;
  approveTool: (
    requestId: string,
    callId: string,
    approved: boolean,
    remember: "once" | "session" | "always",
  ) => Promise<void>;
  handleEvent: (event: SequencedChatEvent) => void;
  /**
   * 一轮结束后回数据库读权威数据并清掉在飞气泡。
   *
   * 必须带上**这次结算的是哪一条**：结算要做两次 IPC，这期间用户完全可能
   * 已经发出了下一条。只按 conversationId 清的话，会把那条新请求的
   * 占位气泡和路由一起删掉 —— 新回答的事件全部被丢弃、界面永远不显示它，
   * 而且「停止」按钮也找不到它了。
   */
  settleConversation: (
    conversationId: string,
    settled: { requestId: string; messageId: string },
  ) => Promise<void>;
}

/** 一张等待裁决的敏感工具审批卡。 */
export interface PendingApproval {
  requestId: string;
  callId: string;
  name: string;
  summary: string;
  canRemember: boolean;
}

export const useChat = create<ChatStore>((set, get) => {
  /** 当前要显示的消息（已落库的 + 当前会话在飞的那条）。 */
  const visible = () => visibleMessages(get());

  const historyFrom = (messages: UiMessage[]) =>
    messages
      // 空的助手占位气泡不能发给模型；但「只带图不写字」的用户消息必须留下。
      .filter((m) => m.content.trim().length > 0 || m.images.length > 0)
      .map((m) => ({
        // 已落库的消息带上 id：后端据此从库里取图片（图片不走 IPC）。
        // `local_` 前缀的是还没落库的乐观气泡，带上也没用。
        id: m.id.startsWith("local_") ? null : m.id,
        role: m.role as string,
        content: m.content,
      }));

  /**
   * 一轮结束后回数据库读权威数据并清掉在飞气泡。
   *
   * `settled` 指明这次结算的是**哪一条**：结算要做两次 IPC，这期间用户完全可能
   * 已经发出了下一条。只按 conversationId 清的话，会把那条新请求的占位气泡和
   * 路由一起删掉 —— 新回答的事件全部被丢弃、界面永远不显示它，
   * 而且「停止」按钮也找不到它了。
   */
  const settleConversation = async (
    conversationId: string,
    settled: { requestId: string; messageId: string },
  ) => {
    let reloaded: UiMessage[] | null = null;

    if (get().activeId === conversationId) {
      try {
        const detail = await api.getConversation(conversationId);
        reloaded = toUiMessages(detail.messages);
      } catch (e) {
        set({ error: String(e) });
      }
    }

    // 一次 `set` 里同时换掉消息和清掉在飞气泡。
    // 拆成两次 `set` 的话，两次之间会渲染一帧「同一条回答既在 messages 里、
    // 又在 live 里」的画面 —— 表现为回答闪着重影一下。
    set((s) => {
      // 结算期间用户又发了下一条：这一轮已经「过期」了，什么都别动。
      // 新请求的终态会自己走到这里来结算。
      if (s.streamingId !== null && s.streamingId !== settled.messageId) {
        return {};
      }

      const live = s.live[conversationId];

      // 工具时间线、「已省略较早的 N 条」这类信息只存在于**这一轮**的在飞气泡上，
      // 数据库里没有对应的字段。结算时直接换成数据库行的话，它们会在回答结束的
      // 一瞬间消失 —— 用户刚刚看到「读了剪贴板」，一转眼就没有了。
      // 所以把这一轮的元信息回填到最后一条助手消息上。
      let merged = reloaded;
      if (merged && live && live.id === settled.messageId) {
        const lastIndex = merged.length - 1;
        const last = merged[lastIndex];
        const worthKeeping =
          live.tools.length > 0 || live.trimmed > 0 || live.summarized > 0;
        if (worthKeeping && last && last.role === "assistant") {
          merged = [
            ...merged.slice(0, lastIndex),
            {
              ...last,
              tools: live.tools,
              trimmed: live.trimmed,
              summarized: live.summarized,
            },
          ];
        }
      }

      // 只清掉属于这次结算的那部分，别人的原样留着。
      const nextLive = { ...s.live };
      if (nextLive[conversationId]?.id === settled.messageId) {
        delete nextLive[conversationId];
      }
      const routes = { ...s.routes };
      if (routes[settled.requestId]?.messageId === settled.messageId) {
        delete routes[settled.requestId];
      }

      return {
        ...(merged && s.activeId === conversationId ? { messages: merged } : {}),
        live: nextLive,
        routes,
        streamingId: s.streamingId === settled.messageId ? null : s.streamingId,
      };
    });
  };

  /**
   * 发请求前的可行性自检，返回给用户看的原因（可行时返回 `null`）。
   *
   * 为什么必须在**删数据之前**做：「重试 / 编辑后重发」是先删掉旧回答再重发的。
   * 如果这次重发注定发不出去（没配 Key、历史超过后台上限），那条旧回答就
   * 白白没了 —— 用户既丢了原来的答案，也没换来新答案，而且什么提示都没有。
   */
  const preflight = (historyLength: number): string | null => {
    const target = currentTarget();
    if (!target) {
      return "还没有可用的服务商或模型，请先到「设置 → 模型」里配置。";
    }
    if (target.needsKey) {
      return `「${target.name}」还没有配置 API Key，请先到「设置 → 模型」里填写。`;
    }
    if (historyLength > MAX_CHAT_MESSAGES) {
      return `单次请求最多 ${MAX_CHAT_MESSAGES} 条消息，这个会话太长了。请新开一个对话，或先删掉一些旧消息。`;
    }
    return null;
  };

  /**
   * 一次提交：确保会话存在 →（可选）把用户消息落库 → 插入在飞气泡 → 发请求。
   *
   * 返回 `true` 表示请求**已经发出去**了（接下来靠事件流推进）。
   * 调用方（输入框）据此决定要不要清空草稿 —— 发不出去时清掉，
   * 用户辛苦打的字和贴的图就白没了。
   *
   * 「重新生成」和「新提问」共用这一条路径，区别只在 `newUserText` 传不传。
   */
  const submit = async (params: SubmitParams): Promise<boolean> => {
    // 闸门必须在**第一个 await 之前**同步落下：`streamingId` 要等建会话、
    // 落库两次 IPC 之后才设得上，那段时间里的第二次提交会挤进来，
    // 两条回答都会写进数据库，但界面上只能看到一条。
    if (get().submitting || get().streamingId) return false;
    set({ submitting: true });

    try {
      return await runSubmit(params);
    } finally {
      set({ submitting: false });
    }
  };

  /** [`submit`] 的实际逻辑（外面包着并发闸门）。返回值同上。 */
  const runSubmit = async (params: SubmitParams): Promise<boolean> => {
    const target = currentTarget();
    if (!target) {
      set({ error: "还没有可用的服务商或模型，请先到「设置 → 模型」里配置。" });
      return false;
    }
    const config = useConfig.getState().config;

    // ---- 1) 确保有会话。空会话等第一条消息真的发出去时才建，免得点一下
    //         「新对话」就在库里留一条空记录。
    let conversationId = get().activeId;
    if (!conversationId) {
      try {
        const conv = await api.createConversation(
          target.providerId,
          target.modelKey,
        );
        conversationId = conv.id;
        set((s) => ({
          activeId: conv.id,
          conversations: [conv, ...s.conversations],
        }));
      } catch (e) {
        set({ error: String(e) });
        return false;
      }
    }
    const convId: string = conversationId;

    // ---- 2) 用户消息先落库：即使接下来请求失败，用户说过的话也不会丢。
    let userMessage: UiMessage | null = null;
    let history = params.history;
    if (params.newUserText !== undefined) {
      const images = params.newImages ?? [];
      try {
        const id = await api.saveUserMessage(convId, params.newUserText, images);
        userMessage = makeUserMessage(id, params.newUserText, Date.now(), images);

        // 把历史里那条「还没落库」的用户消息补上 id —— 后端要按 id 从库里
        // 取图片。不补的话，刚发出去的图这一轮模型就看不到。
        const last = history[history.length - 1];
        if (last && last.role === "user") {
          history = [...history.slice(0, -1), { ...last, id }];
        }
      } catch (e) {
        set({ error: `保存消息失败：${String(e)}` });
        return false;
      }
    }

    // ---- 3) 插入在飞气泡。
    const messageId = localId("local_ai");
    const requestId = newRequestId();
    const placeholder = makeAssistantPlaceholder(messageId, Date.now());
    const route: StreamRoute = { messageId, conversationId: convId };

    set((s) => ({
      messages: userMessage ? [...s.messages, userMessage] : s.messages,
      live: { ...s.live, [convId]: placeholder },
      routes: { ...s.routes, [requestId]: route },
      streamingId: messageId,
      error: null,
    }));

    // ---- 4) 发请求。真正的数据靠 chat-event 通道回来。
    try {
      await api.chatSend({
        providerId: target.providerId,
        modelKey: target.modelKey,
        messages: history,
        systemPrompt: config?.systemPrompt ?? null,
        conversationId: convId,
        requestId,
        temperature: config?.temperature ?? null,
        maxTokens: config?.maxTokens ?? null,
        enableTools: get().toolsEnabled,
        approvalSessionId: convId,
      });
      return true;
    } catch (e) {
      // `chat_send` 本身失败（没配 Key、参数非法、找不到服务商…）：
      // 这种情况下**不会**再有终态事件，必须自己在这里收尾，
      // 否则 `streamingId` 会一直挂着，发送按钮永远停在「停止」。
      set((s) => ({
        live: {
          ...s.live,
          [convId]: { ...placeholder, streaming: false, error: normalizeError(e) },
        },
        routes: omit(s.routes, requestId),
        streamingId: null,
        // 注意：**不**把刚落库的那条用户消息从界面上撤掉。
        // 它确实已经写进数据库了（这是有意的：用户说过的话不该因为一次
        // 请求失败就丢），撤掉会让界面和数据库不一致 —— 切走再切回来
        // 它又冒出来了。草稿由输入框自己保住，用户看到的是「消息在列表里 +
        // 原文还在输入框」，改一改就能重发。
      }));
      return false;
    }
  };

  return {
    conversations: [],
    messages: [],
    live: {},
    routes: {},
    activeId: null,
    streamingId: null,
    loadingMessages: false,
    error: null,
    toolsEnabled: false,
    pendingApprovals: [],
    activeSummary: "",
    activeSummaryCount: 0,
    highlightMessageId: null,
    submitting: false,
    conversationLimit: 200,

    clearError: () => set({ error: null }),

    setHighlight: (id) => set({ highlightMessageId: id }),

    loadMoreConversations: async () => {
      const next = Math.min(CONVERSATION_LIMIT_MAX, get().conversationLimit + 200);
      if (next === get().conversationLimit) return;
      set({ conversationLimit: next });
      await get().refreshConversations();
    },

    bootstrap: async () => {
      await get().refreshConversations();
    },

    refreshConversations: async () => {
      try {
        const conversations = await api.listConversations(get().conversationLimit);
        set({ conversations, error: null });
      } catch (e) {
        set({ error: String(e) });
      }
    },

    selectConversation: async (id) => {
      // **同步清空**消息列表，而不是等 `getConversation` 回来再换。
      //
      // 两个理由：
      // 1. 否则在等待期间按发送，`visible()` 拿到的是**上一个会话**的历史，
      //    而请求会带着 `activeId`（新会话）发出去 —— 模型收到 A 的对话，
      //    回答写进 B。这是数据错乱，不只是显示问题；
      // 2. 大会话要读几百毫秒，留着上一条会话的内容会闪一下「错误的对话」。
      //
      // 在飞的气泡挂在 `live[会话id]` 上，不受这里影响，切回来照样能看到。
      set({
        activeId: id,
        loadingMessages: true,
        messages: [],
        activeSummary: "",
        activeSummaryCount: 0,
        error: null,
      });
      try {
        const detail = await api.getConversation(id);
        // 用户可能在等待期间又切走了：那就不要用旧结果覆盖新界面。
        if (get().activeId !== id) return;
        set({
          messages: toUiMessages(detail.messages),
          error: null,
          activeSummary: detail.conversation.summary ?? "",
          activeSummaryCount: detail.conversation.summaryMessageCount ?? 0,
        });
      } catch (e) {
        set({ error: String(e) });
      } finally {
        if (get().activeId === id) set({ loadingMessages: false });
      }
    },

    newConversation: async () => {
      // 只切换到「空会话」视图。正在跑的那条流**不**受影响 —— 它挂在
      // `live[原会话]` 上，切回去还能接着看。
      set({
        activeId: null,
        messages: [],
        error: null,
        activeSummary: "",
        activeSummaryCount: 0,
        // 审批卡片属于某个会话；新对话里不该还留着上一段的待批工具，
        // 否则用户会在一个「没有上下文」的界面里批准一个他看不到来由的工具。
        pendingApprovals: [],
      });
    },

    renameConversation: async (id, title) => {
      await api.renameConversation(id, title);
      await get().refreshConversations();
    },

    togglePinned: async (id) => {
      const conv = get().conversations.find((c) => c.id === id);
      if (!conv) return;
      await api.setConversationPinned(id, !conv.pinned);
      await get().refreshConversations();
    },

    deleteConversation: async (id) => {
      // 先把这个会话里**还在生成**的请求取消掉。
      //
      // 不取消的话：后端会继续跑到底（白烧 token），它的终态事件又没有路由可去，
      // 而界面上「停止」按钮早就随着 streamingId 清空消失了 —— 用户完全够不着它。
      const { routes } = get();
      const orphan = Object.entries(routes).filter(
        ([, route]) => route.conversationId === id,
      );
      await Promise.all(
        orphan.map(([requestId]) =>
          api.chatCancel(requestId).catch(() => {
            // 已经结束了 —— 不是错误。
          }),
        ),
      );

      const orphanRequests = new Set(orphan.map(([requestId]) => requestId));

      await api.deleteConversation(id);
      set((s) => {
        const dropped = dropConversation(s, id);
        // 顺带撤掉挂给这个会话的审批卡片：会话都没了，卡片却还留着可点，
        // 点下去工具照样会执行。
        return {
          ...(s.activeId === id ? { activeId: null, messages: [] } : {}),
          ...dropped,
          pendingApprovals: s.pendingApprovals.filter(
            (p) => !orphanRequests.has(p.requestId),
          ),
        };
      });
      await get().refreshConversations();
    },

    send: async (text, images = []) => {
      const trimmed = text.trim();
      // 只贴了图、没写字也是合法的一条消息。
      if (!trimmed && images.length === 0) return false;

      // 会话还在从库里读：这时候 `visible()` 是空的，发出去的请求会缺历史。
      // 与其发一条上下文不完整的请求（甚至发错会话的历史），不如让用户等一下。
      if (get().loadingMessages) {
        set({ error: "会话还在载入，请稍等一下再发送。" });
        return false;
      }

      if (get().streamingId || get().submitting) {
        set({
          error: "还有一条消息正在发送或生成中，请先等它结束或点「停止」。",
        });
        return false;
      }

      const history = [
        ...historyFrom(visible()),
        { role: "user", content: trimmed },
      ];

      // 先自检再落库：明知发不出去（没配 Key / 没选模型 / 历史超上限）时
      // 就别先建会话、先存消息了 —— 否则库里留下一条「发不出去的消息」，
      // 而草稿又还给了用户，重发就会变成两条。
      const blocked = preflight(history.length);
      if (blocked) {
        set({ error: blocked });
        return false;
      }

      return submit({ newUserText: trimmed, newImages: images, history });
    },

    /**
     * 删除单条消息。
     *
     * `delete_message` 命令一直存在，但之前只在「编辑后重发」里被当作清理手段
     * 调用 —— 界面上没有任何入口。结果是一条说错的话只能靠「编辑并重发」
     * 或者「删掉整个会话」来处理。
     */
    deleteMessage: async (messageId) => {
      const index = get().messages.findIndex((m) => m.id === messageId);
      if (index < 0) return;

      // 还没落库的乐观气泡：直接从界面上撤掉就行。
      if (!messageId.startsWith("local_")) {
        try {
          await api.deleteMessage(messageId);
        } catch (e) {
          set({ error: `删除这条消息失败：${String(e)}` });
          return;
        }
      }

      set((s) => ({
        messages: s.messages.filter((m) => m.id !== messageId),
        error: null,
      }));
      await get().refreshConversations();
    },

    /**
     * 重新发起某一条助手消息之前的对话。
     *
     * 「重试失败消息」和「重新生成」共用这一条：找到目标助手消息，把它从
     * 库里删掉（否则反复重试会堆出一串没人看的回答），再以它之前的完整历史
     * 重新提问。
     */
    retry: async (messageId) => {
      if (get().streamingId || get().submitting) return;

      const messages = visible();
      const index = messages.findIndex((m) => m.id === messageId);
      if (index < 0) return;
      const target = messages[index];
      if (target.role !== "assistant") return;

      const history = historyFrom(messages.slice(0, index));
      if (!history.some((m) => m.role === "user")) return;

      // **先自检、再删**：这次重发注定发不出去的话，那条旧回答不能白白删掉。
      const blocked = preflight(history.length);
      if (blocked) {
        set({ error: blocked });
        return;
      }

      // 本地占位气泡（local_ 前缀）还没进库，跳过。
      if (!target.id.startsWith("local_")) {
        try {
          await api.deleteMessage(target.id);
        } catch {
          // 已经被删掉了 —— 不是错误。
        }
      }

      set({ messages: messages.slice(0, index) });
      // 关键：不传 `newUserText`，所以那条用户消息不会被重复落库。
      await submit({ history });
    },

    regenerate: async () => {
      const messages = visible();
      const last = messages[messages.length - 1];
      if (!last || last.role !== "assistant") return;
      await get().retry(last.id);
    },

    /**
     * 编辑一条用户消息后重发。
     *
     * 与「重试」的区别：重试只重发最后一条回答之前的历史；这里可以改
     * **中间任意一条**用户消息 —— 改完之后，它后面的所有消息（含那些旧回答）
     * 都会被删掉，因为它们的上下文已经变了，留着就是一条幽灵分支。
     */
    editAndResend: async (messageId, newContent) => {
      const trimmed = newContent.trim();
      if (!trimmed || get().streamingId || get().submitting) return;

      const messages = visible();
      const index = messages.findIndex((m) => m.id === messageId);
      if (index < 0) return;
      const target = messages[index];
      // 只能编辑已落库的用户消息（local_ 前缀的是还没落库的乐观气泡）。
      if (target.role !== "user" || target.id.startsWith("local_")) return;

      // 3) 历史 = 之前的部分 + 编辑后的这一条。
      const history = historyFrom(messages.slice(0, index));
      // **保留原来的 id**：图片不存在前端，后端要按 id 从库里取。
      // 之前这里传 `null`，于是「编辑一条带图的消息再重发」时模型收不到那张图，
      // 而气泡上还显示着它 —— 回答看起来就像没看见图一样。
      history.push({ id: target.id, role: "user", content: trimmed });

      // **先自检、再删**：这次重发注定发不出去的话（没配 Key、历史超上限），
      // 后面那一串删除会把旧回答全部抹掉，而用户什么也换不回来。
      const blocked = preflight(history.length);
      if (blocked) {
        set({ error: blocked });
        return;
      }

      // 1) 改库。
      try {
        await api.updateMessage(target.id, trimmed);
      } catch (e) {
        set({ error: `保存修改失败：${String(e)}` });
        return;
      }

      // 2) 删掉它之后的所有消息（含旧回答）。
      const failures: string[] = [];
      for (const m of messages.slice(index + 1)) {
        if (!m.id.startsWith("local_")) {
          try {
            await api.deleteMessage(m.id);
          } catch (e) {
            failures.push(String(e));
          }
        }
      }

      // 有删失败的就别往下走了：这时候界面截断了、库里没删干净，
      // 下一轮结算会把「已经删掉」的旧回答原样读回来，变成一条幽灵分支。
      if (failures.length > 0) {
        set({
          error: `有 ${failures.length} 条旧消息没能删掉，已取消这次重发以免出现重复回答。请重试一次。`,
        });
        return;
      }

      // 4) 界面截断到这条（用编辑后的内容替换），再重发。
      set({
        messages: [
          ...messages.slice(0, index),
          { ...target, content: trimmed },
        ],
      });
      await submit({ history });
    },

    cancel: async () => {
      const { activeId, routes } = get();

      // 只取消**当前正在看的**这个会话的生成。
      //
      // 旧实现按全局 `streamingId` 找路由：用户在 A 生成时切到 B，B 的输入框
      // 也显示「停止」，一点就把 A 的生成杀掉，而 B 里其实什么都没在跑。
      const entry = Object.entries(routes).find(
        ([, route]) => route.conversationId === activeId,
      );
      if (entry) {
        await api.chatCancel(entry[0]);
      }
    },

    setToolsEnabled: (enabled) => set({ toolsEnabled: enabled }),

    approveTool: async (requestId, callId, approved, remember) => {
      // 乐观地先撤下卡片，再通知后端；后端会通过 ToolStart / ToolRejected
      // 事件推进时间线。
      const removed = get().pendingApprovals.filter((p) => p.callId === callId);
      set((s) => ({
        pendingApprovals: s.pendingApprovals.filter((p) => p.callId !== callId),
      }));
      try {
        const accepted = await api.chatApproveTool(
          requestId,
          callId,
          approved,
          remember,
        );
        // 后端说「这个审批已经失效了」（超时 / 已被别处处理）：卡片该留在
        // 原地已经没有意义，但要告诉用户一声，否则他会以为批准生效了。
        if (!accepted) {
          set({ error: "这次审批已经失效（可能已超时），请重新发起。" });
        }
      } catch (e) {
        // IPC 层失败（命令本身不会失败）：把卡片放回去再报错，
        // 否则卡片没了、后端还在等，60 秒后自动拒绝掉用户想批准的工具。
        set((s) => ({
          pendingApprovals: [...s.pendingApprovals, ...removed],
          error: `审批没能送达：${String(e)}`,
        }));
      }
    },

    handleEvent: (event) => {
      // 摘要挂在**会话**上（不是某条消息），而且这个事件在终态之后才到 ——
      // 那时路由表已经清空，所以它自带 conversationId。
      if (event.type === "summary_updated") {
        set((s) => ({
          conversations: s.conversations.map((c) =>
            c.id === event.conversationId
              ? {
                  ...c,
                  summary: event.summary,
                  summaryMessageCount: event.coveredMessages,
                }
              : c,
          ),
          ...(s.activeId === event.conversationId
            ? {
                activeSummary: event.summary,
                activeSummaryCount: event.coveredMessages,
              }
            : {}),
        }));
        return;
      }

      // 工具审批卡独立于消息折叠维护（可能跨会话），这里单独处理。
      if (event.type === "tool_pending") {
        set((s) => ({
          pendingApprovals: [
            ...s.pendingApprovals,
            {
              requestId: event.requestId,
              callId: event.callId,
              name: event.name,
              summary: event.summary,
              canRemember: event.canRemember,
            },
          ],
        }));
      } else if (event.type === "tool_start" || event.type === "tool_rejected") {
        set((s) => ({
          pendingApprovals: s.pendingApprovals.filter(
            (p) => p.callId !== event.callId,
          ),
        }));
      }

      // 路由要在折叠**之前**读：终态折叠会把它删掉。
      const route = get().routes[event.requestId];
      const patch = reduceChatEvent(get(), event);
      if (!patch) return;
      set(patch);

      if (route && isTerminalEvent(event)) {
        // 终态：清掉这条请求残留的审批卡，再回读数据库。
        set((s) => ({
          pendingApprovals: s.pendingApprovals.filter(
            (p) => p.requestId !== event.requestId,
          ),
        }));
        void (async () => {
          await get().refreshConversations();
          // 把「这次结算的是哪一条」一并传下去：结算要跑两次 IPC，
          // 这期间用户可能已经发出了下一条，不能把它的气泡一起清掉。
          await get().settleConversation(route.conversationId, {
            requestId: event.requestId,
            messageId: route.messageId,
          });
        })();
      }
    },

    settleConversation,
  };
});

/** 订阅后端事件；返回取消订阅函数。在 main.tsx 调一次即可。 */
export function bindChatEvents(): () => void {
  return onChatEvent((event) => useChat.getState().handleEvent(event));
}
