// 一问多答 store 的测试：并发发送、事件路由、采用。
//
// 守的是这个功能里最容易出错的两点：
// - N 路请求必须**各自带一个 requestId**，否则事件无法区分（答案会串台）；
// - 事件必须只落到自己那一列（认不出的原样放过 —— 主动聊天的事件也走这条路）。

import { beforeEach, describe, expect, it, vi } from "vitest";

import type { SequencedChatEvent } from "../types";

const api = {
  chatSend: vi.fn(),
  chatCancel: vi.fn(),
  adoptAnswer: vi.fn(),
  updateSettings: vi.fn(),
};

vi.mock("../services/api", () => ({ api }));
vi.mock("../services/events", () => ({ onChatEvent: () => () => {} }));
vi.mock("../stores/config", () => ({
  useConfig: {
    getState: () => ({ config: null }),
    setState: () => {},
  },
}));

const { useCompare } = await import("./compare");
const { useProviders } = await import("./providers");

function event(over: Partial<SequencedChatEvent> & { type: string }): SequencedChatEvent {
  return { seq: 1, requestId: "x", ...over } as SequencedChatEvent;
}

beforeEach(() => {
  vi.clearAllMocks();
  api.chatSend.mockResolvedValue("rid");
  api.chatCancel.mockResolvedValue(true);
  api.adoptAnswer.mockResolvedValue({ userMessageId: "u1", assistantMessageId: "a1" });
  api.updateSettings.mockResolvedValue(undefined);
  useCompare.setState({
    open: false,
    question: "",
    targets: [],
    results: [],
    notice: null,
  });
  useProviders.setState({
    state: {
      providers: [
        {
          id: "deepseek",
          name: "DeepSeek",
          baseUrl: "",
          models: [{ key: "deepseek-chat", favorite: false }],
          isPreset: true,
        },
        {
          id: "openai",
          name: "OpenAI",
          baseUrl: "",
          models: [{ key: "gpt-4o", favorite: false }],
          isPreset: true,
        },
      ],
      defaultProviderId: "deepseek",
      defaultModelKey: "deepseek-chat",
      error: null,
    },
    loading: false,
    error: null,
  } as never);
});

describe("run", () => {
  it("每个模型一路请求，各自带 requestId、不落库、不带工具", async () => {
    useCompare.setState({
      question: "什么是闭包",
      targets: [
        { providerId: "deepseek", modelKey: "deepseek-chat" },
        { providerId: "openai", modelKey: "gpt-4o" },
      ],
    });

    await useCompare.getState().run();

    expect(api.chatSend).toHaveBeenCalledTimes(2);
    const calls = api.chatSend.mock.calls.map((c) => c[0]);
    const ids = calls.map((c) => c.requestId);
    expect(new Set(ids).size).toBe(2);

    for (const call of calls) {
      expect(call.conversationId).toBeNull(); // 对比是草稿纸，不写进聊天记录
      expect(call.enableTools).toBe(false); // N 路同时弹审批框没人受得了
      expect(call.messages).toEqual([{ role: "user", content: "什么是闭包" }]);
    }

    // 标签带上服务商名，方便并排时区分
    const results = useCompare.getState().results;
    expect(results.map((r) => r.label)).toEqual([
      "DeepSeek · deepseek-chat",
      "OpenAI · gpt-4o",
    ]);
    expect(results.every((r) => r.streaming)).toBe(true);
  });

  it("少选 / 空问题直接给提示，不发请求", async () => {
    useCompare.setState({
      question: "问题",
      targets: [{ providerId: "deepseek", modelKey: "deepseek-chat" }],
    });
    await useCompare.getState().run();
    expect(api.chatSend).not.toHaveBeenCalled();
    expect(useCompare.getState().notice).toContain("至少");

    useCompare.setState({ question: "   ", targets: [] });
    await useCompare.getState().run();
    expect(api.chatSend).not.toHaveBeenCalled();
    expect(useCompare.getState().notice).toContain("问题");
  });

  it("上一轮还在跑时不开新的（否则两轮结果混在一起）", async () => {
    useCompare.setState({
      question: "问题",
      targets: [
        { providerId: "deepseek", modelKey: "deepseek-chat" },
        { providerId: "openai", modelKey: "gpt-4o" },
      ],
      results: [
        {
          providerId: "deepseek",
          modelKey: "deepseek-chat",
          requestId: "old",
          label: "旧的",
          content: "",
          reasoning: "",
          streaming: true,
          cancelled: false,
          error: null,
          inputTokens: 0,
          outputTokens: 0,
          adopted: false,
        },
      ],
    });
    await useCompare.getState().run();
    expect(api.chatSend).not.toHaveBeenCalled();
    expect(useCompare.getState().notice).toContain("还没跑完");
  });

  it("某一路发不出去时，只有那一列标错，其余照常", async () => {
    // 第一路失败、第二路照常发出
    let call = 0;
    api.chatSend.mockImplementation(async () => {
      call += 1;
      if (call === 1) throw new Error("网络断了");
      return "rid";
    });
    useCompare.setState({
      question: "问题",
      targets: [
        { providerId: "deepseek", modelKey: "deepseek-chat" },
        { providerId: "openai", modelKey: "gpt-4o" },
      ],
    });
    await useCompare.getState().run();

    const results = useCompare.getState().results;
    expect(results[0].error).toContain("网络断了");
    expect(results[0].streaming).toBe(false);
    // 另一路不受影响：还在流式，没有错误
    expect(results[1].error).toBeNull();
    expect(results[1].streaming).toBe(true);
  });
});

