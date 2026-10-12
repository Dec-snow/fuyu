// 聊天 store 的状态机测试。
//
// 这些用例守的是这一轮修掉的一批**竞态与静默失败**：它们都很隐蔽
// （要么丢数据、要么什么都不发生），靠手工点界面很难稳定复现，
// 所以把 IPC 全部 mock 掉、直接驱动 store。
//
// 覆盖点：
// - 发不出去时 `send` 必须返回 false（输入框据此保住草稿）
// - 「重试」在注定发不出去时**不能先删掉旧回答**
// - 并发闸门是同步的（两次提交只会发一次请求）
// - 结算只清自己那一轮的气泡，不会误删期间新发起的请求
// - 技能 id 要真的进到请求里（否则「选了技能」只是界面上的假象）

import { beforeEach, describe, expect, it, vi } from "vitest";

import type { UiMessage } from "../types";

const api = {
  createConversation: vi.fn(),
  saveUserMessage: vi.fn(),
  chatSend: vi.fn(),
  chatCancel: vi.fn(),
  deleteMessage: vi.fn(),
  getConversation: vi.fn(),
  listConversations: vi.fn(),
  deleteConversation: vi.fn(),
};

vi.mock("../services/api", () => ({ api }));
vi.mock("../services/events", () => ({
  onChatEvent: () => () => {},
  startEventBridge: async () => {},
}));
// 摘要等配置项：给一个空配置就够，避免真的去读设置。
vi.mock("../stores/config", () => ({
  useConfig: {
    getState: () => ({ config: null, info: null, loaded: true, error: null }),
    setState: () => {},
  },
  applyTheme: () => {},
}));

const { CONVERSATION_LIMIT_MAX, useChat } = await import("./chat");
const { useProviders } = await import("./providers");

const provider = {
  id: "deepseek",
  name: "DeepSeek",
  baseUrl: "https://api.deepseek.com/v1",
  models: [{ key: "deepseek-chat", favorite: false }],
  isPreset: true,
  hasKey: true,
};

const conversation = {
  id: "c1",
  title: "新对话",
  providerId: "deepseek",
  modelKey: "deepseek-chat",
  systemPrompt: null,
  pinned: false,
  createdAt: 1,
  updatedAt: 1,
  messageCount: 0,
  summary: "",
  summaryMessageCount: 0,
};

/** 造一条完整的界面消息（字段很多，测试里只想说清关心的那几个）。 */
function uiMessage(partial: Partial<UiMessage> & Pick<UiMessage, "id" | "role" | "content">): UiMessage {
  return {
    reasoning: "",
    streaming: false,
    error: null,
    cancelled: false,
    inputTokens: null,
    outputTokens: null,
    createdAt: 1,
    trimmed: 0,
    summarized: 0,
    tools: [],
    images: [],
    ...partial,
  };
}

