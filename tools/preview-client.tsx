// 把 Markdown 组件**在浏览器里真正挂载一遍**，用于验证异步/依赖 DOM 的渲染
// （mermaid 画图、KaTeX 布局）。SSR 那条路（`render-preview.tsx`）看不到这些，
// 因为 `useEffect` 在服务端渲染时不执行。
//
// 这个文件会被 `tools/vite.preview.config.ts` 打成一个自包含的 IIFE，
// 由一张极简 HTML 加载。挂载完成后它**自己把验证结果编码成像素**：
// 每项检查一个 24x24 的方块（绿=通过 / 红=失败），位置固定。
// 接着用无头浏览器截图、用 Python 读那几个像素 —— 不需要 OCR，也不用肉眼看。

import { createRoot } from "react-dom/client";

import { Markdown } from "../src/components/Markdown";
// 注意：KaTeX 的样式表是在 `src/main.tsx` 里引的，而这个入口不经过 main.tsx，
// 所以要**自己引一次** —— 否则验证页里公式没有字体，「KaTeX 字体生效」会红。
// （第一版就是这样：看着像产品问题，其实是验证页少了一行 import。）
import "katex/dist/katex.min.css";
import "../src/styles/globals.css";

const MERMAID_FLOW = [
  "```mermaid",
  "graph TD",
  "  A[用户提问] --> B{要不要工具}",
  "  B -->|要| C[执行工具]",
  "  B -->|不要| D[直接回答]",
  "  C --> D",
  "```",
].join("\n");

const MERMAID_SEQ = [
  "```mermaid",
  "sequenceDiagram",
  "  用户->>浮语: 你好",
  "  浮语-->>用户: 你好，有什么可以帮你？",
  "```",
].join("\n");

const MIXED = [
  "下面是一张流程图：",
  "",
  MERMAID_FLOW,
  "",
  "还有一段代码与一个公式：",
  "",
  "```rust",
  "fn main() {",
  '    println!("你好");',
  "}",
  "```",
  "",
  "行内公式 $E = mc^2$，以及块级公式：",
  "",
  "$$",
  "\\int_0^1 x^2 \\, dx = \\frac{1}{3}",
  "$$",
].join("\n");

function mount() {
  document.body.innerHTML = "";

  const light = document.createElement("div");
  light.className = "bubble";
  document.body.appendChild(light);

  const dark = document.createElement("div");
  dark.className = "bubble bubble-dark";
  dark.setAttribute("data-theme", "dark");
  document.body.appendChild(dark);

  createRoot(light).render(<Markdown text={MIXED} />);
  createRoot(dark).render(<Markdown text={MERMAID_SEQ} />);
}

/**
 * 等 mermaid 画完（它是异步的、还要按需加载），再画探针条。
 * 超时也要画 —— 超时本身就是「没画出来」的证据。
 */
function probeWhenReady(deadlineMs = 15000) {
  const started = Date.now();

  const tick = () => {
    const flowSvg = document.querySelectorAll(".md-mermaid svg");
    const stillLoading = document.querySelectorAll(".md-mermaid-loading");
    const darkSvg = document.querySelectorAll(
      "[data-theme='dark'] .md-mermaid svg, .bubble-dark .md-mermaid svg",
    );

    const done = flowSvg.length >= 2 && stillLoading.length === 0;
    if (!done && Date.now() - started < deadlineMs) {
      window.setTimeout(tick, 120);
      return;
    }
    paint({
      flowSvg: flowSvg.length >= 2,
      svgHasText: Array.from(flowSvg).some(
        (svg) => svg.querySelectorAll("text, .nodeLabel").length > 0,
      ),
      notStuckLoading: stillLoading.length === 0,
      darkRendered: darkSvg.length > 0,
      svgSafe:
        Array.from(flowSvg).every(
          (svg) =>
            !/<script/i.test(svg.outerHTML) &&
            !/\son[a-z]+\s*=/i.test(svg.outerHTML) &&
            !/javascript:/i.test(svg.outerHTML),
        ),
      twoDiagrams: flowSvg.length >= 2,
    });
  };

  tick();
}

/** 把结果画成绿/红方块（第一行 8 项是基础渲染，第二行是 mermaid）。 */
function paint(mermaid: Record<string, boolean>) {
  const checks: [string, boolean][] = [
    ["KaTeX 渲染", !!document.querySelector(".katex")],
    ["KaTeX 字体生效", (() => {
      const el = document.querySelector(".katex");
      return !!el && getComputedStyle(el).fontFamily.includes("KaTeX");
    })()],
    ["代码高亮 token", document.querySelectorAll(".hljs-keyword, .hljs-string").length > 0],
    ["块级公式 display", document.querySelectorAll(".katex-display").length > 0],
    ["公式没走降级", document.querySelectorAll(".md-math-fallback").length === 0],
    ["mermaid 渲染出 svg", mermaid.flowSvg],
    ["svg 里有图元文字", mermaid.svgHasText],
    ["没卡在加载态", mermaid.notStuckLoading],
    ["深色主题也画了", mermaid.darkRendered],
    ["svg 里没有脚本/事件", mermaid.svgSafe],
    ["两张图都画了", mermaid.twoDiagrams],
  ];

  const strip = document.createElement("div");
  strip.id = "probe-strip";
  strip.style.cssText =
    "position:fixed;left:0;top:0;z-index:99999;display:flex;flex-direction:row";
  for (const [, ok] of checks) {
    const cell = document.createElement("div");
    cell.style.cssText =
      "width:24px;height:24px;flex:0 0 24px;background:" + (ok ? "#00ff00" : "#ff0000");
    strip.appendChild(cell);
  }
  document.body.appendChild(strip);
  // 方便人工排查时直接从控制台看
  console.log("CHECKS:", JSON.stringify(checks));
}

mount();
probeWhenReady();
