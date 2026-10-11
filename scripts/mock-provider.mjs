/**
 * 本地假服务商 —— 手动测试用的模拟 OpenAI 服务端。
 *
 * 为什么要它：这个应用几乎所有功能都挂在「模型返回什么」上，而真服务商
 * 需要 Key、会花钱、还不肯配合你复现边界情况（分片、慢速、工具调用）。
 * 这个脚本把「模型」变成可控的：**发什么关键词，就返回什么形状的响应**。
 *
 * 它还做一件更重要的事：**把应用真正发出去的请求逐条打印出来**。
 * 界面上「看起来在转圈」时，你需要区分是前端没发、还是后端没回 ——
 * 这个日志就是那条分界线。
 *
 * 用法：
 *   node scripts/mock-provider.mjs          # 默认 8791
 *   node scripts/mock-provider.mjs 9000     # 换端口
 *
 * 然后在「设置 → 模型 → 添加自定义服务商」里填：
 *   Base URL: http://127.0.0.1:8791/v1
 *   API Key : （留空 —— 地址含 127.0.0.1 会被判为本地服务，允许无 Key）
 *   模型    : mock-model
 */

import http from "node:http";

const PORT = Number(process.argv[2] ?? 8791);
/** 慢速分片的片数与间隔（测「停止生成」用）。 */
const SLOW_CHUNKS = 20;
const SLOW_DELAY_MS = 1000;

const MODELS = ["mock-model", "mock-model-mini"];

// ---------------------------------------------------------------- 输出

const DIM = "\x1b[2m";
const CYAN = "\x1b[36m";
const YELLOW = "\x1b[33m";
const RESET = "\x1b[0m";

let seq = 0;

function log(line) {
  process.stdout.write(line + "\n");
}

/** 把一次请求压缩成一行可读事实 —— 这是排查的主力。 */
function describe(req) {
  const { body } = req;
  const messages = body.messages ?? [];
  const roles = messages.map((m) => m.role);
  const tools = body.tools ?? [];
  const lastUser = [...messages].reverse().find((m) => m.role === "user");
  const toolMessages = messages.filter((m) => m.role === "tool");

  seq += 1;
  log(`${CYAN}#${seq}${RESET} stream=${body.stream !== false} model=${body.model}`);
  log(`    roles = [${roles.join(", ")}]`);
  log(
    `    tools = ${tools.length === 0 ? "（未下发）" : tools.map((t) => t.function?.name).join(", ")}`,
  );
  log(`    system = ${JSON.stringify(messages[0]?.role === "system" ? messages[0].content : null)}`);
  log(`    最后一条用户消息 = ${JSON.stringify(lastUser?.content?.slice(0, 60) ?? null)}`);
  for (const m of toolMessages) {
    log(`    工具结果回灌 = ${JSON.stringify(String(m.content).slice(0, 120))}`);
  }
}

// ---------------------------------------------------------------- 响应

function sse(payload) {
  return Buffer.from(`data: ${JSON.stringify(payload)}\n\n`, "utf8");
}

const DONE = Buffer.from("data: [DONE]\n\n", "utf8");

function keyword(text) {
  for (const key of ["慢", "工具", "时间", "计算", "抓取", "摘要", "压缩"]) {
    if (text.includes(key)) return key;
  }
  return null;
}

function sendStream(res, { chunks, delayMs, toolCall, finish = "stop" }) {
  res.writeHead(200, {
    "Content-Type": "text/event-stream",
    "Cache-Control": "no-cache",
    Connection: "close",
  });

  // 被「停止生成」掐断时，写会抛 EPIPE/ECONNRESET —— 这本身就说明取消生效了，
  // 所以这里要显式报出来，别让它看起来像服务端崩了。
  const write = (buf) => {
    try {
      res.write(buf);
    } catch (e) {
      log(`${YELLOW}  客户端断开了连接（说明「停止」生效）：${e.code ?? e.message}${RESET}`);
      throw e;
    }
  };

  try {
    if (toolCall) {
      write(
        sse({
          choices: [{ index: 0, delta: { tool_calls: [toolCall] } }],
        }),
      );
      write(sse({ choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] }));
      write(DONE);
      res.end();
      return;
    }

    let i = 0;
    for (const piece of chunks) {
      write(sse({ choices: [{ index: 0, delta: { content: piece } }] }));
      i += 1;
      // 中途夹一个 usage 帧：有的服务端就是这么干的，前端要能容忍。
      if (i === Math.ceil(chunks.length / 2)) {
        write(sse({ choices: [], usage: { prompt_tokens: 11, completion_tokens: 7 } }));
      }
      if (delayMs > 0) {
        const until = Date.now() + delayMs;
        while (Date.now() < until) {
          // 同步 sleep：这个脚本是单线程测试工具，性能无所谓。
          Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 5);
        }
      }
    }
    write(sse({ choices: [{ index: 0, delta: {}, finish_reason: finish }] }));
    write(DONE);
    res.end();
  } catch {
    /* 客户端断开，收尾即可 */
  }
}

