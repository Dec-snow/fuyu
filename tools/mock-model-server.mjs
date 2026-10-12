// 一个最小的「OpenAI 兼容」模型服务，只为本仓库的真机端到端验证而存在。
//
// 为什么要有它：验证聊天流式、一问多答、画图、以及**模型真的调用 MCP 工具**
// 这几条路，都需要一个真在跑的模型。用真实服务商要 API Key（还要花钱、要联网、
// 结果还不确定）；而这里要验的是**应用这一侧**的行为 —— 请求怎么发、流怎么解析、
// 工具调用怎么执行。一个本地的假模型足够，而且更快、更可复现。
//
// 应用侧不需要任何改动：它的「自定义服务商」本来就能指向任意 OpenAI 兼容端点，
// 而本地端点按设计**不需要填 Key**（见 src-tauri/src/api/image.rs 里的
// `is_local_endpoint` 判断）。
//
// 跑法：node tools/mock-model-server.mjs [端口]
//
// 它做的事：
//   POST /v1/chat/completions
//     - 最后一条用户消息里含 @tool，且 tools 里有 mcp__ 开头的函数
//       → 回一个 tool_call，让应用去执行那个 MCP 工具
//     - 否则 → 流式（或非流式）回一段带标记的文字
//   POST /v1/images/generations → 回一张 1x1 PNG 的 base64
//   GET  /v1/models            → 回一个模型列表
//
// 每个请求都往 stdout 打一行日志，作为"应用确实发了这样的请求"的证据。

import { createServer } from "node:http";

const PORT = Number(process.argv[2] ?? 8765);

const PNG_1X1 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==";

function readBody(req) {
  return new Promise((resolve) => {
    let raw = "";
    req.on("data", (chunk) => {
      raw += chunk;
    });
    req.on("end", () => resolve(raw));
  });
}

function sendJson(res, status, payload) {
  const body = JSON.stringify(payload);
  res.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(body),
  });
  res.end(body);
}

function sse(res) {
  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
  });
}

function chunk(delta, finish = null) {
  return `data: ${JSON.stringify({
    id: "chatcmpl-mock",
    object: "chat.completion.chunk",
    created: Math.floor(Date.now() / 1000),
    model: "mock-model",
    choices: [{ index: 0, delta, finish_reason: finish }],
  })}\n\n`;
}

/** 从请求里挑一个 MCP 工具（名字必须是 mcp__ 开头，也就是应用并进去的那种）。 */
function pickTool(body) {
  const tools = Array.isArray(body.tools) ? body.tools : [];
  const names = tools
    .map((t) => t?.function?.name ?? t?.name)
    .filter((name) => typeof name === "string");
  return {
    all: names,
    mcp: names.find((name) => name.startsWith("mcp__")) ?? null,
  };
}

const server = createServer(async (req, res) => {
  const url = req.url ?? "";
  const raw = await readBody(req);
  let body = {};
  try {
    body = raw ? JSON.parse(raw) : {};
  } catch {
    body = {};
  }

  const messages = Array.isArray(body.messages) ? body.messages : [];
  const lastUser = [...messages].reverse().find((m) => m.role === "user");
  const lastText = typeof lastUser?.content === "string" ? lastUser.content : "";
  const toolInfo = pickTool(body);

  console.log(
    `${req.method} ${url} stream=${Boolean(body.stream)} 消息=${messages.length} 工具=${
      toolInfo.all.length
    } 最后用户消息=${JSON.stringify(lastText.slice(0, 40))}`,
  );

  if (url.endsWith("/models")) {
    sendJson(res, 200, { object: "list", data: [{ id: "mock-model", object: "model" }] });
    return;
  }

  if (url.includes("/images/generations")) {
    console.log(`  → 回一张 1x1 PNG（画图请求的提示词：${JSON.stringify(body.prompt ?? "")}）`);
    sendJson(res, 200, {
      created: Math.floor(Date.now() / 1000),
      data: [{ b64_json: PNG_1X1, revised_prompt: body.prompt ?? "" }],
    });
    return;
  }

  if (!url.includes("/chat/completions")) {
    sendJson(res, 404, { error: { message: `mock 服务没有这个接口：${url}` } });
    return;
  }

  // ① 工具调用：让应用去执行一个 MCP 工具
  //
  // ⚠️ 必须判断"这一轮里已经有过工具结果了吗"：第一次跑的时候我漏了这一条，
  // 于是每一轮都再要一次工具 —— 应用就一轮轮地调下去（日志里 消息=2 → 4 → 6 → 8），
  // 永远不会给出最终回答。**假模型也得讲道理**，否则验的是它的毛病而不是应用的。
  const alreadyRanTool = messages.some((m) => m.role === "tool");
  if (lastText.includes("@tool") && toolInfo.mcp && !alreadyRanTool) {
    const args = JSON.stringify({ text: "来自 mock 的回声" });
    console.log(`  → 回一个 tool_call：${toolInfo.mcp}(${args})`);
    if (body.stream) {
      sse(res);
      res.write(chunk({ role: "assistant", content: "" }));
      res.write(
        chunk({
          tool_calls: [
            {
              index: 0,
              id: "call_mock_1",
              type: "function",
              function: { name: toolInfo.mcp, arguments: "" },
            },
          ],
        }),
      );
      // 参数分两片发：应用必须把分片拼起来才拿得到合法 JSON
      res.write(
        chunk({ tool_calls: [{ index: 0, function: { arguments: args.slice(0, 12) } }] }),
      );
      res.write(
        chunk({ tool_calls: [{ index: 0, function: { arguments: args.slice(12) } }] }),
      );
      res.write(chunk({}, "tool_calls"));
      res.write("data: [DONE]\n\n");
      res.end();
    } else {
      sendJson(res, 200, {
        id: "chatcmpl-mock",
        object: "chat.completion",
        created: Math.floor(Date.now() / 1000),
        model: "mock-model",
        choices: [
          {
            index: 0,
            message: {
              role: "assistant",
              content: "",
              tool_calls: [
                {
                  id: "call_mock_1",
                  type: "function",
                  function: { name: toolInfo.mcp, arguments: args },
                },
              ],
            },
            finish_reason: "tool_calls",
          },
        ],
      });
    }
    return;
  }

  // ② 普通回复：带一个可核对的标记
  const reply = `【mock 回复】收到「${lastText.slice(0, 24)}」，共 ${messages.length} 条消息。`;

  if (body.stream) {
    sse(res);
    res.write(chunk({ role: "assistant", content: "" }));
    for (const piece of [reply.slice(0, 10), reply.slice(10, 24), reply.slice(24)]) {
      res.write(chunk({ content: piece }));
    }
    res.write(chunk({}, "stop"));
    res.write("data: [DONE]\n\n");
    res.end();
    return;
  }

  sendJson(res, 200, {
    id: "chatcmpl-mock",
    object: "chat.completion",
    created: Math.floor(Date.now() / 1000),
    model: "mock-model",
    choices: [
      { index: 0, message: { role: "assistant", content: reply }, finish_reason: "stop" },
    ],
    usage: { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 },
  });
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`mock 模型服务已启动：http://127.0.0.1:${PORT}/v1`);
});
