// 一个最小的 MCP 服务端（stdio 传输），只为测试客户端而存在。
//
// 为什么自己写一个：真的去 `npx -y` 拉一个官方 MCP 服务端既慢又要联网，
// 而这里要验的是**协议**（按行分帧的 JSON-RPC、握手顺序、id 对应、通知要跳过），
// 一个几十行的假服务端就能把它们全测到，而且完全离线、几毫秒跑完。
//
// 它刻意模仿真实服务端会做的那几件「容易把客户端搞挂」的事：
//   1. 握手**之前**先往 stderr 写一行日志（客户端不读 stderr 就会把它堵住）；
//   2. 应答之前先发一条通知（客户端若按「读到一行就当应答」就会错位）；
//   3. 应答之间乱序（这里是反着回：先回第二条再回第一条）。
//
// 跑法：node fake-mcp-server.mjs

import { createInterface } from "node:readline";

const rl = createInterface({ input: process.stdin });

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

// ① 握手前先嚷嚷一句（真实服务端几乎都会这么干）
process.stderr.write("fake-mcp: 正在启动\n");

/** 故意延迟、乱序地回两条应答。 */
const pending = [];

rl.on("line", (line) => {
  const text = line.trim();
  if (!text) return;

  let message;
  try {
    message = JSON.parse(text);
  } catch {
    // 客户端发来的不是 JSON：真实服务端会回一条解析错误
    process.stderr.write(`fake-mcp: 收到无法解析的内容：${text}\n`);
    return;
  }

  const { id, method, params } = message;

  // 通知（没有 id）：不回
  if (id === undefined) return;

  switch (method) {
    case "initialize":
      // ② 先发一条通知，看看客户端会不会把它当成应答
      send({ jsonrpc: "2.0", method: "notifications/message", params: { level: "info" } });
      send({
        jsonrpc: "2.0",
        id,
        result: {
          protocolVersion: "2024-11-05",
          capabilities: { tools: {} },
          serverInfo: { name: "fake-mcp", version: "9.9.9" },
        },
      });
      break;

    case "tools/list":
      send({
        jsonrpc: "2.0",
        id,
        result: {
          tools: [
            {
              name: "echo",
              description: "把参数原样返回",
              inputSchema: {
                type: "object",
                properties: { text: { type: "string" } },
                required: ["text"],
              },
            },
            {
              // 带点的名字：客户端必须把它换成合法字符
              name: "fs.read_file",
              description: "假装读一个文件",
              inputSchema: { type: "object", properties: { path: { type: "string" } } },
            },
          ],
        },
      });
      break;

    case "tools/call": {
      const tool = params?.name;
      const args = params?.arguments ?? {};
      if (tool === "echo") {
        send({
          jsonrpc: "2.0",
          id,
          result: { content: [{ type: "text", text: `回声：${args.text ?? ""}` }] },
        });
      } else if (tool === "fs.read_file") {
        // 一个失败的结果：客户端要把它标成「工具执行失败」
        send({
          jsonrpc: "2.0",
          id,
          result: { isError: true, content: [{ type: "text", text: "文件不存在" }] },
        });
      } else {
        send({
          jsonrpc: "2.0",
          id,
          error: { code: -32602, message: `没有这个工具：${tool}` },
        });
      }
      break;
    }

    case "slow":
      // ③ 拖着不回，用来测客户端的超时（这里只回声一条，避免测试等太久）
      pending.push(id);
      break;

    default:
      send({
        jsonrpc: "2.0",
        id,
        error: { code: -32601, message: `没有这个方法：${method}` },
      });
  }
});
