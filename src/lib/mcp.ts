// MCP 状态的显示逻辑（纯函数，可单测）。

import type { McpServerConfig, McpServerStatus } from "../types";

/** 一个服务器的状态小结。 */
export function describeMcpStatus(status: McpServerStatus): string {
  if (!status.connected) {
    return status.error ? `连不上：${status.error}` : "连不上";
  }
  const count = status.tools.length;
  const version = status.serverVersion ? ` v${status.serverVersion}` : "";
  const self = status.serverName ? `（服务端自称 ${status.serverName}${version}）` : "";
  return count === 0
    ? `已连接，但它没有提供任何工具${self}`
    : `已连接，提供 ${count} 个工具${self}`;
}

/** 服务器列表里那一行：命令 + 参数。 */
export function describeMcpCommand(server: McpServerConfig): string {
  return [server.command, ...server.args].filter((p) => p.trim()).join(" ");
}

/**
 * 这条配置能不能保存。
 *
 * 与后端的校验保持一致（名字与命令必填、id 唯一）—— 前端先拦一道是为了
 * 让用户当场看到原因，而不是提交之后收到一条错误提示。
 */
export function validateMcpServer(
  server: McpServerConfig,
  all: McpServerConfig[],
): string | null {
  if (!server.name.trim()) return "给它起个名字";
  if (!server.command.trim()) return "要填启动命令，例如 npx";
  const duplicated = all.filter((s) => s.id === server.id).length > 1;
  if (duplicated) return "这个服务器的 id 与另一个重复了";
  return null;
}

/** 新服务器的初始内容（给一个常见的起点，省得对着空表单发呆）。 */
export function blankMcpServer(index: number): McpServerConfig {
  const rand =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID().slice(0, 8)
      : Math.random().toString(36).slice(2, 10);
  return {
    id: `mcp-${rand}`,
    name: `MCP 服务 ${index}`,
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-filesystem", "."],
    env: {},
    enabled: true,
  };
}
