// 聊天状态的**纯折叠逻辑**。
//
// 从 `stores/chat.ts` 里抽出来，理由和另外两个 lib 模块一样：store 文件
// import 了 `@tauri-apps/api`，在 node 里测它就得把整个 IPC 层 mock 掉；
// 而这里只吃「一个事件 + 一份状态」，可以直接断言输出。

import type {
  SequencedChatEvent,
  StoredMessage,
  ToolTrace,
  UiMessage,
} from "../types";

/** 一条在飞请求的路由信息。 */
export interface StreamRoute {
  /** 前端本地生成的助手消息 id。 */
  messageId: string;
  /** 这条请求属于哪个会话。 */
  conversationId: string;
}

/**
 * 聊天状态里与「流式」相关的那部分。
 *
 * 关键设计：**在飞的助手消息不放在 `messages` 里，而是单独放 `live`**。
 *
 * `messages` 只表示「当前会话已经落库的内容」。在飞的那条按**它所属的会话**
 * 索引在 `live` 里，于是：
 * - 用户在一轮回答还没结束时切到别的会话，那条回答不会凭空消失；
 * - 再切回来时它还在，继续往下长；
 * - 也不会出现同一条消息在 `messages` 和 `live` 里各存一份、更新时要同步两处
 *   的经典 bug。
 */
export interface ChatStreamState {
  messages: UiMessage[];
  live: Record<string, UiMessage>;
  routes: Record<string, StreamRoute>;
  streamingId: string | null;
  activeId: string | null;
}

/** 界面上真正要显示的消息 = 已落库的 + 当前会话在飞的那条。 */
export function visibleMessages(
  state: Pick<ChatStreamState, "messages" | "live" | "activeId">,
): UiMessage[] {
  const inFlight = state.activeId ? state.live[state.activeId] : undefined;
  return inFlight ? [...state.messages, inFlight] : state.messages;
}

export function toUiMessages(stored: StoredMessage[]): UiMessage[] {
  return stored.map((m) => ({
    id: m.id,
    role: m.role,
    content: m.content,
    reasoning: m.reasoning ?? "",
    streaming: false,
    error: m.status === "error" ? m.error : null,
    cancelled: m.status === "cancelled",
    inputTokens: m.inputTokens,
    outputTokens: m.outputTokens,
    createdAt: m.createdAt,
    trimmed: 0,
    summarized: 0,
    tools: [],
    images: m.images ?? [],
  }));
}

export function makeUserMessage(
  id: string,
  content: string,
  createdAt: number,
  images: string[] = [],
): UiMessage {
  return {
    id,
    role: "user",
    content,
    reasoning: "",
    streaming: false,
    error: null,
    cancelled: false,
    inputTokens: null,
    outputTokens: null,
    createdAt,
    trimmed: 0,
    summarized: 0,
    tools: [],
    images,
  };
}

export function makeAssistantPlaceholder(id: string, createdAt: number): UiMessage {
  return {
    id,
    role: "assistant",
    content: "",
    reasoning: "",
    streaming: true,
    error: null,
    cancelled: false,
    inputTokens: null,
    outputTokens: null,
    createdAt,
    trimmed: 0,
    summarized: 0,
    tools: [],
    images: [],
  };
}

/**
 * 把一个后端事件折叠进状态。
 *
 * 返回 `null` 表示这个事件与当前状态无关（例如对应的请求已经被结算掉了），
 * 调用方应当直接忽略，不要 `set({})`。
 */
