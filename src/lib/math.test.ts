// 公式渲染的测试。
//
// 这个文件守的是**整个前端唯一一处 `dangerouslySetInnerHTML`** 的安全边界：
// - `trust: false` 必须一直开着（能注入链接/图片的能力全靠它挡）；
// - `looksSafe` 兜底必须真的能认出脚本与事件处理器；
// - 任何异常输入都必须退回源码，而不是抛错或注入空内容。

import { describe, expect, it } from "vitest";

import { isDisplayMath, KATEX_OPTIONS, looksSafe, MAX_MATH_CHARS, renderMath } from "./math";

describe("KATEX_OPTIONS（安全相关的参数，值得被钉死）", () => {
  it("trust 必须是 false —— 它挡着 \\href / \\url / \\includegraphics / \\html*", () => {
    expect(KATEX_OPTIONS.trust).toBe(false);
  });

  it("throwOnError 必须是 false（公式写错就标红显示，不能炸掉整条消息）", () => {
    expect(KATEX_OPTIONS.throwOnError).toBe(false);
  });
});

describe("renderMath", () => {
  it("普通公式渲染出 katex 结构", () => {
    const html = renderMath("x^2 + y^2 = z^2", false);
    expect(html).toBeTruthy();
    expect(html!).toContain("katex");
    // 文本内容要出现在结果里
    expect(html!).toContain("x");
  });

  it("块级公式带上 display 模式", () => {
    const inline = renderMath("a+b", false)!;
    const display = renderMath("a+b", true)!;
    expect(inline).not.toBe(display);
    expect(display).toContain("katex-display");
  });

  it("公式写错时仍然返回内容（标红显示），而不是 null", () => {
    // throwOnError:false 的意义就在这里：用户能看到「这里有个公式写错了」，
    // 而不是公式凭空消失。
    const html = renderMath("\\frac{1}{", false);
    expect(html).toBeTruthy();
  });

  it("空公式 / 只有空白 → null", () => {
    expect(renderMath("", false)).toBeNull();
    expect(renderMath("   \n  ", false)).toBeNull();
  });

  it("超长公式 → null（不让一次渲染卡住界面）", () => {
    const huge = "x+".repeat(MAX_MATH_CHARS);
    expect(huge.length).toBeGreaterThan(MAX_MATH_CHARS);
    expect(renderMath(huge, false)).toBeNull();
  });

  it("不抛异常：各种奇怪输入都只是返回 null 或内容", () => {
    for (const bad of ["$", "{{{{", "\\", "\u0000", "\\sqrt{", "中文公式"]) {
      expect(() => renderMath(bad, false), bad).not.toThrow();
    }
  });

  it("trust:false 真的挡住了 href 类命令（不生成链接属性）", () => {
    // KaTeX 在 trust:false 下把 \href 渲染成**红色的错误文本**：
    // 源码会作为转义文本出现，但不会生成任何 href 属性。
    // 所以正确的断言是「没有链接属性」，而不是「输出里不含 javascript: 子串」
    // —— 那个子串会以转义文本的形式正常出现。
    const html = renderMath("\\href{javascript:alert(1)}{点我}", false)!;
    expect(html).toBeTruthy();
    expect(html).not.toContain("href=");
    expect(html).not.toContain("<a ");
    // 它确实把这条命令当成「不支持」处理了（渲染成错误样式）
    expect(html).toContain("#cc0000");
  });
});

describe("looksSafe（注入前的兜底检查）", () => {
  it("正常的 KaTeX 输出是安全的", () => {
    expect(looksSafe(renderMath("\\frac{a}{b}", true)!)).toBe(true);
  });

  it("认出脚本与事件处理器", () => {
    expect(looksSafe('<span onclick="alert(1)">x</span>')).toBe(false);
    expect(looksSafe("<script>alert(1)</script>")).toBe(false);
    expect(looksSafe("<iframe src=x>")).toBe(false);
    expect(looksSafe("<img src=x onerror=alert(1)>")).toBe(false);
    expect(looksSafe('<a href="javascript:alert(1)">x</a>')).toBe(false);
    expect(looksSafe('<a href="data:text/html,<script>">x</a>')).toBe(false);
    expect(looksSafe("<link rel=stylesheet href=x>")).toBe(false);
    expect(looksSafe("<meta http-equiv=refresh>")).toBe(false);
    expect(looksSafe("<form action=x>")).toBe(false);
    expect(looksSafe("<object data=x>")).toBe(false);
    expect(looksSafe("<embed src=x>")).toBe(false);
    expect(looksSafe("<base href=x>")).toBe(false);
  });

  it("不误杀普通内容（含 style 属性与中文）", () => {
    expect(looksSafe('<span class="katex" style="height:1em">公式</span>')).toBe(true);
    expect(looksSafe("只是文本 on 而已")).toBe(true);
    // "on" 后面没有等号，不该被当成事件处理器
    expect(looksSafe("<span>online</span>")).toBe(true);
  });

  it("不误杀 KaTeX 的真实输出形态", () => {
    // 两个曾经误判的地方：
    // ① MathML 里有 <annotation> 标签，不能因为「含 <a」就判危险；
    // ② trust:false 把 \href 源码当**转义文本**显示，其中含 javascript: 子串。
    const html = renderMath("\\href{javascript:alert(1)}{x}", false)!;
    expect(looksSafe(html)).toBe(true);
    expect(looksSafe(renderMath("E = mc^2", true)!)).toBe(true);
  });
});

describe("isDisplayMath", () => {
  it("多行或很长的公式按块级显示", () => {
    expect(isDisplayMath("a+b")).toBe(false);
    expect(isDisplayMath("a\nb")).toBe(true);
    expect(isDisplayMath("x".repeat(80))).toBe(true);
  });
});
