import React from "react";
import ReactDOM from "react-dom/client";

import App from "./App";
import { api } from "./services/api";
import {
  onQuickAction,
  onQuickActionError,
  onShortcutError,
  onWindowPrepareHide,
  onWindowShown,
  startEventBridge,
} from "./services/events";
import { bindChatEvents, useChat } from "./stores/chat";
import { applyTheme, useConfig } from "./stores/config";
import { useProviders } from "./stores/providers";
import { useUi } from "./stores/ui";
import "./styles/globals.css";

/**
 * 最外层的错误出口。**必须在 React 渲染之前装好。**
 *
 * 为什么不能写在 `index.html` 的内联 `<script>` 里：`tauri.conf.json` 的 CSP 是
 * `script-src 'self'`，**内联脚本会被直接拒绝执行**（不报错、不留痕，只是不跑）。
 * 所以这类保障必须放进模块文件 —— 模块属于 `'self'`。
 *
 * 为什么必须有它：`.app` 的初始 `opacity` 是 0（「水浮现」入场动画靠 data-phase
 * 切换才显示），而 React 在渲染期抛异常时会**卸载整棵树** —— 结果是一个
 * 「透明但仍然置顶」的窗口：用户点了图标，看到的是一片空白，
 * 既没有界面也没有报错，只能去猜。
 */
window.addEventListener("error", (event) => {
  // 只处理真正的 JS 异常；图片之类资源 404 不该弹这个面板。
  if (!event.error) return;
  showBootFailure(event.error);
});
window.addEventListener("unhandledrejection", (event) => {
  showBootFailure(event.reason);
});

/** 入场动画时长，与 CSS 里的 transition 保持一致。 */
const ENTER_MS = 380;
/** 退场动画时长。 */
const LEAVE_MS = 200;

/**
 * 退场动画的定时器句柄。
 *
 * 必须能取消：连点两下托盘图标（或快速按两次 Alt+Space）时，第一次的
 * 「隐藏」启动了 200ms 的退场定时器，第二次立刻又把窗口显示出来 ——
 * 那个**过期的**定时器随后照样调 `finish_hide`，而 Rust 侧的 `finish_hide`
 * 是无条件隐藏，于是刚亮出来的窗口又消失了。
 */
let leaveTimer = 0;

function cancelLeave() {
  if (leaveTimer) {
    window.clearTimeout(leaveTimer);
    leaveTimer = 0;
  }
}

function playEnter() {
  cancelLeave();
  useUi.getState().setPhase("entering");
  // 下一帧再切到 shown，浏览器才会为 transform / opacity 播过渡
  // （同一帧内改两次 class，起始值会被合并掉，看不到动画）。
  window.requestAnimationFrame(() => {
    window.setTimeout(() => useUi.getState().setPhase("shown"), 16);
  });
  useUi.getState().guardBlur(ENTER_MS + 320);
}

/**
 * 启动失败时把错误**画到屏幕上**。
 *
 * 为什么必须这么做：`.app` 的初始 `opacity` 是 0，只有切到 `shown` 才会显示
 * （「水浮现」入场动画）。所以启动过程中任何一次异常如果没人管，结果就是
 * **一个透明但仍然置顶的窗口** —— 用户点了图标，看到的是「什么都没发生」，
 * 既没有窗口也没有报错，只能去猜。
 *
 * 这里把 phase 直接设成 shown，再用最朴素的 DOM 写一行错误上去：
 * 不依赖 React（此时它可能根本没挂载成功）、不依赖任何 store。
 */
function showBootFailure(error: unknown) {
  const text = error instanceof Error ? `${error.message}\n${error.stack ?? ""}` : String(error);
  document.documentElement.dataset.phase = "shown";
  const root = document.getElementById("root");
  if (!root) return;
  root.textContent = "";
  const box = document.createElement("div");
  box.className = "boot-failure";
  const title = document.createElement("div");
  title.className = "boot-failure-title";
  title.textContent = "启动失败（请把这段文字反馈给开发者）";
  const body = document.createElement("pre");
  body.textContent = text;
  box.append(title, body);
  root.appendChild(box);
}

async function boot() {
  // 1) 先接上事件桥，再显示任何内容 —— 否则第一段增量可能早于监听器建立。
  //
  // 失败**不阻断**启动：事件通道坏了至少还能看历史、改设置，
  // 总好过整个窗口一片透明。
  try {
    bindChatEvents();
    await startEventBridge();
  } catch (e) {
    useUi.getState().pushNotice(`事件通道初始化失败：${e}`);
  }

  onWindowShown(() => playEnter());

  onWindowPrepareHide(() => {
    useUi.getState().setPhase("leaving");
    cancelLeave();
    leaveTimer = window.setTimeout(() => {
      leaveTimer = 0;
      // 告诉 Rust 侧「动画播完了」；迟到的兜底强制隐藏会被这一步取消。
      void api.finishHide();
      useUi.getState().setPhase("hidden");
    }, LEAVE_MS);
  });

  onShortcutError((messages) => {
    if (messages.length > 0) useUi.getState().setNotices(messages);
  });

  // 划词取词：把选中文本送进输入框；失败（没选中 / UAC 窗口）给个提示。
  onQuickAction((text) => {
    useUi.getState().injectInput(text);
  });
  onQuickActionError((message) => {
    useUi.getState().pushNotice(message);
  });

  // 2) 拉配置与服务商。
  //
  // 这几个 `load()` 内部都自己 try/catch 了，但**不能假设**：
  // 任何漏出来的异常都会让下面「显示窗口」那一步不执行，
  // 于是窗口永远停在 opacity:0 —— 用户看到的是一片空白。
  try {
    await useConfig.getState().load();
    await useProviders.getState().load();
    await useChat.getState().bootstrap();
  } catch (e) {
    useUi.getState().pushNotice(`载入配置失败：${e}`);
  }

  // 3) 恢复上次的窗口形态，再显示窗口。
  //
  // `tauri.conf.json` 里窗口是 `visible: false` 启动的：先把 React 渲染完、
  // 数据拉好，再 show，用户就不会看到一闪而过的白屏。
  const config = useConfig.getState().config;
  const expanded = config?.expanded ?? false;
  useUi.getState().setExpanded(expanded);
  applyTheme(config?.theme ?? "system");

  await api.setWindowMode(expanded).catch(() => undefined);
  await api.showWindow().catch(() => undefined);
  playEnter();

  // 4) 跟随系统主题变化。
  if (typeof window.matchMedia === "function") {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", () => {
      const current = useConfig.getState().config;
      if (current) applyTheme(current.theme);
    });
  }
}

/** 启动入口：任何异常都必须留下可见痕迹，绝不能静默变成一个透明窗口。 */
async function start() {
  try {
    await boot();
  } catch (e) {
    showBootFailure(e);
  }
}

const root = document.getElementById("root");
if (root) {
  ReactDOM.createRoot(root).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

void start();