export function reduceChatEvent(
  state: ChatStreamState,
  event: SequencedChatEvent,
): Partial<ChatStreamState> | null {
  const route = state.routes[event.requestId];
  if (!route) return null;

  const { conversationId } = route;
  const current = state.live[conversationId];
  // 消息已经被结算（或用户清空了会话），这个事件无处可去。
  if (!current || current.id !== route.messageId) return null;

  const replaceLive = (next: UiMessage): Partial<ChatStreamState> => ({
    live: { ...state.live, [conversationId]: next },
  });

  switch (event.type) {
    case "start":
      return replaceLive({ ...current, streaming: true });

    case "delta":
      return replaceLive({ ...current, content: current.content + event.text });

    case "reasoning_delta":
      return replaceLive({
        ...current,
        reasoning: current.reasoning + event.text,
      });

    case "context_trimmed":
      // 后端为了装进上下文预算，从最旧的开始丢掉了 n 条，
      // 另有 m 条改用了摘要替代（这部分信息没丢）。
      return replaceLive({
        ...current,
        trimmed: event.droppedMessages,
        summarized: event.summarizedMessages,
      });

    case "usage":
      return replaceLive({
        ...current,
        inputTokens: event.inputTokens,
        outputTokens: event.outputTokens,
      });

    case "finish":
      return settle(state, event.requestId, conversationId, {
        ...current,
        streaming: false,
      });

    case "cancelled":
      return settle(state, event.requestId, conversationId, {
        ...current,
        streaming: false,
        cancelled: true,
      });

    case "error":
      return settle(state, event.requestId, conversationId, {
        ...current,
        streaming: false,
        error: {
          code: event.code,
          message: event.message,
          retryable: event.retryable,
        },
      });

    case "tool_start":
      return replaceLive(
        upsertTool(current, {
          callId: event.callId,
          name: event.name,
          status: "running",
        }),
      );

    case "tool_end":
      return replaceLive(
        upsertTool(current, {
          callId: event.callId,
          name: event.name,
          status: "ok",
          result: event.result,
        }),
      );

    case "tool_error":
      return replaceLive(
        upsertTool(current, {
          callId: event.callId,
          name: event.name,
          status: "error",
          result: event.message,
        }),
      );

    case "tool_pending":
      return replaceLive(
        upsertTool(current, {
          callId: event.callId,
          name: event.name,
          status: "pending",
          summary: event.summary,
          canRemember: event.canRemember,
        }),
      );

    case "tool_rejected":
      return replaceLive(
        upsertTool(current, {
          callId: event.callId,
          name: event.name,
          status: "rejected",
        }),
      );

    case "summary_updated":
      // 摘要挂在**会话**上，不属于某条消息 —— 由 store 单独处理
      // （见 `handleEvent`），这里不动消息状态。
      return null;
  }
}

/** 在消息的工具时间线里 upsert 一条。同名 callId 只保留一份，状态可前进。 */
function upsertTool(message: UiMessage, trace: ToolTrace): UiMessage {
  const existing = message.tools.find((t) => t.callId === trace.callId);
  const tools = existing
    ? message.tools.map((t) => (t.callId === trace.callId ? { ...t, ...trace } : t))
    : [...message.tools, trace];
  return { ...message, tools };
}

/**
 * 终态收尾。
 *
 * 注意消息**留在 `live` 里**不删：结算需要一次「回数据库读权威数据」的异步
 * 往返，在这个往返完成之前把它删掉，界面上那条气泡会先消失再出现（闪一下）。
 * 真正的删除由 store 的 `settleConversation` 在读完之后做。
 */
function settle(
  state: ChatStreamState,
  requestId: string,
  conversationId: string,
  message: UiMessage,
): Partial<ChatStreamState> {
  const routes = { ...state.routes };
  delete routes[requestId];

  return {
    live: { ...state.live, [conversationId]: message },
    routes,
    streamingId: state.streamingId === message.id ? null : state.streamingId,
  };
}

/** 从路由表里移掉一个会话（删会话 / 清空历史时用）。 */
export function dropConversation(
  state: ChatStreamState,
  conversationId: string,
): Partial<ChatStreamState> {
  const live = { ...state.live };
  delete live[conversationId];

  const routes: Record<string, StreamRoute> = {};
  let streamingId = state.streamingId;
  for (const [requestId, route] of Object.entries(state.routes)) {
    if (route.conversationId === conversationId) {
      if (streamingId === route.messageId) streamingId = null;
      continue;
    }
    routes[requestId] = route;
  }

  return { live, routes, streamingId };
}
