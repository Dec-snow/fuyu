// 代码高亮的测试。
//
// 守两件事：
// 1. 高亮**不注入 HTML**（输出是节点树）—— 这是整个方案的立身之本；
// 2. 任何异常输入都退回纯文本，绝不抛错（渲染一条消息不该因为配色而整条炸掉）。

import { describe, expect, it } from "vitest";

import {
  highlightCode,
  MAX_HIGHLIGHT_CHARS,
  normalizeLanguage,
  supportedLanguages,
  toHighlightNodes,
  type HighlightNode,
} from "./highlight";

/** 只取文本（不带 class 前缀）—— 用来断言「高亮不丢字符」。 */
function textOnly(nodes: HighlightNode[]): string {
  return nodes
    .map((n) => (n.type === "text" ? n.value : textOnly(n.children)))
    .join("");
}

/** 树里出现过的所有 class。 */
function classes(nodes: HighlightNode[]): string[] {
  return nodes.flatMap((n) => (n.type === "text" ? [] : [...n.className, ...classes(n.children)]));
}

describe("normalizeLanguage", () => {
  it("认识的直接返回", () => {
    expect(normalizeLanguage("rust")).toBe("rust");
    expect(normalizeLanguage("TypeScript")).toBe("typescript");
    expect(normalizeLanguage("  python  ")).toBe("python");
  });

  it("lowlight 自带的别名也认", () => {
    // 注意契约：这个函数返回的是「lowlight 能接受的语言名」，
    // 别名（`rs`）会原样返回 —— lowlight 内部自己会把别名解析到 rust。
    // 所以这里断言的是「能用」，不是「必须是规范名」。
    for (const alias of ["rs", "ts", "tsx", "py", "sh", "yml", "md", "cs"]) {
      const lang = normalizeLanguage(alias);
      expect(lang, `${alias} 应被识别`).not.toBeNull();
      expect(highlightCode("let x = 1", lang), `${alias} 应能真的高亮`).not.toBeNull();
    }
  });

  it("补上我们自己映射的别名（这些是我们自己的表，返回规范名）", () => {
    expect(normalizeLanguage("js")).toBe("javascript");
    expect(normalizeLanguage("jsx")).toBe("javascript");
    expect(normalizeLanguage("h")).toBe("c");
    expect(normalizeLanguage("hpp")).toBe("cpp");
    expect(normalizeLanguage("vue")).toBe("xml");
  });

  it("模型乱写的语言标注不会让它崩", () => {
    // 模型经常写 `tsx (React)`、`c++`、`代码` 这类东西
    expect(normalizeLanguage("tsx (React)")).not.toBeNull();
    // `c++` 是 highlight.js 自己的别名，原样返回也算对（能高亮就行）
    expect(highlightCode("int main() {}", normalizeLanguage("c++"))).not.toBeNull();
    expect(normalizeLanguage("代码")).toBeNull();
  });

  it("空 / null / 没见过的语言一律 null（按纯文本渲染）", () => {
    expect(normalizeLanguage("")).toBeNull();
    expect(normalizeLanguage(null)).toBeNull();
    expect(normalizeLanguage(undefined)).toBeNull();
    expect(normalizeLanguage("definitely-not-a-language")).toBeNull();
    // powershell / batch 不在 highlight.js 的 common 里：宁可不上色，也不猜
    expect(normalizeLanguage("ps1")).toBeNull();
    expect(normalizeLanguage("bat")).toBeNull();
  });
});

