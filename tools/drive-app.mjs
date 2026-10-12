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
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

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

const scenarios = {
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
  const info = await evalJs(`
    return {
      title: document.title,
      text: document.body.innerText.replace(/\\n+/g, " | ").slice(0, 600),
      buttons: Array.from(document.querySelectorAll("button")).map((b) =>
        ((b.textContent ?? "").trim().slice(0, 16) || "(无文字)") +
        "  [" + (b.className ?? "").slice(0, 28) + "]"),
      selects: Array.from(document.querySelectorAll("select")).map((s) =>
        (s.className ?? "") + "  选项=" + Array.from(s.options).map((o) => o.textContent).join("/")),
      inputs: Array.from(document.querySelectorAll("input")).map((i) =>
        (i.type ?? "") + "  " + (i.className ?? "").slice(0, 24) + "  placeholder=" + (i.placeholder ?? "")),
    };
  `);
  console.log(`标题：${info.title}`);
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

async function main() {
  if (!existsSync(EXE)) throw new Error(`找不到可执行文件：${EXE}（先 npm run tauri:build）`);
  if (existsSync(STALE)) {
    throw new Error(
      `发现改名前的旧产物 ${STALE} —— 它已经不更新了，跑它等于验证一个旧程序。请先删掉它。`,
    );
  }

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

  if (!KEEP) child.kill();
  console.log(failures === 0 ? "\n全部通过" : `\n有 ${failures} 项未通过`);
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((e) => {
  console.error(`驱动失败：${e.message}`);
  process.exit(2);
});
