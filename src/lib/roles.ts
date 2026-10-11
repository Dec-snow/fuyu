// 角色预设的解析规则（纯函数，可单测）。
//
// 只有一条规则值得单独写文件：**「会话绑定的角色」优先于「全局系统提示词」**，
// 而角色找不到时（用户删了它）要安静地回落到全局，不能报错、更不能把
// 一段过期的提示词继续发出去。

import type { RolePreset } from "../types";

/** 按 id 找人设；找不到返回 null。 */
export function findRole(
  roles: RolePreset[],
  id: string | null | undefined,
): RolePreset | null {
  if (!id) return null;
  return roles.find((r) => r.id === id) ?? null;
}

/**
 * 这个会话最终该用的系统提示词。
 *
 * - 绑定了人设且提示词非空 → 用它；
 * - 人设被删了 / 提示词是空白 → `null`（交给后端回落到全局系统提示词）。
 *
 * 返回 `null` 而不是空串：空串会被后端当成「用户显式要求没有系统提示词」
 * （`resolve_system_prompt` 里 `Some("")` 是不合法的输入），
 * 而 `null` 才表示「没指定，用默认的」。
 */
export function resolveRolePrompt(
  roles: RolePreset[],
  id: string | null | undefined,
): string | null {
  const role = findRole(roles, id);
  if (!role) return null;
  const prompt = role.prompt.trim();
  return prompt.length > 0 ? prompt : null;
}

/** 界面上下拉框里显示的文案。 */
export function roleLabel(role: RolePreset): string {
  return role.builtin ? `${role.name}` : `${role.name}（自定义）`;
}

/**
 * 生成一个新角色的 id。
 *
 * 用随机 uuid 而不是「下一个序号」：序号会在删除后复用，
 * 而复用意味着「一个旧会话可能突然绑到另一个角色上」。
 */
export function newRoleId(): string {
  const rand =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID().slice(0, 8)
      : Math.random().toString(36).slice(2, 10);
  return `role-${rand}`;
}

/** 新角色的初始内容（名字留空让用户填，提示词给一句可改的起点）。 */
export function blankRole(): RolePreset {
  return {
    id: newRoleId(),
    name: "新角色",
    prompt: "",
    builtin: false,
  };
}