describe("toHighlightNodes", () => {
  it("把 hast 树收成我们自己的最小结构", () => {
    const nodes = toHighlightNodes({
      type: "root",
      children: [
        {
          type: "element",
          tagName: "span",
          properties: { className: ["hljs-keyword"] },
          children: [{ type: "text", value: "fn" }],
        },
        { type: "text", value: " main" },
      ],
    });
    expect(nodes).toEqual([
      {
        type: "element",
        tagName: "span",
        className: ["hljs-keyword"],
        children: [{ type: "text", value: "fn" }],
      },
      { type: "text", value: " main" },
    ]);
  });

  it("嵌套的 span 也收得对（highlight.js 会嵌套子语言）", () => {
    const nodes = toHighlightNodes({
      type: "element",
      tagName: "span",
      properties: { className: ["hljs-string"] },
      children: [
        { type: "text", value: '"' },
        {
          type: "element",
          tagName: "span",
          properties: { className: ["hljs-subst"] },
          children: [{ type: "text", value: "x" }],
        },
        { type: "text", value: '"' },
      ],
    });
    expect(nodes).toHaveLength(1);
    expect(nodes[0].type).toBe("element");
    if (nodes[0].type === "element") {
      expect(nodes[0].children).toHaveLength(3);
      expect(nodes[0].children[1]).toMatchObject({ className: ["hljs-subst"] });
    }
  });

  it("非 span 的标签会被剥掉、但内容保留（丢样式不丢内容）", () => {
    // lowlight 只会输出 span。万一输出被污染成 <script>，我们要的是
    // 「把标签丢了、文字留下」，而不是把它原样搬进 React 树。
    const nodes = toHighlightNodes({
      type: "element",
      tagName: "script",
      properties: {},
      children: [{ type: "text", value: "alert(1)" }],
    });
    expect(nodes).toEqual([{ type: "text", value: "alert(1)" }]);
  });

  it("乱七八糟的输入不抛错，只是产出空数组", () => {
    expect(toHighlightNodes(null)).toEqual([]);
    expect(toHighlightNodes(undefined)).toEqual([]);
    expect(toHighlightNodes("字符串")).toEqual([]);
    expect(toHighlightNodes(42)).toEqual([]);
    expect(toHighlightNodes({ type: "unknown" })).toEqual([]);
    expect(toHighlightNodes({ type: "text" })).toEqual([]); // 没有 value
    expect(toHighlightNodes({ type: "text", value: "" })).toEqual([]);
  });
});

describe("highlightCode", () => {
  it("真的高亮 Rust：关键字带上 hljs 类", () => {
    const nodes = highlightCode("fn main() { let x = 1; }", "rust");
    expect(nodes).not.toBeNull();
    const cls = classes(nodes!);
    expect(cls).toContain("hljs-keyword");
    // 文本本身必须完整保留（高亮只加样式，不丢字符）
    expect(textOnly(nodes!)).toBe("fn main() { let x = 1; }");
  });

  it("注释与字符串分别着色", () => {
    const nodes = highlightCode('let s = "hi"; // 注释', "typescript")!;
    const cls = classes(nodes);
    expect(cls).toContain("hljs-string");
    expect(cls).toContain("hljs-comment");
    expect(textOnly(nodes)).toBe('let s = "hi"; // 注释');
  });

  it("没写语言 → null（按纯文本渲染，不改任何观感）", () => {
    expect(highlightCode("fn main() {}", "")).toBeNull();
    expect(highlightCode("fn main() {}", null)).toBeNull();
  });

  it("语言不认识 → null，而不是抛异常", () => {
    // lowlight 对未知语言是**抛错**的（`Unknown language: ...`），
    // 我们不 catch 的话整个气泡就炸了。
    expect(() => highlightCode("x", "definitely-not-a-language")).not.toThrow();
    expect(highlightCode("x", "definitely-not-a-language")).toBeNull();
  });

  it("空代码 → null", () => {
    expect(highlightCode("", "rust")).toBeNull();
  });

  it("超长代码直接不高亮（避免一次渲染卡住界面）", () => {
    const huge = "fn main() {}\n".repeat(5000);
    expect(huge.length).toBeGreaterThan(MAX_HIGHLIGHT_CHARS);
    expect(highlightCode(huge, "rust")).toBeNull();
  });

  it("高亮后的文本与原代码逐字相同（含中文与 emoji）", () => {
    const code = 'const 名字 = "浮语 🌊"; // 注释也带中文\nconsole.log(名字);';
    const nodes = highlightCode(code, "javascript")!;
    expect(textOnly(nodes)).toBe(code);
  });

  it("坏代码（只开了引号）也不会抛错", () => {
    expect(() => highlightCode('const s = "never closed', "javascript")).not.toThrow();
  });
});

describe("supportedLanguages", () => {
  it("至少有 30 种，且包含常用的那些", () => {
    const langs = supportedLanguages();
    expect(langs.length).toBeGreaterThanOrEqual(30);
    for (const l of ["rust", "typescript", "python", "json", "bash", "sql", "yaml"]) {
      expect(langs, l).toContain(l);
    }
  });
});
