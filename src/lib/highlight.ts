// 代码高亮：把源码变成**可渲染的树**，而不是一段 HTML。
//
// 这是这个模块最重要的设计约束。`components/Markdown.tsx` 开头就写着
// 「全程不碰 `dangerouslySetInnerHTML`：模型输出是不可信输入」——
// 而绝大多数高亮方案（highlight.js 的 `hljs.highlight`、Prism 的
// `highlight`、shiki 的 `codeToHtml`）都是**输出 HTML 字符串**，那就必须
// 注入。这里改用 `lowlight`：它是 highlight.js 的包装，输出一棵 hast 语法树
// （`{type:"element", tagName:"span", properties:{className:[...]}}`），
// 于是我们可以把它映射成 React 元素 —— 高亮有了，注入面没有。
//
// 另外两条：
// - **未知语言会抛异常**（`Unknown language: ...`），必须先 `registered()` 判断；
// - 不做 `highlightAuto` 自动探测：中文正文里猜语言经常猜错，
//   而「猜错的配色」比「没有配色」更难看懂。

import { common, createLowlight } from "lowlight";

/** 我们自己的最小节点类型（不依赖 `hast` 的类型包）。 */
export type HighlightNode =
  | { type: "text"; value: string }
  | { type: "element"; tagName: string; className: string[]; children: HighlightNode[] };

const lowlight = createLowlight(common);

/**
 * 语言别名归一化。
 *
 * lowlight 自带的 37 种语言已经注册了不少别名（`rs` / `ts` / `py` / `sh` /
 * `yml` / `md` / `cs` …），这里只补它没有的那些 —— 主要是**网页与脚本类**，
 * 以及我们自己的文本附件白名单里会出现、但 highlight.js 不认识的后缀。
 */
const ALIASES: Record<string, string> = {
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  htm: "xml",
  vue: "xml",
  svelte: "xml",
  svg: "xml",
  h: "c",
  hpp: "cpp",
  cc: "cpp",
  cxx: "cpp",
  // highlight.js 的 common 里没有 powershell / batch：退回纯文本，
  // 而不是塞给一个相近的语言去「猜」（猜错会给出误导性的配色）。
  jsonl: "json",
  yamllint: "yaml",
};

/** 归一化后的语言名；无法识别返回 `null`（调用方按纯文本渲染）。 */
export function normalizeLanguage(language: string | null | undefined): string | null {
  const raw = (language ?? "").trim().toLowerCase();
  if (raw === "") return null;
  // 有些模型会写 `c++`、`objective-c`、`tsx (react)` 这类东西，只取前面的标识部分
  const head = raw.split(/[\s(（]/)[0];
  const candidate = ALIASES[head] ?? head;
  return lowlight.registered(candidate) ? candidate : null;
}

/** 支持的语言（归一化后的名字）。诊断 / 调试用。 */
export function supportedLanguages(): string[] {
  return lowlight.listLanguages().sort();
}

/**
 * 把 lowlight 的 hast 树收成我们自己的最小结构。
 *
 * 为什么要转一层：组件不该依赖 lowlight 的类型与输出细节（它换版本我们就得跟着改），
 * 而且转换本身是纯函数、可以拿手写的输入单测 —— 不需要真的跑高亮。
 *
 * 未知节点一律**丢弃**：这个函数的返回值最终会变成 React 元素，
 * 任何我们没预期的节点都不该混进去。
 */
export function toHighlightNodes(input: unknown): HighlightNode[] {
  const nodes: HighlightNode[] = [];

  const walk = (raw: unknown): void => {
    if (raw === null || typeof raw !== "object") return;
    const node = raw as {
      type?: string;
      value?: unknown;
      tagName?: unknown;
      properties?: { className?: unknown };
      children?: unknown;
    };

    if (node.type === "text") {
      if (typeof node.value === "string" && node.value !== "") {
        nodes.push({ type: "text", value: node.value });
      }
      return;
    }

    if (node.type === "element") {
      // 只认 span：lowlight 只会输出 span，其它标签说明输入被污染了，
      // 直接降级成它的纯文本子节点（丢样式而不是丢内容）。
      const tagName = typeof node.tagName === "string" ? node.tagName : "span";
      const rawClass = node.properties?.className;
      const className = Array.isArray(rawClass)
        ? rawClass.filter((c): c is string => typeof c === "string")
        : [];

      const children: HighlightNode[] = [];
      const before = nodes.length;
      if (Array.isArray(node.children)) {
        for (const child of node.children) walk(child);
      }
      // 把刚收集到的子节点取出来
      children.push(...nodes.splice(before));

      if (tagName !== "span") {
        nodes.push(...children);
        return;
      }
      nodes.push({ type: "element", tagName, className, children });
      return;
    }

    // root 或其它容器：只递归子节点
    if (Array.isArray(node.children)) {
      for (const child of node.children) walk(child);
    }
  };

  walk(input);
  return nodes;
}

/** 单个文本节点数上限：超过这个长度就不高亮了。 */
export const MAX_HIGHLIGHT_CHARS = 40_000;

/**
 * 高亮一段代码。
 *
 * 返回 `null` 表示「按纯文本渲染」，可能的原因：没写语言、语言不认识、
 * 代码太长、或者高亮库自己抛错。**任何情况下都不抛异常** ——
 * 渲染一个消息不该因为配色失败而整条炸掉。
 */
export function highlightCode(
  code: string,
  language: string | null | undefined,
): HighlightNode[] | null {
  if (code.length === 0 || code.length > MAX_HIGHLIGHT_CHARS) return null;
  const lang = normalizeLanguage(language);
  if (lang === null) return null;

  try {
    const nodes = toHighlightNodes(lowlight.highlight(lang, code));
    return nodes.length > 0 ? nodes : null;
  } catch {
    // 高亮是锦上添花：任何失败都退回纯文本，绝不把气泡弄成错误。
    return null;
  }
}
