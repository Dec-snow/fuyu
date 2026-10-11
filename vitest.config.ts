import { defineConfig } from "vitest/config";

// 单独一份配置而不是往 vite.config.ts 里塞 `test`：vite 的 `defineConfig`
// 不认识 `test` 字段，混在一起要靠 `/// <reference types="vitest" />` 才能
// 通过类型检查，不如分开干净。
export default defineConfig({
  test: {
    // 被测试的都是纯逻辑（markdown 解析、事件去重、状态折叠），不碰 DOM，
    // 所以用 node 环境即可 —— 不需要 jsdom，测试也跑得更快。
    environment: "node",
    // `.tsx` 也要收：组件级的验证用 `react-dom/server` 的
    // `renderToStaticMarkup` 做**服务端渲染断言**（不需要 jsdom、
    // 也不需要 @testing-library）—— 见 `src/components/*.test.tsx`。
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
  },
});
