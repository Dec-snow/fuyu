// 一问多答（模型对比）的纯逻辑。
//
// ## 为什么这一块**不**复用主动聊天那套状态
//
// 主动聊天的关键不变式是「一个会话同时只有一条在飞的助手消息」——
// `ChatStreamState.live` 就是按**会话**索引的单条。这条不变式换来的是
// 「回答还没结束时切到别的会话，那条回答不会丢，切回来还在继续长」。
//
// 一问多答要在**同一时刻**跑 N 条并发的回答。把 N 条塞进那个按会话索引的
// `live` 里，等于把上面那条不变式拆掉，而它正是主动聊天里最难、也最值得
// 保住的部分。所以这里另起一套：**对比面板自带 requestId 路由**，
// 并且请求带 `conversationId: null` —— 后端在没有会话时本来就不落库
// （落库那几处都是 `if let Some(conversation_id)`）。
//
// 于是对比是「草稿纸」：并发跑、并排看、不污染聊天历史；看中了再「采用」
// 写进当前会话。这不是退而求其次，而是这个交互本来的样子。

import type { SequencedChatEvent } from "../types";

/** 一次对比最多几个模型。 */
export const MAX_COMPARE_TARGETS = 4;
/** 一次对比至少几个模型（一个就不叫对比了）。 */
export const MIN_COMPARE_TARGETS = 2;

/** 一个对比目标（服务商 + 模型）。 */
export interface CompareTarget {
  providerId: string;
  modelKey: string;
}

/** 一路回答的界面状态。 */
export interface CompareResult extends CompareTarget {
  requestId: string;
  /** 显示用的名字，例如「DeepSeek · deepseek-chat」。 */
  label: string;
  content: string;
  reasoning: string;
  streaming: boolean;
  cancelled: boolean;
  error: string | null;
  inputTokens: number;
  outputTokens: number;
  /** 已经「采用」进会话了（按钮据此变成「已采用」）。 */
  adopted: boolean;
}

/** 目标的显示名。 */
export function compareLabel(
  providerName: string | undefined,
  modelKey: string,
): string {
  const provider = (providerName ?? "").trim();
  return provider ? `${provider} · ${modelKey}` : modelKey;
}

/**
 * 目标去重并夹到上限内。
 *
 * 去重按「服务商 + 模型」：同一个模型对比两次毫无意义（两路回答会一模一样），
 * 而消耗是实打实的。
 */
export function normalizeTargets(targets: CompareTarget[]): CompareTarget[] {
  const seen = new Set<string>();
  const out: CompareTarget[] = [];
  for (const target of targets) {
    const providerId = target.providerId.trim();
    const modelKey = target.modelKey.trim();
    if (!providerId || !modelKey) continue;
    const key = `${providerId}\u0000${modelKey}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ providerId, modelKey });
    if (out.length >= MAX_COMPARE_TARGETS) break;
  }
  return out;
}

/** 这次对比能不能开始；不能的话给出原因（直接显示给用户）。 */
export function checkRunnable(
  targets: CompareTarget[],
  question: string,
): { ok: true } | { ok: false; reason: string } {
  const normalized = normalizeTargets(targets);
  if (question.trim().length === 0) {
    return { ok: false, reason: "先写下要问的问题" };
  }
  if (normalized.length < MIN_COMPARE_TARGETS) {
    return { ok: false, reason: `至少要选 ${MIN_COMPARE_TARGETS} 个模型才能对比` };
  }
  return { ok: true };
}

/** 按目标建出初始结果（进入「流式中」状态）。 */
export function makeResults(
  targets: CompareTarget[],
  labels: Record<string, string>,
  requestIdFor: (target: CompareTarget, index: number) => string,
): CompareResult[] {
  return normalizeTargets(targets).map((target, index) => ({
    ...target,
    requestId: requestIdFor(target, index),
    label: labels[`${target.providerId}\u0000${target.modelKey}`] ?? target.modelKey,
    content: "",
    reasoning: "",
    streaming: true,
    cancelled: false,
    error: null,
    inputTokens: 0,
    outputTokens: 0,
    adopted: false,
  }));
}

/**
 * 把一条流式事件应用到结果列表上。**纯函数**，认不出 requestId 就原样返回。
 *
 * 认不出是常态而非异常：主动聊天的事件也会流到同一个订阅上（只有一个
 * Tauri 监听器），对比面板只关心自己那几个 requestId。
 */
export function reduceCompareEvent(
  results: CompareResult[],
  event: SequencedChatEvent,
): CompareResult[] {
  const index = results.findIndex((r) => r.requestId === event.requestId);
  if (index < 0) return results;

  const patch = (next: Partial<CompareResult>): CompareResult[] => {
    const copy = results.slice();
    copy[index] = { ...copy[index], ...next };
    return copy;
  };

  switch (event.type) {
    case "start":
      return patch({ streaming: true });
    case "delta":
      return patch({ content: results[index].content + event.text });
    case "reasoning_delta":
      return patch({ reasoning: results[index].reasoning + event.text });
    case "usage":
      // 用量字段是可空的（有的服务商只报总量），空就当 0 ——
      // 界面上显示「null tokens」比显示 0 更莫名其妙
      return patch({
        inputTokens: event.inputTokens ?? 0,
        outputTokens: event.outputTokens ?? 0,
      });
    case "finish":
      return patch({ streaming: false });
    case "cancelled":
      return patch({ streaming: false, cancelled: true });
    case "error":
      return patch({
        streaming: false,
        error: `[${event.code}] ${event.message}`,
      });
    default:
      // 工具、上下文裁剪这些在对比里不关心（对比刻意不带工具，见 store）
      return results;
  }
}

/** 还有几路在跑。面板据此显示「停止全部」与进度。 */
export function streamingCount(results: CompareResult[]): number {
  return results.filter((r) => r.streaming).length;
}

/**
 * 这一路能不能「采用」。
 *
 * 空回答、报错、被取消的都不该能采用 —— 否则会把一条空消息写进会话，
 * 用户看到的是一个莫名其妙的气泡。
 */
export function canAdopt(result: CompareResult): boolean {
  return (
    !result.streaming &&
    !result.cancelled &&
    result.error === null &&
    result.content.trim().length > 0
  );
}

/** 结果卡片的状态小结（右上角那行小字）。 */
export function describeResult(result: CompareResult): string {
  if (result.error !== null) return "失败";
  if (result.cancelled) return "已停止";
  if (result.streaming) return "生成中…";
  if (result.adopted) return "已采用";
  const tokens = result.inputTokens + result.outputTokens;
  return tokens > 0 ? `${tokens} tokens` : "完成";
}

/** 把 token 用量合计出来（面板底部的总账）。 */
export function totalTokens(results: CompareResult[]): number {
  return results.reduce((sum, r) => sum + r.inputTokens + r.outputTokens, 0);
}
