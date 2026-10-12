// 一问多答的纯逻辑测试。
//
// 守三类问题：
// - 目标没被去重 / 没夹上限 → 白花钱跑两遍同一个模型；
// - 事件路由认错 requestId → 两个模型的回答串在一起（这是最难看的一种 bug）；
// - 空回答 / 失败的能「采用」 → 会话里多出一个空气泡。

import { describe, expect, it } from "vitest";

import type { SequencedChatEvent } from "../types";
import {
  MAX_COMPARE_TARGETS,
  canAdopt,
  checkRunnable,
  compareLabel,
  describeResult,
  makeResults,
  normalizeTargets,
  reduceCompareEvent,
  streamingCount,
  totalTokens,
  type CompareResult,
  type CompareTarget,
} from "./compare";

const target = (providerId: string, modelKey: string): CompareTarget => ({
  providerId,
  modelKey,
});

/** 造一条事件（`seq` 只是事件闸门要的序号，这里随手给）。 */
function event(over: Partial<SequencedChatEvent> & { type: string }): SequencedChatEvent {
  return { seq: 1, requestId: "r1", ...over } as SequencedChatEvent;
}

function results(): CompareResult[] {
  return makeResults(
    [target("deepseek", "chat"), target("openai", "gpt")],
    { "deepseek\u0000chat": "DeepSeek · chat", "openai\u0000gpt": "OpenAI · gpt" },
    (_t, index) => `req-${index}`,
  );
}

describe("compareLabel", () => {
  it("拼出「服务商 · 模型」", () => {
    expect(compareLabel("DeepSeek", "deepseek-chat")).toBe("DeepSeek · deepseek-chat");
  });

  it("没有服务商名时只用模型名，不留一个孤零零的点", () => {
    expect(compareLabel(undefined, "deepseek-chat")).toBe("deepseek-chat");
    expect(compareLabel("  ", "deepseek-chat")).toBe("deepseek-chat");
  });
});

describe("normalizeTargets", () => {
  it("去重（同一个模型对比两次毫无意义，而消耗是实打实的）", () => {
    const out = normalizeTargets([
      target("a", "m"),
      target("a", "m"),
      target("a", "m2"),
    ]);
    expect(out).toHaveLength(2);
  });

  it("夹到上限", () => {
    const many = Array.from({ length: 10 }, (_, i) => target("p", `m${i}`));
    expect(normalizeTargets(many)).toHaveLength(MAX_COMPARE_TARGETS);
  });

  it("丢掉空的服务商 / 模型（面板里可能是刚清空的下拉框）", () => {
    expect(normalizeTargets([target("", "m"), target("p", "  ")])).toHaveLength(0);
  });
});

describe("checkRunnable", () => {
  it("问题为空时给一句人话", () => {
    const check = checkRunnable([target("a", "m"), target("b", "m")], "   ");
    expect(check.ok).toBe(false);
    if (!check.ok) expect(check.reason).toContain("问题");
  });

  it("少于两个模型不算对比", () => {
    const check = checkRunnable([target("a", "m")], "问题");
    expect(check.ok).toBe(false);
    if (!check.ok) expect(check.reason).toContain("至少");
  });

  it("去重后不够两个也不能开始", () => {
    // 用户勾了三个但其实是同一个模型的两种写法
    const check = checkRunnable(
      [target("a", "m"), target("a", "m"), target("a", "m")],
      "问题",
    );
    expect(check.ok).toBe(false);
  });

  it("两个不同模型 + 问题就能开始", () => {
    expect(checkRunnable([target("a", "m"), target("b", "m")], "问题").ok).toBe(true);
  });
});

describe("makeResults", () => {
  it("每个目标一路，初始都是流式中", () => {
    const list = results();
    expect(list).toHaveLength(2);
    expect(list.map((r) => r.requestId)).toEqual(["req-0", "req-1"]);
    expect(list.every((r) => r.streaming)).toBe(true);
    expect(list[0].label).toBe("DeepSeek · chat");
  });
});

