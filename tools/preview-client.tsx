// 把 Markdown 组件**在浏览器里真正挂载一遍**，用于验证异步/依赖 DOM 的渲染
// （mermaid 画图、KaTeX 布局）。SSR 那条路（`render-preview.tsx`）看不到这些，
// 因为 `useEffect` 在服务端渲染时不执行。
//
// 这个文件会被 `tools/vite.preview.config.ts` 打成一个自包含的 IIFE，
// 由一张极简 HTML 加载。挂载完成后它**自己把验证结果编码成像素**：
// 每项检查一个 24x24 的方块（绿=通过 / 红=失败），位置固定。
// 接着用无头浏览器截图、用 Python 读那几个像素 —— 不需要 OCR，也不用肉眼看。

import { createRoot } from "react-dom/client";

import { ComparePanel } from "../src/components/ComparePanel";
import { Markdown } from "../src/components/Markdown";
import { useCompare } from "../src/stores/compare";
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

  mountCompare();
}

/**
 * 对比面板：往 store 里塞一份「跑完的」状态再挂载。
 *
 * 这样能验证的是**渲染**（N 列、模型名、按钮、失败态、流式态），
 * 不是网络 —— 面板的并发发送由 store 的单测覆盖。
 */
function mountCompare() {
  // 面板的遮罩是 `position: fixed`（真机上要盖住整个窗口）。在这个验证页里
  // 那会把上面两个用来查**高亮配色**的样例一起压暗，于是「keyword #7c3aed」
  // 之类的精确配色检查全部落空 —— 那是验证页的问题，不是产品的问题。
  // 所以在挂载点范围内把遮罩改回静态定位。
  const scope = document.createElement("style");
  scope.textContent =
    "#compare-host .compare-backdrop{position:static;background:none;padding:0;}" +
    "#compare-host .compare-panel{box-shadow:none;}";
  document.head.appendChild(scope);

  const host = document.createElement("div");
  host.id = "compare-host";
  document.body.appendChild(host);

  useCompare.setState({
    open: true,
    question: "用一句话解释什么是闭包",
    targets: [
      { providerId: "a", modelKey: "模型甲" },
      { providerId: "b", modelKey: "模型乙" },
      { providerId: "c", modelKey: "模型丙" },
      { providerId: "d", modelKey: "模型丁" },
    ],
    results: [
      {
        providerId: "a",
        modelKey: "模型甲",
        requestId: "r-a",
        label: "服务商甲 · 模型甲",
        content: "闭包是**带着环境**的函数。",
        reasoning: "",
        streaming: false,
        cancelled: false,
        error: null,
        inputTokens: 10,
        outputTokens: 20,
        adopted: false,
      },
      {
        providerId: "b",
        modelKey: "模型乙",
        requestId: "r-b",
        label: "服务商乙 · 模型乙",
        content: "",
        reasoning: "",
        streaming: true,
        cancelled: false,
        error: null,
        inputTokens: 0,
        outputTokens: 0,
        adopted: false,
      },
      {
        providerId: "c",
        modelKey: "模型丙",
        requestId: "r-c",
        label: "服务商丙 · 模型丙",
        content: "闭包就是一个能记住出生环境的函数。",
        reasoning: "",
        streaming: false,
        cancelled: false,
        error: null,
        inputTokens: 5,
        outputTokens: 8,
        adopted: true,
      },
      {
        providerId: "d",
        modelKey: "模型丁",
        requestId: "r-d",
        label: "服务商丁 · 模型丁",
        content: "",
        reasoning: "",
        streaming: false,
        cancelled: false,
        error: "[TIMEOUT] 请求超时",
        inputTokens: 0,
        outputTokens: 0,
        adopted: false,
      },
    ],
    notice: null,
  } as never);

  createRoot(host).render(<ComparePanel />);
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

/** 把结果画成绿/红方块（第一行 8 项是基础渲染，第二行是 mermaid，第三行是对比面板）。 */
function paint(mermaid: Record<string, boolean>) {
  const compareCols = document.querySelectorAll(".compare-col");
  const compareErrors = document.querySelectorAll(".compare-error");
  const compareStates = Array.from(
    document.querySelectorAll(".compare-col-state"),
  ).map((el) => el.textContent ?? "");
  const compareButtons = Array.from(
    document.querySelectorAll(".compare-col-foot button"),
  ).map((el) => el.textContent ?? "");

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
    // ---- 对比面板
    ["对比面板渲染出 4 列", compareCols.length === 4],
    ["三列有内容、一列报错", compareErrors.length === 1 && compareCols.length === 4],
    ["流式列显示生成中", compareStates.some((s) => s.includes("生成中"))],
    ["采用过的显示已采用", compareStates.some((s) => s.includes("已采用"))],
    ["每列都有「采用这条」", compareButtons.filter((t) => t.includes("采用")).length === 4],
    ["失败那列没有内容但有错误", compareErrors.length === 1],
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
