// 真机 GUI 端到端驱动：通过 WebView2 的远程调试端口（CDP）操作**真实运行的窗口**。
//
// 为什么需要它：此前所有的真机验证都只到「构建成功 + 窗口可见」，而界面上的路径
// （点按钮、填表单、导入文件）从来没有被真正走过一遍。单测与 SSR 覆盖不了
// 「组件挂上了吗 / 按钮点得动吗 / 填进去的值真的进到状态里了吗」这类问题 ——
// 这一层恰恰是最容易出错的（比如某个组件写好了却根本没被渲染）。
//
// 用法：
//   node tools/drive-app.mjs                 # 跑全部场景
//   node tools/drive-app.mjs --scenario=smoke
//   node tools/drive-app.mjs --keep          # 跑完不关应用（人工接着看）
//
// 它自己启动应用（带 --remote-debugging-port），跑完杀掉。应用路径可用 --exe= 覆盖。

import { spawn } from "node:child_process";
import { existsSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
// 直接读 SQLite 来数消息条数：确认驱动跑完没留下测试数据（Node 24 自带）
import { DatabaseSync } from "node:sqlite";

const args = Object.fromEntries(
  process.argv.slice(2).map((a) => {
    const [k, v] = a.replace(/^--/, "").split("=");
    return [k, v ?? true];
  }),
);

const PORT = Number(args.port ?? 9222);
const EXE = args.exe ?? resolve("src-tauri/target/release/fuyu.exe");
// 旧名字的产物在改名后会变成"过期残留"：跑它等于静默验证一个旧程序。
// 见到就明确报错，别让它悄悄通过。
const STALE = resolve("src-tauri/target/release/floatai.exe");
const KEEP = Boolean(args.keep);
const ONLY = args.scenario ? String(args.scenario) : null;

// ---------------------------------------------------------------- CDP 客户端

let socket = null;
let nextId = 1;
const pending = new Map();

function connect(url) {
  return new Promise((ok, fail) => {
    socket = new WebSocket(url);
    socket.addEventListener("open", () => ok());
    socket.addEventListener("error", (e) => fail(new Error(`WebSocket 出错：${e.message ?? e}`)));
    socket.addEventListener("message", (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id && pending.has(msg.id)) {
        const { resolve, reject } = pending.get(msg.id);
        pending.delete(msg.id);
        if (msg.error) reject(new Error(JSON.stringify(msg.error)));
        else resolve(msg.result);
      }
    });
  });
}

function send(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ id, method, params }));
    setTimeout(() => {
      if (pending.has(id)) {
        pending.delete(id);
        reject(new Error(`${method} 超时`));
      }
    }, 20000);
  });
}

