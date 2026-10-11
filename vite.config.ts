import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 期望一个固定端口的开发服务器，端口被占就直接失败而不是自动换端口
// （否则 Rust 侧的 devUrl 会指向一个空地址）。
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Rust 源码由 cargo 自己 watch，这里排除掉避免触发两次重载。
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    // WebView2 内核版本足够新，可以放心用现代语法。
    target: "chrome105",
    minify: "esbuild",
    sourcemap: false,
  },
});
