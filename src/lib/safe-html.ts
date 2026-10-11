// 「这段 HTML 能不能安全注入」的共用判据。
//
// 前端有**两处**不得不注入 HTML 的地方：
// - KaTeX（`lib/math.ts`）：公式没有节点树输出；
// - mermaid（`lib/mermaid.ts`）：图表输出的是 SVG 字符串。
//
// 两处的兜底检查必须是同一套规则，否则迟早出现「这边挡了、那边没挡」。
// 所以抽到这里共用，并且**每一条判据都有测试**。
//
// ⚠️ 一个踩过的坑：**不要搜 `javascript:` 这种子串**。
// KaTeX 在 `trust:false` 下会把 `\href{javascript:...}` 渲染成**红色的错误文本**，
// 那个子串会以转义文本的形式无害地出现；按子串判会让用户写
// `$\text{javascript: 是协议}$` 时公式莫名退回源码。
// 危险协议只有在 `href=` / `src=` 的**值**里才危险。

const DANGEROUS = [
  // 这些标签一旦出现，就说明不是 KaTeX / mermaid 的正常输出
  /<\s*(script|iframe|object|embed|link|meta|base|form)\b/i,
  // 事件处理器属性：onclick= / onerror= / onload= ...
  /\son[a-z]+\s*=/i,
  // 危险协议，但**必须**出现在 href/src 的值里才算
  /(?:href|src|xlink:href)\s*=\s*["']?\s*(?:javascript|vbscript|data:text\/html)/i,
];

/** 这段渲染结果能不能安全注入？ */
export function looksSafe(html: string): boolean {
  return !DANGEROUS.some((pattern) => pattern.test(html));
}
