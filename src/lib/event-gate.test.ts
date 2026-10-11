import { describe, expect, it } from "vitest";

import { ChatEventGate, isTerminalEvent } from "./event-gate";
import type { SequencedChatEvent } from "../types";

function delta(requestId: string, seq: number): SequencedChatEvent {
  return { type: "delta", requestId, seq, text: "x" };
}

function finish(requestId: string, seq: number): SequencedChatEvent {
  return { type: "finish", requestId, seq, reason: "stop" };
}

function errorEvent(requestId: string, seq: number): SequencedChatEvent {
  return {
    type: "error",
    requestId,
    seq,
    code: "server",
    message: "boom",
    retryable: true,
  };
}

/** 终态之后才发出的后台事件（真实场景：滚动摘要）。 */
function summary(requestId: string, seq: number): SequencedChatEvent {
  return {
    type: "summary_updated",
    requestId,
    seq,
    conversationId: "c1",
    summary: "摘要",
    coveredMessages: 12,
  };
}

/** 收集被安排的回调，测试里手动触发，不依赖真实计时器。 */
function collectingScheduler(): {
  schedule: (fn: () => void, ms: number) => void;
  runAll: () => void;
  scheduled: Array<{ ms: number }>;
} {
  const callbacks: Array<() => void> = [];
  const scheduled: Array<{ ms: number }> = [];
  return {
    scheduled,
    schedule: (fn, ms) => {
      callbacks.push(fn);
      scheduled.push({ ms });
    },
    runAll: () => {
      while (callbacks.length > 0) callbacks.shift()?.();
    },
  };
}

describe("isTerminalEvent", () => {
  it("只把 finish / error / cancelled 视为终态", () => {
    expect(isTerminalEvent(finish("r", 1))).toBe(true);
    expect(isTerminalEvent(errorEvent("r", 1))).toBe(true);
    expect(
      isTerminalEvent({ type: "cancelled", requestId: "r", seq: 1 }),
    ).toBe(true);

    expect(isTerminalEvent(delta("r", 1))).toBe(false);
    expect(isTerminalEvent({ type: "start", requestId: "r", seq: 0 })).toBe(false);
    expect(
      isTerminalEvent({
        type: "usage",
        requestId: "r",
        seq: 1,
        inputTokens: 1,
        outputTokens: 2,
      }),
    ).toBe(false);
  });
});

