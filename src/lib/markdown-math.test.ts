// 数学公式的**解析规则**单独成文件：误判风险集中在这里。
//
// 行内公式的判据只要松一点，`价格是 $5 和 $10` 这种普通句子就会变成公式，
// 而症状是「用户看到自己的话被排版成了乱码」—— 很难联想到解析器。

import { describe, expect, it } from "vitest";

import { splitInline, splitMarkdownBlocks } from "./markdown";

describe("块级公式", () => {
  it("整行 $$...$$ 变成一个公式块", () => {
    expect(splitMarkdownBlocks("$$E = mc^2$$")).toEqual([
      { kind: "math", content: "E = mc^2" },
    ]);
  });

  it("多行 $$ 围栏", () => {
    const blocks = splitMarkdownBlocks("前文\n\n$$\n\\frac{a}{b}\n= c\n$$\n\n后文");
    // 文本块会带上相邻的空行（既有行为，HTML 里会被折叠，不影响观感）；
    // 这里关心的是「公式被单独切出来了、内容完整」。
    expect(blocks.map((b) => b.kind)).toEqual(["text", "math", "text"]);
    expect(blocks[1]).toEqual({ kind: "math", content: "\\frac{a}{b}\n= c" });
    expect((blocks[0] as { content: string }).content).toContain("前文");
    expect((blocks[2] as { content: string }).content).toContain("后文");
  });

  it("未闭合的 $$ 也按公式渲染（流式输出中常见）", () => {
    const blocks = splitMarkdownBlocks("$$\nx^2 + y^2");
    expect(blocks).toEqual([{ kind: "math", content: "x^2 + y^2" }]);
  });

  it("公式块与代码块可以相邻且互不干扰", () => {
    const source = "```rust\nlet x = 1;\n```\n\n$$\nx = 1\n$$\n\n```py\nx = 1\n```";
    const kinds = splitMarkdownBlocks(source).map((b) => b.kind);
    expect(kinds).toEqual(["code", "math", "code"]);
  });

  it("代码块**里面**的 $$ 不会被当成公式（必须先进代码块状态）", () => {
    const blocks = splitMarkdownBlocks("```latex\n$$\nx\n$$\n```");
    expect(blocks).toEqual([
      { kind: "code", language: "latex", content: "$$\nx\n$$" },
    ]);
  });

  it("只有一个 $ 的行仍是普通文本", () => {
    expect(splitMarkdownBlocks("一共 5$")).toEqual([
      { kind: "text", content: "一共 5$" },
    ]);
  });
});

describe("行内公式", () => {
  const kinds = (s: string) => splitInline(s).map((t) => `${t.kind}:${t.value}`);

  it("普通的 $x^2$ 认成公式", () => {
    expect(kinds("由 $x^2$ 可得")).toEqual([
      "text:由 ",
      "math:x^2",
      "text: 可得",
    ]);
  });

  it("一句里两个公式都能认出来", () => {
    expect(kinds("$a$ 与 $b$")).toEqual([
      "math:a",
      "text: 与 ",
      "math:b",
    ]);
  });

  it("**价格不会被当成公式**（这一条最重要）", () => {
    // 两个 `$` 后面都跟着数字 → 不配对
    expect(kinds("价格是 $5 和 $10")).toEqual(["text:价格是 $5 和 $10"]);
    expect(kinds("$100")).toEqual(["text:$100"]);
    expect(kinds("花了 $5.5 元")).toEqual(["text:花了 $5.5 元"]);
  });

  it("公式里带空格 / 前后有空格的情况", () => {
    // `$ x $`（两端贴近 $ 的是空格）不当公式
    expect(kinds("$ x $")).toEqual(["text:$ x $"]);
    // 但公式内部可以有空格
    expect(kinds("$a + b$")).toEqual(["math:a + b"]);
  });

  it("转义的 \\$ 不当公式", () => {
    expect(kinds("\\$x\\$")).toEqual(["text:\\$x\\$"]);
  });

  it("$ 后面直接跟 $ （空公式）不当公式", () => {
    expect(kinds("$$")).toEqual(["text:$$"]);
  });

  it("跨越换行的 $ 不配对（逐行解析）", () => {
    expect(kinds("$a")).toEqual(["text:$a"]);
    expect(kinds("a$")).toEqual(["text:a$"]);
  });

  it("公式里可以包含反斜杠命令与花括号", () => {
    expect(kinds("$\\frac{1}{2}$")).toEqual(["math:\\frac{1}{2}"]);
    expect(kinds("$\\sqrt{x}$")).toEqual(["math:\\sqrt{x}"]);
  });

  it("和行内代码、粗体共存", () => {
    expect(kinds("**粗** `码` $x$")).toEqual([
      "bold:粗",
      "text: ",
      "code:码",
      "text: ",
      "math:x",
    ]);
  });

  it("行内代码里的 $ 不受影响（反引号优先）", () => {
    expect(kinds("`$x$`")).toEqual(["code:$x$"]);
  });

  it("数字紧跟结束 $ 时不当公式", () => {
    expect(kinds("$x$2")).toEqual(["text:$x$2"]);
  });
});
