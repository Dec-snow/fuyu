// 一问多答（模型对比）的状态。
//
// 刻意**独立于**聊天 store：主动聊天的不变式是「一个会话同时只有一条在飞的
// 助手消息」（`ChatStreamState.live` 按会话索引单条），而对比要在同一时刻跑
// N 条。理由写在 `lib/compare.ts` 的文件头。
//
// 这个 store 自己管路由：每个目标一个 requestId，事件按 requestId 分发。

import { create } from "zustand";
import { describeError } from "../lib/errors";

import {
  canAdopt,
  checkRunnable,
  compareLabel,
  makeResults,
  normalizeTargets,
  reduceCompareEvent,
  streamingCount,
  type CompareResult,
  type CompareTarget,
} from "../lib/compare";
import { api } from "../services/api";
import { onChatEvent } from "../services/events";
import type { SequencedChatEvent } from "../types";
import { useConfig } from "./config";
import { useProviders } from "./providers";

function newRequestId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `cmp_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`;
}

interface CompareStore {
  open: boolean;
  question: string;
  targets: CompareTarget[];
  results: CompareResult[];
  /** 面板级的提示（例如「至少要选 2 个模型」）。 */
  notice: string | null;

  openPanel: () => void;
  closePanel: () => void;
  setQuestion: (question: string) => void;
  /** 改目标并持久化（下次打开面板还是这几个）。 */
  setTargets: (targets: CompareTarget[]) => Promise<void>;
  run: () => Promise<void>;
  stopAll: () => Promise<void>;
  adopt: (requestId: string, conversationId: string | null) => Promise<void>;
  handleEvent: (event: SequencedChatEvent) => void;
}

/** 从服务商配置里取「服务商名 · 模型」标签。 */
function labelsFor(targets: CompareTarget[]): Record<string, string> {
  const providers = useProviders.getState().state?.providers ?? [];
  const labels: Record<string, string> = {};
  for (const target of targets) {
    const provider = providers.find((p) => p.id === target.providerId);
    labels[`${target.providerId}\u0000${target.modelKey}`] = compareLabel(
      provider?.name,
      target.modelKey,
    );
  }
  return labels;
}

export const useCompare = create<CompareStore>((set, get) => ({
  open: false,
  question: "",
  targets: [],
  results: [],
  notice: null,

  openPanel: () => {
    // 每次打开都把配置里记着的那几个目标读进来：用户可能在设置里删过服务商，
    // 那份「上次用过的目标」需要跟着现在的配置重新规整一遍。
    const stored = useConfig.getState().config?.compareTargets ?? [];
    set({ open: true, targets: normalizeTargets(stored) });
  },

  closePanel: () => set({ open: false }),

  setQuestion: (question) => set({ question }),

  setTargets: async (targets) => {
    const normalized = normalizeTargets(targets);
    set({ targets: normalized });
    // 存下来是为了下次打开不用重选。失败不影响本次使用（只是没记住）。
    await api.updateSettings({ compareTargets: normalized }).catch(() => undefined);
  },

  run: async () => {
    const { question, targets, results } = get();
    const check = checkRunnable(targets, question);
    if (!check.ok) {
      set({ notice: check.reason });
      return;
    }
    // 上一轮还在跑就先别开新的：两轮的结果会混在同一个列表里
    if (streamingCount(results) > 0) {
      set({ notice: "上一轮还没跑完，先等它结束或点「全部停止」" });
      return;
    }

    const normalized = normalizeTargets(targets);
    const next = makeResults(normalized, labelsFor(normalized), () => newRequestId());
    set({ results: next, notice: null });

    // 并发发出去。`chat_send` 是**立刻返回**的（真正的流在后面 spawn），
    // 所以这里不需要等谁：内容靠事件回来。
    await Promise.all(
      next.map(async (result) => {
        try {
          await api.chatSend({
            providerId: result.providerId,
            modelKey: result.modelKey,
            messages: [{ role: "user", content: question.trim() }],
            // 不落库：后端在没有会话时本来就不写库（对比是草稿纸）
            conversationId: null,
            requestId: result.requestId,
            // 不带工具：N 路同时弹 N 个审批框，而面板没有审批界面
            enableTools: false,
          });
        } catch (e) {
          set((state) => ({
            results: state.results.map((r) =>
              r.requestId === result.requestId
                ? { ...r, streaming: false, error: String(e) }
                : r,
            ),
          }));
        }
      }),
    );
  },

  stopAll: async () => {
    const running = get().results.filter((r) => r.streaming);
    await Promise.all(
      running.map((r) => api.chatCancel(r.requestId).catch(() => false)),
    );
  },

  adopt: async (requestId, conversationId) => {
    const { results, question } = get();
    const result = results.find((r) => r.requestId === requestId);
    if (!result || !canAdopt(result)) return;
    if (conversationId === null) {
      set({ notice: "先新建或打开一个会话，再采用这条回答" });
      return;
    }
    try {
      await api.adoptAnswer({
        conversationId,
        question,
        answer: result.content,
        providerId: result.providerId,
        modelKey: result.modelKey,
      });
      set((state) => ({
        results: state.results.map((r) =>
          r.requestId === requestId ? { ...r, adopted: true } : r,
        ),
        notice: "已采用，写进当前会话了",
      }));
    } catch (e) {
      set({ notice: `采用失败：${describeError(e)}` });
    }
  },

  handleEvent: (event) => {
    const { results } = get();
    if (results.length === 0) return;
    const next = reduceCompareEvent(results, event);
    // 认不出来的事件（主动聊天那些）会原样返回同一个引用 —— 别白触发一次更新
    if (next !== results) set({ results: next });
  },
}));

/** 在 main.tsx 里挂一次：把所有流式事件喂给对比面板。 */
export function bindCompareEvents(): () => void {
  return onChatEvent((event) => useCompare.getState().handleEvent(event));
}
