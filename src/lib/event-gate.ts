// 流式事件的**去重闸门**。
//
// 从 `services/events.ts` 里抽出来是为了能单测：原来这段逻辑埋在
// `listen()` 的回调里，要测它就得先把 Tauri 的 IPC 整个 mock 掉。
//
// 为什么需要它：IPC 在高频推送下并不保证「恰好一次」—— 事件可能重复投递，
// 也可能乱序到达。后端给每个事件都打了一个单调递增的 `seq`，这里按
// `requestId` 分别记录已处理到的最大 `seq`。

import type { SequencedChatEvent } from "../types";

/** 终态事件：收到它之后这条请求就不会再产生新事件了。 */
export function isTerminalEvent(event: SequencedChatEvent): boolean {
  return (
    event.type === "finish" ||
    event.type === "error" ||
    event.type === "cancelled"
  );
}

type Schedule = (fn: () => void, ms: number) => void;

const defaultSchedule: Schedule = (fn, ms) => {
  setTimeout(fn, ms);
};

export class ChatEventGate {
  /** requestId → 已处理到的最大 seq。 */
  private readonly lastSeq = new Map<string, number>();

  /**
   * 已经收到过终态、但还在「善后期」里的请求。
   *
   * 为什么需要它：有些事件是**终态之后**才发的 —— 最典型的是 `summary_updated`
   * （滚动摘要是后台任务，最长 45 秒，终态事件早就发了）。如果只在终态那一刻
   * 排一次「过 5 秒忘掉」，那条迟到的摘要会把记录重新写回来，而它自己不是终态
   * 事件、不会再排删除 —— 这条记录就留下了。
   *
   * 注意这只覆盖「摘要来得够快（还在遗忘窗口内）」的情况。摘要常常在 45 秒后
   * 才回来，那时这里的标记已经随[`forget`](Self::forget)清掉了，所以**不能**
   * 只靠它 —— 真正的兜底是下面的 [`maxTracked`](Self::max_tracked) 上限。
   */
  private readonly terminalSeen = new Set<string>();

  constructor(
    /**
     * 终态之后隔多久才忘掉这条请求。
     *
     * 不能立刻忘：终态事件本身也可能被重复投递，若马上清掉记录，那个重复的
     * `finish` 就会被当成新事件再走一遍流程（前端会再拉一次数据库）。
     */
    private readonly forgetAfterMs: number = 5000,
    private readonly schedule: Schedule = defaultSchedule,
    /**
     * 同时跟踪的请求数上限，超出就淘汰最旧的。
     *
     * 这是「记录只增不减」的**结构性**兜底：只靠定时器的话，任何
     * 「终态之后又来事件」的组合都可能把记录重新写回来。上限保证内存有界，
     * 最坏情况也只是很旧的 requestId 被提前忘掉（它的重复事件可能被多处理一次，
     * 而那时请求早已结束，多处理一次无害）。
     */
    private readonly maxTracked: number = 256,
  ) {}

  /**
   * 返回 `true` 表示这个事件是新的、应当处理。
   *
   * 重复投递（`seq` 小于等于已见最大值）返回 `false`。
   */
  accept(event: SequencedChatEvent): boolean {
    const seen = this.lastSeq.get(event.requestId);
    if (seen !== undefined && event.seq <= seen) {
      return false;
    }
    this.lastSeq.set(event.requestId, event.seq);

    const terminal = isTerminalEvent(event);
    const wasTerminal = this.terminalSeen.has(event.requestId);
    if (terminal) {
      this.terminalSeen.add(event.requestId);
    }

    // 终态本身、以及任何**终态之后**才到的事件，都要（重新）排一次清理。
    if (terminal || wasTerminal) {
      this.schedule(() => this.forget(event.requestId), this.forgetAfterMs);
    }

    this.evictOldest();
    return true;
  }

  /** 超出上限时淘汰最旧的记录（`Map` 保持插入顺序）。 */
  private evictOldest(): void {
    while (this.lastSeq.size > this.maxTracked) {
      const oldest = this.lastSeq.keys().next().value;
      if (oldest === undefined) return;
      this.forget(oldest);
    }
  }

  /** 主动忘掉一条请求（例如用户清空了会话）。 */
  forget(requestId: string): void {
    this.lastSeq.delete(requestId);
    this.terminalSeen.delete(requestId);
  }

  /** 正在跟踪的请求数，仅用于测试与诊断。 */
  get tracked(): number {
    return this.lastSeq.size;
  }
}
