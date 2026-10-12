// 知识库界面的纯辅助函数（可单测）。

import type { KbDocument, KbHit, KbStats } from "../types";

/** 列表里显示的来源与规模，例如「报告.docx · 12 片 · 8.4 千字」。 */
export function describeDocument(doc: KbDocument): string {
  const parts: string[] = [];
  if (doc.source.trim()) parts.push(doc.source.trim());
  parts.push(`${doc.chunkCount} 片`);
  parts.push(`${formatChars(doc.chars)} 字`);
  return parts.join(" · ");
}

/** 字数：过千用「千字」，过百万用「百万字」—— 一长串数字没人读得出来。 */
export function formatChars(chars: number): string {
  if (chars < 1000) return String(chars);
  if (chars < 1_000_000) return `${(chars / 1000).toFixed(1)} 千`;
  return `${(chars / 1_000_000).toFixed(1)} 百万`;
}

/** 概览行。空库时明确说「还没有文档」，而不是显示一排 0。 */
export function describeStats(stats: KbStats | null): string {
  if (!stats || stats.documents === 0) {
    return "知识库还是空的。导入文档后，模型就能在回答时检索它们。";
  }
  return `${stats.documents} 篇 · ${stats.chunks} 片 · 索引词 ${formatChars(stats.terms)} 个 · 共 ${formatChars(stats.chars)} 字`;
}

/** 命中的标题：`浮语手册 · 第 3 片`。 */
export function describeHit(hit: KbHit): string {
  return `${hit.title} · 第 ${hit.ordinal} 片`;
}

/**
 * 结果预览：把换行压成空格并截断。
 *
 * 截断时加省略号，**不要**悄悄截掉 —— 用户需要知道这里不是全文。
 */
export function previewText(text: string, limit = 160): string {
  const flat = text.replace(/\s+/g, " ").trim();
  if (flat.length <= limit) return flat;
  return `${flat.slice(0, limit)}…`;
}

/** 能不能收进知识库的文件（Office 走后端解析，文本走前端读取）。 */
export const KB_OFFICE_EXTENSIONS = ["docx", "xlsx", "pptx"] as const;
export const KB_TEXT_EXTENSIONS = [
  "txt",
  "md",
  "markdown",
  "csv",
  "json",
  "yaml",
  "yml",
  "log",
  "rs",
  "ts",
  "tsx",
  "js",
  "py",
  "go",
  "java",
  "c",
  "cpp",
  "h",
  "sql",
  "html",
  "css",
] as const;

export function kbFileKind(name: string): "office" | "text" | null {
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot === name.length - 1) return null;
  const ext = name.slice(dot + 1).toLowerCase();
  if ((KB_OFFICE_EXTENSIONS as readonly string[]).includes(ext)) return "office";
  if ((KB_TEXT_EXTENSIONS as readonly string[]).includes(ext)) return "text";
  return null;
}

/** 默认标题：去掉扩展名。 */
export function titleFromFileName(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(0, dot) : name;
}