describe("ChatEventGate", () => {
  it("按 seq 递增正常放行", () => {
    const gate = new ChatEventGate(1000, () => {});
    expect(gate.accept(delta("r1", 0))).toBe(true);
    expect(gate.accept(delta("r1", 1))).toBe(true);
    expect(gate.accept(delta("r1", 2))).toBe(true);
  });

  it("重复的 seq 被拒绝（IPC 可能重复投递同一个事件）", () => {
    const gate = new ChatEventGate(1000, () => {});
    expect(gate.accept(delta("r1", 5))).toBe(true);
    expect(gate.accept(delta("r1", 5))).toBe(false);
    expect(gate.accept(delta("r1", 5))).toBe(false);
  });

  it("乱序到达的旧事件被拒绝", () => {
    const gate = new ChatEventGate(1000, () => {});
    expect(gate.accept(delta("r1", 10))).toBe(true);
    expect(gate.accept(delta("r1", 9))).toBe(false);
    expect(gate.accept(delta("r1", 1))).toBe(false);
    // 更新的仍然放行。
    expect(gate.accept(delta("r1", 11))).toBe(true);
  });

  it("不同 requestId 各自独立计数", () => {
    const gate = new ChatEventGate(1000, () => {});
    expect(gate.accept(delta("r1", 7))).toBe(true);
    // r2 的 seq 从 0 开始，不能被 r1 的 7 影响。
    expect(gate.accept(delta("r2", 0))).toBe(true);
    expect(gate.accept(delta("r2", 1))).toBe(true);
    expect(gate.accept(delta("r1", 8))).toBe(true);
    expect(gate.tracked).toBe(2);
  });

  it("终态事件会安排一次延迟遗忘", () => {
    const scheduler = collectingScheduler();
    const gate = new ChatEventGate(5000, scheduler.schedule);

    gate.accept(finish("r1", 3));
    expect(gate.tracked).toBe(1);
    expect(scheduler.scheduled).toEqual([{ ms: 5000 }]);

    scheduler.runAll();
    expect(gate.tracked).toBe(0);
  });

  it("中间事件不会触发遗忘", () => {
    const scheduler = collectingScheduler();
    const gate = new ChatEventGate(5000, scheduler.schedule);

    gate.accept(delta("r1", 0));
    gate.accept(delta("r1", 1));
    gate.accept({
      type: "usage",
      requestId: "r1",
      seq: 2,
      inputTokens: 1,
      outputTokens: 1,
    });

    expect(scheduler.scheduled).toHaveLength(0);
    expect(gate.tracked).toBe(1);
  });

  it("终态之后迟到的重复事件仍然被挡住", () => {
    // 这正是「不能立刻遗忘」的原因：finish 自己也可能被重复投递，
    // 若马上清掉记录，那个重复的 finish 会被当成新事件再走一遍流程。
    const scheduler = collectingScheduler();
    const gate = new ChatEventGate(5000, scheduler.schedule);

    expect(gate.accept(finish("r1", 3))).toBe(true);
    expect(gate.accept(finish("r1", 3))).toBe(false);
    expect(gate.accept(delta("r1", 2))).toBe(false);

    // 遗忘之后，同一个 requestId 重新开始计数（换请求会换 id，所以没关系）。
    scheduler.runAll();
    expect(gate.accept(delta("r1", 0))).toBe(true);
  });

  it("遗忘窗口内迟到的摘要会重新安排遗忘", () => {
    // 真实现场：`summary_updated` 是后台压缩任务发的，可能晚于终态事件。
    // 旧实现只在终态那一刻排一次删除：这条迟到的摘要把记录写回来，
    // 它自己又不是终态、不会再排删除，记录就留下了。
    const scheduler = collectingScheduler();
    const gate = new ChatEventGate(5000, scheduler.schedule);

    gate.accept(finish("r1", 3));
    expect(scheduler.scheduled).toHaveLength(1);

    // 还没到遗忘时间，摘要就到了：应当重新排一次。
    expect(gate.accept(summary("r1", 4))).toBe(true);
    expect(scheduler.scheduled).toHaveLength(2);
    expect(gate.tracked).toBe(1);

    scheduler.runAll();
    expect(gate.tracked).toBe(0);
  });

  it("遗忘窗口之外迟到的摘要不会让记录无限增长", () => {
    // 摘要最长要 45 秒才回来，那时遗忘窗口（5 秒）早就过了。
    // 这种组合下没法靠定时器兜住，所以有一条**结构性**上限兜底。
    const scheduler = collectingScheduler();
    const gate = new ChatEventGate(5000, scheduler.schedule, 64);

    for (let i = 0; i < 500; i += 1) {
      const requestId = `r${i}`;
      gate.accept(finish(requestId, 1));
      scheduler.runAll(); // 遗忘窗口过去
      gate.accept(summary(requestId, 2));
    }

    expect(gate.tracked).toBeLessThanOrEqual(64);
  });

  it("迟到的摘要也不会把终态之前的重复事件放进来", () => {
    const scheduler = collectingScheduler();
    const gate = new ChatEventGate(5000, scheduler.schedule);

    gate.accept(finish("r1", 3));
    scheduler.runAll();

    gate.accept(summary("r1", 4));
    // 比已见 seq 旧的事件依旧被挡。
    expect(gate.accept(delta("r1", 2))).toBe(false);
    expect(gate.accept(finish("r1", 3))).toBe(false);
  });

  it("forget 可以立刻清掉一条请求", () => {
    const gate = new ChatEventGate(1000, () => {});
    gate.accept(delta("r1", 4));
    gate.forget("r1");
    expect(gate.tracked).toBe(0);
    expect(gate.accept(delta("r1", 0))).toBe(true);
  });
});
