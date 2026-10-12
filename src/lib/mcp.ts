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

/**
 * 把「参数」输入框里的一行文字拆成参数数组。
 *
 * 为什么需要引号：**路径里带空格太常见了**，而应用自己的安装目录就可能是
 * `D:\项目\桌面 AI 聊天助手应用\...`。第一版直接 `split(/\s+/)`，
 * 于是这条路径被拆成三个参数，MCP 服务端起不来 —— 我自己在真机验证时立刻踩到。
 *
 * 规则（刻意简单，不做完整 shell 解析）：
 * - 空白分隔；
 * - 双引号与单引号内的空白不拆；
 * - 引号本身不保留；
 * - 不成对的引号：把剩下的都当作一个参数（宁可给一个"太长"的参数让用户看出来，
 *   也不要静默丢掉后半截）。
 */
export function parseArgs(text: string): string[] {
  const out: string[] = [];
  let current = "";
  let quote: '"' | "'" | null = null;
  let started = false;

  for (const char of text) {
    if (quote) {
      if (char === quote) {
        quote = null;
      } else {
        current += char;
      }
      continue;
    }
    if (char === '"' || char === "'") {
      quote = char;
      started = true;
      continue;
    }
    if (/\s/.test(char)) {
      if (started) {
        out.push(current);
        current = "";
        started = false;
      }
      continue;
    }
    current += char;
    started = true;
  }
  if (started) out.push(current);
  return out;
}

/** [`parseArgs`] 的反向操作：给含空格的参数补上引号，便于显示回输入框。 */
export function formatArgs(args: string[]): string {
  return args
    .map((arg) => (/[\s"']/.test(arg) ? `"${arg.replace(/"/g, '\\"')}"` : arg))
    .join(" ");
}

/**
 * 把「基于旧快照拼出来的新列表」合并到**最新**列表上。
 *
 * 解决的是一类真实的竞态：每次编辑发的都是**整表补丁**，而补丁是按当前渲染出的
 * 列表拼的。用户连着改两个字段时，第二次补丁可能还按旧列表拼 —— 于是第一次的
 * 修改被覆盖，表现是「我明明填了名字，它却变回去了」。
 * （真机验证时我一次填了三个字段，存下来的名字就是旧的。）
 *
 * 做法：只把 `next` 相对 `prev` **真正变了**的字段应用到 `latest` 上。
 * 这样调用方一行都不用改，而并发编辑不会互相覆盖。
 *
 * 三个情况：
 * - 在 `next` 里消失的 id → 删除；
 * - 只出现在 `next` 里的 id → 新增（原样加进去）；
 * - 两边都有的 id → 逐字段比对，只搬运变了的字段。
 */
export function mergeServerEdit(
  prev: McpServerConfig[],
  next: McpServerConfig[],
  latest: McpServerConfig[],
): McpServerConfig[] {
  const prevIds = new Set(prev.map((s) => s.id));
  const nextIds = new Set(next.map((s) => s.id));

  // 只删「快照里有、新列表里没有」的那些 —— 也就是用户确实点过删除的。
  // 不能写成「保留 nextIds 里的」：那样会把 latest 里我没见过的条目一起带走
  // （别的窗口刚加的、或者上一次补丁的响应还没回到我这次渲染里）。
  // 这条是被测试抓出来的：第一版就是这么删的。
  const removed = new Set(
    prev.filter((server) => !nextIds.has(server.id)).map((server) => server.id),
  );
  const merged = latest.filter((server) => !removed.has(server.id));
  for (const item of next) {
    if (!prevIds.has(item.id)) merged.push(item);
  }

  return merged.map((item) => {
    const before = prev.find((s) => s.id === item.id);
    const after = next.find((s) => s.id === item.id);
    if (!before || !after) return item;
    const change: Partial<McpServerConfig> = {};
    for (const key of Object.keys(after) as (keyof McpServerConfig)[]) {
      if (JSON.stringify(after[key]) !== JSON.stringify(before[key])) {
        (change as Record<string, unknown>)[key] = after[key];
      }
    }
    return { ...item, ...change };
  });
}
