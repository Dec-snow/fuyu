import { describe, expect, it } from "vitest";

import {
  dropConversation,
  makeAssistantPlaceholder,
  makeUserMessage,
  reduceChatEvent,
  toUiMessages,
  visibleMessages,
  type ChatStreamState,
  type StreamRoute,
} from "./chat-state";
import type { SequencedChatEvent, StoredMessage } from "../types";

const ROUTE: StreamRoute = { messageId: "m1", conversationId: "c1" };

/** 一个「c1 会话正在流式生成」的初始状态。 */
function streamingState(): ChatStreamState {
  return {
    messages: [makeUserMessage("u1", "你好", 1)],
    live: { c1: makeAssistantPlaceholder("m1", 2) },
    routes: { r1: ROUTE },
    streamingId: "m1",
    activeId: "c1",
  };
}

function apply(
  state: ChatStreamState,
  patch: Partial<ChatStreamState> | null,
): ChatStreamState {
  return patch ? { ...state, ...patch } : state;
}

const delta = (text: string, seq = 1): SequencedChatEvent => ({
  type: "delta",
  requestId: "r1",
  seq,
  text,
});

describe("reduceChatEvent · 增量", () => {
  it("delta 累加正文", () => {
    let state = streamingState();
    state = apply(state, reduceChatEvent(state, delta("你")));
    state = apply(state, reduceChatEvent(state, delta("好", 2)));

    expect(state.live.c1.content).toBe("你好");
    expect(state.live.c1.streaming).toBe(true);
  });

  it("reasoning_delta 与正文分开累加", () => {
    let state = streamingState();
    state = apply(
      state,
      reduceChatEvent(state, {
        type: "reasoning_delta",
        requestId: "r1",
        seq: 1,
        text: "先想想",
      }),
    );
    state = apply(state, reduceChatEvent(state, delta("答案")));

    expect(state.live.c1.reasoning).toBe("先想想");
    expect(state.live.c1.content).toBe("答案");
  });

  it("usage 写进 token 计数", () => {
    const state = apply(
      streamingState(),
      reduceChatEvent(streamingState(), {
        type: "usage",
        requestId: "r1",
        seq: 9,
        inputTokens: 12,
        outputTokens: 34,
      }),
    );

    expect(state.live.c1.inputTokens).toBe(12);
    expect(state.live.c1.outputTokens).toBe(34);
  });

  it("context_trimmed 分别记下「丢弃」与「已摘要」的条数", () => {
    const state = streamingState();
    const next = apply(
      state,
      reduceChatEvent(state, {
        type: "context_trimmed",
        requestId: "r1",
        seq: 1,
        droppedMessages: 7,
        // 这两者语义不同：丢弃是真丢了，摘要只是不再逐条发原文。
        summarizedMessages: 30,
        estimatedTokens: 30_000,
      }),
    );

    expect(next.live.c1.trimmed).toBe(7);
    expect(next.live.c1.summarized).toBe(30);
    // 后续文本不受影响。
    const after = apply(next, reduceChatEvent(next, delta("继续", 2)));
    expect(after.live.c1.trimmed).toBe(7);
    expect(after.live.c1.summarized).toBe(30);
    expect(after.live.c1.content).toBe("继续");
  });

  it("summary_updated 不碰消息状态（摘要挂在会话上）", () => {
    const state = streamingState();
    const patch = reduceChatEvent(state, {
      type: "summary_updated",
      requestId: "r1",
      seq: 1,
      conversationId: "c1",
      summary: "用户在做读书笔记。",
      coveredMessages: 30,
    });
    expect(patch).toBeNull();
  });

  it("start 把气泡标成流式中", () => {
    const state = streamingState();
    state.live.c1.streaming = false;
    const next = apply(
      state,
      reduceChatEvent(state, { type: "start", requestId: "r1", seq: 0 }),
    );
    expect(next.live.c1.streaming).toBe(true);
  });
});