describe("handleEvent", () => {
  it("增量只落到自己那一列", async () => {
    useCompare.setState({
      question: "问题",
      targets: [
        { providerId: "deepseek", modelKey: "deepseek-chat" },
        { providerId: "openai", modelKey: "gpt-4o" },
      ],
    });
    await useCompare.getState().run();
    const [first, second] = useCompare.getState().results;

    useCompare.getState().handleEvent(
      event({ type: "delta", requestId: second.requestId, text: "乙" }),
    );
    useCompare.getState().handleEvent(
      event({ type: "delta", requestId: first.requestId, text: "甲" }),
    );

    const after = useCompare.getState().results;
    expect(after[0].content).toBe("甲");
    expect(after[1].content).toBe("乙");
  });

  it("认不出的 requestId 不改任何状态", async () => {
    useCompare.setState({
      question: "问题",
      targets: [
        { providerId: "deepseek", modelKey: "deepseek-chat" },
        { providerId: "openai", modelKey: "gpt-4o" },
      ],
    });
    await useCompare.getState().run();
    const before = useCompare.getState().results;

    useCompare.getState().handleEvent(
      event({ type: "delta", requestId: "别人的请求", text: "不该出现" }),
    );

    // 同一个引用：连一次无谓的重渲染都不该触发
    expect(useCompare.getState().results).toBe(before);
  });
});

describe("adopt", () => {
  it("没有会话时给提示，不写库", async () => {
    useCompare.setState({
      results: [
        {
          providerId: "a",
          modelKey: "m",
          requestId: "r1",
          label: "A",
          content: "答案",
          reasoning: "",
          streaming: false,
          cancelled: false,
          error: null,
          inputTokens: 0,
          outputTokens: 0,
          adopted: false,
        },
      ],
    });
    await useCompare.getState().adopt("r1", null);
    expect(api.adoptAnswer).not.toHaveBeenCalled();
    expect(useCompare.getState().notice).toContain("会话");
  });

  it("采用成功会标上「已采用」", async () => {
    useCompare.setState({
      question: "问题",
      results: [
        {
          providerId: "a",
          modelKey: "m",
          requestId: "r1",
          label: "A",
          content: "答案",
          reasoning: "",
          streaming: false,
          cancelled: false,
          error: null,
          inputTokens: 0,
          outputTokens: 0,
          adopted: false,
        },
      ],
    });
    await useCompare.getState().adopt("r1", "conv-1");

    expect(api.adoptAnswer).toHaveBeenCalledWith({
      conversationId: "conv-1",
      question: "问题",
      answer: "答案",
      providerId: "a",
      modelKey: "m",
    });
    expect(useCompare.getState().results[0].adopted).toBe(true);
  });

  it("空回答 / 失败的不能采用", async () => {
    useCompare.setState({
      results: [
        {
          providerId: "a",
          modelKey: "m",
          requestId: "r1",
          label: "A",
          content: "   ",
          reasoning: "",
          streaming: false,
          cancelled: false,
          error: null,
          inputTokens: 0,
          outputTokens: 0,
          adopted: false,
        },
      ],
    });
    await useCompare.getState().adopt("r1", "conv-1");
    expect(api.adoptAnswer).not.toHaveBeenCalled();
  });
});

describe("stopAll", () => {
  it("只取消还在跑的那几路", async () => {
    useCompare.setState({
      results: [
        {
          providerId: "a",
          modelKey: "m",
          requestId: "running",
          label: "A",
          content: "",
          reasoning: "",
          streaming: true,
          cancelled: false,
          error: null,
          inputTokens: 0,
          outputTokens: 0,
          adopted: false,
        },
        {
          providerId: "b",
          modelKey: "m",
          requestId: "done",
          label: "B",
          content: "答案",
          reasoning: "",
          streaming: false,
          cancelled: false,
          error: null,
          inputTokens: 0,
          outputTokens: 0,
          adopted: false,
        },
      ],
    });
    await useCompare.getState().stopAll();
    expect(api.chatCancel).toHaveBeenCalledTimes(1);
    expect(api.chatCancel).toHaveBeenCalledWith("running");
  });
});
