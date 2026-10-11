import { useEffect, useRef, useState } from "react";

import { api } from "../services/api";
import { CONVERSATION_LIMIT_MAX, useChat } from "../stores/chat";
import type { MessageHit } from "../types";

/** 命中项的日期：只给「月-日 时:分」，列表里不需要年份和秒。 */
function formatHitTime(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "";
  const date = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export function Sidebar() {
  const conversations = useChat((s) => s.conversations);
  const activeId = useChat((s) => s.activeId);
  const select = useChat((s) => s.selectConversation);
  const remove = useChat((s) => s.deleteConversation);
  const togglePinned = useChat((s) => s.togglePinned);
  const rename = useChat((s) => s.renameConversation);
  const newConversation = useChat((s) => s.newConversation);
  const conversationLimit = useChat((s) => s.conversationLimit);
  const loadMore = useChat((s) => s.loadMoreConversations);

  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<MessageHit[]>([]);
  const [searching, setSearching] = useState(false);
  const setHighlight = useChat((s) => s.setHighlight);
  /** 搜索请求序号：用来丢弃过期响应。 */
  const searchSeq = useRef(0);

  // 搜索防抖：每敲一个字就打一次 SQLite FTS 是浪费，尤其是中文输入法
  // 会连续触发 composition 事件。
  //
  // 这里还要防**过期响应**：先搜 "abc" 再搜 "abcd"，先发的那次可能后回来，
  // 把旧结果盖在新结果上（还会提前把「搜索中」关掉）。用一个自增的请求序号
  // 把不属于当前查询的结果丢掉。
  useEffect(() => {
    const q = query.trim();
    if (q.length === 0) {
      setHits([]);
      // 清空输入框时也要复位，否则「搜索中…」会一直挂着
      //（旧代码在这里直接 return，状态就永远停在 true）。
      setSearching(false);
      return;
    }

    setSearching(true);
    const seq = ++searchSeq.current;
    const timer = window.setTimeout(async () => {
      try {
        const found = await api.searchMessages(q, 30);
        if (seq !== searchSeq.current) return; // 已经有更新的查询了
        setHits(found);
      } catch {
        if (seq === searchSeq.current) setHits([]);
      } finally {
        if (seq === searchSeq.current) setSearching(false);
      }
    }, 220);
    return () => window.clearTimeout(timer);
  }, [query]);

  const onRename = async (id: string, current: string) => {
    const next = window.prompt("重命名会话", current);
    if (next && next.trim() && next !== current) {
      await rename(id, next.trim());
    }
  };

  return (
    <aside className="sidebar">
      <div className="sidebar-search">
        <input
          value={query}
          placeholder="搜索消息正文…"
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>

      {query.trim() ? (
        <div className="sidebar-list">
          {searching ? <div className="sidebar-hint">搜索中…</div> : null}
          {/* 搜索中不显示上一轮的结果：否则用户会以为「abc」的结果就是
              「abcd」的结果。 */}
          {!searching && hits.length === 0 ? (
            <div className="sidebar-hint">没有匹配的消息</div>
          ) : null}
          {!searching &&
            hits.map((hit) => (
              <button
                key={hit.messageId}
                className="hit-item"
                onClick={() => {
                  // 先加载目标会话，再让它滚到并高亮命中的那一条 ——
                  // 只切会话的话用户落在最底部，根本不知道命中的是哪句。
                  void (async () => {
                    await select(hit.conversationId);
                    setHighlight(hit.messageId);
                    setQuery("");
                  })();
                }}
              >
                <div className="hit-title">{hit.conversationTitle}</div>
                <div className="hit-snippet">{hit.snippet}</div>
                <div className="hit-meta">
                  {hit.role === "user" ? "我" : "助手"} ·{" "}
                  {formatHitTime(hit.createdAt)}
                </div>
              </button>
            ))}
        </div>
      ) : (
        <>
          <button
            className="sidebar-new"
            onClick={() => void newConversation()}
          >
            + 新对话
          </button>
          <div className="sidebar-list">
            {conversations.length === 0 ? (
              <div className="sidebar-hint">还没有历史会话</div>
            ) : null}
            {conversations.map((conv) => (
              <div
                key={conv.id}
                className={`conv-item ${conv.id === activeId ? "active" : ""}`}
              >
                <button
                  className="conv-main"
                  onClick={() => void select(conv.id)}
                  title={conv.title}
                >
                  <span className="conv-title">
                    {conv.pinned ? "📌 " : ""}
                    {conv.title}
                  </span>
                  <span className="conv-meta">
                    {conv.messageCount} 条 · {conv.modelKey || "未指定模型"}
                  </span>
                </button>
                <div className="conv-actions">
                  <button
                    title="重命名"
                    onClick={() => void onRename(conv.id, conv.title)}
                  >
                    ✎
                  </button>
                  <button
                    title={conv.pinned ? "取消置顶" : "置顶"}
                    onClick={() => void togglePinned(conv.id)}
                  >
                    ⇧
                  </button>
                  <button
                    title="删除"
                    onClick={() => {
                      if (window.confirm(`删除会话「${conv.title}」？`)) {
                        void remove(conv.id);
                      }
                    }}
                  >
                    🗑
                  </button>
                </div>
              </div>
            ))}

            {/* 一次只拉 conversationLimit 条。以前固定 200 条且没有任何入口，
                第 201 个会话就永远够不着了。 */}
            {conversations.length >= conversationLimit ? (
              conversationLimit < CONVERSATION_LIMIT_MAX ? (
                <button
                  className="sidebar-more"
                  onClick={() => void loadMore()}
                >
                  加载更多（已显示 {conversations.length} 个）
                </button>
              ) : (
                <div className="sidebar-hint">
                  只显示最近 {CONVERSATION_LIMIT_MAX} 个会话 —— 更早的请用上面的搜索
                </div>
              )
            ) : null}
          </div>
        </>
      )}
    </aside>
  );
}
