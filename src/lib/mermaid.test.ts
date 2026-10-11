// mermaid 的测试。
//
// 这里能测的是**纯逻辑与安全配置**：真正的画图需要浏览器布局（`getBBox` 等），
// node 环境里跑不起来。而「跑不起来时会不会炸」恰恰也是要测的一条 ——
// 所以专门有一条「没有 DOM 时返回 null 而不是抛异常」。
//
// 真实渲染的验证走浏览器：见 `tools/` 下的预览与探针脚本。

import { describe, expect, it } from "vitest";

import {
  isMermaidBlock,
  MAX_MERMAID_CHARS,
  MERMAID_CONFIG,
  nextMermaidId,
  renderMermaid,
} from "./mermaid";

describe("MERMAID_CONFIG（安全相关的参数，值得被钉死）", () => {
  it("securityLevel 必须是 strict —— mermaid 靠它清洗生成的 SVG", () => {
    expect(MERMAID_CONFIG.securityLevel).toBe("strict");
  });

  it("startOnLoad 必须是 false：我们手动渲染，不要它自己扫描页面", () => {
    // true 的话 mermaid 会在加载时扫描整个文档，把用户消息里恰好长得像
    // mermaid 的普通文本也拿去渲染。
    expect(MERMAID_CONFIG.startOnLoad).toBe(false);
  });

  it("关掉自带的错误图：报错时我们自己显示源码，比红叉图更清楚", () => {
    expect(MERMAID_CONFIG.suppressErrorRendering).toBe(true);
  });
});

describe("isMermaidBlock", () => {
  it("认 mermaid 与 mmd 两种写法，大小写和空格都不敏感", () => {
    expect(isMermaidBlock("mermaid")).toBe(true);
    expect(isMermaidBlock("Mermaid")).toBe(true);
    expect(isMermaidBlock("  mmd ")).toBe(true);
  });

  it("其它语言不是 mermaid", () => {
    for (const lang of ["rust", "js", "text", "", null, undefined, "mermaidx"]) {
      expect(isMermaidBlock(lang), String(lang)).toBe(false);
    }
  });
});

describe("nextMermaidId", () => {
  it("每次都给不同的 id（mermaid 用它建临时 DOM，重复会互相覆盖）", () => {
    const ids = new Set(Array.from({ length: 50 }, () => nextMermaidId()));
    expect(ids.size).toBe(50);
    for (const id of ids) expect(id.startsWith("mmd-")).toBe(true);
  });
});

describe("renderMermaid 的降级行为", () => {
  it("空源码 / 只有空白 → null", async () => {
    expect(await renderMermaid("", "default")).toBeNull();
    expect(await renderMermaid("   \n ", "default")).toBeNull();
  });

  it("超长源码 → null（不让一张巨图把界面卡住）", async () => {
    const huge = "A-->B\n".repeat(MAX_MERMAID_CHARS);
    expect(huge.length).toBeGreaterThan(MAX_MERMAID_CHARS);
    expect(await renderMermaid(huge, "default")).toBeNull();
  });

  it("没有 DOM 的环境里不抛异常，而是返回 null（降级显示源码）", async () => {
    // node 环境没有 document：mermaid 会失败。这条守的是「测试环境/异常环境下
    // 渲染一条消息不能把整个界面炸掉」。
    await expect(renderMermaid("graph TD\nA-->B", "default")).resolves.toBeNull();
  });
});
