// 技能解析规则的测试。
//
// 守两类问题：
// - 斜杠命令「误触发 / 不触发」：普通文本里的 `/` 被当成命令，或者手打了命令
//   却没生效（后者用户很难自己想明白）；
// - 只打命令没内容时往模型那里送一条空消息。

import { describe, expect, it } from "vitest";

import type { SkillPreset } from "../types";
import {
  blankSkill,
  describeSkillTools,
  findSkill,
  newSkillId,
  parseSkillCommand,
  resolveOutgoing,
  skillLabel,
} from "./skills";

const SKILLS: SkillPreset[] = [
  {
    id: "builtin-translate",
    name: "翻译",
    description: "翻成另一种语言",
    prompt: "翻译它",
    tools: [],
    builtin: true,
  },
  {
    id: "builtin-research",
    name: "联网调研",
    description: "先查再答",
    prompt: "先搜索",
    tools: ["web_search", "fetch_webpage"],
    builtin: true,
  },
  {
    id: "mine-1",
    name: "Review",
    description: "",
    prompt: "审查代码",
    tools: ["calculator"],
    builtin: false,
  },
];

describe("parseSkillCommand", () => {
  it("认出 `/名字 内容` 并去掉命令部分", () => {
    const parsed = parseSkillCommand("/翻译 hello world", SKILLS);
    expect(parsed?.skill.id).toBe("builtin-translate");
    expect(parsed?.rest).toBe("hello world");
  });

  it("英文名忽略大小写", () => {
    expect(parseSkillCommand("/review 这段代码", SKILLS)?.skill.id).toBe("mine-1");
    expect(parseSkillCommand("/REVIEW 这段代码", SKILLS)?.skill.id).toBe("mine-1");
  });

  it("也认 id", () => {
    expect(parseSkillCommand("/builtin-research 固态电池", SKILLS)?.skill.id).toBe(
      "builtin-research",
    );
  });

  it("前面的空白不影响", () => {
    expect(parseSkillCommand("   /翻译 你好", SKILLS)?.rest).toBe("你好");
  });

  it("普通文本里的斜杠不算命令", () => {
    // 价格、路径、日期里都可能有 /
    for (const text of ["价格是 3/5 元", "看 C:/Users 这个目录", "2024/05/01 的报表"]) {
      expect(parseSkillCommand(text, SKILLS), text).toBeNull();
    }
  });

  it("不认识的名字不算命令（照原样发出去）", () => {
    expect(parseSkillCommand("/不存在的技能 内容", SKILLS)).toBeNull();
  });

  it("只打命令没内容时不算命令", () => {
    // 否则会往模型那里送一条空的用户消息 —— 比「没生效」更让人困惑
    expect(parseSkillCommand("/翻译", SKILLS)).toBeNull();
    expect(parseSkillCommand("/翻译   ", SKILLS)).toBeNull();
  });

  it("多行内容整段保留", () => {
    const parsed = parseSkillCommand("/翻译 第一行\n第二行", SKILLS);
    expect(parsed?.rest).toBe("第一行\n第二行");
  });
});

describe("resolveOutgoing", () => {
  it("斜杠命令优先于下拉框的选择", () => {
    // 手打了 /翻译 却因为下拉框还挂着别的技能而没生效，用户很难想明白
    const out = resolveOutgoing("/翻译 hello", SKILLS, "builtin-research");
    expect(out.skillId).toBe("builtin-translate");
    expect(out.skillName).toBe("翻译");
    expect(out.text).toBe("hello");
  });

  it("没有命令时用下拉框选的技能", () => {
    const out = resolveOutgoing("hello", SKILLS, "builtin-research");
    expect(out.skillId).toBe("builtin-research");
    expect(out.text).toBe("hello");
  });

  it("都没有就什么技能也不用", () => {
    const out = resolveOutgoing("hello", SKILLS, null);
    expect(out.skillId).toBeNull();
    expect(out.skillName).toBeNull();
    expect(out.text).toBe("hello");
  });

  it("下拉框里选了一个已经被删掉的技能时安静回落", () => {
    const out = resolveOutgoing("hello", SKILLS, "已经删掉的 id");
    expect(out.skillId).toBeNull();
    expect(out.text).toBe("hello");
  });
});

describe("显示与新建", () => {
  it("自定义技能带上后缀", () => {
    expect(skillLabel(SKILLS[0])).toBe("翻译");
    expect(skillLabel(SKILLS[2])).toBe("Review（自定义）");
  });

  it("把工具白名单写清楚（这是技能最容易被误解的地方）", () => {
    expect(describeSkillTools(SKILLS[0])).toBe("工具：不干预");
    expect(describeSkillTools(SKILLS[1])).toBe("工具：只用 web_search、fetch_webpage");
  });

  it("findSkill 找不到就返回 null", () => {
    expect(findSkill(SKILLS, null)).toBeNull();
    expect(findSkill(SKILLS, "没有这个")).toBeNull();
    expect(findSkill(SKILLS, "mine-1")?.name).toBe("Review");
  });

  it("新 id 不重复，新技能默认不限制工具", () => {
    const a = newSkillId();
    const b = newSkillId();
    expect(a).not.toBe(b);
    expect(a.startsWith("skill-")).toBe(true);
    const blank = blankSkill();
    expect(blank.builtin).toBe(false);
    expect(blank.tools).toEqual([]);
  });
});
