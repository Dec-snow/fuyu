// 把 Markdown 组件的渲染结果导出成一个可截图的 HTML 页面。
//
// 为什么这样做视觉验证，而不是去点应用界面：
// - 输入完全可控（我要测的就是「代码高亮 + 公式」这几条渲染路径）；
// - 用的是**真实的组件代码**（`src/components/Markdown.tsx`），不是复制一份；
// - 样式直接引用 `npm run build` 产出的 CSS，与线上完全一致。
//
// 跑法：npx vite-node tools/render-preview.mts  （在 dist/ 下产出 preview.html）

import { readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { renderToStaticMarkup } from "react-dom/server";

import { Markdown } from "../src/components/Markdown";

const SAMPLE = [
  "先看一段 **Rust** 代码，再算一下质能方程 $E = mc^2$。",
  "",
  "```rust",
  "// 计算平方（带上关键字 / 字符串 / 数字 / 注释四种 token）",
  "fn square(x: i64) -> i64 {",
  '    let name = "浮语";',
  "    let scale = 42;",
  "    x * x * scale",
  "}",
  "```",
  "",
  "行内公式 $a^2 + b^2 = c^2$ 与块级公式：",
  "",
  "$$",
  "\\int_0^1 x^2 \\, dx = \\frac{1}{3}",
  "$$",
  "",
  "价格是 $5 和 $10 —— 这两个 $ 不该被当成公式。",
].join("\n");

const dist = join(process.cwd(), "dist");
const assets = join(dist, "assets");
const cssFiles = readdirSync(assets).filter((f) => f.endsWith(".css"));
if (cssFiles.length === 0) {
  throw new Error("dist/assets 里没有 CSS —— 先跑 npm run build");
}

const html = `<!doctype html>
<html lang="zh-CN" data-theme="light">
<head>
<meta charset="utf-8">
${cssFiles.map((f) => `<link rel="stylesheet" href="./assets/${f}">`).join("\n")}
<style>
  /* 复刻聊天气泡的最小环境：白底、固定宽度，方便截图对比 */
  body { margin: 0; padding: 14px; background: #f4f5f8; }
  .bubble {
    width: 560px;
    padding: 10px 14px;
    border-radius: 12px;
    background: var(--bg-elevated, #fff);
    color: var(--fg);
    font-size: var(--msg-font-size, 13.5px);
    line-height: 1.6;
  }
</style>
</head>
<body>
  <div class="bubble">${renderToStaticMarkup(<Markdown text={SAMPLE} />)}</div>
  <div class="bubble" data-theme="dark" style="margin-top:12px; background:#20232d">
    <div data-theme="dark">${renderToStaticMarkup(<Markdown text={"```typescript\nconst greeting: string = \"你好\";\n```\n\n$\\sqrt{x^2+y^2}$" } />)}</div>
  </div>
  <div id="probe-strip" style="position:fixed;left:0;top:0;display:flex;z-index:9999"></div>
  <script>
    // 这个环境里 msedge 的 --dump-dom 拿不到输出（新版无头模式不支持），
    // 所以改成一个不依赖 stdout 的办法：**把验证结果编码成像素**。
    // 每个检查项画一个 24x24 的方块，绿 = 通过、红 = 失败，位置固定。
    // 截图之后用 Python 读那几个像素就知道每一项过没过 —— 不需要 OCR，也不需要人看。
    window.addEventListener("load", () => {
      const katex = document.querySelector(".katex");
      const checks = [
        // 0: KaTeX 真的渲染出结构
        document.querySelectorAll(".katex").length > 0,
        // 1: 块级公式（$$…$$）渲染成 display 模式
        document.querySelectorAll(".katex-display").length > 0,
        // 2: KaTeX 的样式/字体真的生效了（CSS 被加载）
        !!katex && getComputedStyle(katex).fontFamily.includes("KaTeX"),
        // 3: 代码高亮产生了 token
        document.querySelectorAll(".hljs-keyword, .hljs-string").length > 0,
        // 4: 带语言的代码块真的走了高亮分支
        document.querySelectorAll("code.hljs").length > 0,
        // 5: 没有任何公式走到降级分支（说明渲染都成功了）
        document.querySelectorAll(".md-math-fallback").length === 0,
        // 6: 价格那句仍是普通文本（$ 没被误判成公式）
        document.body.innerText.includes("价格是 $5 和 $10"),
        // 7: 深色气泡里也有高亮（主题变量两套都要生效）
        document
          .querySelectorAll("[data-theme='dark'] code.hljs")
          .length > 0,
      ];
      const strip = document.getElementById("probe-strip");
      for (const ok of checks) {
        const cell = document.createElement("div");
        cell.style.cssText =
          "width:24px;height:24px;background:" + (ok ? "#00ff00" : "#ff0000");
        strip.appendChild(cell);
      }
    });
  </script>
</body>
</html>`;

writeFileSync(join(dist, "preview.html"), html, "utf8");
console.log("已写出 dist/preview.html（CSS:", cssFiles.join(", "), "）");