function handleChat(req, res) {
  const { body } = req;
  const messages = body.messages ?? [];
  const text = messages.map((m) => String(m.content ?? "")).join("\n");
  const lastUser = [...messages].reverse().find((m) => m.role === "user");
  const lastUserText = String(lastUser?.content ?? "");
  const hasToolResult = messages.some((m) => m.role === "tool");
  const toolsOffered = (body.tools ?? []).length > 0;

  // 摘要请求是非流式的。
  if (body.stream === false || text.includes("摘要") || text.includes("压缩")) {
    const content = "（假摘要）用户在和本地假模型聊天，主要在看流式、工具与取消。";
    const out = Buffer.from(
      JSON.stringify({
        choices: [{ message: { role: "assistant", content }, finish_reason: "stop" }],
        usage: { prompt_tokens: 12, completion_tokens: 9 },
      }),
    );
    res.writeHead(200, { "Content-Type": "application/json", "Content-Length": out.length });
    res.end(out);
    return;
  }

  // 已经拿到工具结果 → 收尾，并**把结果原样念一遍**：
  // 这是「工具真的执行了」最直接的证据。
  if (hasToolResult && toolsOffered) {
    const result = String(messages.filter((m) => m.role === "tool").at(-1)?.content ?? "");
    sendStream(res, {
      chunks: ["工具返回：", result.slice(0, 80), "。", "（以上是假模型念回来的结果）"],
      delayMs: 60,
    });
    return;
  }

  const key = keyword(lastUserText);

  if (key === "慢" || key === "§慢") {
    const chunks = Array.from({ length: SLOW_CHUNKS }, (_, i) => `第${i}片。`);
    log(`${DIM}   → 慢速分片：${SLOW_CHUNKS} 片 × ${SLOW_DELAY_MS}ms（用它测「停止生成」）${RESET}`);
    sendStream(res, { chunks, delayMs: SLOW_DELAY_MS });
    return;
  }

  if (toolsOffered && (key === "工具" || key === "时间")) {
    const call =
      key === "时间"
        ? {
            index: 0,
            id: "call_mock_time",
            type: "function",
            function: { name: "get_current_time", arguments: "{}" },
          }
        : {
            index: 0,
            id: "call_mock_clip",
            type: "function",
            function: { name: "read_clipboard_text", arguments: "{}" },
          };
    log(
      `${DIM}   → 返回 tool_calls：${call.function.name}` +
        `（${call.function.name === "read_clipboard_text" ? "敏感工具，应弹审批卡" : "非敏感，应直接执行"}）${RESET}`,
    );
    sendStream(res, { chunks: [], delayMs: 0, toolCall: call });
    return;
  }

  if (toolsOffered && key === "计算") {
    sendStream(res, { chunks: ["我算一下。"], delayMs: 40, toolCall: {
      index: 0,
      id: "call_mock_calc",
      type: "function",
      function: { name: "calculator", arguments: JSON.stringify({ expression: "12*34" }) },
    } });
    return;
  }

  if (toolsOffered && key === "抓取") {
    // 抓取网页是「外发型」工具：每次都要审批，且审批卡上必须能看到 URL。
    const url = (lastUserText.match(/https?:\/\/\S+/) ?? ["https://example.com"])[0];
    log(`${DIM}   → 返回 tool_calls：fetch_webpage(${url})${RESET}`);
    sendStream(res, {
      chunks: [],
      delayMs: 0,
      toolCall: {
        index: 0,
        id: "call_mock_fetch",
        type: "function",
        function: { name: "fetch_webpage", arguments: JSON.stringify({ url }) },
      },
    });
    return;
  }

  if (toolsOffered && key && key !== "摘要" && key !== "压缩") {
    log(`${DIM}   → 关键词「${key}」需要工具，但本次请求没带 tools${RESET}`);
  }

  sendStream(res, { chunks: ["你好，", "我是", "本地假模型", "。"], delayMs: 50 });
}

// ---------------------------------------------------------------- 路由

const server = http.createServer((req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);

  if (req.method === "GET" && url.pathname.endsWith("/models")) {
    log(`${DIM}GET  ${url.pathname}  → 返回 ${MODELS.length} 个模型${RESET}`);
    const out = Buffer.from(
      JSON.stringify({
        object: "list",
        data: MODELS.map((id) => ({ id, object: "model", owned_by: "mock" })),
      }),
    );
    res.writeHead(200, { "Content-Type": "application/json", "Content-Length": out.length });
    res.end(out);
    return;
  }

  if (req.method !== "POST") {
    res.writeHead(404).end();
    return;
  }

  const parts = [];
  req.on("data", (c) => parts.push(c));
  req.on("end", () => {
    let body = {};
    try {
      body = JSON.parse(Buffer.concat(parts).toString("utf8") || "{}");
    } catch {
      log(`${YELLOW}收到无法解析的请求体${RESET}`);
    }
    const wrapped = { body };
    try {
      describe(wrapped);
      handleChat(wrapped, res);
    } catch (e) {
      log(`${YELLOW}处理请求出错：${e.stack ?? e}${RESET}`);
      if (!res.headersSent) res.writeHead(500);
      res.end();
    }
  });
});

server.listen(PORT, "127.0.0.1", () => {
  log(`假服务商已启动：http://127.0.0.1:${PORT}/v1`);
  log(`  · GET  /v1/models             → ${MODELS.join(", ")}`);
  log(`  · POST /v1/chat/completions   → 按关键词返回不同形状`);
  log("");
  log("关键词：");
  log("  慢    → 20 片 × 1 秒（测「停止生成」）");
  log("  工具  → tool_calls: read_clipboard_text（敏感，应弹审批卡）");
  log("  时间  → tool_calls: get_current_time（非敏感，应直接执行）");
  log("  计算  → tool_calls: calculator(12*34)");
  log("  抓取  → tool_calls: fetch_webpage(URL)（外发型，必须每次审批）");
  log("  摘要  → 非流式返回一段假摘要");
  log("");
  log("在应用里添加服务商：Base URL = " + `http://127.0.0.1:${PORT}/v1` + "，Key 留空。");
  log(`${DIM}Ctrl+C 退出${RESET}`);
});

process.on("SIGINT", () => {
  log("\n假服务商已退出");
  server.close(() => process.exit(0));
});