describe("reduceChatEvent · 终态", () => {
  it("finish 清掉 streamingId 与路由，但消息留在 live 里等结算", () => {
    const state = streamingState();
    const next = apply(
      state,
      reduceChatEvent(state, { type: "finish", requestId: "r1", seq: 5, reason: "stop" }),
    );

    expect(next.live.c1.streaming).toBe(false);
    expect(next.streamingId).toBeNull();
    expect(next.routes.r1).toBeUndefined();
    // 关键：消息还在。结算要等一次「回数据库读」的异步往返，
    // 现在删掉它界面上会闪一下。
    expect(next.live.c1).toBeDefined();
  });

  it("cancelled 打上已停止标记", () => {
    const state = streamingState();
    const next = apply(
      state,
      reduceChatEvent(state, { type: "cancelled", requestId: "r1", seq: 3 }),
    );
    expect(next.live.c1.cancelled).toBe(true);
    expect(next.live.c1.streaming).toBe(false);
    expect(next.streamingId).toBeNull();
  });

  it("error 把错误挂到气泡上并保留可重试标记", () => {
    const state = streamingState();
    const next = apply(
      state,
      reduceChatEvent(state, {
        type: "error",
        requestId: "r1",
        seq: 4,
        code: "rate_limited",
        message: "太频繁了",
        retryable: true,
      }),
    );

    expect(next.live.c1.error).toEqual({
      code: "rate_limited",
      message: "太频繁了",
      retryable: true,
    });
    expect(next.live.c1.streaming).toBe(false);
  });

  it("只清掉属于自己的 streamingId", () => {
    const state = streamingState();
    state.streamingId = "别的消息";
    const next = apply(
      state,
      reduceChatEvent(state, { type: "finish", requestId: "r1", seq: 1, reason: null }),
    );
    expect(next.streamingId).toBe("别的消息");
  });
});

describe("reduceChatEvent · 无关事件", () => {
  it("未知 requestId 返回 null", () => {
    const state = streamingState();
    expect(
      reduceChatEvent(state, { type: "delta", requestId: "不存在", seq: 1, text: "x" }),
    ).toBeNull();
  });

  it("已结算（live 里没了）后的事件返回 null", () => {
    const state = streamingState();
    delete state.live.c1;
    expect(reduceChatEvent(state, delta("迟到"))).toBeNull();
  });

  it("live 里 id 不匹配时返回 null", () => {
    const state = streamingState();
    state.live.c1 = makeAssistantPlaceholder("另一条", 3);
    expect(reduceChatEvent(state, delta("x"))).toBeNull();
  });
});

describe("reduceChatEvent · 会话隔离", () => {
  it("用户切到别的会话后，在飞的事件照样更新那条气泡", () => {
    // 回归测试：以前 `live` 不存在，切会话会把 messages 整个换掉，
    // 在飞的气泡直接消失、后续 delta 全部丢失。
    const state = streamingState();
    state.activeId = "c2"; // 用户已经切到别的会话

    const next = apply(state, reduceChatEvent(state, delta("继续长")));

    expect(next.live.c1.content).toBe("继续长");
    // 而且**不能**把它塞进 c2 的消息列表。
    expect(next.messages).toHaveLength(1);
    expect(next.messages[0].role).toBe("user");
  });

  it("只给当前会话追加在飞气泡", () => {
    const state = streamingState();

    expect(visibleMessages(state).map((m) => m.id)).toEqual(["u1", "m1"]);

    state.activeId = "c2";
    expect(visibleMessages(state).map((m) => m.id)).toEqual(["u1"]);

    state.activeId = null;
    expect(visibleMessages(state).map((m) => m.id)).toEqual(["u1"]);
  });

  it("两个会话的在飞气泡互不影响", () => {
    const state = streamingState();
    state.live.c2 = makeAssistantPlaceholder("m2", 4);

    const next = apply(state, reduceChatEvent(state, delta("只给 c1")));

    expect(next.live.c1.content).toBe("只给 c1");
    expect(next.live.c2.content).toBe("");
  });
});

