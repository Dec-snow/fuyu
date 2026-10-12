// 技能的解析规则（纯函数，可单测）。
//
// 与角色（`lib/roles.ts`）的分工：
// - 角色是**会话级**身份，选一次影响整段对话；
// - 技能是**一次性**任务指令，只在发出去的这一条上生效，并且能指定用哪些工具。
//
// 这里负责两件事：`/名字` 斜杠命令的解析，以及「最终发出去的那条消息 + 用哪个技能」。

import type { SkillPreset } from "../types";

/** 斜杠命令的前缀。 */
export const SKILL_COMMAND_PREFIX = "/";

/** 按 id 找技能；找不到返回 null。 */
export function findSkill(
  skills: SkillPreset[],
  id: string | null | undefined,
): SkillPreset | null {
  if (!id) return null;
  return skills.find((s) => s.id === id) ?? null;
}

/** 下拉框里显示的文案。 */
export function skillLabel(skill: SkillPreset): string {
  return skill.builtin ? skill.name : `${skill.name}（自定义）`;
}

/**
 * 工具白名单的一句话说明。
 *
 * 必须显眼地写出来：技能最容易被误解的地方就是「它悄悄改变了哪些工具能用」——
 * 而「纯离线总结」这类技能一旦被误解成「只是加了一句提示词」，
 * 用户会以为数据没出去，其实出去了（或者反过来，以为出去了其实没出去）。
 */
export function describeSkillTools(skill: SkillPreset): string {
  if (skill.tools.length === 0) return "工具：不干预";
  return `工具：只用 ${skill.tools.join("、")}`;
}

/**
 * 解析 `/名字 内容`。
 *
 * 三条规则：
 * - 必须**整条**消息以 `/` 开头（前面可以有空白）——「价格是 3/5」不该被当成命令；
 * - 名字按**名字或 id**匹配，忽略大小写（中文名不受影响，英文名要好敲）；
 * - **后面必须有内容**：只有 `/翻译` 一个词时按普通文本发出去。
 *   否则会往模型那里送一条空的用户消息 —— 那比「没生效」更让人困惑。
 */
export function parseSkillCommand(
  text: string,
  skills: SkillPreset[],
): { skill: SkillPreset; rest: string } | null {
  const trimmed = text.trimStart();
  if (!trimmed.startsWith(SKILL_COMMAND_PREFIX)) return null;

  const body = trimmed.slice(SKILL_COMMAND_PREFIX.length);
  const match = /^(\S+)\s*([\s\S]*)$/.exec(body);
  if (!match) return null;

  const name = match[1].toLowerCase();
  const rest = match[2].trim();
  if (rest.length === 0) return null;

  const skill = skills.find(
    (s) => s.name.toLowerCase() === name || s.id.toLowerCase() === name,
  );
  if (!skill) return null;
  return { skill, rest };
}

/**
 * 这条消息最终发什么、用哪个技能。
 *
 * 斜杠命令优先于下拉框的选择：用户手打了 `/翻译` 却因为下拉框里还挂着上一次
 * 选的技能而没生效，是很难自己想明白的。
 */
export function resolveOutgoing(
  text: string,
  skills: SkillPreset[],
  pendingSkillId: string | null,
): { text: string; skillId: string | null; skillName: string | null } {
  const parsed = parseSkillCommand(text, skills);
  if (parsed) {
    return { text: parsed.rest, skillId: parsed.skill.id, skillName: parsed.skill.name };
  }
  const pending = findSkill(skills, pendingSkillId);
  return { text, skillId: pending?.id ?? null, skillName: pending?.name ?? null };
}

/**
 * 生成一个新技能的 id。
 *
 * 与角色同一个理由：用随机 uuid 而不是序号 —— 序号会在删除后复用，
 * 而复用意味着「一次发送可能突然用上另一个技能」。
 */
export function newSkillId(): string {
  const rand =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID().slice(0, 8)
      : Math.random().toString(36).slice(2, 10);
  return `skill-${rand}`;
}

/** 新技能的初始内容。 */
export function blankSkill(): SkillPreset {
  return {
    id: newSkillId(),
    name: "新技能",
    description: "",
    prompt: "",
    tools: [],
    builtin: false,
  };
}
