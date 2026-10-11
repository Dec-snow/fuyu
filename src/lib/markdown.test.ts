import { describe, expect, it } from "vitest";

import { splitInline, splitMarkdownBlocks } from "./markdown";

describe("splitMarkdownBlocks", () => {
  it("把围栏代码块和正文分开", () => {
    const blocks = splitMarkdownBlocks("看这段\n```rust\nfn main() {}\n```\n就这样");

    expect(blocks).toEqual([
      { kind: "text", content: "看这段" },
      { kind: "code", language: "rust", content: "fn main() {}" },
      { kind: "text", content: "就这样" },
    ]);
  });

  it("未闭合的围栏仍然按代码块渲染", () => {
    // 流式输出时代码正写到一半，围栏还没来。
    // 若这时按正文渲染，代码会在写完后突然「长出背景」，视觉上跳一下。
    const blocks = splitMarkdownBlocks("看这个：\n```python\nprint(1)");

    expect(blocks).toEqual([
      { kind: "text", content: "看这个：" },
      { kind: "code", language: "python", content: "print(1)" },
    ]);
  });

  it("没有语言标注的围栏也能解析", () => {
    const blocks = splitMarkdownBlocks("```\nplain\n```");
    expect(blocks).toEqual([{ kind: "code", language: "", content: "plain" }]);
  });

  it("识别常见的语言标记", () => {
    for (const lang of ["ts", "tsx", "c++", "c#", "bash", "json", "sh"]) {
      const blocks = splitMarkdownBlocks(`\`\`\`${lang}\nx\n\`\`\``);
      expect(blocks[0]).toEqual({ kind: "code", language: lang, content: "x" });
    }
  });

  it("纯正文只产出一个文本块，并保留空行", () => {
    const blocks = splitMarkdownBlocks("第一行\n\n第三行");
    expect(blocks).toEqual([{ kind: "text", content: "第一行\n\n第三行" }]);
  });

  it("空字符串产出空数组", () => {
    expect(splitMarkdownBlocks("")).toEqual([]);
  });

  it("代码块内出现的 ``` 只要不在行首就不算结束", () => {
    const blocks = splitMarkdownBlocks("```\na ``` b\n```");
    expect(blocks).toEqual([{ kind: "code", language: "", content: "a ``` b" }]);
  });

  it("多个代码块按顺序保留", () => {
    const blocks = splitMarkdownBlocks("```js\n1\n```\n中间\n```js\n2\n```");
    expect(blocks.map((b) => b.kind)).toEqual(["code", "text", "code"]);
    expect(blocks[2]).toEqual({ kind: "code", language: "js", content: "2" });
  });

  it("把 HTML 当普通文本，不做任何解释", () => {
    // 模型输出是不可信输入。解析结果里只会有 text 块，
    // 组件把它当文本节点渲染，不经过 HTML 解析。
    const blocks = splitMarkdownBlocks('<script>alert("x")</script>');
    expect(blocks).toEqual([
      { kind: "text", content: '<script>alert("x")</script>' },
    ]);
  });
});

describe("splitInline", () => {
  it("识别行内代码与粗体", () => {
    expect(splitInline("用 `cargo test` 跑，**必须**通过")).toEqual([
      { kind: "text", value: "用 " },
      { kind: "code", value: "cargo test" },
      { kind: "text", value: " 跑，" },
      { kind: "bold", value: "必须" },
      { kind: "text", value: "通过" },
    ]);
  });

  it("没有标记时只有一个文本片段", () => {
    expect(splitInline("普通一句话")).toEqual([
      { kind: "text", value: "普通一句话" },
    ]);
  });

  it("空字符串产出空数组", () => {
    expect(splitInline("")).toEqual([]);
  });

  it("不跨行匹配（未闭合的标记保留成普通文本）", () => {
    // 流式输出时 `**粗` 还没写完，不能把它吞掉 —— 否则已经显示的字会消失。
    expect(splitInline("**粗")).toEqual([{ kind: "text", value: "**粗" }]);
    expect(splitInline("`code")).toEqual([{ kind: "text", value: "`code" }]);
  });

  it("相邻的标记都能识别", () => {
    expect(splitInline("`a`**b**")).toEqual([
      { kind: "code", value: "a" },
      { kind: "bold", value: "b" },
    ]);
  });

  it("行内代码里的星号不会被当成粗体", () => {
    expect(splitInline("`a**b**c`")).toEqual([
      { kind: "code", value: "a**b**c" },
    ]);
  });
});