function resetStore() {
  useChat.setState({
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
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  resetStore();
  api.listConversations.mockResolvedValue([]);
  useProviders.setState({
    state: { providers: [provider], defaultProviderId: "deepseek", defaultModelKey: "deepseek-chat" },
    loading: false,
    error: null,
  });
});

describe("send 的返回值", () => {
  it("没有可用模型时返回 false，并把原因写进 error", async () => {
    useProviders.setState({ state: { providers: [], defaultProviderId: null, defaultModelKey: null } });

    const accepted = await useChat.getState().send("你好");

    expect(accepted).toBe(false);
    expect(useChat.getState().error).toContain("服务商");
    expect(api.chatSend).not.toHaveBeenCalled();
  });

  it("缺少 API Key 时不发请求，返回 false", async () => {
    useProviders.setState({
      state: {
        providers: [{ ...provider, hasKey: false }],
        defaultProviderId: "deepseek",
        defaultModelKey: "deepseek-chat",
      },
    });

    const accepted = await useChat.getState().send("你好");

    expect(accepted).toBe(false);
    expect(useChat.getState().error).toContain("API Key");
    expect(api.chatSend).not.toHaveBeenCalled();
  });

  it("会话还在载入时拒绝发送（否则会带着空历史发出去）", async () => {
    useChat.setState({ loadingMessages: true });

    const accepted = await useChat.getState().send("你好");

    expect(accepted).toBe(false);
    expect(api.chatSend).not.toHaveBeenCalled();
  });

  it("后端拒收时返回 false，并把乐观气泡标成错误、清干净 streamingId", async () => {
    api.createConversation.mockResolvedValue(conversation);
    api.saveUserMessage.mockResolvedValue("m1");
    api.chatSend.mockRejectedValue(new Error("服务商拒绝了这次请求"));

    const accepted = await useChat.getState().send("你好");

    expect(accepted).toBe(false);
    const state = useChat.getState();
    // 收尾必须彻底：streamingId 挂着的话发送按钮会永远停在「停止」。
    expect(state.streamingId).toBeNull();
    expect(state.routes).toEqual({});
    // 已经落库的用户消息**留在界面上**：数据库里有它，界面就不能装作没有，
    // 否则切走再切回来它会「复活」，看起来像幽灵消息。
    expect(state.messages.some((m) => m.content === "你好")).toBe(true);
    const live = Object.values(state.live);
    expect(live.length).toBe(1);
    expect(live[0].error).toBeTruthy();
  });

  it("成功发出时返回 true，并挂上在飞气泡与路由", async () => {
    api.createConversation.mockResolvedValue(conversation);
    api.saveUserMessage.mockResolvedValue("m1");
    api.chatSend.mockResolvedValue(undefined);

    const accepted = await useChat.getState().send("你好");

    expect(accepted).toBe(true);
    const state = useChat.getState();
    expect(state.streamingId).not.toBeNull();
    expect(Object.keys(state.routes)).toHaveLength(1);
    expect(state.messages.some((m) => m.content === "你好")).toBe(true);
  });
});

describe("并发闸门", () => {
  it("第一次提交还卡在 IPC 上时，第二次提交被同步挡住", async () => {
    api.createConversation.mockResolvedValue(conversation);
    api.chatSend.mockResolvedValue(undefined);

    // 第一次提交停在「落库」这一步 —— 这正是旧实现最危险的窗口：
    // `streamingId` 还没设上，第二次提交会挤进来，两条回答都写进数据库，
    // 但界面上只能看到一条。
    let releaseSave = () => {};
    api.saveUserMessage.mockImplementation(
      () =>
        new Promise<string>((resolve) => {
          releaseSave = () => resolve("m1");
        }),
    );

    const first = useChat.getState().send("第一条");
    const second = await useChat.getState().send("第二条");

    expect(second).toBe(false);
    expect(api.chatSend).not.toHaveBeenCalled();

    releaseSave();
    expect(await first).toBe(true);
    expect(api.chatSend).toHaveBeenCalledTimes(1);
    // 闸门要放开，否则界面会被永久锁死。
    expect(useChat.getState().submitting).toBe(false);
  });
});

describe("重试前的预检", () => {
  it("注定发不出去时不会先删掉旧回答", async () => {
    useChat.setState({
      messages: [
        uiMessage({ id: "u1", role: "user", content: "问题", createdAt: 1 }),
        uiMessage({ id: "a1", role: "assistant", content: "旧回答", createdAt: 2 }),
      ],
    });
    // 模型被切到一个没有 Key 的服务商上。
    useProviders.setState({
      state: {
        providers: [{ ...provider, hasKey: false }],
        defaultProviderId: "deepseek",
        defaultModelKey: "deepseek-chat",
      },
    });

    await useChat.getState().retry("a1");

    // 关键：旧回答一个字都不能动 —— 以前是先删再发，发不出去就永久丢了。
    expect(api.deleteMessage).not.toHaveBeenCalled();
    expect(useChat.getState().messages).toHaveLength(2);
    expect(useChat.getState().error).toContain("API Key");
  });
});

describe("会话列表上限", () => {
  it("刷新时把当前上限传给后端（默认 200）", async () => {
    api.listConversations.mockResolvedValue([]);
    await useChat.getState().refreshConversations();
    expect(api.listConversations).toHaveBeenCalledWith(200);
  });

  it("「加载更多」会放大上限并重新拉取", async () => {
    api.listConversations.mockResolvedValue([]);
    await useChat.getState().loadMoreConversations();

    expect(useChat.getState().conversationLimit).toBe(400);
    expect(api.listConversations).toHaveBeenLastCalledWith(400);
  });

  it("到后端的硬上限就停住，不会无限要", async () => {
    api.listConversations.mockResolvedValue([]);
    useChat.setState({ conversationLimit: CONVERSATION_LIMIT_MAX });
    await useChat.getState().loadMoreConversations();

    // 后端 `clamp(1, 1000)`，要得再多也只会拿到 1000 条。
    expect(useChat.getState().conversationLimit).toBe(CONVERSATION_LIMIT_MAX);
  });
});

describe("结算", () => {
  it("结算期间新发起的请求不会被误删", async () => {
    api.createConversation.mockResolvedValue(conversation);
    api.saveUserMessage.mockResolvedValue("m1");
    api.chatSend.mockResolvedValue(undefined);

    await useChat.getState().send("第一问");
    const firstRoute = Object.entries(useChat.getState().routes)[0];
    const [firstRequestId, firstRouteValue] = firstRoute;

    // 第一轮的终态先到：折叠会把 streamingId 清掉。
    useChat.getState().handleEvent({
      type: "finish", requestId: firstRequestId, seq: 100, reason: "stop",
    });
    expect(useChat.getState().streamingId).toBeNull();

    // 就在结算的两次 IPC 还没跑完时，用户又发了第二条。
    api.getConversation.mockResolvedValue({
      conversation, messages: [], summaryPending: false,
    });
    const settling = useChat.getState().settleConversation("c1", {
      requestId: firstRequestId,
      messageId: firstRouteValue.messageId,
    });

    await useChat.getState().send("第二问");
    const secondStreamingId = useChat.getState().streamingId;
    expect(secondStreamingId).not.toBeNull();

    await settling;

    // 第二轮的气泡和路由必须原封不动地活着。
    expect(useChat.getState().streamingId).toBe(secondStreamingId);
    expect(Object.keys(useChat.getState().routes)).toHaveLength(1);
    expect(useChat.getState().routes[firstRequestId]).toBeUndefined();
  });

  it("技能 id 真的进到请求里（否则「选了技能」只是界面上的假象）", async () => {
    api.createConversation.mockResolvedValue(conversation);
    api.saveUserMessage.mockResolvedValue("m1");
    api.chatSend.mockResolvedValue("rid");

    await useChat.getState().send("帮我审一下这段代码", [], "mine-review");
    expect(api.chatSend).toHaveBeenCalledWith(
      expect.objectContaining({ skillId: "mine-review" }),
    );

    // 不选技能时要显式传 null（而不是漏掉这个字段）。
    // 先把上一轮的「在飞」状态清掉 —— 否则并发闸门会直接拦住第二次发送，
    // 那时断言的是闸门而不是技能字段（这一条我第一次就写错了）。
    useChat.setState({ streamingId: null, submitting: false, routes: {} });
    await useChat.getState().send("普通提问");
    expect(api.chatSend).toHaveBeenLastCalledWith(
      expect.objectContaining({ skillId: null }),
    );
  });
});