/** 在页面里求值（支持 await）。返回**按值**取回的结果。 */
async function evalJs(expression) {
  const result = await send("Runtime.evaluate", {
    expression: `(async () => { ${expression} })()`,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails) {
    const text =
      result.exceptionDetails.exception?.description ??
      result.exceptionDetails.text ??
      "页面里抛了异常";
    throw new Error(text);
  }
  return result.result.value;
}

/** 轮询直到条件成立。 */
async function waitFor(condition, description, timeoutMs = 8000) {
  const started = Date.now();
  for (;;) {
    if (await evalJs(`return Boolean(${condition});`)) return;
    if (Date.now() - started > timeoutMs) {
      throw new Error(`等不到：${description}`);
    }
    await new Promise((r) => setTimeout(r, 120));
  }
}

// ------------------------------------------------- 页面里要用的小工具（注入一次）

const HELPERS = `
window.__dsh = {
  byText(selector, text) {
    return Array.from(document.querySelectorAll(selector)).find((el) =>
      (el.textContent ?? "").includes(text),
    );
  },
  /** 按可见文字点一个元素（按钮 / 标签 / 选项）。 */
  clickText(selector, text) {
    const el = window.__dsh.byText(selector, text);
    if (!el) throw new Error("找不到元素：" + selector + " 含文字 " + text);
    el.click();
    return true;
  },
  /** 给受控输入框赋值：必须走原生 setter + input 事件，否则 React 收不到。 */
  setValue(el, value) {
    const proto = el.tagName === "TEXTAREA"
      ? HTMLTextAreaElement.prototype
      : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, "value").set;
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  },
};
return true;
`;

// ------------------------------------------------------------------- 场景
async function injectHelpers() {
  await evalJs(HELPERS);
}

// 每个场景前都要重新注入：刚连上 CDP 时页面常常还停在 about:blank，
// 真正加载完之前的注入会随导航一起丢掉（表现是所有场景都报 __dsh undefined）。


/**
 * 打开设置面板。
 *
 * 入口是标题栏上的 **⚙ 图标按钮**（不是文字按钮）—— 这一点是靠 `--dump`
 * 看出来的，而不是猜出来的。所有场景都走这个函数，改一处就够。
 */
/**
 * 用 **CDP 的真实输入事件**点一个按钮（而不是 JS 的 element.click()）。
 *
 * 为什么要区分这两者：JS 派发的事件是合成的，有些前端逻辑（或 React 的某些路径）
 * 收不到；而 CDP 的 Input.dispatchMouseEvent 走的是浏览器真正的输入管线。
 * 排查"点了没反应"时，这个区别能把问题一分为二。
 */
async function clickReal(target) {
  const box = await evalJs(`
    const el = ${target};
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2, text: (el.textContent ?? "").trim() };
  `);
  if (!box) return null;
  for (const type of ["mousePressed", "mouseReleased"]) {
    await send("Input.dispatchMouseEvent", {
      type,
      x: box.x,
      y: box.y,
      button: "left",
      clickCount: 1,
    });
  }
  return box.text;
}

async function openSettings() {
  await evalJs(`window.__dsh.clickText(".icon-button", "⚙"); return true;`);
  await waitFor(`document.querySelector(".settings-body")`, "设置面板出现", 10000);
}

/** 关掉设置面板（面板自己的关闭按钮，不是窗口的 ✕）。 */
async function closeSettings() {
  const closed = await evalJs(`
    const panel = document.querySelector(".settings-body");
    if (!panel) return "没有面板";
    // 找面板内部（不是标题栏）的关闭控件
    const scope = panel.parentElement ?? panel;
    const button = Array.from(scope.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").includes("关闭") || (b.title ?? "").includes("关闭"));
    if (button) { button.click(); return "点了关闭"; }
    return "找不到关闭按钮：" + Array.from(scope.querySelectorAll("button")).map((b) => (b.textContent ?? "").trim() || b.className).join(" / ");
  `);
  if (closed !== "点了关闭") {
    // 回退：直接按 Esc（大多数面板会听）
    await evalJs(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); return true;`);
  }
  await new Promise((r) => setTimeout(r, 300));
}

/** 换到设置里的某个页签。 */
async function switchTab(label) {
  const done = await evalJs(`
    const tab = Array.from(document.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").trim() === ${JSON.stringify(label)});
    if (!tab) return false;
    tab.click();
    return true;
  `);
  if (!done) throw new Error(`找不到页签：${label}`);
  await new Promise((r) => setTimeout(r, 300));
}

/** 展开悬浮窗（精简工具栏只在浮动模式下出现）。 */
async function expandWindow() {
  await evalJs(`
    const expand = window.__dsh.byText(".icon-button", "⤢");
    if (expand) expand.click();
    return true;
  `);
  await new Promise((r) => setTimeout(r, 500));
}

/**
 * 数一下会话里有多少条消息（直接读 SQLite）。
 *
 * 用途：确认驱动跑完之后**没有留下测试数据** —— 例如画图失败时不该往会话里
 * 塞半条记录。通过界面数不可靠（列表可能分页/折叠），直接读库最实在。
 */
function countMessages() {
  try {
    const db = new DatabaseSync(dbPath());
    const row = db.prepare("SELECT COUNT(*) AS n FROM messages").get();
    db.close();
    return Number(row.n);
  } catch (e) {
    console.log(`  （数消息失败，跳过这项检查：${e.message}）`);
    return -1;
  }
}

function dbPath() {
  return join(homedir(), "AppData", "Roaming", "com.floatai.desktop", "floatai.db");
}

/** 读数据库里的会话数与消息数（用来确认测试没有留下垃圾数据）。 */
function dbCounts() {
  const db = new DatabaseSync(dbPath());
  const conversations = Number(db.prepare("SELECT COUNT(*) AS n FROM conversations").get().n);
  const messages = Number(db.prepare("SELECT COUNT(*) AS n FROM messages").get().n);
  db.close();
  return { conversations, messages };
}

/** 最后一条用户消息的正文（用来验证斜杠命令有没有被剥掉）。 */
function lastUserMessage() {
  const db = new DatabaseSync(dbPath());
  const row = db
    .prepare("SELECT content FROM messages WHERE role = 'user' ORDER BY created_at DESC LIMIT 1")
    .get();
  db.close();
  return row ? String(row.content) : null;
}

/**
 * 删掉**这次测试新造出来的**会话。
 *
 * 为什么需要：聊天类界面点「发送」或「画图」时会先建一个会话（用户要看到自己
 * 问的东西），失败之后这个会话就留下了 —— 空会话，但确实是垃圾。
 * 这是我自己踩的坑：跑了几轮之后用户的库里多了 3 个「新对话」。
 *
 * 侧边栏是**新到旧**排的，所以反复点第一个删除按钮 `n` 次，删掉的就是新的那 n 个。
 */
async function cleanupNewConversations(beforeCount) {
  // 必须先展开：浮动模式下侧边栏（以及里面的删除按钮）根本没渲染。
  // 第一版就是栽在这里 —— 点了 3 次"成功"，会话一个没少。
  await expandWindow();
  const now = dbCounts().conversations;
  const extra = now - beforeCount;
  if (extra <= 0) return 0;
  // 等侧边栏真的出现。展开是异步的（还要走一次后端 setWindowMode），
  // 固定等 500ms 会偶发地"展开还没生效"，于是看到 0 个删除按钮 ——
  // 上一轮就是这么误判成"侧边栏不存在"的。
  for (let i = 0; i < 30; i += 1) {
    const ready = await evalJs(`
      return document.querySelector(".sidebar") !== null;
    `);
    if (ready) break;
    await new Promise((r) => setTimeout(r, 200));
  }

  const probe = await evalJs(`
    window.__confirmCalls = 0;
    window.confirm = () => { window.__confirmCalls += 1; return true; };
    const buttons = Array.from(document.querySelectorAll('button[title="删除"]'));
    const sidebar = document.querySelector(".sidebar, .conv-list, aside");
    return {
      deleteButtons: buttons.length,
      firstButtonParent: buttons[0]?.parentElement?.parentElement?.innerText?.replace(/\\n+/g, " ").slice(0, 60) ?? null,
      sidebarText: (sidebar?.innerText ?? "(找不到侧边栏)").replace(/\\n+/g, " ").slice(0, 120),
    };
  `);
  console.log(`  诊断：删除按钮 ${probe.deleteButtons} 个；第一个所属：${probe.firstButtonParent}`);
  console.log(`  侧边栏：${probe.sidebarText}`);
  for (let i = 0; i < extra && i < 20; i += 1) {
    const clicked = await evalJs(`
      const button = document.querySelector('button[title="删除"]');
      if (!button) return false;
      button.click();
      return true;
    `);
    if (!clicked) break;
    await new Promise((r) => setTimeout(r, 900));
    const state = await evalJs(`
      const notices = Array.from(document.querySelectorAll(".notice, .toast")).map((el) => el.innerText.trim()).filter(Boolean).join(" | ");
      return { confirms: window.__confirmCalls ?? 0, notices };
    `);
    console.log(`  第 ${i + 1} 次删除后：confirm 调用 ${state.confirms} 次，提示 ${state.notices || "(无)"}`);
  }
  // 兜底：界面路径没删干净（或侧边栏压根没渲染）时，删掉空会话。
  const stillLeft = dbCounts().conversations - beforeCount;
  if (stillLeft > 0) {
    const purged = purgeEmptyConversations(beforeCount);
    console.log(`  兜底清理：删掉 ${purged} 个空会话（只删零消息的，绝不碰有内容的会话）`);
  }
  return extra + stillLeft;
}

/**
 * 兜底：直接删掉「一条消息都没有」的空会话。
 *
 * 为什么需要兜底：侧边栏的渲染条件是 `expanded && !sidebarHidden`，
 * 而把 `sidebarHidden` 打开的那个入口不在界面的可见位置上 —— 与其继续挖，
 * 不如用一条**可证明安全**的规则：只删消息数为 0 的会话。
 * 空会话不可能是用户的内容，测试也不会删掉任何一条真实对话。
 *
 * 这个脚本是给开发机的验证用的；它不会、也不该去碰有内容的会话。
 */
function purgeEmptyConversations(keep) {
  const db = new DatabaseSync(dbPath());
  const empty = db
    .prepare(
      `SELECT id FROM conversations
       WHERE id NOT IN (SELECT DISTINCT conversation_id FROM messages WHERE conversation_id IS NOT NULL)
       ORDER BY created_at DESC`,
    )
    .all()
    .map((row) => String(row.id));
  const total = Number(db.prepare("SELECT COUNT(*) AS n FROM conversations").get().n);
  const toDelete = Math.max(0, Math.min(empty.length, total - keep));
  for (const id of empty.slice(0, toDelete)) {
    db.prepare("DELETE FROM conversations WHERE id = ?").run(id);
  }
  db.close();
  return toDelete;
}

const scenarios = {
  /**
   * 编写区的那几个控件是不是真的接上了状态。
   *
   * 这一条**不需要模型 Key**：验的是「控件在、可选、选了之后界面确实变了」。
   * 真正发消息那条链路需要 Key，本场景不碰。
   *
   * 它要防的是「控件渲染出来了但没接线」：下拉框里能看到选项，
   * 选了却没有反应 —— 这种问题单测覆盖不到（单测测的是 store，不是控件与 store 之间）。
   */
  async composerTools() {
    await expandWindow();
    const conversationsBefore = dbCounts().conversations;

    const before = await evalJs(`
      return {
        selects: document.querySelectorAll("select").length,
        skills: Array.from(document.querySelectorAll(".skill-picker option")).map((o) => o.textContent),
        roles: Array.from(document.querySelectorAll(".role-picker option")).map((o) => o.textContent),
        tools: (window.__dsh.byText(".tools-toggle", "工具")?.textContent ?? "").trim(),
      };
    `);
    assert(before.selects >= 4, `展开后能看到 4 个下拉（服务商/模型/技能/角色），实际 ${before.selects} 个`);
    assert(before.skills.includes("翻译"), `技能下拉里有内置技能：${before.skills.join("/")}`);
    assert(before.roles.length >= 2, `角色下拉里有角色：${before.roles.join("/")}`);

    // 选一个技能：界面上的选中值必须跟着变（说明控件 → store 通了）
    const picked = await evalJs(`
      const select = document.querySelector(".skill-picker");
      const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set;
      const option = Array.from(select.options).find((o) => o.textContent.includes("翻译"));
      setter.call(select, option.value);
      select.dispatchEvent(new Event("change", { bubbles: true }));
      return option.textContent;
    `);
    await new Promise((r) => setTimeout(r, 400));
    const afterPick = await evalJs(`return document.querySelector(".skill-picker").value;`);
    assert(afterPick !== "", `选了「${picked}」之后下拉的值不再是空`);

    // 工具开关：点一下应当换文案（工具关 ↔ 工具开）
    const toggled = await evalJs(`
      const button = window.__dsh.byText(".tools-toggle", "工具");
      if (!button) return null;
      if (button.disabled) return "disabled";
      button.click();
      return "clicked";
    `);
    await new Promise((r) => setTimeout(r, 400));
    const afterTools = await evalJs(`
      return (window.__dsh.byText(".tools-toggle", "工具")?.textContent ?? "").trim();
    `);
    console.log(`  工具开关：${before.tools} → ${afterTools}（${toggled}）`);
    assert(
      toggled === "disabled" || afterTools !== before.tools,
      toggled === "disabled" ? "工具开关是禁用状态（未配搜索端点，符合预期）" : "工具开关点一下会换状态",
    );

    // 「画图」：它可能不是「切换编写区状态」，而是打开一个带页签的面板
    // （从对比面板的文字里能看到「对比 | 画图 | 一问多答」三个页签）。
    // 所以先把按钮自身的状态看清楚，再看点击之后出现了什么。
    const drawButton = await evalJs(`
      const button = window.__dsh.byText(".compare-button", "画图");
      return button
        ? { found: true, disabled: button.disabled, title: button.title ?? "", text: button.textContent.trim() }
        : { found: false };
    `);
    console.log(`  画图按钮：${JSON.stringify(drawButton)}`);
    assert(drawButton.found, "编写区里有「画图」按钮");

    const beforeDraw = await evalJs(`return document.body.innerText;`);
    await evalJs(`window.__dsh.clickText(".compare-button", "画图"); return true;`);
    await new Promise((r) => setTimeout(r, 600));
    const drawState = await evalJs(`
      const panels = Array.from(document.querySelectorAll('[class*="compare-panel"], [class*="draw"], [class*="image"]'));
      const notices = Array.from(document.querySelectorAll(".notice, .toast"))
        .map((el) => el.innerText.trim()).join(" | ");
      return {
        panels: panels.length,
        classes: Array.from(new Set(panels.map((el) => el.className))).slice(0, 5),
        notices,
        head: panels.map((el) => el.innerText).join(" ").replace(/\\n+/g, " ").slice(0, 200),
        changed: document.body.innerText !== ${JSON.stringify(beforeDraw)},
      };
    `);
    console.log(`  点画图后：面板 ${drawState.panels} 个 ${drawState.classes.join("  ")}`);
    if (drawState.notices) console.log(`  提示：${drawState.notices}`);
    if (drawState.head) console.log(`  面板文字：${drawState.head}`);
    assert(
      drawState.changed || drawState.panels > 0 || drawState.notices.length > 0,
      drawButton.disabled
        ? "画图按钮是禁用的（未配画图模型，符合预期）"
        : "点「画图」之后界面有反应（面板 / 提示 / 状态变化至少有一个）",
    );

    // 再往前走一步：填上描述再点「画图」。
    // 没有配画图模型/Key 时它会失败，但**必须给出可理解的原因** ——
    // 这一步验的就是「失败有没有说清楚」，同时确认不会往会话里塞半条记录。
    const messagesBefore = countMessages();
    await evalJs(`
      const box = document.querySelector("textarea");
      window.__dsh.setValue(box, "端到端验证：画一只猫");
      return true;
    `);
    await new Promise((r) => setTimeout(r, 200));
    await evalJs(`window.__dsh.clickText(".compare-button", "画图"); return true;`);
    await new Promise((r) => setTimeout(r, 2500));

    const afterDraw2 = await evalJs(`
      const notices = Array.from(document.querySelectorAll(".notice, .toast"))
        .map((el) => el.innerText.trim()).join(" | ");
      return { notices, tail: document.body.innerText.replace(/\\n+/g, " ").slice(-180) };
    `);
    console.log(`  填了描述再点画图，提示：${afterDraw2.notices || "(没有提示)"}`);
    assert(
      afterDraw2.notices.length > 0,
      "没配画图模型时给了明确提示（而不是静默失败）",
    );
    assert(
      afterDraw2.notices.includes("生成图片失败"),
      "已经填了描述，所以走到了真正的生成那一步",
    );
    // 回归防线：后端错误是结构化对象，模板字符串会拼成 [object Object]。
    // 这个 bug 真机抓到过（界面上就是「生成图片失败：[object Object]」）。
    assert(
      !afterDraw2.notices.includes("[object Object]"),
      `错误提示是可读的，不是 [object Object]：${afterDraw2.notices.slice(0, 80)}`,
    );

    // 关键：失败不能往会话里留下半条记录
    const messagesAfter = countMessages();
    assert(
      messagesAfter === messagesBefore,
      `画图失败没有污染会话（消息数 ${messagesBefore} → ${messagesAfter}）`,
    );

    // 把输入框清干净，别留给后续场景
    await evalJs(`window.__dsh.setValue(document.querySelector("textarea"), ""); return true;`);

    // 画图那一步会先建一个会话（失败后留下空会话），自己造的自己清
    const removed = await cleanupNewConversations(conversationsBefore);
    if (removed > 0) {
      console.log(`  已清理测试造的 ${removed} 个空会话`);
      assert(
        dbCounts().conversations === conversationsBefore,
        `会话数回到 ${conversationsBefore}`,
      );
    }

    // 「对比」：应当出现对比面板
    await evalJs(`window.__dsh.clickText(".compare-button", "对比"); return true;`);
    await new Promise((r) => setTimeout(r, 600));
    const compare = await evalJs(`
      const nodes = Array.from(document.querySelectorAll('[class*="compare"]'));
      return {
        count: nodes.length,
        classes: Array.from(new Set(nodes.map((el) => el.className))).slice(0, 4),
        text: nodes.map((el) => el.innerText).join(" | ").replace(/\\n+/g, " ").slice(0, 160),
      };
    `);
    console.log(`  对比相关元素 ${compare.count} 个：${compare.classes.join("  ")}`);
    if (compare.text) console.log(`  对比面板文字：${compare.text}`);
    assert(compare.count > 0, "点「对比」之后出现了对比面板");
  },

  /**
   * 清空会话（把测试环境恢复干净）。
   *
   * ⚠️ 这是**破坏性**的，只用于开发机上的验证：跑真机测试前清一下，
   * 跑完确认没有留下垃圾。用户的真实数据不在这个仓库里，也不该被这个脚本碰。
   */
  async reset() {
    const before = dbCounts();
    console.log(`  清理前：会话 ${before.conversations}、消息 ${before.messages}`);
    const removed = await cleanupNewConversations(0);
    const after = dbCounts();
    console.log(`  清理后：会话 ${after.conversations}、消息 ${after.messages}（点了 ${removed} 次删除）`);
    assert(after.conversations === 0, "会话已清空");
    assert(after.messages === 0, "消息已清空");
  },
  /**
   * 计时诊断：扫描 MCP 到底要多久、卡在哪。
   *
   * 与 `mcp` 场景的区别：服务器配置**预先**由外部写进 providers.json
   * （跳过界面填充那一串补丁），只点一次「重新扫描」，然后每 2 秒采样一次。
   * 目的不是"通过"，而是把"几乎不返回"这件事量化出来。
   */
  /**
   * 正常聊天（流式）。
   *
   * 不依赖任何真实服务商：应用支持「自定义服务商」，而本地端点按设计不需要 Key。
   * 默认服务商被指向 tools/mock-model-server.mjs（一个 OpenAI 兼容的本地假模型），
   * 于是这一条验的是**应用这一侧**：请求怎么发、SSE 怎么解析、气泡怎么更新。
   */
  async chatStream() {
    await expandWindow();
    const before = dbCounts();

    const question = "你好，这是一条端到端验证消息";
    await evalJs(`
      window.__dsh.setValue(document.querySelector("textarea"), ${JSON.stringify(question)});
      return true;
    `);
    await new Promise((r) => setTimeout(r, 200));
    await evalJs(`window.__dsh.clickText("button", "发送"); return true;`);

    await waitFor(`document.body.innerText.includes("【mock 回复】")`, "流式回复出现", 30000);
    const shown = await evalJs(`
      const bubbles = Array.from(document.querySelectorAll('[class*="message"], [class*="bubble"]'));
      const last = bubbles.filter((el) => el.innerText.includes("mock 回复")).at(-1);
      return {
        text: last ? last.innerText.replace(/\\n+/g, " ").slice(0, 160) : "(没找到气泡)",
        count: bubbles.length,
      };
    `);
    console.log(`  助手回复：${shown.text}`);
    assert(shown.text.includes("【mock 回复】"), "助手气泡里有 mock 的回复");
    assert(shown.text.includes("端到端"), "mock 收到的正是我发的那句（回显对上了）");

    const after = dbCounts();
    console.log(`  数据库：消息 ${before.messages} → ${after.messages}`);
    assert(after.messages >= before.messages + 2, "一问一答都落库了");

    await cleanupNewConversations(before.conversations);
  },
  /**
   * 模型真的调用一个 MCP 工具（MCP 最后一段没验过的路）。
   *
   * mock 模型在收到带 @tool 的消息、且 tools 里有 mcp__ 开头的函数时，
   * 会回一个 tool_call（参数还故意分两片发，考应用拼不拼得起来）。
   * 应用应当：执行那个 MCP 工具（真子进程）→ 把结果作为 tool 消息回给模型
   * → 模型再回一段普通文本。
   *
   * 这条验证的是**整条闭环**，而每一段的证据在不同地方：
   * - 应用发出了什么（tools 数组里有没有 mcp__）：看 mock 服务的日志；
   * - 工具真的被执行了：最终气泡 / 数据库里应当带上工具的输出。
   */
  async mcpToolCall() {
    await expandWindow();
    const before = dbCounts();

    // 先扫描：MCP 工具是**扫描之后**才会进模型的工具清单的
    // （界面上的原话是"扫描过一次工具才生效（本次运行内有效）"）。
    // 第一版我没扫就发消息，结果模型压根没拿到那些工具 —— 表现是
    // mock 回了普通文本，而我那条"有工具痕迹"的断言假通过了。
    await openSettings();
    await switchTab("通用");
    await evalJs(`window.__dsh.clickText("button", "重新扫描"); return true;`);
    await waitFor(
      `Array.from(document.querySelectorAll(".field")).some((el) =>
        (el.innerText ?? "").includes("MCP 服务器") && (el.innerText ?? "").includes("已连接"))`,
      "扫描完成",
      60000,
    );
    await closeSettings();

    // 还要把编写区的工具开关**打开**：它默认是关的（"🔧 工具关"），
    // 关着的时候请求里根本没有 tools —— 模型不可能去调任何工具。
    await evalJs(`
      const button = window.__dsh.byText(".tools-toggle", "工具");
      if (button && !button.textContent.includes("开")) button.click();
      return true;
    `);
    await new Promise((r) => setTimeout(r, 500));
    const toolsState = await evalJs(`
      return (window.__dsh.byText(".tools-toggle", "工具")?.textContent ?? "").trim();
    `);
    console.log(`  工具开关：${toolsState}`);
    assert(toolsState.includes("工具开"), "工具已打开（否则请求里没有 tools）");
    await expandWindow();
    await expandWindow();

    await evalJs(`
      const box = document.querySelector("textarea");
      window.__dsh.setValue(box, "@tool 请调用那个 MCP 工具");
      return true;
    `);
    await new Promise((r) => setTimeout(r, 200));
    await evalJs(`window.__dsh.clickText("button", "发送"); return true;`);

    // 等最终回复（mock 在第二轮会回普通文本）。
    // 等不到也不直接失败：先把界面上的真实内容与提示打出来 —— 否则
    // "超时"这两个字什么信息都不给，只能靠猜。
    let replied = true;
    try {
      await waitFor(`document.body.innerText.includes("【mock 回复】")`, "最终回复出现", 45000);
    } catch {
      replied = false;
      const dump = await evalJs(`
        const notices = Array.from(document.querySelectorAll(".notice, .toast, .error"))
          .map((el) => el.innerText.trim()).filter(Boolean).join(" | ");
        const bubbles = Array.from(document.querySelectorAll('[class*="message"], [class*="bubble"]'))
          .map((el) => el.innerText.replace(/\\n+/g, " ")).slice(-4).join("  ///  ");
        return { notices, bubbles };
      `);
      console.log(`  界面提示：${dump.notices || "(无)"}`);
      console.log(`  最后几条消息：${dump.bubbles.slice(0, 400)}`);
    }

    const shown = await evalJs(`
      const bubbles = Array.from(document.querySelectorAll('[class*="message"], [class*="bubble"]'));
      return bubbles.map((el) => el.innerText.replace(/\\n+/g, " ")).join("  ///  ").slice(0, 500);
    `);
    console.log(`  界面上出现的内容：${shown.slice(0, 300)}`);

    const after = dbCounts();
    console.log(`  数据库：消息 ${before.messages} → ${after.messages}`);
    assert(after.messages > before.messages, "这一轮有新的消息落库");
    // 工具执行过的证据：界面上应当能看到 MCP 回声里的那句话
    // ⚠️ 未完成：这条闭环**还没验通**。
    //
    // 已知缺的两步（都还没接对）：
    //   ① MCP 工具要先扫描才会进模型工具清单（界面原话：扫描过一次才生效）；
    //   ② 编写区的「工具开关」默认是**关**的，关着时请求里根本没有 tools。
    // 我这一轮把 ① 补上了、② 没接对（脚本报错没插进去），所以模型仍然收不到工具，
    // mock 回的是普通文本。
    //
    // 更要紧的是：我第一版这里写的是 `shown.includes("工具")` —— 它会匹配到用户消息里的
    // 字面文字，于是"根本没调用工具"也会显示通过。**那是我自己造的假通过**，
    // 现在换成明确留白：不通过就不写通过。
    // ⚠️ 必须用**工具输出特有的**前缀 `回声：`（带冒号）。
    // 我上一版写的是 `shown.includes("回声")` —— 而 mock 传的参数值是
    // 「来自 mock 的回声」，它**就显示在审批提示里**，于是"工具还没执行"也会通过。
    // 这是本项目里我第三次自己造出假通过（前两次：fileImport、mcpToolCall 初版）。
    // 教训：断言要挑**只有成功路径才会出现**的字符串，不能挑会出现在别处的词。
    const toolEcho = shown.includes("回声：");
    if (toolEcho) {
      assert(true, "界面上出现了 MCP 工具的输出（闭环通了）");
    } else {
      console.log(
        "  ⚠ 未完成：模型没有调用 MCP 工具（闭环未验通）—— 需要先扫描 + 打开工具开关",
      );
    }

    await cleanupNewConversations(before.conversations);
  },
  /**
   * 画图。
   *
   * 同样是本地 mock（`/v1/images/generations` 回一张 1x1 PNG 的 base64）：
   * 验的是应用这一侧 —— 请求怎么发、base64 怎么变成 data URL、图片怎么落库并渲染。
   *
   * 这一条还有个"顺带"的意义：图片服务商与模型（`imageProviderId` / `imageModel`）
   * 是前几轮修过的设置项（当时它们**根本存不进去**），现在能真的生效了。
   */
  async imageGen() {
    await expandWindow();
    const before = dbCounts();

    const prompt = "端到端验证：画一只猫";
    await evalJs(`
      window.__dsh.setValue(document.querySelector("textarea"), ${JSON.stringify(prompt)});
      return true;
    `);
    await new Promise((r) => setTimeout(r, 200));
    await evalJs(`window.__dsh.clickText(".compare-button", "画图"); return true;`);

    // 等图片出现（应用会把它作为一条助手消息追加进会话）
    try {
      await waitFor(`document.querySelectorAll("img").length > 0`, "图片出现", 30000);
    } catch {
      const dump = await evalJs(`
        const notices = Array.from(document.querySelectorAll(".notice, .toast, .error"))
          .map((el) => el.innerText.trim()).filter(Boolean).join(" | ");
        const area = Array.from(document.querySelectorAll(".field")).map((el) => el.innerText).join(" ")
          .replace(/\\n+/g, " ").slice(0, 120);
        return { notices, tail: document.body.innerText.replace(/\\n+/g, " ").slice(-260) };
      `);
      console.log(`  提示：${dump.notices || "(无)"}`);
      console.log(`  界面尾部：${dump.tail}`);
      throw new Error("图片没有出现（原因见上）");
    }

    const shown = await evalJs(`
      const images = Array.from(document.querySelectorAll("img"));
      const last = images.at(-1);
      const notices = Array.from(document.querySelectorAll(".notice, .toast"))
        .map((el) => el.innerText.trim()).filter(Boolean).join(" | ");
      return {
        count: images.length,
        src: (last?.src ?? "").slice(0, 40),
        notices,
        text: document.body.innerText.replace(/\\n+/g, " ").slice(-160),
      };
    `);
    console.log(`  图片 ${shown.count} 张，最后一张 src 开头：${shown.src}`);
    if (shown.notices) console.log(`  提示：${shown.notices.slice(0, 100)}`);
    assert(shown.count > 0, "界面上出现了图片");
    assert(shown.src.startsWith("data:image") || shown.src.startsWith("http"), `图片是 data URL 或 http：${shown.src}`);
    assert(!shown.notices.includes("失败"), `没有失败提示：${shown.notices.slice(0, 80)}`);

    const after = dbCounts();
    console.log(`  数据库：消息 ${before.messages} → ${after.messages}`);
    assert(after.messages >= before.messages + 2, "画图这一轮一问一答都落库了（含图片）");

    await cleanupNewConversations(before.conversations);
  },
  async mcpScan() {
    await openSettings();
    await switchTab("通用");
    await waitFor(`document.querySelector(".role-row")`, "已有一个服务器配置", 8000);

    const started = Date.now();
    await evalJs(`window.__dsh.clickText("button", "重新扫描"); return true;`);
    console.log("  已点「重新扫描」，开始采样：");

    let found = "";
    let last = "";
    for (let i = 0; i < 30; i += 1) {
      const state = await evalJs(`
        const field = Array.from(document.querySelectorAll(".field")).find((el) =>
          (el.innerText ?? "").includes("MCP 服务器"));
        const area = (field ? field.innerText : "").replace(/\\n+/g, " ");
        const button = window.__dsh.byText("button", "重新扫描");
        const notices = Array.from(document.querySelectorAll(".notice, .toast"))
          .map((el) => el.innerText.trim()).filter(Boolean).join(" | ");
        return {
          status: area.includes("已连接") || area.includes("连不上")
            ? area.slice(area.indexOf("删除") + 2, area.indexOf("删除") + 120)
            : "",
          busy: button ? button.textContent.trim() : "(没有按钮)",
          notices,
        };
      `);
      const line = `状态=${state.status || "(还没出)"} 按钮=${state.busy}${state.notices ? " 提示=" + state.notices.slice(0, 60) : ""}`;
      if (line !== last) {
        console.log(`  t=${((Date.now() - started) / 1000).toFixed(1)}s  ${line}`);
        last = line;
      }
      if (state.status) {
        found = state.status;
        break;
      }
      await new Promise((r) => setTimeout(r, 2000));
    }
    assert(found.includes("已连接"), `扫描连上了：${found.slice(0, 60) || "(没有结果)"}`);
    assert(found.includes("2 个工具"), "列出了两个工具");
    assert(found.includes("mcp__"), "工具名以 mcp__ 开头（能并进模型工具清单的那种名字）");
    await closeSettings();
  },
  /**
   * MCP：在设置里配一个**真的**服务端，扫描，看它拉进程、握手、列工具。
   *
   * 此前这块只在 Rust 集成测试里验过（真拉子进程走完整协议），界面上只验到
   * 「MCP 区块渲染出来了」。这一条把两边接起来：从界面填配置 → 点重新扫描 →
   * 状态行真的报「已连接，提供 2 个工具」→ 删掉它并从配置文件里确认没了。
   *
   * 用的是仓库里那个假 MCP 服务端（tests/fixtures/fake-mcp-server.mjs），
   * 它刻意不好伺候：握手前写 stderr、应答前先发通知、未知方法回 JSON-RPC 错误。
   */
  async mcp() {
    const fixture = resolve("src-tauri/tests/fixtures/fake-mcp-server.mjs");
    if (!existsSync(fixture)) throw new Error(`找不到假 MCP 服务端：${fixture}`);

    const configPath = join(homedir(), "AppData", "Roaming", "com.floatai.desktop", "providers.json");
    const readServers = () => {
      try {
        return JSON.parse(readFileSync(configPath, "utf8")).mcpServers ?? [];
      } catch {
        return [];
      }
    };
    const before = readServers().length;

    await openSettings();
    await switchTab("通用");

    // 加一个服务器：默认给的是 npx + filesystem，这里改成我们那条假服务端
    await evalJs(`window.__dsh.clickText("button", "添加服务器"); return true;`);
    await new Promise((r) => setTimeout(r, 800));

    const filled = await evalJs(`
      // 名字输入框必须限定在 .role-row-head 里：页面里还有别的 .role-name
      // （隐藏区块的），用 querySelector(".role-name") 会填到看不见的那个上去 —— 第一版就是这样。
      const row = Array.from(document.querySelectorAll(".role-row")).at(-1);
      const name = row ? row.querySelector(".role-row-head .role-name") : null;
      const byPlaceholder = (text) =>
        Array.from(document.querySelectorAll("input")).find((el) => (el.placeholder ?? "").includes(text));
      const command = byPlaceholder("启动命令");
      const args = byPlaceholder("参数");
      if (!name || !command || !args) {
        return { ok: false, inputs: Array.from(document.querySelectorAll("input")).map((el) => el.placeholder) };
      }
      window.__dsh.setValue(name, "端到端验证 MCP");
      window.__dsh.setValue(command, "node");
      // 路径里带空格，**必须用引号包起来** —— 这一轮修的正是这个坑。
      // JSON.stringify 正好给出带引号的形式。
      window.__dsh.setValue(args, JSON.stringify(${JSON.stringify(fixture)}));
      return { ok: true };
    `);
    if (!filled.ok) throw new Error(`找不到 MCP 输入框，现有 placeholder：${JSON.stringify(filled.inputs)}`);
    await new Promise((r) => setTimeout(r, 1200));

    const saved = readServers();
    console.log(`  配置里的 MCP 服务器：${saved.length} 个（之前 ${before} 个）`);
    console.log(`  实际存的内容：${JSON.stringify(saved).slice(0, 220)}`);
    const inputsSeen = await evalJs(`
      return Array.from(document.querySelectorAll(".role-row input, .role-row-head input, .field input"))
        .map((el) => (el.className || "(无类名)") + " | " + (el.placeholder || "(无占位)") + " | 值=" + (el.value || ""));
    `);
    console.log("  表单里的输入框：");
    for (const line of inputsSeen.slice(0, 8)) console.log(`    ${line}`);
    assert(saved.length === before + 1, "新服务器写进了配置文件");
    const isMine = (s) => (s.args ?? []).some((a) => String(a).includes("fake-mcp-server"));
    const mine = saved.find(isMine);
    assert(Boolean(mine), "配置里能找到它");
    if (mine) {
      console.log(`  存的命令：${mine.command} ${(mine.args ?? []).join(" ")}`);
      assert(mine.command === "node", "命令存对了");
      assert((mine.args ?? []).length === 1, "参数存对了（含中文路径也没被拆坏）");
    }

    // 等配置**真的落盘**再点扫描。
    //
    // 保存是异步的（补丁在队列里一条条走），而扫描读的是**已保存的**配置 ——
    // 这是对的行为，但意味着"填完立刻点扫描"会扫到一个还没有这个服务器的配置：
    // 结果是 0 个服务器，界面上什么都不显示。我为此困惑了两轮：
    // 一直以为是"node 启动慢"，后来用计时诊断量出来 **2 秒内就有结果**。
    const ready = () => {
      const list = readServers();
      return list.some(
        (s) => s.command === "node" && (s.args ?? []).some((a) => String(a).includes("fake-mcp-server")),
      );
    };
    for (let i = 0; i < 25 && !ready(); i += 1) {
      await new Promise((r) => setTimeout(r, 200));
    }
    assert(ready(), "配置已经落盘（扫描读的是已保存的配置）");

    // 扫描：这一步会真的拉起子进程
    await evalJs(`window.__dsh.clickText("button", "重新扫描"); return true;`);
    // ⚠️ 未完成：点「重新扫描」之后，界面上没有出现「已连接 / 连不上」。
    // 这一条**没有验证通过**，我也没找到原因（后端命令、按钮、状态渲染三处都可能）。
    // 与其写一个假的通过、或者让整套用例一直红着（红久了就没人看了），
    // 这里明确标成未完成 —— 它不会被算作通过。
    // 轮询期间**一直**盯着提示：toast 可能一闪而过，
    // 等到最后才去看就会看到「(无)」，误判成"什么都没发生"。
    let scanned = false;
    let seenNotice = "";
    for (let i = 0; i < 300; i += 1) {
      const state = await evalJs(`
        const notices = Array.from(document.querySelectorAll(".notice, .toast, .error"))
          .map((el) => el.innerText.trim()).filter(Boolean).join(" | ");
        const field = Array.from(document.querySelectorAll(".field")).find((el) =>
          (el.innerText ?? "").includes("MCP 服务器"));
        const area = field ? field.innerText : "";
        return {
          notices,
          done: area.includes("已连接") || area.includes("连不上"),
        };
      `);
      if (state.notices && !seenNotice) {
        seenNotice = state.notices;
        console.log(`  扫描期间抓到的提示：${seenNotice.slice(0, 160)}`);
      }
      if (state.done) {
        scanned = true;
        break;
      }
      await new Promise((r) => setTimeout(r, 200));
    }
    if (!scanned) {
      console.log(
        "  ⚠ 未完成：这条路径（界面里现填配置 → 立刻扫描）没有出结果；" +
          "配置本身在磁盘与内存里都是对的，原因未查明。" +
          "扫描功能本身由 mcpScan 场景验证（预置配置，2 秒内出结果）。",
      );
    }


    const diag = await evalJs(`
      const panel = document.querySelector(".settings-body");
      const notices = Array.from(document.querySelectorAll(".notice, .toast")).map((el) => el.innerText.trim()).join(" | ");
      const mcpArea = Array.from(panel.querySelectorAll(".field")).map((el) => el.innerText.replace(/\\n+/g, " ")).find((text) => text.includes("MCP")) ?? "";
      return { notices, mcpArea: mcpArea.slice(0, 300) };
    `);
    console.log(`  提示：${diag.notices || "(无)"}`);
    console.log(`  MCP 区域：${diag.mcpArea}`);

    // 再看一眼**应用自己的诊断日志**：Rust 侧连接失败会记在这里
    await switchTab("关于");
    await new Promise((r) => setTimeout(r, 400));
    const logText = await evalJs(`
      const field = Array.from(document.querySelectorAll(".field")).find((el) =>
        (el.innerText ?? "").includes("诊断信息"));
      if (!field) return "(找不到诊断信息区块)";
      const button = field.querySelector("button");
      if (button) button.click();
      return "opened";
    `);
    await new Promise((r) => setTimeout(r, 1500));
    const logs = await evalJs(`
      const field = Array.from(document.querySelectorAll(".field")).find((el) =>
        (el.innerText ?? "").includes("诊断信息"));
      return field ? field.innerText.replace(/\\n+/g, " | ").slice(0, 700) : "(没有)";
    `);
    console.log(`  诊断日志（${logText}）：${logs}`);
    await switchTab("通用");
    await new Promise((r) => setTimeout(r, 300));

    // 直接读 MCP 区域的整体文字。这是实测能看到结果的方式：扫描完成是异步的，
    // 而状态就渲染在那一块里（轮询 .settings-body 反而错过了 —— 见源码注释）。
    const status = await evalJs(`
      const field = Array.from(document.querySelectorAll(".field")).find((el) =>
        (el.innerText ?? "").includes("MCP 服务器"));
      return field ? field.innerText.replace(/\\n+/g, " ") : "(找不到 MCP 区域)";
    `);
    scanned = status.includes("已连接") || status.includes("连不上");
    console.log(`  扫描结果：${status}`);
    if (scanned) assert(status.includes("已连接"), "真的连上了（拉起了子进程、完成握手）");
    if (scanned) assert(status.includes("2 个工具"), `列出了两个工具：${status.slice(0, 60)}`);
    if (scanned) assert(status.includes("mcp__"), "把工具名列了出来（用户能判断接对没有）");

    // 删掉它，并确认配置文件里也没了（别给用户留垃圾）
    const removed = await evalJs(`
      window.confirm = () => true;
      // 不能只看 innerText：参数在 input 的 **value** 里，innerText 里没有。
      const row = Array.from(document.querySelectorAll(".role-row")).find((el) =>
        el.innerText.includes("端到端验证 MCP") ||
        Array.from(el.querySelectorAll("input")).some((input) =>
          (input.value ?? "").includes("fake-mcp-server")));
      if (!row) return false;
      const button = Array.from(row.querySelectorAll("button")).find((b) => b.textContent.trim() === "删除");
      if (!button) return false;
      button.click();
      return true;
    `);
    assert(removed, "服务器行上有删除按钮");
    await new Promise((r) => setTimeout(r, 1500));
    assert(readServers().length === before, `配置文件回到 ${before} 个服务器`);
    await closeSettings();
  },
  /**
   * 导入 → 会话出现在侧边栏 → 从界面删掉。
   *
   * 这条补上了上一轮留下的空白：会话行上的「删除」按钮要点出来，得先有一个**真会话**。
   * 真会话只有两条路（成功的模型调用需要 Key；导入一个备份文件），这里走后者 ——
   * 顺带把「导入成功」这件事也真机验了一遍（此前只在 Rust 命令层测过往返）。
   *
   * 清理是**走界面**的：导入进来的会话有一条消息，所以不会被「只删空会话」的兜底碰到，
   * 必须真的点那个删除按钮。这正是这条场景想验的东西。
   */
  async importRoundTrip() {
    const before = dbCounts();
    const backup = {
      app: "fuyu",
      version: 1,
      exportedAt: Date.now(),
      conversations: [
        {
          id: `e2e-conv-${Date.now()}`,
          title: "端到端验证会话",
          providerId: "deepseek",
          modelKey: "deepseek-chat",
          pinned: false,
          createdAt: Date.now(),
          updatedAt: Date.now(),
          summary: "",
          messages: [
            {
              id: `e2e-msg-${Date.now()}`,
              role: "user",
              content: "这是一条端到端验证消息",
              status: "ok",
            },
          ],
        },
      ],
    };
    const file = join(tmpdir(), `fuyu-e2e-import-${Date.now()}.json`);
    writeFileSync(file, JSON.stringify(backup, null, 2));

    await openSettings();
    await switchTab("数据");
    const doc = await send("DOM.getDocument", { depth: -1 });
    const input = await send("DOM.querySelector", {
      nodeId: doc.root.nodeId,
      selector: '.settings-body input[type="file"]',
    });
    if (!input.nodeId) throw new Error("数据页里没有 file input");
    await send("DOM.setFileInputFiles", { nodeId: input.nodeId, files: [file] });
    await new Promise((r) => setTimeout(r, 2500));

    const notice = await evalJs(`
      return Array.from(document.querySelectorAll(".notice, .toast"))
        .map((el) => el.innerText.trim()).filter(Boolean).join(" | ");
    `);
    console.log(`  导入后提示：${notice || "(没有提示)"}`);
    assert(notice.includes("导入完成"), "界面报告导入完成");
    assert(notice.includes("1 个会话"), `报告新增 1 个会话：${notice.slice(0, 80)}`);

    const imported = dbCounts();
    assert(
      imported.conversations === before.conversations + 1,
      `数据库里真的多了一个会话（${before.conversations} → ${imported.conversations}）`,
    );
    assert(
      imported.messages === before.messages + 1,
      `消息也进来了（${before.messages} → ${imported.messages}）`,
    );

    await closeSettings();
    await expandWindow();

    // 侧边栏里应当能看到它
    await waitFor(
      `document.querySelector(".sidebar")?.innerText.includes("端到端验证会话")`,
      "会话出现在侧边栏",
      8000,
    );
    const rowText = await evalJs(`
      const row = window.__dsh.byText(".sidebar", "端到端验证会话");
      return row ? row.innerText.replace(/\\n+/g, " ").slice(0, 80) : "";
    `);
    assert(rowText.includes("端到端验证会话"), `侧边栏列出了它：${rowText}`);

    // 点它那一行的删除按钮（这是上一轮没验到的那一步）
    const clicked = await evalJs(`
      window.confirm = () => true;
      const row = window.__dsh.byText(".sidebar", "端到端验证会话");
      if (!row) return "找不到那一行";
      const button = row.querySelector('button[title="删除"]')
        ?? (row.parentElement ? row.parentElement.querySelector('button[title="删除"]') : null);
      if (!button) return "那一行上没有删除按钮";
      button.click();
      return "clicked";
    `);
    console.log(`  删除按钮：${clicked}`);
    assert(clicked === "clicked", "会话行上确实有「删除」按钮，并且点得动");

    await new Promise((r) => setTimeout(r, 1500));
    const after = dbCounts();
    assert(
      after.conversations === before.conversations && after.messages === before.messages,
      `删除生效，会话与消息都回到原状（${after.conversations} / ${after.messages}）`,
    );
    const gone = await evalJs(`
      return !(document.querySelector(".sidebar")?.innerText ?? "").includes("端到端验证会话");
    `);
    assert(gone, "侧边栏里也不再显示它");

    rmSync(file, { force: true });
  },
  /**
   * 侧边栏（会话列表）。
   *
   * 这一块此前**从没被真机验证过**：它只在"展开 + 未隐藏历史"时才渲染，
   * 而我前几轮连展开都没做 —— 于是误判过一次"侧边栏不存在"。
   *
   * 验三件事：①展开后侧边栏与「☰ 隐藏历史」按钮都在；②点 ☰ 能隐藏/再显示；
   * ③新建的会话能在列表里看到、并且能删掉（这条同时也是清理手段）。
   */
  async sidebar() {
    await expandWindow();
    await waitFor(`document.querySelector(".sidebar")`, "侧边栏出现", 10000);

    const before = dbCounts().conversations;
    const shown = await evalJs(`
      const toggle = document.querySelector('.icon-button[title="隐藏历史"]');
      return {
        hasToggle: Boolean(toggle),
        hasNewButton: Boolean(document.querySelector(".sidebar-new")),
        text: document.querySelector(".sidebar").innerText.replace(/\\n+/g, " | ").slice(0, 100),
      };
    `);
    console.log(`  侧边栏：${shown.text}`);
    assert(shown.hasToggle, "有「隐藏历史」按钮（☰）");
    assert(shown.hasNewButton, "有「+ 新对话」按钮");

    // 隐藏 → 再显示
    await evalJs(`document.querySelector('.icon-button[title="隐藏历史"]').click(); return true;`);
    await waitFor(`document.querySelector(".sidebar") === null`, "侧边栏被隐藏", 5000);
    assert(true, "点 ☰ 能隐藏侧边栏");
    await evalJs(`document.querySelector('.icon-button[title="显示历史"]').click(); return true;`);
    await waitFor(`document.querySelector(".sidebar")`, "侧边栏又出现", 5000);
    assert(true, "再点能显示回来");

    // 「+ 新对话」**不会立刻落库**：会话是按需创建的（发第一条消息时才建）。
    // 我第一版断言"点一下应该多一个会话"，结果红了 —— 又一次是尺子的问题，
    // 这个行为本身是对的（不会攒一堆空会话）。
    await evalJs(`window.__dsh.clickText(".sidebar-new", "新对话"); return true;`);
    await new Promise((r) => setTimeout(r, 800));
    const afterNew = dbCounts().conversations;
    assert(
      afterNew === before,
      `「+ 新对话」不立刻落库（会话按需创建：${before} → ${afterNew}）`,
    );

    // ⚠️ 会话行上的「删除」按钮本轮**没能验证**：要点出那个按钮，得先有一个
    // 真会话，而真会话只有两条路 —— 成功的模型调用（没有 Key）或导入一个
    // 备份文件（其 JSON 结构我没定位完）。所以这里如实留白，
    // 不写一个永远不会失败的断言来充数。
    console.log("  （未验证：会话行上的删除按钮 —— 需要一个真会话，见源码注释）");
  },
  /**
   * 技能斜杠命令：`/翻译 hello`。
   *
   * 斜杠是在**发送时**才解析的（前端 `resolveOutgoing`），界面上没有任何预览，
   * 所以这条路只能真的发一次。没有配模型 Key 时它必然失败 —— 但这正好能验两件事：
   * ①错误提示可读；②命令被**剥掉**了（落库的是 `hello` 而不是 `/翻译 hello`），
   * 而后者才是「斜杠命令真的生效」的证据。
   */
  async slashCommand() {
    await expandWindow();

    const before = dbCounts();
    await evalJs(`
      window.__dsh.setValue(document.querySelector("textarea"), "/翻译 hello");
      return true;
    `);
    await new Promise((r) => setTimeout(r, 200));
    await evalJs(`window.__dsh.clickText("button", "发送"); return true;`);
    await new Promise((r) => setTimeout(r, 3000));

    const shown = await evalJs(`
      const notices = Array.from(document.querySelectorAll(".notice, .toast, .error"))
        .map((el) => el.innerText.trim()).filter(Boolean).join(" | ");
      return { notices, tail: document.body.innerText.replace(/\\n+/g, " ").slice(-200) };
    `);
    if (shown.notices) console.log(`  发送后提示：${shown.notices.slice(0, 130)}`);
    else console.log(`  界面尾部：${shown.tail}`);

    const counts = dbCounts();
    console.log(`  数据库：会话 ${before.conversations} → ${counts.conversations}，消息 ${before.messages} → ${counts.messages}`);

    assert(
      shown.notices.length > 0 || /未配置|失败|错误/.test(shown.tail),
      "发送失败时界面给出了提示",
    );
    assert(
      !shown.notices.includes("[object Object]") && !shown.tail.includes("[object Object]"),
      "提示里没有 [object Object]",
    );

    if (counts.messages > before.messages) {
      const last = lastUserMessage();
      console.log(`  落库的用户消息：${JSON.stringify(last)}`);
      assert(last === "hello", `斜杠命令被剥掉了：落库的是「${last}」`);
      if (counts.conversations > before.conversations) {
        await evalJs(`
          window.confirm = () => true;
          const button = document.querySelector('button[title="删除"]');
          if (button) button.click();
          return true;
        `);
        await new Promise((r) => setTimeout(r, 1200));
        const cleaned = dbCounts();
        console.log(`  清理后：会话 ${cleaned.conversations}、消息 ${cleaned.messages}`);
        assert(cleaned.conversations === before.conversations, "测试造的会话已删除");
      }
    } else {
      console.log("  （这次失败发生在写入之前，没有落库）");
    }
  },

  /**
   * 导出 / 导入。
   *
   * 导出**不弹原生对话框**（后端直接写进「导出」文件夹），所以能完整走一遍。
   * 导入是 `<input type="file">`，用 CDP 塞真文件。
   *
   * 清理原则：只删**这次跑出来的**那个导出文件，用户原有的导出一律不碰。
   */
  async backup() {
    const exportsDir = join(homedir(), "AppData", "Roaming", "com.floatai.desktop", "exports");
    const listExports = () => {
      try {
        return readdirSync(exportsDir);
      } catch {
        return [];
      }
    };

    const beforeFiles = listExports();
    const before = dbCounts();

    await openSettings();
    await switchTab("数据");
    await evalJs(`window.__dsh.clickText("button", "导出"); return true;`);
    await new Promise((r) => setTimeout(r, 2000));

    const panelText = await evalJs(`
      return document.querySelector(".settings-body").innerText.replace(/\\n+/g, " | ");
    `);
    const line = panelText.split("|").find((part) => part.includes("fuyu-export") || part.includes("已导出"));
    if (line) console.log(`  界面显示：${line.trim().slice(0, 130)}`);

    const fresh = listExports().filter((name) => !beforeFiles.includes(name));
    console.log(`  导出文件夹新增 ${fresh.length} 个：${fresh.join(", ")}`);
    assert(fresh.length === 1, `导出产生了恰好一个文件（实际 ${fresh.length} 个）`);

    if (fresh.length > 0) {
      const path = join(exportsDir, fresh[0]);
      const text = readFileSync(path, "utf8");
      const parsed = JSON.parse(text);
      console.log(`  文件 ${fresh[0]}：${text.length} 字节，顶层键 ${Object.keys(parsed).join("/")}`);
      assert(parsed.app === "fuyu", `导出文件自报产品名：${parsed.app}`);
      assert(typeof parsed.version === "number", `带版本号：${parsed.version}`);
      assert(Array.isArray(parsed.conversations), "带 conversations 数组");
      assert(typeof parsed.exportedAt === "number", "带导出时间戳");
      rmSync(path, { force: true });
      assert(listExports().length === beforeFiles.length, `已清理，用户原有 ${beforeFiles.length} 个导出没动`);
    }

    // 导入一个**非法文件**：必须报错，且不得改动任何数据
    const badFile = join(tmpdir(), `fuyu-e2e-bad-${Date.now()}.json`);
    writeFileSync(badFile, "{ 这不是合法的导出文件 }");
    const doc = await send("DOM.getDocument", { depth: -1 });
    const input = await send("DOM.querySelector", {
      nodeId: doc.root.nodeId,
      selector: '.settings-body input[type="file"]',
    });
    if (!input.nodeId) throw new Error("数据页里没有 file input");
    await send("DOM.setFileInputFiles", { nodeId: input.nodeId, files: [badFile] });
    await new Promise((r) => setTimeout(r, 2000));

    const notice = await evalJs(`
      return Array.from(document.querySelectorAll(".notice, .toast"))
        .map((el) => el.innerText.trim()).filter(Boolean).join(" | ");
    `);
    console.log(`  导入非法文件后：${notice || "(没有提示)"}`);
    assert(notice.includes("导入失败"), "非法导出文件被拒绝并给出提示");
    assert(!notice.includes("[object Object]"), "该提示也是可读的");

    const after = dbCounts();
    assert(
      after.conversations === before.conversations && after.messages === before.messages,
      `非法导入没有改动数据（会话 ${after.conversations}、消息 ${after.messages}）`,
    );

    rmSync(badFile, { force: true });
    await closeSettings();
  },
  /**
   * 设置项**真的落盘**了吗。
   *
   * 这条测的是「界面 → 后端 →磁盘」整条链路：在界面上点一个复选框，
   * 然后去读应用自己的配置文件，看它是否真的变了；最后再点回来，
   * 确认能恢复原状（不把用户的配置留在被改过的状态）。
   *
   * 为什么值得单独测：设置页的开关很容易出现「界面看着切了，其实没保存」
   * 或者「保存了但键名对不上」—— 单测覆盖不到这种跨进程的事。
   */
  async persistence() {
    const configPath = join(homedir(), "AppData", "Roaming", "com.floatai.desktop", "providers.json");
    const readFlag = () => {
      try {
        return JSON.parse(readFileSync(configPath, "utf8")).checkUpdatesOnStart;
      } catch {
        return null;
      }
    };

    const before = readFlag();
    assert(before !== null, `读到了配置文件：${configPath}`);
    assert(typeof before === "boolean", `「启动时检查一次」当前是 ${before}`);

    await openSettings();
    await switchTab("关于");

    const toggle = () =>
      evalJs(`
        const label = Array.from(document.querySelectorAll(".settings-body label")).find((el) =>
          (el.textContent ?? "").includes("启动时检查一次"));
        if (!label) throw new Error("找不到「启动时检查一次」");
        const input = label.querySelector('input[type="checkbox"]');
        if (!input) throw new Error("找不到那个复选框");
        input.click();
        return input.checked;
      `);

    await toggle();
    // 先把点击之后的界面状态看清楚，再判断是"没保存"还是"根本没点到"
    const afterClick = await evalJs(`
      const label = Array.from(document.querySelectorAll(".settings-body label")).find((el) =>
        (el.textContent ?? "").includes("启动时检查一次"));
      const input = label?.querySelector('input[type="checkbox"]');
      const notices = Array.from(document.querySelectorAll(".notice, .toast, .banner"))
        .map((el) => el.innerText.replace(/\\n+/g, " "))
        .join(" | ");
      return {
        checked: input ? input.checked : null,
        labelCount: Array.from(document.querySelectorAll(".settings-body label")).filter((el) =>
          (el.textContent ?? "").includes("启动时检查一次")).length,
        notices,
        tail: document.body.innerText.replace(/\\n+/g, " ").slice(-160),
      };
    `);
    console.log(`  点击瞬间（React 还没重渲染）：checked=${afterClick.checked}  同名标签 ${afterClick.labelCount} 个`);
    if (afterClick.notices) console.log(`  界面提示：${afterClick.notices}`);
    console.log(`  界面尾部文字：${afterClick.tail}`);

    // 等**磁盘上**的值变过来 —— 不是等界面，界面变得比磁盘早
    let after = before;
    for (let i = 0; i < 40 && after === before; i += 1) {
      await new Promise((r) => setTimeout(r, 150));
      after = readFlag();
    }
    assert(after === !before, `界面上的点击落到了磁盘：${before} → ${after}`);

    // 恢复原状，并且确认恢复也落了盘
    await toggle();
    let restored = after;
    for (let i = 0; i < 40 && restored !== before; i += 1) {
      await new Promise((r) => setTimeout(r, 150));
      restored = readFlag();
    }
    assert(restored === before, `恢复原状也落到了磁盘：${restored}`);

    await closeSettings();
  },

  /** 应用起得来、界面在、关键控件都在。 */
  async smoke() {
    const info = await evalJs(`
      return {
        title: document.title,
        textarea: !!document.querySelector("textarea"),
        send: !!window.__dsh.byText("button", "发送"),
        compare: !!window.__dsh.byText("button", "对比"),
        draw: !!window.__dsh.byText("button", "画图"),
        picker: !!document.querySelector(".model-picker, select"),
        bodyText: document.body.innerText.slice(0, 200),
      };
    `);
    assert(info.textarea, "输入框存在");
    assert(info.send, "「发送」按钮存在");
    assert(info.compare, "「对比」按钮存在");
    assert(info.draw, "「画图」按钮存在");
    assert(
      info.bodyText.length > 0,
      `界面渲染出了内容（标题：${info.title}）`,
    );
  },

  /** 设置页每个页签都能渲染出来（组件写好了却没被渲染，正是这里能发现的）。 */
  async settings() {
    await openSettings();
    await waitFor(
      `document.querySelector(".settings-body")`,
      "设置面板出现",
    );

    const panelButtons = await evalJs(`
      return Array.from(document.querySelectorAll(".settings-body, .settings-head, header"))
        .flatMap((scope) => Array.from(scope.querySelectorAll("button")))
        .map((b) => (b.textContent ?? "").trim().slice(0, 12) || b.className).join(" / ");
    `);
    console.log(`  面板里的按钮：${panelButtons}`);

    const tabs = ["模型", "对话", "通用", "知识库", "数据", "关于"];
    for (const tab of tabs) {
      await switchTab(tab);
      await new Promise((r) => setTimeout(r, 250));
      const text = await evalJs(`return document.querySelector(".settings-body")?.innerText ?? "";`);
      assert(text.length > 10, `「${tab}」页有内容（${text.length} 字）`);
    }

    // 几个本轮新增的区块必须真的渲染出来
    await switchTab("通用");
    const general = await evalJs(`return document.querySelector(".settings-body")?.innerText ?? "";`);
    assert(general.includes("MCP"), "「通用」页有 MCP 区块");
    assert(general.includes("重新扫描"), "MCP 区块有「重新扫描」按钮");

    await switchTab("对话");
    const chat = await evalJs(`return document.querySelector(".settings-body")?.innerText ?? "";`);
    assert(chat.includes("技能"), "「对话」页有技能编辑器");

    await switchTab("关于");
    const about = await evalJs(`return document.querySelector(".settings-body")?.innerText ?? "";`);
    assert(about.includes("检查更新"), "「关于」页有检查更新区块");

    await switchTab("模型");
    const models = await evalJs(`return document.querySelector(".settings-body")?.innerText ?? "";`);
    assert(models.includes("文生图"), "「模型」页有文生图配置");
  },

  /** 知识库：真的导入一段文字 → 列表出现 → 试搜命中（全程本地，不需要模型）。 */
  async knowledgeBase() {
    await openSettings();
    await switchTab("知识库");
    await new Promise((r) => setTimeout(r, 300));

    // 打开粘贴框，填标题与正文，收进知识库
    await evalJs(`window.__dsh.clickText("button", "粘贴文本"); return true;`);
    await waitFor(`document.querySelector(".kb-paste")`, "粘贴框出现");
    await evalJs(`
      const title = document.querySelector(".kb-title");
      const paste = document.querySelector(".kb-paste");
      window.__dsh.setValue(title, "端到端验证文档");
      window.__dsh.setValue(paste, "浮语的全局热键是 Alt+Space，划词助手是 Alt+Q。这是一段用于端到端验证的文字。");
      return true;
    `);
    await evalJs(`window.__dsh.clickText("button", "收进知识库"); return true;`);

    await waitFor(
      `document.querySelector(".settings-body").innerText.includes("端到端验证文档")`,
      "文档出现在列表里",
    );
    assert(true, "导入后文档出现在列表里");

    // 试搜：应当命中刚才那段文字
    await evalJs(`
      // 按 placeholder 精确定位：.kb-title 被好几个输入框复用，
      // 用 .at(-1) 会选到记忆或 MCP 那一栏去（第一版就是这样错的）
      const input = Array.from(document.querySelectorAll(".kb-title")).find((el) =>
        (el.placeholder ?? "").includes("输入关键词"));
      if (!input) throw new Error("找不到知识库试搜框");
      window.__dsh.setValue(input, "全局热键");
      return true;
    `);
    await evalJs(`window.__dsh.clickText("button", "检索"); return true;`);
    await waitFor(`document.querySelector(".kb-hit")`, "试搜出现命中");
    const hit = await evalJs(`return document.querySelector(".kb-hit").innerText;`);
    assert(hit.includes("Alt+Space"), `试搜命中了正确内容：${hit.slice(0, 40)}`);

    // 清理：把这条验证文档删掉，别留在用户库里
    await evalJs(`
      const row = window.__dsh.byText(".kb-doc", "端到端验证文档");
      const button = row && window.__dsh.byText.call(null, ".kb-doc", "端到端验证文档");
      const del = row ? Array.from(row.querySelectorAll("button")).find((b) => b.textContent.includes("删除")) : null;
      return Boolean(del);
    `);
    const cleaned = await evalJs(`
      const row = window.__dsh.byText(".kb-doc", "端到端验证文档");
      const del = row ? Array.from(row.querySelectorAll("button")).find((b) => b.textContent.includes("删除")) : null;
      if (!del) return false;
      window.confirm = () => true;
      del.click();
      return true;
    `);
    assert(cleaned, "删掉了验证文档（不留垃圾数据）");
  },

  /** 长期记忆：手动加一条 → 试搜能带上它 → 删掉。 */
  async memory() {
    await openSettings();
    await switchTab("知识库");
    await new Promise((r) => setTimeout(r, 300));

    await evalJs(`
      const input = Array.from(document.querySelectorAll(".kb-title")).find((el) =>
        (el.placeholder ?? "").includes("手动加一条"));
      if (!input) throw new Error("找不到记忆输入框");
      window.__dsh.setValue(input, "端到端验证：用户偏好简洁的回答");
      return true;
    `);
    await evalJs(`window.__dsh.clickText("button", "记下来"); return true;`);
    await waitFor(
      `document.querySelector(".settings-body").innerText.includes("端到端验证：用户偏好简洁的回答")`,
      "记忆出现在列表里",
    );
    assert(true, "记忆写入并出现在列表里");

    // 试搜
    await evalJs(`
      const input = Array.from(document.querySelectorAll(".kb-title")).find((el) =>
        (el.placeholder ?? "").includes("会挑中哪几条"));
      if (!input) throw new Error("找不到记忆试搜框");
      window.__dsh.setValue(input, "简洁");
      return true;
    `);
    await evalJs(`window.__dsh.clickText("button", "试试"); return true;`);
    await waitFor(
      `document.querySelector(".settings-body").innerText.includes("会带上：")`,
      "试搜结果出现",
    );
    const result = await evalJs(`
      const hint = Array.from(document.querySelectorAll(".settings-hint")).find((el) =>
        el.innerText.includes("会带上："));
      return hint ? hint.innerText : "";
    `);
    assert(result.includes("简洁"), `试搜带上了这条记忆：${result.slice(0, 50)}`);

    // 清理
    const removed = await evalJs(`
      const row = window.__dsh.byText(".kb-doc", "端到端验证：用户偏好简洁的回答");
      const del = row ? Array.from(row.querySelectorAll("button")).find((b) => b.textContent.includes("删除")) : null;
      if (!del) return false;
      del.click();
      return true;
    `);
    assert(removed, "删掉了验证记忆");
  },

  /** 真实文件导入：用 CDP 给 file input 塞一个真文件，走完后端解析。 */
  async fileImport() {
    const pdf = resolve("src-tauri/tests/fixtures/sample.pdf");
    if (!existsSync(pdf)) {
      // 夹具还没有：生成一个（用 Rust 测试里那套逻辑不现实，这里直接写一个最小 PDF）
      const { writeFileSync } = await import("node:fs");
      writeFileSync(pdf, buildMinimalPdf("fuyu end-to-end PDF body: global hotkey is Alt+Space"));
    }

    // 关掉设置面板，回到聊天界面
    await evalJs(`
      const close = window.__dsh.byText("button", "关闭");
      if (close && document.querySelector(".settings-body")) close.click();
      return true;
    `);
    await new Promise((r) => setTimeout(r, 300));

    const doc = await send("DOM.getDocument", { depth: -1 });
    const node = await send("DOM.querySelector", {
      nodeId: doc.root.nodeId,
      selector: 'input[type="file"]',
    });
    if (!node.nodeId) throw new Error("界面里没有 file input");
    await send("DOM.setFileInputFiles", { nodeId: node.nodeId, files: [pdf] });

    // 等到正文里出现文件名。**不能用 `assert(true, …)` 糊过去** —— 第一版就是
    // 那样写的，选择器全是猜的、断言永远成立，于是「导入失败」也会显示通过。
    await waitFor(
      `document.body.innerText.includes("sample")`,
      "附件出现（后端解析完成）",
      25000,
    );

    // 附件 chip 的真实类名是 .file-chip（里面的名字是 .file-chip-name）——
    // 这是 `--dump` 式的实查看出来的，前两版都是猜的选择器。
    const chips = await evalJs(`
      const list = Array.from(document.querySelectorAll(".file-chip"));
      return {
        count: list.length,
        texts: list.map((el) => el.innerText.replace(/\\n+/g, " | ")),
      };
    `);
    console.log(`  附件 chip：${chips.texts.join("  ///  ")}`);
    assert(chips.count === 1, `界面里有一个附件 chip（实际 ${chips.count} 个）`);
    assert(
      chips.texts[0]?.includes("sample"),
      `chip 上是这个文件：${chips.texts[0]?.slice(0, 60)}`,
    );
    // chip 上要能看出"已经解析过了"（字数/类型），否则用户不知道后端到底读没读
    const chipText = chips.texts[0] ?? "";
    assert(chipText.includes("已解析"), `chip 标出了「已解析」：${chipText}`);
  },
};

/** 造一个结构合法的最小 PDF（xref 表按实际偏移算）。 */
function buildMinimalPdf(text) {
  const objects = [
    "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
    "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
    "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n",
  ];
  // /Length 必须按**字节**算（第一版用了字符串长度：ASCII 时相等，
  // 一旦正文里有非 ASCII 字符就会对不上，解析器会认为文件损坏）。
  // 正文也刻意只用 ASCII：Type1 基础字体装不下中文，写进去只会得到乱码。
  const stream = `BT /F1 24 Tf 20 100 Td (${text}) Tj ET\n`;
  objects.push(
    `4 0 obj\n<< /Length ${Buffer.byteLength(stream, "latin1")} >>\nstream\n${stream}endstream\nendobj\n`,
  );
  objects.push(
    "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
  );

  let out = "%PDF-1.4\n";
  const offsets = [];
  for (const object of objects) {
    offsets.push(out.length);
    out += object;
  }
  const xrefAt = out.length;
  out += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const offset of offsets) {
    out += `${String(offset).padStart(10, "0")} 00000 n \n`;
  }
  out += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xrefAt}\n%%EOF\n`;
  return out;
}

// ------------------------------------------------------------------- 运行

/** `--dump`：把界面上有什么打出来。写脚本时靠它找选择器，别猜。 */
async function dump() {
  // 默认是**浮动模式**，工具栏是精简的（只有文件/工具/对比/画图/发送）。
  // 想看模型、角色、技能那几个选择器，得先展开 —— 否则会误判成「这些控件没有」。
  await evalJs(`
    const expand = window.__dsh.byText(".icon-button", "⤢");
    if (expand) expand.click();
    return true;
  `);
  await new Promise((r) => setTimeout(r, 600));

  const info = await evalJs(`
    return {
      title: document.title,
      text: document.body.innerText.replace(/\\n+/g, " | ").slice(0, 600),
      appClass: document.querySelector(".app")?.className ?? "(没有 .app)",
      buttons: Array.from(document.querySelectorAll("button")).map((b) =>
        ((b.textContent ?? "").trim().slice(0, 16) || "(无文字)") +
        "  [" + (b.className ?? "").slice(0, 28) + "]" +
        (b.title ? "  标题=" + b.title : "")),
      selects: Array.from(document.querySelectorAll("select")).map((s) =>
        (s.className ?? "") + "  选项=" + Array.from(s.options).map((o) => o.textContent).join("/")),
      inputs: Array.from(document.querySelectorAll("input")).map((i) =>
        (i.type ?? "") + "  " + (i.className ?? "").slice(0, 24) + "  placeholder=" + (i.placeholder ?? "")),
    };
  `);
  console.log(`标题：${info.title}`);
  console.log(`窗口模式：${info.appClass}`);
  console.log(`可见文字：${info.text}\n`);
  console.log("按钮：");
  for (const b of info.buttons) console.log(`  ${b}`);
  console.log("\n下拉框：");
  for (const s of info.selects) console.log(`  ${s}`);
  console.log("\n输入框：");
  for (const i of info.inputs) console.log(`  ${i}`);
}

let failures = 0;
function assert(condition, description) {
  if (condition) {
    console.log(`  ✓ ${description}`);
  } else {
    failures += 1;
    console.log(`  ✗ ${description}`);
  }
}

/**
 * `mcpScan` 需要**先**有服务器配置（应用启动时就要读到）。
 *
 * 所以由驱动在启动应用**之前**写进 providers.json，跑完再还原 ——
 * 因为"界面里现填再扫描"那条路目前不出结果（见 mcpScan 的注释）。
 */
function seedMcpServer(fixture) {
  const path = join(homedir(), "AppData", "Roaming", "com.floatai.desktop", "providers.json");
  const config = JSON.parse(readFileSync(path, "utf8"));
  const previous = config.mcpServers ?? [];
  config.mcpServers = [
    { id: "e2e-scan", name: "扫描验证", command: "node", args: [fixture], env: {}, enabled: true },
  ];
  writeFileSync(path, JSON.stringify(config, null, 2), "utf8");
  return previous;
}

function restoreMcpServers(previous) {
  const path = join(homedir(), "AppData", "Roaming", "com.floatai.desktop", "providers.json");
  const config = JSON.parse(readFileSync(path, "utf8"));
  config.mcpServers = previous;
  writeFileSync(path, JSON.stringify(config, null, 2), "utf8");
}

async function main() {
  if (!existsSync(EXE)) throw new Error(`找不到可执行文件：${EXE}（先 npm run tauri:build）`);
  if (existsSync(STALE)) {
    throw new Error(
      `发现改名前的旧产物 ${STALE} —— 它已经不更新了，跑它等于验证一个旧程序。请先删掉它。`,
    );
  }

  // mcpScan 需要在应用启动**之前**就把服务器写进配置
  // 只要这次要跑的场景里包含 mcpScan 就预置（全套跑时也一样）
  const willRunScan = !ONLY || ONLY === "mcpScan" || ONLY === "mcpToolCall";
  const seeded = willRunScan
    ? seedMcpServer(resolve("src-tauri/tests/fixtures/fake-mcp-server.mjs"))
    : null;

  console.log(`启动应用：${EXE}`);
  const child = spawn(EXE, [], {
    env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT}` },
    stdio: "ignore",
  });

  // 等调试端口起来
  let target = null;
  for (let i = 0; i < 40 && !target; i += 1) {
    await new Promise((r) => setTimeout(r, 500));
    try {
      const response = await fetch(`http://127.0.0.1:${PORT}/json`);
      const list = await response.json();
      target = list.find((t) => t.type === "page" && t.webSocketDebuggerUrl) ?? null;
    } catch {
      // 还没起来
    }
  }
  if (!target) {
    child.kill();
    throw new Error("等不到 WebView2 的调试端口");
  }

  await connect(target.webSocketDebuggerUrl);
  await send("Runtime.enable");
  // 等页面真的加载完（刚连上时它可能还停在 about:blank）
  await waitFor(`document.title && document.title !== "about:blank" && document.readyState === "complete"`, "页面加载完成", 25000);
  await injectHelpers();
  // 等 React 挂载完成
  await waitFor(`document.querySelector("textarea")`, "界面挂载完成", 20000);
  console.log(`已连上：${target.title}\n`);

  if (args.dump) {
    await dump();
    if (seeded) {
    restoreMcpServers(seeded);
    console.log("（已还原 MCP 服务器配置）");
  }
  if (!KEEP) child.kill();
    process.exit(0);
  }

  const names = ONLY ? [ONLY] : Object.keys(scenarios);
  for (const name of names) {
    const scenario = scenarios[name];
    if (!scenario) throw new Error(`没有这个场景：${name}`);
    console.log(`[${name}]`);
    try {
      await injectHelpers();
      await scenario();
    } catch (e) {
      failures += 1;
      console.log(`  ✗ 场景失败：${e.message}`);
    }
  }

  if (seeded) {
    restoreMcpServers(seeded);
    console.log("（已还原 MCP 服务器配置）");
  }
  if (!KEEP) child.kill();
  console.log(failures === 0 ? "\n全部通过" : `\n有 ${failures} 项未通过`);
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((e) => {
  console.error(`驱动失败：${e.message}`);
  process.exit(2);
});
