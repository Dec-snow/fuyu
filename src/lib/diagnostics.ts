// 诊断信息的格式化（纯函数，可单测）。
//
// 为什么要单独测一个「拼字符串」的函数：这段文本是**用户唯一会复制出去给人看**
// 的东西。格式乱一点、少一个关键字段，排查的人就得反过来问一轮 ——
// 而它要回答的问题恰恰是「你那边到底是什么环境」。

import type { Diagnostics } from "../types";

/** 把字节数写成人能读的形式（与设置里的显示保持一致）。 */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1048576).toFixed(2)} MB`;
}

/** 毫秒时间戳 → `MM-DD HH:mm:ss`（本地时间，够用且不引入日期库）。 */
export function formatLogTime(at: number): string {
  const d = new Date(at);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/**
 * 拼成一段可以直接粘贴出去的诊断文本。
 *
 * 刻意**不包含**：消息内容、API Key、请求正文。用户会把这段发给别人，
 * 它天然是一条外发通道 —— 只放环境与错误码这类不敏感信息。
 */
export function formatDiagnostics(d: Diagnostics): string {
  const lines: string[] = [
    `浮语 v${d.version}（${d.os}/${d.arch}）`,
    `数据库：${d.dbPath}（${formatBytes(d.dbBytes)}，schema v${d.dbVersion}，${d.conversations} 个会话 / ${d.messages} 条消息）`,
    `配置：${d.configPath}（${formatBytes(d.configBytes)}，${d.providers} 个服务商）`,
    `联网搜索：${d.webSearchConfigured ? "已配置" : "未配置"}`,
    `永久允许的工具：${d.alwaysAllowedTools.length > 0 ? d.alwaysAllowedTools.join("、") : "无"}`,
  ];

  if (d.logs.length === 0) {
    lines.push("最近错误：无");
  } else {
    lines.push(`最近错误（${d.logs.length} 条，最新在前）：`);
    for (const entry of d.logs) {
      lines.push(`  [${formatLogTime(entry.at)}] ${entry.source} · ${entry.message}`);
    }
  }

  return lines.join("\n");
}

/** 有没有值得用户注意的错误（用来决定诊断入口要不要标个点）。 */
export function hasProblems(d: Diagnostics | null): boolean {
  return !!d && d.logs.length > 0;
}
