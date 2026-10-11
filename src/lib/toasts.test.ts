import { describe, expect, it } from "vitest";

import { collectToasts } from "./toasts";

const empty = {
  chatError: null,
  configError: null,
  providersError: null,
  notices: [],
};

describe("collectToasts", () => {
  it("什么都没有时不显示任何提示", () => {
    expect(collectToasts(empty)).toEqual([]);
  });

  // 这一组是这次修复的核心回归防线：这四个通道以前**全都没有渲染**，
  // 于是「没配模型就按回车」这类失败在界面上完全没有任何反馈。
  it("聊天错误会显示（没配模型、发送失败等）", () => {
    const items = collectToasts({
      ...empty,
      chatError: "还没有可用的服务商或模型，请先到「设置 → 模型」里配置。",
    });
    expect(items).toHaveLength(1);
    expect(items[0].text).toContain("服务商");
    expect(items[0].kind).toBe("error");
  });

  it("设置错误会显示，并标明来源", () => {
    const items = collectToasts({ ...empty, configError: "Base URL 必须以 http:// 开头" });
    expect(items).toHaveLength(1);
    expect(items[0].text).toContain("设置：");
    expect(items[0].text).toContain("http://");
  });

  it("模型配置错误会显示", () => {
    const items = collectToasts({ ...empty, providersError: "读取密钥失败" });
    expect(items).toHaveLength(1);
    expect(items[0].text).toContain("模型配置：");
  });

  it("通知会显示（快捷键被占用、图片超限等）", () => {
    const items = collectToasts({
      ...empty,
      notices: ["快捷键 Alt+Q 已被其它程序占用", "一条消息最多 4 张图片"],
    });
    expect(items.map((i) => i.kind)).toEqual(["notice", "notice"]);
    expect(items[0].text).toContain("占用");
    expect(items[1].text).toContain("4 张");
  });

  it("顺序是「先通知、后错误」：错误更需要用户处理", () => {
    const items = collectToasts({
      chatError: "发送失败",
      configError: null,
      providersError: null,
      notices: ["快捷键没注册上"],
    });
    expect(items.map((i) => i.kind)).toEqual(["notice", "error"]);
  });

  it("空白字符串不算提示（避免出现空气泡）", () => {
    const items = collectToasts({
      chatError: "   ",
      configError: "",
      providersError: null,
      notices: ["", "  "],
    });
    expect(items).toEqual([]);
  });

  it("同一路来源只产生一条，key 不重复", () => {
    const items = collectToasts({
      chatError: "错误",
      configError: "错误",
      providersError: "错误",
      notices: ["通知"],
    });
    const keys = items.map((i) => i.key);
    expect(new Set(keys).size).toBe(keys.length);
  });
});