describe("reduceChatEvent · 工具时间线", () => {
  it("tool_start / tool_end 推进状态并记录结果", () => {
    let state = streamingState();
    state = apply(
      state,
      reduceChatEvent(state, {
        type: "tool_start",
        requestId: "r1",
        seq: 1,
        callId: "c1",
        name: "calculator",
        arguments: "{}",
      }),
    );
    expect(state.live.c1.tools).toEqual([
      { callId: "c1", name: "calculator", status: "running" },
    ]);

    state = apply(
      state,
      reduceChatEvent(state, {
        type: "tool_end",
        requestId: "r1",
        seq: 2,
        callId: "c1",
        name: "calculator",
        result: "42",
      }),
    );
    expect(state.live.c1.tools).toEqual([
      { callId: "c1", name: "calculator", status: "ok", result: "42" },
    ]);
  });

  it("tool_pending 与 tool_rejected 正确标记", () => {
    let state = streamingState();
    state = apply(
      state,
      reduceChatEvent(state, {
        type: "tool_pending",
        requestId: "r1",
        seq: 1,
        callId: "c1",
        name: "read_clipboard_text",
        summary: "（无参数）",
        canRemember: true,
      }),
    );
    expect(state.live.c1.tools[0]).toEqual({
      callId: "c1",
      name: "read_clipboard_text",
      status: "pending",
      summary: "（无参数）",
      canRemember: true,
    });

    state = apply(
      state,
      reduceChatEvent(state, {
        type: "tool_rejected",
        requestId: "r1",
        seq: 2,
        callId: "c1",
        name: "read_clipboard_text",
      }),
    );
    // 状态前进为 rejected，但保留 pending 阶段的 summary / canRemember 字段。
    expect(state.live.c1.tools[0].status).toBe("rejected");
    expect(state.live.c1.tools[0].summary).toBe("（无参数）");
  });

  it("多个工具调用互不干扰（按 callId 索引）", () => {
    let state = streamingState();
    state = apply(
      state,
      reduceChatEvent(state, {
        type: "tool_start",
        requestId: "r1",
        seq: 1,
        callId: "a",
        name: "calculator",
        arguments: "{}",
      }),
    );
    state = apply(
      state,
      reduceChatEvent(state, {
        type: "tool_start",
        requestId: "r1",
        seq: 2,
        callId: "b",
        name: "get_current_time",
        arguments: "{}",
      }),
    );
    state = apply(
      state,
      reduceChatEvent(state, {
        type: "tool_end",
        requestId: "r1",
        seq: 3,
        callId: "b",
        name: "get_current_time",
        result: "2026-01-01",
      }),
    );

    expect(state.live.c1.tools).toEqual([
      { callId: "a", name: "calculator", status: "running" },
      { callId: "b", name: "get_current_time", status: "ok", result: "2026-01-01" },
    ]);
  });
});

describe("dropConversation", () => {
  it("删掉该会话的在飞气泡、路由，并清掉 streamingId", () => {
    const state = streamingState();
    const patch = dropConversation(state, "c1");

    expect(patch.live).toEqual({});
    expect(patch.routes).toEqual({});
    expect(patch.streamingId).toBeNull();
  });

  it("不动其他会话", () => {
    const state = streamingState();
    state.live.c2 = makeAssistantPlaceholder("m2", 4);
    state.routes.r2 = { messageId: "m2", conversationId: "c2" };
    state.streamingId = "m1";

    const patch = dropConversation(state, "c1");

    expect(Object.keys(patch.live ?? {})).toEqual(["c2"]);
    expect(Object.keys(patch.routes ?? {})).toEqual(["r2"]);
    // m1 没了，但 streamingId 指的是 m1 —— 必须清掉，
    // 否则发送按钮会永远停在「停止」上。
    expect(patch.streamingId).toBeNull();
  });

  it("删的不是正在流式的会话时，streamingId 保持不变", () => {
    const state = streamingState();
    state.live.c2 = makeAssistantPlaceholder("m2", 4);
    state.routes.r2 = { messageId: "m2", conversationId: "c2" };
    state.streamingId = "m2";

    const patch = dropConversation(state, "c1");
    expect(patch.streamingId).toBe("m2");
  });
});