describe("reduceCompareEvent", () => {
  it("认不出的 requestId 原样返回（主动聊天的事件也会流到这里）", () => {
    const list = results();
    const next = reduceCompareEvent(list, event({ type: "delta", requestId: "别人的", text: "x" }));
    expect(next).toBe(list);
  });

  it("增量拼到**自己**那一路，不串台", () => {
    let list = results();
    list = reduceCompareEvent(list, event({ type: "delta", requestId: "req-1", text: "B" }));
    list = reduceCompareEvent(list, event({ type: "delta", requestId: "req-0", text: "A" }));
    list = reduceCompareEvent(list, event({ type: "delta", requestId: "req-1", text: "2" }));

    expect(list[0].content).toBe("A");
    expect(list[1].content).toBe("B2");
  });

  it("思维链与用量分开记", () => {
    let list = results();
    list = reduceCompareEvent(
      list,
      event({ type: "reasoning_delta", requestId: "req-0", text: "想想" }),
    );
    list = reduceCompareEvent(
      list,
      event({ type: "usage", requestId: "req-0", inputTokens: 10, outputTokens: 20 }),
    );
    expect(list[0].reasoning).toBe("想想");
    expect(list[0].content).toBe("");
    expect(list[0].inputTokens).toBe(10);
    expect(totalTokens(list)).toBe(30);
  });

  it("终态结束流式；取消与错误各自标记", () => {
    let list = results();
    list = reduceCompareEvent(list, event({ type: "finish", requestId: "req-0" }));
    expect(list[0].streaming).toBe(false);
    expect(list[0].error).toBeNull();

    list = reduceCompareEvent(list, event({ type: "cancelled", requestId: "req-1" }));
    expect(list[1].cancelled).toBe(true);
    expect(list[1].streaming).toBe(false);

    list = reduceCompareEvent(
      list,
      event({ type: "error", requestId: "req-1", code: "TIMEOUT", message: "超时", retryable: true }),
    );
    expect(list[1].error).toContain("超时");
    expect(list[1].error).toContain("TIMEOUT");
  });

  it("不关心的事件类型（工具调用等）不改状态", () => {
    const list = results();
    const next = reduceCompareEvent(
      list,
      event({ type: "tool_start", requestId: "req-0", callId: "c", name: "calculator" }),
    );
    // 内容与状态都不该因为工具事件而改变
    expect(next[0].content).toBe("");
    expect(next[0].streaming).toBe(true);
  });
});

describe("streamingCount / canAdopt / describeResult", () => {
  it("数出还在跑的路数", () => {
    let list = results();
    expect(streamingCount(list)).toBe(2);
    list = reduceCompareEvent(list, event({ type: "finish", requestId: "req-0" }));
    expect(streamingCount(list)).toBe(1);
  });

  it("空回答 / 报错 / 被取消的都不能采用", () => {
    const base = results()[0];
    expect(canAdopt({ ...base, streaming: false, content: "答案" })).toBe(true);
    expect(canAdopt({ ...base, streaming: false, content: "   " })).toBe(false);
    expect(canAdopt({ ...base, streaming: true, content: "答案" })).toBe(false);
    expect(canAdopt({ ...base, streaming: false, content: "答案", error: "炸了" })).toBe(false);
    expect(canAdopt({ ...base, streaming: false, content: "答案", cancelled: true })).toBe(false);
  });

  it("状态小结覆盖各种终态", () => {
    const base = results()[0];
    expect(describeResult({ ...base, streaming: true })).toBe("生成中…");
    expect(describeResult({ ...base, streaming: false, cancelled: true })).toBe("已停止");
    expect(describeResult({ ...base, streaming: false, error: "x" })).toBe("失败");
    expect(describeResult({ ...base, streaming: false, adopted: true })).toBe("已采用");
    expect(describeResult({ ...base, streaming: false })).toBe("完成");
    expect(
      describeResult({ ...base, streaming: false, inputTokens: 3, outputTokens: 4 }),
    ).toBe("7 tokens");
  });
});
