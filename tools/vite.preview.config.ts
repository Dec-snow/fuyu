// 只为验证脚本服务的构建配置：把 tools/preview-client.tsx 打成一个自包含的 IIFE。
//
// 为什么要单独一份配置：正常构建产物（`dist/assets/*.js`）是整个应用，
// 依赖 Tauri 的 IPC 环境，直接在浏览器里打开会白屏。而这里需要的是
// 「组件 + React + mermaid，能独立在浏览器里跑起来」。

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  // 相对路径：产物用 file:// 打开也能找到资源
  base: "./",
  build: {
    outDir: "dist/preview",
    emptyOutDir: true,
    // 验证页面不需要按需分块：合成一个文件最省事，也避免 file:// 下的路径问题
    cssCodeSplit: false,
    rollupOptions: {
      input: "tools/preview-client.tsx",
      output: {
        format: "iife",
        entryFileNames: "preview-client.js",
        assetFileNames: "preview-client.[ext]",
        // IIFE 不能有多个 chunk
        inlineDynamicImports: true,
      },
    },
    // 压缩关掉：验证脚本出错时堆栈可读，产物也方便对照
    minify: false,
  },
});
