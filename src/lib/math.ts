// 数学公式（KaTeX）。
//
// ⚠️ 这是整个前端**唯一**会用到 `dangerouslySetInnerHTML` 的地方，
// 而 `components/Markdown.tsx` 原来的原则是「全程不碰它，因为模型输出不可信」。
// 所以这里必须把「为什么这次可以」写清楚，而不是悄悄破例：
//
// 1. KaTeX **只有** HTML 输出（没有节点树 API），公式排版无法用 React 元素表达；
// 2. 输出不是「模型给的 HTML」，而是**KaTeX 自己生成的**：文本内容会被转义，
//    结构由渲染器决定。模型能影响的只有公式源码；
// 3. `trust: false`（默认值，这里显式写出来并单测）会**拒绝** `\href`、`\url`、
//    `\includegraphics`、`\htmlClass`/`\htmlId`/`\htmlStyle`/`\htmlData` ——
//    也就是 KaTeX 里所有能注入链接、图片、自定义属性的能力；
// 4. 外面还有一层 `looksSafe()` 兜底：即便 KaTeX 出了漏洞（或将来换了实现），
//    输出里只要出现脚本/事件处理器/危险协议就**退回纯文本**。
//
// 结论：注入面被限制在「KaTeX 已知的 span+style 输出」内，且守不住时降级显示
// 公式源码（用户看得见 `$x^2$` 本身，而不是一片空白）。

import katex from "katex";

/**
 * KaTeX 渲染参数。
 *
 * 导出成常量是为了**能被单测断言** —— 这几项全是安全相关的，
 * 谁哪天顺手打开 `trust` 就该有测试红。
 */
export const KATEX_OPTIONS = {
  /** 拒绝 \href / \url / \includegraphics / \html* 系列（安全关键）。 */
  trust: false,
  /** 公式有语法错误时**照常渲染**（KaTeX 会把错误标红显示），不抛异常。 */
  throwOnError: false,
  /** 同时输出 HTML 与 MathML：数学阅读器/复制公式时更友好。 */
  output: "htmlAndMathml",
  /**
   * 不用 strict 模式限制（那会把一些常见写法变成警告）。
   * 它和安全无关 —— 安全靠 `trust: false`。
   */
  strict: false as const,
} as const;

/** 单条公式的长度上限：防止有人用超长公式让渲染卡死。 */
export const MAX_MATH_CHARS = 5_000;

/**
 * 渲染一条公式；失败返回 `null`（调用方退回显示源码）。
 *
 * `displayMode` 为真时按块级公式排版（居中、独立一行）。
 */
export function renderMath(tex: string, displayMode: boolean): string | null {
  const source = tex.trim();
  if (source === "" || source.length > MAX_MATH_CHARS) return null;

  try {
    return katex.renderToString(source, { ...KATEX_OPTIONS, displayMode });
  } catch {
    // `throwOnError: false` 之后基本到不了这里，但渲染失败不该让整条消息炸掉。
    return null;
  }
}

/**
 * 危险内容的特征。用于「守住最后一道」的兜底检查。
 *
 * ⚠️ 这里踩过一个坑，值得写下来：**不能直接搜 `javascript:` 这个子串**。
 * KaTeX 在 `trust: false` 下会把 `\href{javascript:...}` 渲染成**红色的错误文本**，
 * 也就是 `javascript:` 会以**转义后的文本**出现在输出里；直接搜子串会把这种
 * 完全无害的情况判成危险，于是用户写 `$\text{javascript: 是一种协议}$` 时
 * 公式会莫名其妙退回源码。
 *
 * 正确的判据是**属性上下文**：协议只有出现在 `href=` / `src=` 的**值**里才危险。
 * 同理 `<a` 也不能直接搜 —— KaTeX 的 MathML 输出里有一个 `<annotation>` 标签。
 */
const DANGEROUS = [
  // 这些标签一旦出现就说明不是 KaTeX 的正常输出
  /<\s*(script|iframe|object|embed|link|meta|base|form)\b/i,
  // 事件处理器属性：onclick= / onerror= / onload= ...
  /\son[a-z]+\s*=/i,
  // 危险协议，但**必须**出现在 href/src 的值里才算
  /(?:href|src|xlink:href)\s*=\s*["']?\s*(?:javascript|vbscript|data:text\/html)/i,
];

/**
 * 这段渲染结果能不能安全注入？
 *
 * 它是**兜底**，不是主要防线：主要防线是 `trust: false`。
 * 之所以还要有它：注入点是这个项目里风险最高的一行代码，
 * 多一道可以在测试里钉死的检查，成本极低。
 */
export function looksSafe(html: string): boolean {
  return !DANGEROUS.some((pattern) => pattern.test(html));
}

/** 公式源码要不要按「块级」渲染（`$$...$$`）。 */
export function isDisplayMath(tex: string): boolean {
  return tex.includes("\n") || tex.length > 60;
}
