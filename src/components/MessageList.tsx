import { memo, useEffect, useMemo, useRef, useState } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";

import { visibleMessages } from "../lib/chat-state";
import { toolLabel } from "../lib/tools";
import { useChat, type PendingApproval } from "../stores/chat";
import type { ToolTrace, UiMessage } from "../types";
import { Markdown } from "./Markdown";

function toolIcon(status: ToolTrace["status"]): string {
  switch (status) {
    case "pending":
      return "…";
    case "running":
      return "⚙";
    case "ok":
      return "✓";
    case "error":
      return "✗";
    case "rejected":
      return "⊘";
  }
}

function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

/**
 * 一条消息气泡。
 *
 * `memo` 依然有用：虚拟列表只解决「几百条消息只渲染视口内的那几条」，
 * 但**流式期间**视口内的已落库气泡引用不变、不该跟着每段增量重渲染。
 */
const MessageBubble = memo(function MessageBubble({
  message,
  onRetry,
  onEdit,
  onDelete,
  highlighted = false,
}: {
  message: UiMessage;
  onRetry: (messageId: string) => void;
  onEdit: (messageId: string, newContent: string) => void;
  onDelete: (messageId: string) => void;
  highlighted?: boolean;
}) {
  const [showReasoning, setShowReasoning] = useState(false);
  const isUser = message.role === "user";

  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(message.content);

  const startEdit = () => {
    if (!isUser) return;
    setDraft(message.content);
    setEditing(true);
  };

  const commit = () => {
    const trimmed = draft.trim();
    setEditing(false);
    if (trimmed && trimmed !== message.content) {
      onEdit(message.id, trimmed);
    }
  };

  return (
    <div className={`bubble-row ${isUser ? "from-user" : "from-ai"}`}>
      <div
        className={`bubble ${isUser ? "bubble-user" : "bubble-ai"} ${
          highlighted ? "highlighted" : ""
        }`}
      >
        {!isUser && message.reasoning ? (
          <div className="reasoning">
            <button
              className="reasoning-toggle"
              onClick={() => setShowReasoning((v) => !v)}
            >
              {showReasoning ? "▾" : "▸"} 思考过程
            </button>
            {showReasoning ? <pre>{message.reasoning}</pre> : null}
          </div>
        ) : null}

        {!isUser && message.tools.length > 0 ? (
          <div className="tool-timeline">
            {message.tools.map((tool) => (
              <div key={tool.callId} className="tool-step">
                <span className={`tool-icon tool-${tool.status}`}>
                  {toolIcon(tool.status)}
                </span>
                <span className="tool-name">{tool.name}</span>
                {tool.status === "ok" || tool.status === "error" ? (
                  <span className="tool-result">
                    {truncate(tool.result ?? "", 48)}
                  </span>
                ) : null}
              </div>
            ))}
          </div>
        ) : null}

        {isUser ? (
          editing ? (
            <div className="edit-box">
              <textarea
                autoFocus
                rows={Math.min(8, draft.split("\n").length + 1)}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    commit();
                  } else if (e.key === "Escape") {
                    // 必须拦下来：窗口级的 Esc 处理会把整个悬浮窗藏起来，
                    // 于是「取消编辑」和「关掉窗口」撞在同一个按键上 ——
                    // 用户只想退出编辑，结果连窗口带草稿一起没了。
                    e.stopPropagation();
                    setEditing(false);
                  }
                }}
              />
              <div className="edit-actions">
                <span className="edit-hint">Enter 重发 · Esc 取消</span>
                <button
                  className="edit-send"
                  disabled={draft.trim().length === 0}
                  onClick={commit}
                >
                  重发
                </button>
              </div>
            </div>
          ) : (
            <>
              {message.images.length > 0 ? (
                <div className="bubble-images">
                  {message.images.map((url, index) => (
                    <img
                      // 图片没有稳定 id，用「下标 + 尾部内容」当 key 即可：
                      // 同一个气泡里的图片不会重排。
                      key={`${index}-${url.slice(-16)}`}
                      className="bubble-image"
                      src={url}
                      alt={`图片 ${index + 1}`}
                    />
                  ))}
                </div>
              ) : null}
              {/* 只贴图不写字时，别渲染一个空的文字块。 */}
              {message.content ? (
                <div className="plain" title="双击编辑" onDoubleClick={startEdit}>
                  {message.content}
                </div>
              ) : null}
            </>
          )
        ) : (
          <Markdown text={message.content} />
        )}

        {message.streaming ? <span className="caret" /> : null}

        {message.error ? (
          <div className="bubble-error">
            <span className="bubble-error-text">{message.error.message}</span>
            <button className="retry-button" onClick={() => onRetry(message.id)}>
              重试
            </button>
          </div>
        ) : null}

        {message.cancelled ? <div className="bubble-note">已停止生成</div> : null}

        {!isUser && message.trimmed > 0 ? (
          <div className="bubble-note">
            已省略较早的 {message.trimmed} 条消息以适配上下文长度
          </div>
        ) : null}

        {!isUser && message.summarized > 0 ? (
          <div className="bubble-note">
            较早的 {message.summarized} 条已压缩为摘要（见顶部）
          </div>
        ) : null}

        {!isUser && (message.inputTokens || message.outputTokens) ? (
          <div className="bubble-meta">
            ↑{message.inputTokens ?? "–"} ↓{message.outputTokens ?? "–"} tokens
          </div>
        ) : null}
      </div>

      {/* 单条删除。之前界面上完全没有入口：一条说错的话只能靠「编辑并重发」
          或者「删掉整个会话」来处理，而 `delete_message` 命令其实早就有了。 */}
      {!message.streaming ? (
        <button
          className="bubble-delete"
          title="删除这条消息"
          onClick={() => {
            if (window.confirm("删除这条消息？")) onDelete(message.id);
          }}
        >
          🗑
        </button>
      ) : null}
    </div>
  );
});

