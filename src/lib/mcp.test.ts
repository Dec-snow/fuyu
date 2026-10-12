// MCP 显示逻辑的测试。
//
// 守的是「状态显示得让人误解」这一类问题：连不上却显示成「已连接 0 个工具」、
// 校验漏掉重复 id（后端会拒，但用户得先看到原因）。

import { describe, expect, it } from "vitest";

import type { McpServerConfig, McpServerStatus } from "../types";
import {
  blankMcpServer,
  describeMcpCommand,
  describeMcpStatus,
  validateMcpServer,
} from "./mcp";

const status = (over: Partial<McpServerStatus> = {}): McpServerStatus => ({
  id: "s1",
  name: "文件服务",
  connected: true,
  serverName: "fake-mcp",
  serverVersion: "9.9.9",
  tools: [],
  error: null,
  ...over,
});

const server = (over: Partial<McpServerConfig> = {}): McpServerConfig => ({
  id: "s1",
  name: "文件服务",
  command: "npx",
  args: ["-y", "server-filesystem", "/tmp"],
  env: {},
  enabled: true,
  ...over,
});

describe("describeMcpStatus", () => {
  it("连不上时显示原因", () => {
    const line = describeMcpStatus(status({ connected: false, error: "找不到命令 npx" }));
    expect(line).toContain("连不上");
    expect(line).toContain("找不到命令");
  });

  it("连上了但没工具时说清楚 —— 不能显示成「0 个工具」了事", () => {
    const line = describeMcpStatus(status({ tools: [] }));
    expect(line).toContain("没有提供任何工具");
  });

  it("正常时给出工具数与服务端自称的名字", () => {
    const line = describeMcpStatus(
      status({
        tools: [
          { rawName: "echo", name: "mcp__fake__echo", description: "" },
          { rawName: "read", name: "mcp__fake__read", description: "" },
        ],
      }),
    );
    expect(line).toContain("2 个工具");
    expect(line).toContain("fake-mcp");
  });

  it("服务端没自报名字时不留一对空括号", () => {
    const line = describeMcpStatus(status({ serverName: null, serverVersion: null }));
    expect(line).not.toContain("（）");
  });
});

describe("describeMcpCommand", () => {
  it("拼出命令与参数", () => {
    expect(describeMcpCommand(server())).toBe("npx -y server-filesystem /tmp");
  });

  it("空参数不留多余空格", () => {
    expect(describeMcpCommand(server({ args: ["", "  ", "-y"] }))).toBe("npx -y");
  });
});

describe("validateMcpServer", () => {
  it("名字与命令必填", () => {
    expect(validateMcpServer(server({ name: "  " }), [server()])).toContain("名字");
    expect(validateMcpServer(server({ command: "" }), [server()])).toContain("命令");
  });

  it("id 重复要拦下来（后端也会拒，但用户得当场看到原因）", () => {
    const duplicated = [server(), server({ name: "另一个" })];
    expect(validateMcpServer(duplicated[0], duplicated)).toContain("重复");
  });

  it("正常配置通过", () => {
    expect(validateMcpServer(server(), [server()])).toBeNull();
  });
});

describe("blankMcpServer", () => {
  it("给一个能直接跑的起点（文件系统服务），而不是空表单", () => {
    const blank = blankMcpServer(2);
    expect(blank.command).toBe("npx");
    expect(blank.args.join(" ")).toContain("server-filesystem");
    expect(blank.name).toContain("2");
    expect(blank.enabled).toBe(true);
    // id 必须唯一，否则第二个就存不进去
    expect(blankMcpServer(1).id).not.toBe(blankMcpServer(2).id);
  });
});
