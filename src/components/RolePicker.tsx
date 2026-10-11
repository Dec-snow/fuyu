import { roleLabel } from "../lib/roles";
import { useChat } from "../stores/chat";
import { useConfig } from "../stores/config";

/**
 * 输入框上方的角色选择器。
 *
 * 只改**当前会话**的人设（不是全局默认）：这是刻意的 —— 同一个应用里
 * 「翻译一段话」和「帮我改代码」通常发生在不同的会话里，把人设做成全局开关
 * 会让用户每次都要来回改。全局默认仍然是「设置 → 对话」里的系统提示词。
 *
 * 没有活动会话时（刚启动、还没建会话）不允许改：人设是挂在会话上的，
 * 此时改了没有地方存，只会让用户以为设置丢了。
 */
export function RolePicker() {
  const config = useConfig((s) => s.config);
  const activeId = useChat((s) => s.activeId);
  const conversations = useChat((s) => s.conversations);
  const setConversationRole = useChat((s) => s.setConversationRole);

  const roles = config?.roles ?? [];
  if (roles.length === 0) return null;

  const active = conversations.find((c) => c.id === activeId);
  const current = active?.roleId ?? "";

  return (
    <select
      className="role-picker"
      value={current}
      disabled={!active}
      title={
        active
          ? "这个会话用的人设（只影响当前会话）"
          : "先发一条消息建立会话，再选人设"
      }
      onChange={(e) => {
        if (!active) return;
        const next = e.target.value;
        void setConversationRole(active.id, next === "" ? null : next);
      }}
    >
      <option value="">默认人设</option>
      {roles.map((role) => (
        <option key={role.id} value={role.id}>
          {roleLabel(role)}
        </option>
      ))}
    </select>
  );
}
