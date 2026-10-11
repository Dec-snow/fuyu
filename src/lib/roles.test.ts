// 角色解析规则的测试。
//
// 守的是「改了人设却没生效」和「删了人设后发出去的还是旧提示词」这两类问题 ——
// 它们都不会报错，只会在回答里表现成「模型怎么不听我的了」。

import { describe, expect, it } from "vitest";

import type { RolePreset } from "../types";
import { blankRole, findRole, newRoleId, resolveRolePrompt, roleLabel } from "./roles";

const r = (id: string, name: string, prompt: string, builtin = false): RolePreset => ({
  id,
  name,
  prompt,
  builtin,
});

const ROLES: RolePreset[] = [
  r("builtin-translator", "翻译官", "你是翻译。", true),
  r("builtin-coder", "代码助手", "你是工程师。", true),
  r("mine", "我的人设", "  带空白的提示词  "),
];

describe("findRole", () => {
  it("按 id 找人设", () => {
    expect(findRole(ROLES, "builtin-coder")?.name).toBe("代码助手");
  });

  it("找不到 / 没传 id 时返回 null", () => {
    expect(findRole(ROLES, "不存在")).toBeNull();
    expect(findRole(ROLES, null)).toBeNull();
    expect(findRole(ROLES, undefined)).toBeNull();
    expect(findRole(ROLES, "")).toBeNull();
  });
});

describe("resolveRolePrompt", () => {
  it("绑定的人设优先，并把首尾空白去掉", () => {
    expect(resolveRolePrompt(ROLES, "mine")).toBe("带空白的提示词");
  });

  it("人设被删掉之后回落到 null（而不是抛错或沿用旧值）", () => {
    expect(resolveRolePrompt(ROLES, "已经删掉的 id")).toBeNull();
  });

  it("没绑人设时返回 null", () => {
    expect(resolveRolePrompt(ROLES, null)).toBeNull();
  });

  it("提示词是空白的人设等于没设（不能把空串发出去）", () => {
    // 空串在后端会被当成「用户显式要求没有系统提示词」，
    // 而我们要的是「没指定，用默认的」—— 所以必须是 null。
    const blank = [r("b", "空的", "   ")];
    expect(resolveRolePrompt(blank, "b")).toBeNull();
  });

  it("改动人设内容后立刻生效（解析的是当前配置，不是快照）", () => {
    const roles = [r("x", "甲", "旧提示词")];
    expect(resolveRolePrompt(roles, "x")).toBe("旧提示词");
    roles[0] = r("x", "甲", "新提示词");
    expect(resolveRolePrompt(roles, "x")).toBe("新提示词");
  });
});

describe("roleLabel", () => {
  it("自建角色标出「自定义」，内置的不标", () => {
    expect(roleLabel(ROLES[0])).toBe("翻译官");
    expect(roleLabel(ROLES[2])).toBe("我的人设（自定义）");
  });
});

describe("newRoleId / blankRole", () => {
  it("每次生成的 id 都不同（否则旧会话会串到新角色上）", () => {
    const ids = new Set(Array.from({ length: 50 }, () => newRoleId()));
    expect(ids.size).toBe(50);
    for (const id of ids) expect(id.startsWith("role-")).toBe(true);
  });

  it("blankRole 给出可编辑的起点", () => {
    const role = blankRole();
    expect(role.builtin).toBe(false);
    expect(role.name.length).toBeGreaterThan(0);
    expect(role.id).toMatch(/^role-/);
  });
});
