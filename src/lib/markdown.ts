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
  | { kind: "math"; content: string }
  | { kind: "text"; content: string };

export type InlineToken =
  | { kind: "code"; value: string }
  | { kind: "bold"; value: string }
  | { kind: "math"; value: string }
  | { kind: "text"; value: string };

const FENCE_OPEN = /^\s*```\s*([\w+#-]*)\s*$/;
const FENCE_CLOSE = /^\s*```\s*$/;
/** 一整行就是一 Formula：`$$ ... $$` */
const MATH_ONE_LINE = /^\s*\$\$\s*(.+?)\s*\$\$\s*$/;
/** `$$` 单独一行：后面若干行都是公式，直到再遇到一个 `$$`。 */
const MATH_FENCE = /^\s*\$\$\s*$/;

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
  let inMath = false;
  let language = "";
  let codeLines: string[] = [];
  let mathLines: string[] = [];

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
    if (inCode) {
      if (FENCE_CLOSE.test(line)) {
        blocks.push({ kind: "code", language, content: codeLines.join("\n") });
        inCode = false;
        language = "";
        codeLines = [];
        continue;
      }
      codeLines.push(line);
      continue;
    }

    if (inMath) {
      if (MATH_FENCE.test(line)) {
        blocks.push({ kind: "math", content: mathLines.join("\n") });
        inMath = false;
        mathLines = [];
        continue;
      }
      mathLines.push(line);
      continue;
    }

    // 整行 `$$...$$`：块级公式，不进入「多行公式」状态
    const oneLine = line.match(MATH_ONE_LINE);
    if (oneLine) {
      flushText();
      blocks.push({ kind: "math", content: oneLine[1] });
      continue;
    }

    if (MATH_FENCE.test(line)) {
      flushText();
      inMath = true;
      mathLines = [];
      continue;
    }

    const open = line.match(FENCE_OPEN);
    if (open) {
      flushText();
      inCode = true;
      language = open[1] ?? "";
      codeLines = [];
      continue;
    }
    textBuffer.push(line);
  }

  if (inCode) {
    // 未闭合：仍然当作代码块（正在流式生成中）。
    blocks.push({ kind: "code", language, content: codeLines.join("\n") });
  }
  if (inMath) {
    // 同样：流式输出时 `$$` 往往还没闭合。
    blocks.push({ kind: "math", content: mathLines.join("\n") });
  }
  flushText();

  return blocks;
}

/**
 * 行内标记：反引号代码、`**粗体**`、以及行内公式 `$...$`。
 *
 * 行内公式这条最容易误判，判据有四条（缺一条就会把价格当公式）：
 * - 开头的 `$` **前面不能是数字或反斜杠**（否则 `价格 5$ 和 6$` 会被配对）；
 * - 开头的 `$` **后面不能是空白或数字** —— 这一条挡住 `$5 和 $10`；
 * - 结尾的 `$` **前面不能是空白**（`$ x $` 不当公式）；
 * - 结尾的 `$` **后面不能是数字**（`$x$2` 不当公式）。
 *
 * 反斜杠转义（`\$`）也自然被排除：`\\` 在 `$` 前面就不匹配。
 */
const INLINE_PATTERN =
  /(`[^`\n]+`|\*\*[^*\n]+\*\*|(?<![\d\\$])\$(?![\s\d$])(?:[^$\n]|\\.)+?(?<![\s\\])\$(?!\d))/g;

// 分类用的**锚定**判据。与上面那条 `INLINE_PATTERN` 的对应分支保持一致，
// 见 `splitInline` 里的注释：不一致就会出现「正则拒了、分类又收回来」。
const CODE_PART = /^`[^`\n]+`$/;
const BOLD_PART = /^\*\*[^*\n]+\*\*$/;
const MATH_PART = /^\$(?![\s\d$])(?:[^$\n]|\\.)+?(?<![\s\\])\$$/;

/** 把一行文本切成「普通文字 / 行内代码 / 粗体 / 行内公式」几种片段。 */
export function splitInline(text: string): InlineToken[] {
  const tokens: InlineToken[] = [];

  for (const part of text.split(INLINE_PATTERN)) {
    if (part === "") continue;

    // 注意：这里**必须**用锚定的、逐分支的正则来判断片段种类，
    // 不能用 `startsWith("$") && endsWith("$")` 这种写法。
    //
    // 踩过的坑：`$ x $`（两端是空格）被上面那条正则**正确地**拒绝了，
    // 于是整串作为一个 text 片段返回；但分类代码看见它「以 $ 开头、以 $ 结尾」
    // 又把它判成了公式 —— 拒绝白拒了，用户还是会看到 `$ x $` 被排版成公式。
    // 用同一套判据（锚定版）分类，才能保证「不合法的就是不合法的」。
    if (CODE_PART.test(part)) {
      tokens.push({ kind: "code", value: part.slice(1, -1) });
      continue;
    }
    if (BOLD_PART.test(part)) {
      tokens.push({ kind: "bold", value: part.slice(2, -2) });
      continue;
    }
    if (MATH_PART.test(part)) {
      tokens.push({ kind: "math", value: part.slice(1, -1) });
      continue;
    }
    tokens.push({ kind: "text", value: part });
  }

  return tokens;
}