function ApprovalCard({ approval }: { approval: PendingApproval }) {
  const approveTool = useChat((s) => s.approveTool);
  const act = (approved: boolean, remember: "once" | "session" | "always") =>
    void approveTool(approval.requestId, approval.callId, approved, remember);

  return (
    <div className="approval-card">
      <div className="approval-title">模型想调用工具「{toolLabel(approval.name)}」</div>
      <div className="approval-summary">{approval.summary}</div>
      <div className="approval-actions">
        <button className="approval-allow" onClick={() => act(true, "once")}>
          允许一次
        </button>
        {approval.canRemember ? (
          <>
            <button onClick={() => act(true, "session")}>本次会话允许</button>
            <button onClick={() => act(true, "always")}>永久允许</button>
          </>
        ) : null}
        <button className="approval-deny" onClick={() => act(false, "once")}>
          拒绝
        </button>
      </div>
    </div>
  );
}

export function MessageList() {
  const stored = useChat((s) => s.messages);
  const live = useChat((s) => s.live);
  const activeId = useChat((s) => s.activeId);
  const loading = useChat((s) => s.loadingMessages);
  const retry = useChat((s) => s.retry);
  const editAndResend = useChat((s) => s.editAndResend);
  const deleteMessage = useChat((s) => s.deleteMessage);
  const pendingApprovals = useChat((s) => s.pendingApprovals);
  const activeSummary = useChat((s) => s.activeSummary);
  const activeSummaryCount = useChat((s) => s.activeSummaryCount);
  const highlightId = useChat((s) => s.highlightMessageId);
  const setHighlight = useChat((s) => s.setHighlight);

  const [showSummary, setShowSummary] = useState(false);

  const messages = useMemo(
    () => visibleMessages({ messages: stored, live, activeId }),
    [stored, live, activeId],
  );

  const virtuosoRef = useRef<VirtuosoHandle>(null);
  const hasMessages = messages.length > 0;

  // 切会话 / 首屏载入完成后落到最后一条。
  //
  // 这里有两个坑，缺任何一个长会话都会「打开时停在最旧的一条」：
  // 1. 载入期间组件提前 return 了「载入中…」，Virtuoso 根本没挂载，ref 是 null；
  //    等数据回来时 `activeId` 没变、effect 不会重跑 —— 所以依赖里必须带上
  //    `loading` 和「有没有消息」；
  // 2. 光靠 `followOutput` 不够：react-virtuoso 默认只在**已经在底部**时才跟随，
  //    而长列表首次渲染的位置是顶部。
  useEffect(() => {
    if (loading || !hasMessages) return;
    const timer = window.setTimeout(() => {
      virtuosoRef.current?.scrollToIndex({ index: "LAST", behavior: "auto" });
    }, 0);
    return () => window.clearTimeout(timer);
  }, [activeId, loading, hasMessages]);

  // 从搜索结果跳进来：滚到命中那条并短暂描边。
  //
  // 用 ref 读消息列表而不是把它放进依赖：`highlightMessageId` 会在界面上
  // 停留两秒多，这期间如果依赖里带着 messages，每个流式 token 都会让这个
  // 效果重跑一次、反复把视口拽回那条消息。
  const messagesRef = useRef(messages);
  messagesRef.current = messages;

  useEffect(() => {
    if (!highlightId || loading) return;
    const index = messagesRef.current.findIndex((m) => m.id === highlightId);
    if (index < 0) return;

    // 注册在「滚到底部」之后，所以这里的 timeout 会后执行、把视口夺回来。
    const scrollTimer = window.setTimeout(() => {
      virtuosoRef.current?.scrollToIndex({
        index,
        align: "center",
        behavior: "auto",
      });
    }, 0);
    // 只闪一下：长期描边会让用户误以为它是「选中」状态。
    const clearTimer = window.setTimeout(() => setHighlight(null), 2400);
    return () => {
      window.clearTimeout(scrollTimer);
      window.clearTimeout(clearTimer);
    };
  }, [highlightId, loading, hasMessages, setHighlight]);

  if (loading && messages.length === 0) {
    return <div className="message-list empty">载入中…</div>;
  }

  if (messages.length === 0) {
    return (
      <div className="message-list empty">
        <div className="empty-hint">
          <div className="empty-title">按 Alt+Space 随时唤出</div>
          <div className="empty-sub">
            选中文字按 Alt+Q 也能直接取词提问
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="message-list">
      {activeSummary ? (
        <div className="summary-banner">
          <button
            className="summary-toggle"
            onClick={() => setShowSummary((v) => !v)}
          >
            {showSummary ? "▾" : "▸"} 历史摘要
            <span className="summary-count">
              较早的 {activeSummaryCount} 条已压缩
            </span>
          </button>
          {showSummary ? (
            <div className="summary-body">{activeSummary}</div>
          ) : null}
        </div>
      ) : null}

      <div className="message-scroll">
        <Virtuoso
          ref={virtuosoRef}
          // 换会话就整体重挂：`initialTopMostItemIndex` 只在挂载时生效，
          // 而「打开一个长会话就该停在最新一条」正是挂载时刻的事。
          key={activeId ?? "new"}
          data={messages}
          computeItemKey={(_, message) => message.id}
          initialTopMostItemIndex={Math.max(0, messages.length - 1)}
          // 只在用户本来就在底部时才跟随。`"smooth"` 会在每个 token 上
          // 重新起一段滚动动画，流式输出时看起来是「粘在中间」。
          followOutput={(isAtBottom) => (isAtBottom ? "auto" : false)}
          itemContent={(_, message) => (
            <MessageBubble
              message={message}
              onRetry={retry}
              onEdit={editAndResend}
              onDelete={deleteMessage}
              highlighted={message.id === highlightId}
            />
          )}
          style={{ height: "100%" }}
          increaseViewportBy={{ top: 600, bottom: 600 }}
        />
      </div>

      {pendingApprovals.length > 0 ? (
        <div className="approval-stack">
          {pendingApprovals.map((approval) => (
            <ApprovalCard key={approval.callId} approval={approval} />
          ))}
        </div>
      ) : null}
    </div>
  );
}