describe("toUiMessages", () => {
  it("把数据库记录转成 UI 消息", () => {
    const stored: StoredMessage[] = [
      {
        id: "m1",
        conversationId: "c1",
        role: "assistant",
        content: "回答",
        reasoning: "思路",
        providerId: "deepseek",
        modelKey: "deepseek-chat",
        inputTokens: 5,
        outputTokens: 6,
        createdAt: 100,
        status: "ok",
        error: null,
        images: [],
      },
    ];

    expect(toUiMessages(stored)).toEqual([
      {
        id: "m1",
        role: "assistant",
        content: "回答",
        reasoning: "思路",
        streaming: false,
        error: null,
        cancelled: false,
        inputTokens: 5,
        outputTokens: 6,
        createdAt: 100,
        trimmed: 0,
        summarized: 0,
        tools: [],
        images: [],
      },
    ]);
  });

  it("把 status 映射成 cancelled / error，供气泡渲染", () => {
    const err = { code: "server", message: "boom", retryable: true };
    const stored: StoredMessage[] = [
      {
        id: "c1",
        conversationId: "c",
        role: "assistant",
        content: "",
        reasoning: null,
        providerId: null,
        modelKey: null,
        inputTokens: null,
        outputTokens: null,
        createdAt: 1,
        status: "cancelled",
        error: null,
        images: [],
      },
      {
        id: "e1",
        conversationId: "c",
        role: "assistant",
        content: "",
        reasoning: null,
        providerId: null,
        modelKey: null,
        inputTokens: null,
        outputTokens: null,
        createdAt: 2,
        status: "error",
        error: err,
        images: [],
      },
    ];

    const ui = toUiMessages(stored);
    expect(ui[0].cancelled).toBe(true);
    expect(ui[0].error).toBeNull();

    expect(ui[1].cancelled).toBe(false);
    expect(ui[1].error).toEqual(err);
  });

  it("reasoning 为 null 时补成空串，避免渲染出 null", () => {
    const stored: StoredMessage[] = [
      {
        id: "m1",
        conversationId: "c1",
        role: "user",
        content: "问",
        reasoning: null,
        providerId: null,
        modelKey: null,
        inputTokens: null,
        outputTokens: null,
        createdAt: 1,
        status: "ok",
        error: null,
        images: [],
      },
    ];
    expect(toUiMessages(stored)[0].reasoning).toBe("");
  });

  it("把图片一起转出来，且缺字段时退化成空数组", () => {
    const base = {
      conversationId: "c",
      reasoning: null,
      providerId: null,
      modelKey: null,
      inputTokens: null,
      outputTokens: null,
      createdAt: 1,
      status: "ok" as const,
      error: null,
    };

    const withImage = toUiMessages([
      {
        ...base,
        id: "u1",
        role: "user",
        content: "",
        images: ["data:image/png;base64,AAAA"],
      },
    ]);
    expect(withImage[0].images).toEqual(["data:image/png;base64,AAAA"]);

    // 后端字段缺失（旧数据 / 未来版本）时不该崩，退化成「没有图片」。
    const legacy = toUiMessages([
      {
        ...base,
        id: "u2",
        role: "user",
        content: "旧消息",
      } as StoredMessage,
    ]);
    expect(legacy[0].images).toEqual([]);
  });

  it("新落的用户消息能带上图片", () => {
    const message = makeUserMessage("u1", "看图", 1, [
      "data:image/jpeg;base64,BBBB",
    ]);
    expect(message.images).toEqual(["data:image/jpeg;base64,BBBB"]);

    // 不传图片时是空数组，而不是 undefined —— 下游 `m.images.length` 才安全。
    expect(makeUserMessage("u2", "无图", 1).images).toEqual([]);
    expect(makeAssistantPlaceholder("a1", 1).images).toEqual([]);
  });
});
