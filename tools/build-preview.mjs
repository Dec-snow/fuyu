// 构建「客户端渲染验证页」：把组件真在浏览器里挂载一遍（mermaid 这类异步、
// 依赖 DOM 的渲染只有这样才能验证），并生成加载它的最小 HTML。
//
// 跑法：node tools/build-preview.mjs
// 产物：dist/preview/preview-client.js/.css + dist/preview-mermaid.html

import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const root = process.cwd();

console.log("1) 打包客户端预览入口…");
// 直接用 node 跑 vite 的入口，而不是 `npx`：
// Windows 上 execFileSync 调 .cmd 需要 shell，而在 node 里绕 shell 又容易出别的毛病。
const viteBin = join(root, "node_modules", "vite", "bin", "vite.js");
execFileSync(process.execPath, [viteBin, "build", "--config", "tools/vite.preview.config.ts"], {
  cwd: root,
  stdio: "inherit",
});

const outDir = join(root, "dist", "preview");
if (!existsSync(outDir)) throw new Error("没有产出 dist/preview");

const files = readdirSync(outDir);
const js = files.find((f) => f.endsWith(".js"));
const css = files.find((f) => f.endsWith(".css"));
if (!js) throw new Error(`dist/preview 里没有 js：${files.join(", ")}`);

console.log("2) 生成 dist/preview-mermaid.html…");
const html = `<!doctype html>
<html lang="zh-CN" data-theme="light">
<head>
<meta charset="utf-8">
${css ? `<link rel="stylesheet" href="./preview/${css}">` : ""}
<style>
  /* 复刻聊天气泡的最小环境（这两条不属于应用样式，只服务于验证页） */
  body { margin: 0; padding: 14px; background: #f4f5f8; }
  .bubble {
    width: 560px;
    margin-bottom: 12px;
    padding: 10px 14px;
    border-radius: 12px;
    background: var(--bg-elevated, #fff);
    color: var(--fg);
    font-size: var(--msg-font-size, 13.5px);
    line-height: 1.6;
  }
  .bubble-dark { background: #20232d; }
</style>
</head>
<body>
<script src="./preview/${js}"></script>
</body>
</html>`;

writeFileSync(join(root, "dist", "preview-mermaid.html"), html, "utf8");
console.log(`   已写出（js=${js}${css ? `, css=${css}` : ""}）`);
