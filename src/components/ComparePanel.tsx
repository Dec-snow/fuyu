// 一问多答：同一个问题并发问多个模型，并排看回答。
//
// ## 为什么不是「在聊天里一次冒出四条回答」
//
// 那需要把聊天状态里「一个会话只有一条在飞的消息」这条不变式拆掉 —— 而它正是
// 「回答到一半切走会话、回来还在继续」能成立的原因。为一个不需要它的功能去改
// 那条不变式，风险与收益不成比例。
//
// 所以对比是**独立的草稿纸**：并发跑、并排看、不写进聊天历史；看中了再点
// 「采用这条」写进当前会话。详细理由见 `lib/compare.ts` 的文件头。

import { useEffect, useMemo, useRef } from "react";

import { describeResult, MAX_COMPARE_TARGETS } from "../lib/compare";
import { api } from "../services/api";
import { useChat } from "../stores/chat";
import { useCompare } from "../stores/compare";
import { useProviders } from "../stores/providers";
import { Markdown } from "./Markdown";

export function ComparePanel() {
  const open = useCompare((s) => s.open);
  const closePanel = useCompare((s) => s.closePanel);

  if (!open) return null;
  return (
    <div className="compare-backdrop" onClick={closePanel}>
      <div
        className="compare-panel"
        // 点面板内部不该关掉它
        onClick={(e) => e.stopPropagation()}
      >
        <CompareBody />
      </div>
    </div>
  );
}

function CompareBody() {
  const {
    question,
    targets,
    results,
    notice,
    setQuestion,
    setTargets,
    run,
    stopAll,
    adopt,
    closePanel,
  } = useCompare();
  const providers = useProviders((s) => s.state?.providers ?? []);
  const conversationId = useChat((s) => s.activeId);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // 可选模型：所有服务商的所有模型，平铺成一个列表。
  const options = useMemo(
    () =>
      providers.flatMap((provider) =>
        provider.models.map((model) => ({
          providerId: provider.id,
          modelKey: model.key,
          label: `${provider.name} · ${model.key}`,
        })),
      ),
    [providers],
  );

  const running = results.some((r) => r.streaming);
  const selected = new Set(
    targets.map((t) => `${t.providerId}\u0000${t.modelKey}`),
  );

  const toggle = (providerId: string, modelKey: string) => {
    const key = `${providerId}\u0000${modelKey}`;
    const next = selected.has(key)
      ? targets.filter((t) => `${t.providerId}\u0000${t.modelKey}` !== key)
      : [...targets, { providerId, modelKey }];
    void setTargets(next);
  };

  return (
    <>
      <div className="compare-head">
        <span className="compare-title">一问多答</span>
        <span className="compare-sub">
          同一个问题并发问多个模型，回答并排显示。对比不会写进聊天记录，
          看中了再点「采用这条」。
        </span>
        <button className="ghost-button" onClick={closePanel}>
          关闭
        </button>
      </div>

      <div className="compare-form">
        <textarea
          ref={inputRef}
          className="compare-question"
          rows={2}
          placeholder="写下要对比的问题…（Ctrl+Enter 开始）"
          value={question}
          onChange={(e) => setQuestion(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) void run();
          }}
        />
        <div className="compare-actions">
          <button
            className="primary-button"
            disabled={running}
            onClick={() => void run()}
          >
            {running ? "对比中…" : "开始对比"}
          </button>
          {running ? (
            <button className="ghost-button" onClick={() => void stopAll()}>
              全部停止
            </button>
          ) : null}
          {notice !== null ? <span className="compare-notice">{notice}</span> : null}
        </div>

        <ModelPicker
          options={options}
          selected={selected}
          onToggle={toggle}
          note={
            targets.length >= MAX_COMPARE_TARGETS
              ? `最多 ${MAX_COMPARE_TARGETS} 个`
              : `已选 ${targets.length} 个`
          }
        />
      </div>

      {results.length === 0 ? null : (
        <>
          <div className="compare-grid">
            {results.map((result) => (
              <div className="compare-col" key={result.requestId}>
                <div className="compare-col-head">
                  <span className="compare-col-label" title={result.label}>
                    {result.label}
                  </span>
                  <span className="compare-col-state">
                    {describeResult(result)}
                  </span>
                </div>
                <div className="compare-col-body">
                  {result.error !== null ? (
                    <div className="compare-error">{result.error}</div>
                  ) : result.content.length === 0 ? (
                    <div className="compare-empty">
                      {result.streaming ? "等待第一个字…" : "（没有内容）"}
                    </div>
                  ) : (
                    <Markdown text={result.content} />
                  )}
                </div>
                <div className="compare-col-foot">
                  <button
                    className="ghost-button"
                    disabled={result.content.trim().length === 0}
                    onClick={() => adopt(result.requestId, conversationId)}
                    title={
                      conversationId === null
                        ? "先新建或打开一个会话"
                        : "把这一问一答写进当前会话"
                    }
                  >
                    {result.adopted ? "已采用" : "采用这条"}
                  </button>
                  {result.streaming ? (
                    <button
                      className="ghost-button"
                      onClick={() =>
                        void api.chatCancel(result.requestId).catch(() => false)
                      }
                    >
                      停止
                    </button>
                  ) : null}
                </div>
              </div>
            ))}
          </div>
        </>
      )}
    </>
  );
}

function ModelPicker({
  options,
  selected,
  onToggle,
  note,
}: {
  options: { providerId: string; modelKey: string; label: string }[];
  selected: Set<string>;
  onToggle: (providerId: string, modelKey: string) => void;
  note: string;
}) {
  if (options.length === 0) {
    return (
      <div className="settings-hint">
        还没有可用的模型。先在「设置 → 模型」里加上服务商与模型。
      </div>
    );
  }
  return (
    <div className="compare-picker">
      <span className="compare-picker-note">{note}</span>
      <div className="compare-picker-list">
        {options.map((option) => {
          const key = `${option.providerId}\u0000${option.modelKey}`;
          return (
            <label className="compare-chip" key={key}>
              <input
                type="checkbox"
                checked={selected.has(key)}
                onChange={() => onToggle(option.providerId, option.modelKey)}
              />
              <span>{option.label}</span>
            </label>
          );
        })}
      </div>
    </div>
  );
}
