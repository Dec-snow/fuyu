// mermaid 图表。
//
// 这是第二处、也是最后一处注入 HTML 的地方：mermaid 输出的是 **SVG 字符串**
// （没有节点树 API，图表布局必须由它自己算）。安全上做了两层：
//
// 1. `securityLevel: "strict"`（显式写出并单测钉死）—— mermaid 会用它自带的
//    DOMPurify 清洗生成的 SVG，禁用点击交互与脚本；
// 2. 外面再过一遍共用的 `looksSafe()`（`lib/safe-html.ts`）。
//
// 另外两个刻意的设计：
//
// - **按需加载**：mermaid 压缩后仍有数百 KB，静态引入会让每次启动都白等。
//   所以只有消息里真的出现 ```mermaid 代码块时才 `import("mermaid")`。
// - **降级可见**：加载失败 / 语法错误 / 兜底检查不过，都显示**源码**而不是空白 ——
//   用户至少能看到那段 mermaid 文本，从而知道这里原本有张图。

import { looksSafe } from "./safe-html";

/** mermaid 的配置。导出是为了能被单测断言（securityLevel 是安全相关的）。 */
export const MERMAID_CONFIG = {
  startOnLoad: false,
  securityLevel: "strict" as const,
  // 关掉错误图：报错时我们自己显示源码，比 mermaid 自带的红叉图更清楚
  suppressErrorRendering: true,
  fontFamily: "inherit",
} as const;

/** 支持 mermaid 的语言标注写法。 */
const MERMAID_ALIASES = ["mermaid", "mmd"];

/** 这个代码块是不是一张 mermaid 图？ */
export function isMermaidBlock(language: string | null | undefined): boolean {
  const lang = (language ?? "").trim().toLowerCase();
  return MERMAID_ALIASES.includes(lang);
}

/** 单张图的源码长度上限（防止一条消息里塞一个巨型图把界面卡住）。 */
export const MAX_MERMAID_CHARS = 20_000;

/**
 * mermaid 要求每个图一个唯一 id（它会用这个 id 建临时 DOM）。
 * 用自增计数而不是随机数：随机数在测试里不好断言，自增也足够唯一。
 */
let counter = 0;
export function nextMermaidId(): string {
  counter += 1;
  return `mmd-${counter}`;
}

type MermaidApi = {
  initialize: (config: unknown) => void;
  render: (id: string, code: string) => Promise<{ svg: string }>;
};

let loading: Promise<MermaidApi> | null = null;
let lastTheme = "";

/**
 * 加载并初始化 mermaid（只做一次）。
 *
 * `theme` 变化时必须重新 `initialize` —— mermaid 把主题缓存在初始化参数里，
 * 不重新初始化的话，切到深色主题后**新画的图仍然是浅色**。
 */
async function loadMermaid(theme: "default" | "dark"): Promise<MermaidApi> {
  if (!loading) {
    loading = import("mermaid").then((mod) => {
      const api = (mod.default ?? mod) as unknown as MermaidApi;
      return api;
    });
  }
  const api = await loading;
  if (lastTheme !== theme) {
    api.initialize({ ...MERMAID_CONFIG, theme });
    lastTheme = theme;
  }
  return api;
}

export interface MermaidResult {
  svg: string;
}

/**
 * 渲染一张图。
 *
 * 返回 `null` 表示「按源码显示」，原因可能是：源码太长、加载失败、
 * 语法错误、或者兜底检查不过。**任何情况下都不抛异常**。
 */
export async function renderMermaid(
  code: string,
  theme: "default" | "dark",
): Promise<MermaidResult | null> {
  const source = code.trim();
  if (source === "" || source.length > MAX_MERMAID_CHARS) return null;

  try {
    const api = await loadMermaid(theme);
    const { svg } = await api.render(nextMermaidId(), source);
    if (typeof svg !== "string" || svg.trim() === "") return null;
    // mermaid 自己也清洗过（securityLevel: strict），这里再兜一层。
    if (!looksSafe(svg)) return null;
    return { svg };
  } catch {
    // 语法错误、jsdom 里跑不起来、动态 import 失败……全都退化成源码显示。
    return null;
  }
}

/** 测试用：清掉「已加载」的缓存（vitest 里每个文件是独立模块图，一般用不到）。 */
export function resetMermaidCacheForTests(): void {
  loading = null;
  lastTheme = "";
}
