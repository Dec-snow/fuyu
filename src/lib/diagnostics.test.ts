// 诊断文本格式化的测试。
//
// 这段文本是用户复制出去给人看的**唯一**东西，所以它必须：包含排查需要的
// 关键字段、时间可读、并且**绝不包含**消息内容或密钥。

import { describe, expect, it } from "vitest";

import type { Diagnostics } from "../types";
import { formatBytes, formatDiagnostics, formatLogTime, hasProblems } from "./diagnostics";

function diag(over: Partial<Diagnostics> = {}): Diagnostics {
  return {
    version: "0.1.0",
    os: "windows",
    arch: "x86_64",
    dbPath: "C:/Users/x/AppData/Roaming/com.floatai.desktop/floatai.db",
    dbBytes: 12 * 1024 * 1024,
    dbVersion: 13,
    conversations: 2,
    messages: 41,
    configPath: "C:/Users/x/AppData/Roaming/com.floatai.desktop/providers.json",
    configBytes: 3200,
    providers: 9,
    webSearchConfigured: false,
    alwaysAllowedTools: [],
    logs: [],
    ...over,
  };
}

describe("formatBytes", () => {
  it("按量级选择单位", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(999)).toBe("999 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
    expect(formatBytes(12 * 1024 * 1024)).toBe("12.00 MB");
  });
});

describe("formatLogTime", () => {
  it("输出 MM-DD HH:mm:ss 这种可直接读的形式", () => {
    const at = new Date(2026, 9, 11, 14, 2, 3).getTime(); // 2026-10-11 14:02:03
    expect(formatLogTime(at)).toBe("10-11 14:02:03");
  });

  it("月/日/时/分/秒都补零", () => {
    const at = new Date(2026, 0, 5, 9, 8, 7).getTime();
    expect(formatLogTime(at)).toBe("01-05 09:08:07");
  });
});

describe("formatDiagnostics", () => {
  it("带上排查需要的每个关键字段", () => {
    const text = formatDiagnostics(diag());
    expect(text).toContain("浮语 v0.1.0");
    expect(text).toContain("windows/x86_64");
    expect(text).toContain("schema v13");
    expect(text).toContain("2 个会话 / 41 条消息");
    expect(text).toContain("9 个服务商");
    expect(text).toContain("联网搜索：未配置");
    expect(text).toContain("永久允许的工具：无");
    expect(text).toContain("最近错误：无");
  });

  it("有错误时逐条列出（含来源与时间）", () => {
    const text = formatDiagnostics(
      diag({
        logs: [
          { at: new Date(2026, 9, 11, 14, 2, 3).getTime(), source: "chat", message: "[network] 连接超时" },
          { at: new Date(2026, 9, 11, 14, 0, 0).getTime(), source: "config", message: "写盘失败" },
        ],
      }),
    );
    expect(text).toContain("最近错误（2 条，最新在前）");
    expect(text).toContain("[10-11 14:02:03] chat · [network] 连接超时");
    expect(text).toContain("[10-11 14:00:00] config · 写盘失败");
  });

  it("列出永久允许的工具（排查「为什么没弹审批」要用）", () => {
    const text = formatDiagnostics(
      diag({ alwaysAllowedTools: ["read_clipboard_text", "calculator"] }),
    );
    expect(text).toContain("read_clipboard_text、calculator");
  });

  it("联网搜索配了就说配了", () => {
    expect(formatDiagnostics(diag({ webSearchConfigured: true }))).toContain(
      "联网搜索：已配置",
    );
  });
});

describe("hasProblems", () => {
  it("有日志才算有问题；还没加载出来时不算", () => {
    expect(hasProblems(null)).toBe(false);
    expect(hasProblems(diag())).toBe(false);
    expect(hasProblems(diag({ logs: [{ at: 1, source: "chat", message: "x" }] }))).toBe(true);
  });
});
