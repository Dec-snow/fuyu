// Markdown 的**纯解析逻辑**。
//
// 从 `components/Markdown.tsx` 里抽出来是为了能单测：组件文件一旦 import React，
// 在 node 测试环境里就要额外配 jsdom + 转译 JSX，成本远大于把它拆成一个
// 不依赖任何框架的函数。
//
// 解析结果只描述「有哪些块 / 行内片段是什么」，**不含任何 HTML**。
// 组件只把它映射成 React 元素，全程不碰 `dangerouslySetInnerHTML` ——
// 模型输出是不可信输入。

export type MarkdownBlock =
  | { kind: "code"; language: string; content: string }
  | { kind: "text"; content: string };

export type InlineToken =
  | { kind: "code"; value: string }
  | { kind: "bold"; value: string }
  | { kind: "text"; value: string };

const FENCE_OPEN = /^\s*```\s*([\w+#-]*)\s*$/;
const FENCE_CLOSE = /^\s*```\s*$/;

/**
 * 把 Markdown 源码切成「代码块」和「文本块」。
 *
 * 流式输出时围栏常常还没闭合（模型正写到一半），此时剩余内容**仍然**按代码块
 * 渲染 —— 否则代码会在渲染中途从「带背景的块」跳变成「正文段落」，视觉上闪一下。
 */
export function splitMarkdownBlocks(source: string): MarkdownBlock[] {
  const blocks: MarkdownBlock[] = [];
  const lines = source.split("\n");

  let textBuffer: string[] = [];
  let inCode = false;
  let language = "";
  let codeLines: string[] = [];

  const flushText = () => {
    if (textBuffer.length === 0) return;
    const content = textBuffer.join("\n");
    textBuffer = [];
    // 纯空白的文本块不产出。
    //
    // `"".split("\n")` 会得到 `[""]`，照单全收的话空字符串也会渲染出一个
    // `<p></p>` —— 在气泡里就是一行莫名其妙的空隙。代码块前后的空行同理。
    if (content.trim() === "") return;
    blocks.push({ kind: "text", content });
  };

  for (const line of lines) {
    if (!inCode) {
      const open = line.match(FENCE_OPEN);
      if (open) {
        flushText();
        inCode = true;
        language = open[1] ?? "";
        codeLines = [];
        continue;
      }
      textBuffer.push(line);
      continue;
    }

    if (FENCE_CLOSE.test(line)) {
      blocks.push({ kind: "code", language, content: codeLines.join("\n") });
      inCode = false;
      language = "";
      codeLines = [];
      continue;
    }
    codeLines.push(line);
  }

  if (inCode) {
    // 未闭合：仍然当作代码块（正在流式生成中）。
    blocks.push({ kind: "code", language, content: codeLines.join("\n") });
  }
  flushText();

  return blocks;
}

/** 行内标记：反引号包裹的代码，或 `**粗体**`。 */
const INLINE_PATTERN = /(`[^`\n]+`|\*\*[^*\n]+\*\*)/g;

/** 把一行文本切成「普通文字 / 行内代码 / 粗体」三种片段。 */
export function splitInline(text: string): InlineToken[] {
  const tokens: InlineToken[] = [];

  for (const part of text.split(INLINE_PATTERN)) {
    if (part === "") continue;

    if (part.length > 2 && part.startsWith("`") && part.endsWith("`")) {
      tokens.push({ kind: "code", value: part.slice(1, -1) });
      continue;
    }
    if (part.length > 4 && part.startsWith("**") && part.endsWith("**")) {
      tokens.push({ kind: "bold", value: part.slice(2, -2) });
      continue;
    }
    tokens.push({ kind: "text", value: part });
  }

  return tokens;
}
